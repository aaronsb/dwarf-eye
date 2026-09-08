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

use crate::mesh::{FLOOR_HEIGHT, MeshData, Z_SCALE};

use super::field::Field;
use super::fine::FineSurface;
use super::shade::{jitter, riser_color, to_linear, top_color};
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

    /// The survey, relieved and quantised, at a cell's centre.
    fn survey_level(&self, cx: i32, cz: i32) -> i32 {
        let cell = self.field.relieved(cx + self.window.origin.0, cz + self.window.origin.1);
        (cell.surface() - self.window.origin.2 as f32).round() as i32
    }

    /// The z-level a coarse cell stands at.
    ///
    /// At the rim of the fine map it is the neighbouring fine column's own
    /// level, so the two surfaces meet exactly; the lowest of them where
    /// several are adjacent, so the coarse never rides over fine ground.
    /// Elsewhere it is the quantised survey.
    pub fn cell_level(&self, cx: i32, cz: i32, pitch: i32) -> i32 {
        let mut snapped: Option<i32> = None;
        for (dx, dz) in [(pitch, 0), (-pitch, 0), (0, pitch), (0, -pitch)] {
            if let Some(l) = self.fine.level(cx + dx, cz + dz) {
                snapped = Some(snapped.map_or(l, |s: i32| s.min(l)));
            }
        }
        snapped.unwrap_or_else(|| self.survey_level(cx, cz))
    }

    /// The z-level of the ground over a tile, from whichever tier owns it.
    pub fn level_at(&self, tx: i32, tz: i32) -> Option<i32> {
        if let Some(level) = self.fine.level(tx, tz) {
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
        // coarse band would draw over it anyway.
        if self.fine.covers(cx, cz) {
            return;
        }
        let level = self.cell_level(cx, cz, pitch);
        let y = self.slab_top(level);
        let cell = self.field.relieved(cx + self.window.origin.0, cz + self.window.origin.1);
        let top = jitter(
            top_color(&cell, self.canopy_at(cx, cz)),
            cx + self.window.origin.0,
            cz + self.window.origin.1,
            JITTER,
        );
        let (fx0, fz0) = (x0 as f32, z0 as f32);
        let (fx1, fz1) = ((x0 + pitch) as f32, (z0 + pitch) as f32);
        mesh.push_quad(
            [[fx0, y, fz0], [fx0, y, fz1], [fx1, y, fz1], [fx1, y, fz0]],
            [0.0, 1.0, 0.0],
            to_linear(top),
        );

        let riser = to_linear(riser_color(&cell, top));
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
            push_riser(mesh, [fx0, fx1], [bottom, y], [fz0, fz1], (dx, dz), riser);
        }
    }
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
    mesh.push_quad(corners, normal, color);
}

/// The render height a smooth sample sits at, for the bands that keep one.
pub fn smooth_height(window: &Window, elevation: f32) -> f32 {
    window.height(0) + elevation * Z_SCALE + FLOOR_HEIGHT
}
