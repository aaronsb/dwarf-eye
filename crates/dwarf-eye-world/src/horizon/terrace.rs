//! The terraced heightfield: the near and middle coarse bands.
//!
//! Fine tiles step by whole z-levels, so a smooth coarse surface can only ever
//! cut through them or float over them. Inside the region details the coarse
//! bands are therefore built the way a fine floor is: one flat slab per cell at
//! a whole z-level, with a vertical riser wherever the neighbouring cell sits
//! lower. Refining the survey then buys detail in plan — the terrace edges wind
//! the way contour lines do — instead of buying a smoothness the fine map
//! cannot match.
//!
//! Three pitches, coarsening outward, so a level is never smaller than the
//! detail it carries: 6 tiles near the window, 24 further out, 48 (the survey's
//! own pitch) to the edge of the region details. Past that a level is under a
//! pixel and `mod.rs` goes back to a smooth grid off the world map.

use crate::heightfield::Ground;
use crate::mesh::{FLOOR_HEIGHT, MeshData, Z_SCALE};

use super::field::Field;
use super::fine::FineSurface;
use super::shade::{jitter, riser_color, to_linear, top_color};
use super::skin::{self, Skins};
use super::stitch;
use super::{REGION_TILE, Window};

/// Cell pitch in tiles and the radius, in tiles from the window centre, out to
/// which that pitch is used. Every pitch divides the next, so cells nest and a
/// band boundary never splits one.
pub const BANDS: [(i32, i32); 3] = [(6, 384), (24, 1152), (48, 1920)];

/// Where the terraces stop and the smooth world grid takes over: the edge of
/// the region details, and about the distance at which a z-level stops being a
/// pixel tall.
pub const FAR: i32 = BANDS[BANDS.len() - 1].1;

/// How far a riser hangs below the cell it drops to, in render units. Cells of
/// two different pitches meet at a band boundary and need not agree on a
/// level; an overhang is invisible and a crack is not.
const RISER_SKIRT: f32 = 2.0;
/// The skirt hung from a coarse cell at the rim of the fine map, in render
/// units: one tile, enough to cover the join when the two disagree by a level.
const FINE_SKIRT: f32 = 1.0;
/// The skirt hung from the outermost terrace, under which the smooth world
/// grid passes.
const OUTER_SKIRT: f32 = 4.0;

/// Vertex brightness jitter, so flat ground doesn't read as painted colour
/// next to the fine map's texture noise.
const JITTER: f32 = 0.06;

/// How much of the tile's foliage the ground itself has to carry where crowns
/// are scattered on top of it. The rest of the way out it carries all of it.
const CANOPY_UNDER_CROWNS: f32 = 0.7;

/// The coarse ground as a function of position: the survey, the fine map that
/// overrides it, and the window they are both measured against.
pub struct Terrain<'a> {
    pub field: &'a Field,
    pub fine: &'a FineSurface,
    pub window: &'a Window,
    /// Where the ground sprites the coarse bands wear sit in the atlas.
    pub skins: &'a Skins,
    /// Radii at which crowns are at full density and at none, in tiles, so the
    /// ground knows how much canopy to carry itself.
    pub crown_near: f32,
    pub crown_reach: f32,
}

/// The lower corner of the cell of `pitch` holding a render-local tile.
pub fn cell_origin(t: i32, pitch: i32) -> i32 {
    t.div_euclid(pitch) * pitch
}

/// The centre tile of that cell.
pub fn cell_centre(t: i32, pitch: i32) -> i32 {
    cell_origin(t, pitch) + pitch / 2
}

impl Terrain<'_> {
    /// Render height of the top of a slab at a z-level: a floor slab's own
    /// surface, so a coarse cell and a fine floor at the same level are the
    /// same plane.
    pub fn slab_top(&self, level: i32) -> f32 {
        level as f32 * Z_SCALE + FLOOR_HEIGHT
    }

    /// Chebyshev distance of a tile from the window centre.
    pub fn radius(&self, tx: i32, tz: i32) -> i32 {
        (tx - self.window.centre.0).abs().max((tz - self.window.centre.1).abs())
    }

    /// The band pitch over a tile, chosen by the region tile it falls in so
    /// that every cell of every pitch lies wholly inside one band.
    pub fn pitch_at(&self, tx: i32, tz: i32) -> Option<i32> {
        let r = self.radius(cell_centre(tx, REGION_TILE), cell_centre(tz, REGION_TILE));
        BANDS.iter().find(|(_, outer)| r < *outer).map(|(pitch, _)| *pitch)
    }

    /// The survey, relieved, at a cell's centre, in levels above the window's
    /// own floor and not yet rounded to one.
    pub(crate) fn survey_surface(&self, cx: i32, cz: i32) -> f32 {
        let cell = self.field.relieved(cx + self.window.origin.0, cz + self.window.origin.1);
        cell.surface() - self.window.origin.2 as f32
    }

    /// The survey, relieved and quantised, at a cell's centre.
    pub(crate) fn survey_level(&self, cx: i32, cz: i32) -> i32 {
        self.survey_surface(cx, cz).round() as i32
    }

    /// The un-quantised survey at a cell **corner**: the mean of the four cell
    /// centres that meet there.
    ///
    /// Two cells of one pitch either side of a corner average the same four
    /// centres, so they agree on it and the smooth band is one sheet. Where
    /// the pitch changes, or a stitched cell is next door, they do not, and
    /// `emit_cell` hangs a skirt over the join instead.
    fn corner_top(&self, x: i32, z: i32, pitch: i32) -> f32 {
        let h = pitch / 2;
        let total: f32 = [(-h, -h), (h, -h), (-h, h), (h, h)]
            .into_iter()
            .map(|(dx, dz)| self.survey_surface(x + dx, z + dz))
            .sum();
        total * 0.25 * Z_SCALE + FLOOR_HEIGHT
    }

    /// The z-level a coarse cell stands at.
    ///
    /// At the rim of the fine map it is the lowest fine **tile** along the
    /// sides it faces (`stitch::Terrain::stitched_level`), so the coarse never
    /// rides over fine ground and a neighbouring slab's riser hangs from the
    /// same level the stitched cell's body holds. Elsewhere it is the
    /// quantised survey.
    pub fn cell_level(&self, cx: i32, cz: i32, pitch: i32) -> i32 {
        let (x0, z0) = (cell_origin(cx, pitch), cell_origin(cz, pitch));
        if stitch::touches_fine(self.fine, x0, z0, pitch) {
            return self.stitched_level(x0, z0, pitch);
        }
        self.survey_level(cx, cz)
    }

    /// The z-level of the ground over a tile, from whichever tier owns it.
    ///
    /// Inside the fine map it is that tile's own top, not its block column's
    /// percentile: this is what a scattered tree stands on and what a riser
    /// drops to.
    pub fn level_at(&self, tx: i32, tz: i32) -> Option<i32> {
        if let Some(level) = self.fine.tile_level(tx, tz).or_else(|| self.fine.level(tx, tz)) {
            return Some(level);
        }
        let pitch = self.pitch_at(tx, tz)?;
        Some(self.cell_level(cell_centre(tx, pitch), cell_centre(tz, pitch), pitch))
    }

    /// How much of a tile's foliage the ground carries as colour: none of it
    /// where crowns stand thickest, all of it past where they stop.
    pub fn canopy_at(&self, tx: i32, tz: i32) -> f32 {
        let r = self.radius(tx, tz) as f32;
        let density = if r <= self.crown_near {
            1.0
        } else {
            (1.0 - (r - self.crown_near) / (self.crown_reach - self.crown_near)).clamp(0.0, 1.0)
        };
        1.0 - density * (1.0 - CANOPY_UNDER_CROWNS)
    }

    /// Draws every terraced band: one slab per cell, risers between cells of
    /// different level, a skirt at the rim of the fine map and at the outer
    /// edge where the world grid takes over.
    pub fn emit(&self, mesh: &mut MeshData) {
        let (cx0, cz0) = (self.window.centre.0, self.window.centre.1);
        let region_lo = (cz0 - FAR).div_euclid(REGION_TILE);
        let region_hi = (cz0 + FAR).div_euclid(REGION_TILE);
        let region_lo_x = (cx0 - FAR).div_euclid(REGION_TILE);
        let region_hi_x = (cx0 + FAR).div_euclid(REGION_TILE);
        for rz in region_lo..=region_hi {
            for rx in region_lo_x..=region_hi_x {
                let (base_x, base_z) = (rx * REGION_TILE, rz * REGION_TILE);
                let Some(pitch) = self.pitch_at(base_x, base_z) else { continue };
                let per = REGION_TILE / pitch;
                for j in 0..per {
                    for i in 0..per {
                        self.emit_cell(mesh, base_x + i * pitch, base_z + j * pitch, pitch);
                    }
                }
            }
        }
    }

    fn emit_cell(&self, mesh: &mut MeshData, x0: i32, z0: i32, pitch: i32) {
        let (cx, cz) = (x0 + pitch / 2, z0 + pitch / 2);
        // The fine map owns this ground; the block mask discards anything the
        // coarse band would draw over it anyway. Every corner, not the centre:
        // a sixteen-tile block column and a six-tile cell do not line up, and
        // dropping a cell whose middle is covered left the tiles at its edge
        // with no ground from either tier.
        let corners = [(x0, z0), (x0 + pitch - 1, z0), (x0, z0 + pitch - 1), (x0 + pitch - 1, z0 + pitch - 1)];
        if corners.iter().all(|&(x, z)| self.fine.covers(x, z)) {
            return;
        }
        let cell = self.field.relieved(cx + self.window.origin.0, cz + self.window.origin.1);
        let ground = skin::ground_of(&cell);
        let side = skin::side_of(&cell);

        // A cell that touches the fine map is stitched to it rather than laid
        // flat: the level difference is spread across the cell's own width and
        // its fine-facing edge follows the fine tiles tile by tile.
        if stitch::touches_fine(self.fine, x0, z0, pitch) {
            let top = jitter(
                top_color(&cell, self.canopy_at(cx, cz)),
                cx + self.window.origin.0,
                cz + self.window.origin.1,
                JITTER,
            );
            let riser = riser_color(&cell, top);
            self.emit_stitched(
                mesh,
                x0,
                z0,
                pitch,
                ground,
                self.skins.tint(ground, top),
                side,
                self.skins.side_tint(side, riser),
            );
            return;
        }

        let level = self.cell_level(cx, cz, pitch);
        let y = self.slab_top(level);
        let top = jitter(
            top_color(&cell, self.canopy_at(cx, cz)),
            cx + self.window.origin.0,
            cz + self.window.origin.1,
            JITTER,
        );
        let (fx0, fz0) = (x0 as f32, z0 as f32);
        let (fx1, fz1) = ((x0 + pitch) as f32, (z0 + pitch) as f32);
        // Water has no sprite: the coarse sea keeps its flat colour on the
        // white cell, the way the world grid and the rivers do.
        let (top_uv, top_color) = if cell.underwater() {
            (dwarf_eye_art::atlas::WHITE_UV, to_linear(top))
        } else {
            (self.skins.uv(ground), self.skins.tint(ground, top))
        };
        let riser = self.skins.side_tint(side, riser_color(&cell, top));
        let riser_uv = self.skins.side_uv(side);

        // Smooth mode: the fine tier is a heightfield, so the coarse band is
        // one too. The cell's four corners come from the un-quantised survey
        // and neighbouring cells of one pitch share them, so the band is a
        // sheet with no risers in it. Only the joins it cannot share — a
        // change of pitch, a stitched cell, the fine rim, the world grid —
        // still want a skirt.
        if Ground::current().smooth() {
            let corner = |x, z| self.corner_top(x, z, pitch);
            let (nw, ne, sw, se) = (
                corner(x0, z0),
                corner(x0 + pitch, z0),
                corner(x0, z0 + pitch),
                corner(x0 + pitch, z0 + pitch),
            );
            mesh.push_textured_quad(
                [[fx0, nw, fz0], [fx0, sw, fz1], [fx1, se, fz1], [fx1, ne, fz0]],
                [0.0, 1.0, 0.0],
                top_color,
                [top_uv; 4],
            );
            for (dx, dz) in [(pitch, 0), (-pitch, 0), (0, pitch), (0, -pitch)] {
                let (nx, nz) = (cx + dx, cz + dz);
                let neighbour = self.pitch_at(nx, nz);
                let hang = if neighbour.is_none() {
                    OUTER_SKIRT
                } else if self.fine.covers(nx, nz) {
                    FINE_SKIRT
                } else if neighbour != Some(pitch)
                    || stitch::touches_fine(
                        self.fine,
                        cell_origin(nx, pitch),
                        cell_origin(nz, pitch),
                        pitch,
                    )
                {
                    RISER_SKIRT
                } else {
                    // Same pitch: the corners are shared and there is no crack.
                    continue;
                };
                let (a, b) = match (dx, dz) {
                    (_, d) if d < 0 => (([fx1, ne, fz0]), ([fx0, nw, fz0])),
                    (_, d) if d > 0 => (([fx0, sw, fz1]), ([fx1, se, fz1])),
                    (d, _) if d < 0 => (([fx0, nw, fz0]), ([fx0, sw, fz1])),
                    _ => (([fx1, se, fz1]), ([fx1, ne, fz0])),
                };
                push_skirt(mesh, a, b, hang, riser, riser_uv);
            }
            return;
        }

        mesh.push_textured_quad(
            [[fx0, y, fz0], [fx0, y, fz1], [fx1, y, fz1], [fx1, y, fz0]],
            [0.0, 1.0, 0.0],
            top_color,
            [top_uv; 4],
        );

        for (dx, dz) in [(pitch, 0), (-pitch, 0), (0, pitch), (0, -pitch)] {
            let (nx, nz) = (cx + dx, cz + dz);
            // At the rim of the fine map, hang a short skirt whatever the
            // neighbouring level says: the fine floor's own rim faces cover
            // the rest, and a gap here is a hole in the world.
            let bottom = match self.level_at(nx, nz) {
                Some(neighbour) if neighbour < level => self.slab_top(neighbour) - RISER_SKIRT,
                _ if self.fine.covers(nx, nz) => y - FINE_SKIRT,
                Some(_) => continue,
                // Outside the terraced bands: the smooth world grid runs
                // under this edge, so drop a skirt over the step.
                None => y - OUTER_SKIRT,
            };
            push_riser(mesh, [fx0, fx1], [bottom, y], [fz0, fz1], (dx, dz), riser, riser_uv);
        }
    }
}

/// A skirt under one edge of a smooth cell, hanging `drop` below the two
/// corners it runs between. `a` to `b` walks the edge counter-clockwise seen
/// from above, so the quad faces outward.
fn push_skirt(
    mesh: &mut MeshData,
    a: [f32; 3],
    b: [f32; 3],
    drop: f32,
    color: [f32; 4],
    uv: [f32; 2],
) {
    let normal = stitch::outward([a[0], a[2]], [b[0], b[2]]);
    mesh.push_textured_quad(
        [b, [b[0], b[1] - drop, b[2]], [a[0], a[1] - drop, a[2]], a],
        normal,
        color,
        [uv; 4],
    );
}

/// One vertical face of a terrace step, on the side of the cell the drop is
/// on, wound so the outside faces the lower ground.
fn push_riser(
    mesh: &mut MeshData,
    [x0, x1]: [f32; 2],
    [y0, y1]: [f32; 2],
    [z0, z1]: [f32; 2],
    (dx, dz): (i32, i32),
    color: [f32; 4],
    uv: [f32; 2],
) {
    if y1 - y0 <= 0.0 {
        return;
    }
    let (corners, normal) = if dz < 0 {
        ([[x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0]], [0.0, 0.0, -1.0])
    } else if dz > 0 {
        ([[x1, y0, z1], [x1, y1, z1], [x0, y1, z1], [x0, y0, z1]], [0.0, 0.0, 1.0])
    } else if dx < 0 {
        ([[x0, y0, z1], [x0, y1, z1], [x0, y1, z0], [x0, y0, z0]], [-1.0, 0.0, 0.0])
    } else {
        ([[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]], [1.0, 0.0, 0.0])
    };
    mesh.push_textured_quad(corners, normal, color, [uv; 4]);
}

/// The render height a smooth sample sits at, for the bands that keep one.
pub fn smooth_height(window: &Window, elevation: f32) -> f32 {
    window.height(0) + elevation * Z_SCALE + FLOOR_HEIGHT
}
