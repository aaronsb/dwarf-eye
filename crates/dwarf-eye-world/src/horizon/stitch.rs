//! Where the coarse band meets the fine map: an explicit transition strip.
//!
//! A flat slab snapped to a block column's 30th percentile met the fine floor
//! only where the block was flat. Everywhere else it stood two or three levels
//! off it, with a pale riser between them and the fine window's own strata
//! showing under its rim.
//!
//! The cells that touch the fine map are therefore not slabs. Each is a
//! **triangle fan** over its own perimeter: along a side that faces fine
//! ground the perimeter carries one vertex per **tile**, at that fine tile's
//! own top level, and along every other side it carries the cell's quantised
//! level. The level difference is then spread across the width of the cell
//! instead of standing up as a riser, and the fan's fine-side vertices are the
//! fine tiles' own corners, so the two surfaces are one plane tile by tile.
//!
//! Two consequences worth stating. Consecutive perimeter vertices are shared
//! between the triangles either side of them and every perimeter vertex is a
//! corner of a triangle, so the fan has no gaps and no T-junctions inside
//! itself. And terrace quantisation still applies **beyond** the strip: a
//! stitched cell is the only place a coarse surface holds a level that is not
//! a whole one.

use crate::mesh::MeshData;
use crate::world::BLOCK;

use super::skin::{Ground, Side};
use super::{fine::FineSurface, terrace::Terrain};

/// The four sides of a cell, as `(dx, dz)` in tiles.
pub const SIDES: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

/// One cell's perimeter: the points around its edge, in order, with the height
/// each stands at and whether it came from the fine map.
///
/// Counter-clockwise seen from above in render space, which is the winding
/// `MeshData` wants for an upward face.
pub struct Perimeter {
    pub points: Vec<Point>,
    /// The fan's hub: the cell's centre at its own quantised level.
    pub centre: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// True where this height came from a fine tile rather than the survey.
    pub fine: bool,
}

/// Whether a cell touches the fine map on any side, and so wants stitching
/// rather than a slab.
pub fn touches_fine(fine: &FineSurface, x0: i32, z0: i32, pitch: i32) -> bool {
    near_fine(fine, x0, z0, pitch)
        && SIDES.iter().any(|&side| border(fine, x0, z0, pitch, side).is_some())
}

/// Whether any grounded block column is close enough for a side search to find
/// one. The search itself is `pitch` squared lookups a side, and the outermost
/// band's cells are forty-eight tiles across, so most of them have to be able
/// to say no from a handful of block-level tests.
fn near_fine(fine: &FineSurface, x0: i32, z0: i32, pitch: i32) -> bool {
    let lo_x = (x0 - pitch).div_euclid(BLOCK);
    let hi_x = (x0 + 2 * pitch).div_euclid(BLOCK);
    let lo_z = (z0 - pitch).div_euclid(BLOCK);
    let hi_z = (z0 + 2 * pitch).div_euclid(BLOCK);
    (lo_z..=hi_z).any(|bz| (lo_x..=hi_x).any(|bx| fine.covers_block(bx, bz)))
}

/// How far out one side of a cell has to look to find fine ground, and the
/// lowest fine tile it finds there. `None` where that side faces none.
///
/// The search runs outward rather than probing one row, because the two grids
/// do not line up: block columns are sixteen tiles and the near band's cells
/// are six, so the last cell before the fine map is usually a tile or two
/// short of it. It never runs past the cell's own width, so a cell only ever
/// stitches to the fine edge it is actually beside.
fn border(fine: &FineSurface, x0: i32, z0: i32, pitch: i32, side: (i32, i32)) -> Option<(i32, i32)> {
    (1..=pitch).find_map(|out| {
        let (px, pz) = probe(x0, z0, pitch, side, out, 0);
        let axis = if side.0 == 0 { (1, 0) } else { (0, 1) };
        fine.border_level(px, pz, axis, pitch).map(|level| (out, level))
    })
}

/// The tile `out` rows outside one side of a cell and `along` tiles down it.
fn probe(x0: i32, z0: i32, pitch: i32, side: (i32, i32), out: i32, along: i32) -> (i32, i32) {
    match side {
        (1, 0) => (x0 + pitch - 1 + out, z0 + along),
        (-1, 0) => (x0 - out, z0 + along),
        (0, 1) => (x0 + along, z0 + pitch - 1 + out),
        _ => (x0 + along, z0 - out),
    }
}

impl Terrain<'_> {
    /// The perimeter of a cell that touches the fine map.
    ///
    /// A side facing fine ground is walked tile by tile and each vertex takes
    /// that tile's own level; every other side keeps the cell's. A vertex
    /// shared by two sides takes the lower of what they ask for, so the outline
    /// never rides over fine ground and the two neighbouring cells agree on it.
    pub fn perimeter(&self, x0: i32, z0: i32, pitch: i32) -> Perimeter {
        let level = self.stitched_level(x0, z0, pitch);
        let y = self.slab_top(level);
        // Height of the fine tile this side is stitching to, opposite a point
        // `i` tiles along it. `border` already found how far out that is.
        let reach: Vec<Option<i32>> =
            SIDES.iter().map(|&s| border(self.fine, x0, z0, pitch, s).map(|(o, _)| o)).collect();
        let fine_at = |side: (i32, i32), i: i32| -> Option<i32> {
            let at = SIDES.iter().position(|s| *s == side)?;
            let out = reach[at]?;
            let (px, pz) = probe(x0, z0, pitch, side, out, i);
            self.fine.tile_level(px, pz)
        };

        // Walk the four sides counter-clockwise seen from +Y, sharing the
        // corner between one side and the next.
        let mut points: Vec<Point> = Vec::with_capacity((pitch as usize + 1) * 4);
        let mut push = |x: f32, z: f32, level: Option<i32>| {
            let (y, fine) = match level {
                Some(l) => (self.slab_top(l), true),
                None => (y, false),
            };
            points.push(Point { x, y, z, fine });
        };
        let walk = |i: i32, side: (i32, i32)| -> Option<i32> {
            // A point between two tiles takes the lower of them, so the
            // outline is one polyline rather than a staircase with gaps.
            let a = fine_at(side, (i - 1).clamp(0, pitch - 1));
            let b = fine_at(side, i.clamp(0, pitch - 1));
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, b) => b,
            }
        };
        let (fx0, fz0) = (x0 as f32, z0 as f32);
        for i in 0..pitch {
            push(fx0, fz0 + i as f32, walk(i, (-1, 0)));
        }
        for i in 0..pitch {
            push(fx0 + i as f32, fz0 + pitch as f32, walk(i, (0, 1)));
        }
        for i in 0..pitch {
            push(fx0 + pitch as f32, fz0 + (pitch - i) as f32, walk(pitch - i, (1, 0)));
        }
        for i in 0..pitch {
            push(fx0 + (pitch - i) as f32, fz0, walk(pitch - i, (0, -1)));
        }

        Perimeter {
            points,
            centre: [fx0 + pitch as f32 * 0.5, y, fz0 + pitch as f32 * 0.5],
        }
    }

    /// The level a stitched cell's own body sits at: the lowest fine tile on
    /// any side it faces, so the cell never rides over fine ground, and the
    /// quantised survey where it faces none.
    pub fn stitched_level(&self, x0: i32, z0: i32, pitch: i32) -> i32 {
        let mut lowest: Option<i32> = None;
        for side in SIDES {
            if let Some((_, l)) = border(self.fine, x0, z0, pitch, side) {
                lowest = Some(lowest.map_or(l, |s: i32| s.min(l)));
            }
        }
        lowest.unwrap_or_else(|| self.survey_level(x0 + pitch / 2, z0 + pitch / 2))
    }

    /// Draws one stitched cell: the fan, and a skirt hanging from every stretch
    /// of its outline that the fine map set, so the fine window's own strata are
    /// covered and no pale face shows at the seam.
    pub fn emit_stitched(
        &self,
        mesh: &mut MeshData,
        x0: i32,
        z0: i32,
        pitch: i32,
        ground: Ground,
        top: [f32; 4],
        side: Side,
        riser: [f32; 4],
    ) {
        let perimeter = self.perimeter(x0, z0, pitch);
        let uv = self.skins.uv(ground);
        let side_uv = self.skins.side_uv(side);
        let n = perimeter.points.len();
        if n < 3 {
            return;
        }

        let base = mesh.positions.len() as u32;
        mesh.positions.push(perimeter.centre);
        mesh.normals.push([0.0, 1.0, 0.0]);
        mesh.colors.push(top);
        mesh.uvs.push(uv);
        for p in &perimeter.points {
            mesh.positions.push([p.x, p.y, p.z]);
            mesh.normals.push([0.0, 1.0, 0.0]);
            mesh.colors.push(top);
            mesh.uvs.push(uv);
        }
        for i in 0..n {
            let a = base + 1 + i as u32;
            let b = base + 1 + ((i + 1) % n) as u32;
            mesh.indices.extend_from_slice(&[base, a, b]);
        }

        // The skirt: one quad per outline segment the fine map set, hanging a
        // tile below it. Covers the crack where two cells of different pitch
        // disagree, and the strata the fine map shows where its edge is on a
        // slope.
        for i in 0..n {
            let a = perimeter.points[i];
            let b = perimeter.points[(i + 1) % n];
            if !a.fine && !b.fine {
                continue;
            }
            mesh.push_textured_quad(
                [
                    [b.x, b.y, b.z],
                    [b.x, b.y - SKIRT, b.z],
                    [a.x, a.y - SKIRT, a.z],
                    [a.x, a.y, a.z],
                ],
                outward([a.x, a.z], [b.x, b.z]),
                riser,
                [side_uv; 4],
            );
        }
    }
}

/// How far a stitch skirt hangs below the outline, in render units.
const SKIRT: f32 = 1.5;

/// The outward horizontal normal of an outline segment walked counter-clockwise
/// seen from above: turn the direction of travel to its right.
fn outward([ax, az]: [f32; 2], [bx, bz]: [f32; 2]) -> [f32; 3] {
    let (dx, dz) = (bx - ax, bz - az);
    let len = (dx * dx + dz * dz).sqrt().max(1e-6);
    [-dz / len, 0.0, dx / len]
}

/// The area a perimeter's fan covers, seen from above. The fan is watertight
/// exactly when this is the cell's own area.
pub fn fan_area(perimeter: &Perimeter) -> f32 {
    let n = perimeter.points.len();
    let (cx, cz) = (perimeter.centre[0], perimeter.centre[2]);
    (0..n)
        .map(|i| {
            let a = perimeter.points[i];
            let b = perimeter.points[(i + 1) % n];
            ((a.x - cx) * (b.z - cz) - (b.x - cx) * (a.z - cz)) * 0.5
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::horizon::field::{Cell, Field};
    use crate::horizon::skin::Skins;
    use crate::horizon::{Window, scatter};

    const SKINS: Skins = Skins::none();

    fn flat(elevation: f32) -> Field {
        let mut field = Field::default();
        for ry in -10..10 {
            for rx in -10..10 {
                field.insert(
                    rx,
                    ry,
                    Cell {
                        elevation,
                        water: elevation - 30.0,
                        vegetation: 60.0,
                        rainfall: 50.0,
                        drainage: 40.0,
                        snow: 0.0,
                        ground: None,
                        detail: true,
                    },
                );
            }
        }
        field
    }

    fn window() -> Window {
        Window { x0: 0, y0: 0, origin: (0, 0, 100), centre: (8, 8) }
    }

    fn terrain<'a>(field: &'a Field, fine: &'a FineSurface, window: &'a Window) -> Terrain<'a> {
        Terrain {
            field,
            fine,
            window,
            skins: &SKINS,
            crown_near: scatter::NEAR,
            crown_reach: scatter::REACH,
        }
    }

    /// A block column whose tiles step down from 40 to 35 across it, so a
    /// percentile and the tiles themselves disagree.
    fn stepped() -> FineSurface {
        let tiles: Vec<((i32, i32), i32)> = (0..16)
            .flat_map(|y| (0..16).map(move |x| ((x, y), 40 - x / 3)))
            .collect();
        FineSurface::with_tiles(&[((0, 0), 39)], &tiles)
    }

    /// The strip's fine side is the fine tiles' own tops, not the block
    /// column's statistic.
    #[test]
    fn the_fine_side_is_the_fine_tiles_own_heights() {
        let field = flat(140.0);
        let window = window();
        let fine = stepped();
        let terrain = terrain(&field, &fine, &window);
        // The cell just east of the block, whose west side faces tiles 15 of
        // each row: those stand at 40 - 15/3 = 35.
        let p = terrain.perimeter(16, 0, 6);
        let on_the_border: Vec<f32> =
            p.points.iter().filter(|q| q.fine).map(|q| q.y).collect();
        assert!(!on_the_border.is_empty(), "no side faced the fine map");
        for y in &on_the_border {
            assert!((y - terrain.slab_top(35)).abs() < 1e-4, "stood at {y}");
        }
        // The percentile would have put it at 39, four levels high.
        assert!(terrain.slab_top(39) - on_the_border[0] > 3.0);
    }

    /// Watertight: the fan covers the cell exactly, every perimeter point is a
    /// corner of two triangles, and consecutive triangles share their vertices.
    #[test]
    fn the_fan_is_watertight() {
        let field = flat(140.0);
        let window = window();
        let fine = stepped();
        let terrain = terrain(&field, &fine, &window);
        for pitch in [6, 24] {
            let p = terrain.perimeter(16, 0, pitch);
            assert_eq!(p.points.len(), pitch as usize * 4, "pitch {pitch}");
            let area = fan_area(&p);
            assert!(
                (area.abs() - (pitch * pitch) as f32).abs() < 1e-2,
                "pitch {pitch} fan covers {area} of {}",
                pitch * pitch
            );
            // Every point is on the cell's own outline: no vertex wanders in.
            for q in &p.points {
                let on_x = q.x == 16.0 || q.x == (16 + pitch) as f32;
                let on_z = q.z == 0.0 || q.z == pitch as f32;
                assert!(on_x || on_z, "{q:?} is off the outline");
            }
            // The outline closes: consecutive points are a tile apart and the
            // last meets the first.
            for i in 0..p.points.len() {
                let a = p.points[i];
                let b = p.points[(i + 1) % p.points.len()];
                let step = (a.x - b.x).abs() + (a.z - b.z).abs();
                assert!((step - 1.0).abs() < 1e-4, "{a:?} to {b:?} is {step}");
            }
        }
    }

    /// No T-junctions: a fine-facing side carries a vertex at every tile
    /// boundary, so a fine tile's corner always lands on a vertex of the strip
    /// rather than in the middle of an edge.
    #[test]
    fn a_fine_side_has_a_vertex_at_every_tile() {
        let field = flat(140.0);
        let window = window();
        let fine = stepped();
        let terrain = terrain(&field, &fine, &window);
        let p = terrain.perimeter(16, 0, 6);
        // Six vertices down the fine-facing side, one per tile, plus the
        // corner it shares with the side beyond the fine map's edge.
        let west: Vec<&Point> =
            p.points.iter().filter(|q| q.x == 16.0 && q.z < 6.0).collect();
        assert_eq!(west.len(), 6, "the fine-facing side lost its tiles");
        assert!(west.iter().all(|q| q.fine));
    }

    /// A cell with no fine ground beside it is not stitched at all.
    #[test]
    fn a_cell_clear_of_the_fine_map_is_a_plain_slab() {
        let fine = stepped();
        assert!(touches_fine(&fine, 16, 0, 6));
        assert!(!touches_fine(&fine, 60, 60, 6));
    }

    /// The stitched cell's own body never rides over the fine ground it meets.
    #[test]
    fn a_stitched_cell_sits_at_the_lowest_fine_tile_it_faces() {
        let field = flat(140.0);
        let window = window();
        let fine = stepped();
        let terrain = terrain(&field, &fine, &window);
        assert_eq!(terrain.stitched_level(16, 0, 6), 35);
    }
}
