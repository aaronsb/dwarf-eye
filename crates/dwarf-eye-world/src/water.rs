//! Water as a surface rather than a stack of cubes.
//!
//! DF gives a fill level 0-7 per tile and nothing else, so a pool arrives as a
//! flat set of tiles that a cuboid per voxel turns into a blue brick. Here the
//! level becomes a height at each *corner* of a tile, shared with the three
//! tiles that touch that corner, so neighbouring tiles always agree along their
//! edge and the sheet is continuous. A dry neighbour on the same level pulls the
//! corner to the ground, which is what tapers a pool to its bank.
//!
//! The result is its own mesh on its own translucent material
//! (`main.rs:WaterMaterial`). Magma is the same surface on its own material
//! (`magma.rs`), and the corner rule here is what both read.

use crate::mesh::{MeshData, MeshOptions, Z_SCALE, shade, to_linear};
use crate::palette::{Solid, water_color};
use crate::world::{BLOCK, Chunk, Voxel, World};

/// A full tile of liquid, as DF counts it.
pub const FULL: u8 = 7;

/// How many tiles of water under the surface still darken it.
const DEPTH_LIMIT: i32 = 5;

/// Below this a corner is at ground level and the face there is not worth
/// drawing.
pub(crate) const EPSILON: f32 = 1.0e-3;

/// Which of DF's two liquids a surface is made of. The corner rule is one rule;
/// only the fill level it reads differs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Liquid {
    Water,
    Magma,
}

impl Liquid {
    pub fn fill(self, v: Voxel) -> u8 {
        match self {
            Liquid::Water => v.water,
            Liquid::Magma => v.magma,
        }
    }
}

/// Whether a tile blocks the sheet: a wall neither holds water nor lets it
/// drain, so it leaves the surface beside it alone.
pub(crate) fn wall(solid: Solid) -> bool {
    matches!(solid, Solid::Cube | Solid::Fortification)
}

/// The fill level at a tile, or `None` where there is no liquid of that kind.
pub(crate) fn level(world: &World, x: i32, y: i32, z: i32, of: Liquid) -> Option<u8> {
    world.voxel(x, y, z).map(|v| of.fill(v)).filter(|&w| w > 0)
}

/// What one tile contributes to a corner of a surface whose own level is `own`.
///
/// Liquid contributes its own fill. A wall, an unloaded tile, or a column that
/// carries on above contributes `own`, which leaves the corner where it was.
/// Anything else — floor, ramp, open air — is somewhere the liquid is not, and
/// contributes nothing, so the corner sinks toward the ground.
fn contribution(world: &World, x: i32, y: i32, z: i32, own: u8, of: Liquid) -> f32 {
    let Some(v) = world.voxel(x, y, z) else { return own as f32 };
    if of.fill(v) > 0 {
        if level(world, x, y, z + 1, of).is_some() {
            return own as f32;
        }
        return of.fill(v) as f32;
    }
    if wall(v.solid) {
        return own as f32;
    }
    0.0
}

/// Height of one corner of a liquid tile's surface, in cells.
///
/// `corner` is a pair of 0 or 1 and picks the corner; the four tiles that share it
/// are averaged, so the two tiles either side of an edge compute the same pair
/// of heights and the sheet has no seam.
pub fn corner_height_of(
    world: &World,
    x: i32,
    y: i32,
    z: i32,
    own: u8,
    corner: (i32, i32),
    of: Liquid,
) -> f32 {
    let (cx, cy) = corner;
    let mut sum = 0.0;
    for dy in [cy - 1, cy] {
        for dx in [cx - 1, cx] {
            sum += contribution(world, x + dx, y + dy, z, own, of);
        }
    }
    (sum / 4.0 / FULL as f32).clamp(0.0, 1.0) * Z_SCALE
}

/// The four corner heights of a liquid tile, indexed `[cx][cy]`.
pub fn corners_of(world: &World, x: i32, y: i32, z: i32, own: u8, of: Liquid) -> [[f32; 2]; 2] {
    let mut h = [[0.0; 2]; 2];
    for cx in 0..2 {
        for cy in 0..2 {
            h[cx as usize][cy as usize] = corner_height_of(world, x, y, z, own, (cx, cy), of);
        }
    }
    h
}

/// [`corner_height_of`] for water.
pub fn corner_height(world: &World, x: i32, y: i32, z: i32, own: u8, cx: i32, cy: i32) -> f32 {
    corner_height_of(world, x, y, z, own, (cx, cy), Liquid::Water)
}

/// [`corners_of`] for water.
pub fn corners(world: &World, x: i32, y: i32, z: i32, own: u8) -> [[f32; 2]; 2] {
    corners_of(world, x, y, z, own, Liquid::Water)
}

/// Tiles of liquid stacked under this one, for how dark the surface reads.
pub(crate) fn depth_below(world: &World, x: i32, y: i32, z: i32, of: Liquid) -> f32 {
    let mut deep = 0.0;
    for step in 1..=DEPTH_LIMIT {
        if level(world, x, y, z - step, of).is_none() {
            break;
        }
        deep += 1.0;
    }
    deep
}

/// Vertex colour for a point on the surface standing over `depth` tiles of
/// water: deeper is darker and less transparent.
fn tint(depth: f32, face: f32) -> [f32; 4] {
    let (rgb, alpha) = water_color(depth);
    shade(to_linear(rgb, alpha), face)
}

/// Meshes every liquid tile of one chunk into `mesh`.
///
/// Faces: the top of each column, and a side only where the tile beside it is
/// open air. Nothing between two liquid tiles, nothing underneath.
pub fn build_chunk(world: &World, chunk: &Chunk, opts: MeshOptions, mesh: &mut MeshData) {
    let (ox, oy, oz) = chunk.origin();
    if oz > opts.z_ceiling {
        return;
    }

    for ly in 0..BLOCK {
        for lx in 0..BLOCK {
            let voxel = chunk.get(lx, ly);
            if voxel.water == 0 || (voxel.hidden && !opts.show_hidden) {
                continue;
            }
            let (x, y, z) = (ox + lx, oy + ly, oz);
            // DF is x-east / y-south / z-up; Bevy is y-up, so y and z swap.
            let fx = x as f32;
            let fz = y as f32;
            let fy = z as f32 * Z_SCALE;

            // Above the cut plane there is nothing, so a submerged tile still
            // shows its surface when the view slices into the water.
            let above = (z + 1 <= opts.z_ceiling)
                .then(|| world.voxel(x, y, z + 1))
                .flatten();
            let submerged = above.is_some_and(|n| n.water > 0);
            let h = if submerged {
                [[Z_SCALE; 2]; 2]
            } else {
                corners(world, x, y, z, voxel.water)
            };

            let depth = depth_below(world, x, y, z, Liquid::Water);
            let corner_depth = |cx: usize, cy: usize| depth + h[cx][cy] / Z_SCALE;

            // A lid rests on a full surface, so the two would fight for the
            // same plane; anything shallower still shows its top.
            let full = h.iter().all(|c| c.iter().all(|&v| v >= Z_SCALE - EPSILON));
            let lid = above
                .is_some_and(|n| wall(n.solid) || n.solid == Solid::Floor)
                && full;

            if !submerged && !lid {
                // The slope of the sheet, so a shore catches light differently
                // from open water.
                let dx = (h[1][0] + h[1][1] - h[0][0] - h[0][1]) * 0.5;
                let dz = (h[0][1] + h[1][1] - h[0][0] - h[1][0]) * 0.5;
                let len = (dx * dx + 1.0 + dz * dz).sqrt();
                let normal = [-dx / len, 1.0 / len, -dz / len];
                mesh.push_quad_colors(
                    [
                        [fx, fy + h[0][0], fz],
                        [fx, fy + h[0][1], fz + 1.0],
                        [fx + 1.0, fy + h[1][1], fz + 1.0],
                        [fx + 1.0, fy + h[1][0], fz],
                    ],
                    normal,
                    [
                        tint(corner_depth(0, 0), 1.0),
                        tint(corner_depth(0, 1), 1.0),
                        tint(corner_depth(1, 1), 1.0),
                        tint(corner_depth(1, 0), 1.0),
                    ],
                );
            }

            // A side shows only against open air: never between two liquids,
            // never into a wall, and never where an unloaded neighbour might
            // yet turn out to be more water.
            let open = |dx: i32, dy: i32| match world.voxel(x + dx, y + dy, z) {
                None => false,
                Some(n) => n.water == 0 && !wall(n.solid),
            };
            let mut side = |corners: [[f32; 3]; 4], normal: [f32; 3], face: f32, d: [f32; 2]| {
                if corners[1][1] - corners[0][1] < EPSILON
                    && corners[2][1] - corners[3][1] < EPSILON
                {
                    return;
                }
                let low = tint(depth, face);
                mesh.push_quad_colors(
                    corners,
                    normal,
                    [low, tint(d[0], face), tint(d[1], face), low],
                );
            };

            if open(0, -1) {
                side(
                    [
                        [fx, fy, fz],
                        [fx, fy + h[0][0], fz],
                        [fx + 1.0, fy + h[1][0], fz],
                        [fx + 1.0, fy, fz],
                    ],
                    [0.0, 0.0, -1.0],
                    0.8,
                    [corner_depth(0, 0), corner_depth(1, 0)],
                );
            }
            if open(0, 1) {
                side(
                    [
                        [fx + 1.0, fy, fz + 1.0],
                        [fx + 1.0, fy + h[1][1], fz + 1.0],
                        [fx, fy + h[0][1], fz + 1.0],
                        [fx, fy, fz + 1.0],
                    ],
                    [0.0, 0.0, 1.0],
                    0.8,
                    [corner_depth(1, 1), corner_depth(0, 1)],
                );
            }
            if open(-1, 0) {
                side(
                    [
                        [fx, fy, fz + 1.0],
                        [fx, fy + h[0][1], fz + 1.0],
                        [fx, fy + h[0][0], fz],
                        [fx, fy, fz],
                    ],
                    [-1.0, 0.0, 0.0],
                    0.68,
                    [corner_depth(0, 1), corner_depth(0, 0)],
                );
            }
            if open(1, 0) {
                side(
                    [
                        [fx + 1.0, fy, fz],
                        [fx + 1.0, fy + h[1][0], fz],
                        [fx + 1.0, fy + h[1][1], fz + 1.0],
                        [fx + 1.0, fy, fz + 1.0],
                    ],
                    [1.0, 0.0, 0.0],
                    0.68,
                    [corner_depth(1, 0), corner_depth(1, 1)],
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::Palette;
    use crate::world::{TILES_PER_BLOCK, Voxel};
    use dfhack_remote::rfr::{MaterialList, TiletypeList};

    /// A world of two chunks: a 3x3 patch at tiles (1,1)..(3,3) with `fill`
    /// around it on z 0, and `over` everywhere on z 1.
    fn grid(patch: [[(u8, Solid); 3]; 3], fill: Solid, over: Voxel) -> World {
        let mut world = World::new(Palette::new(TiletypeList::default(), MaterialList::default()));
        let mut voxels = vec![Voxel { solid: fill, ..Default::default() }; TILES_PER_BLOCK];
        for (dy, row) in patch.iter().enumerate() {
            for (dx, &(water, solid)) in row.iter().enumerate() {
                let i = (dy as i32 + 1) * BLOCK + dx as i32 + 1;
                voxels[i as usize] = Voxel { solid, water, ..Default::default() };
            }
        }
        world.restore((0, 0, 0), voxels);
        world.restore((0, 0, 1), vec![over; TILES_PER_BLOCK]);
        world
    }

    /// Open sky over the patch.
    fn sky() -> Voxel {
        Voxel::default()
    }

    const DRY: (u8, Solid) = (0, Solid::Floor);
    const AIR: (u8, Solid) = (0, Solid::Empty);
    const POOL: (u8, Solid) = (7, Solid::Floor);

    fn triangles(world: &World) -> usize {
        let mut mesh = MeshData::default();
        build_chunk(world, world.chunk(0, 0, 0).unwrap(), MeshOptions::default(), &mut mesh);
        mesh.triangle_count()
    }

    #[test]
    fn walls_leave_a_full_pool_flat() {
        let world = grid([[POOL; 3]; 3], Solid::Cube, sky());
        assert_eq!(corners(&world, 2, 2, 0, 7), [[1.0; 2]; 2]);
    }

    #[test]
    fn a_dry_bank_pulls_the_corner_to_the_ground() {
        // The middle column is water, the rest dry floor at the same level.
        let world = grid([[DRY, POOL, DRY]; 3], Solid::Cube, sky());
        let h = corners(&world, 2, 2, 0, 7);
        // West corners share two dry tiles and two wet, east corners likewise.
        assert!((h[0][0] - 0.5).abs() < 1e-6, "{h:?}");
        assert!((h[1][1] - 0.5).abs() < 1e-6, "{h:?}");
    }

    #[test]
    fn a_corner_is_the_mean_of_the_four_tiles_that_touch_it() {
        // Water 7 at the centre, 3 to the east, dry floor to the north-east.
        let mut patch = [[DRY; 3]; 3];
        patch[1][1] = POOL;
        patch[1][2] = (3, Solid::Floor);
        let world = grid(patch, Solid::Cube, sky());
        // The north-east corner of the centre tile touches the centre (7), the
        // tile east of it (3) and two dry tiles (0).
        let h = corner_height(&world, 2, 2, 0, 7, 1, 0);
        assert!((h - (7.0 + 3.0) / 4.0 / 7.0).abs() < 1e-6, "{h}");
    }

    #[test]
    fn neighbours_agree_on_a_shared_corner() {
        let mut patch = [[DRY; 3]; 3];
        patch[1][1] = POOL;
        patch[1][2] = (4, Solid::Floor);
        let world = grid(patch, Solid::Cube, sky());
        // South-east of the centre tile is south-west of the tile east of it.
        let mine = corner_height(&world, 2, 2, 0, 7, 1, 1);
        let theirs = corner_height(&world, 3, 2, 0, 4, 0, 1);
        assert!((mine - theirs).abs() < 1e-6, "{mine} vs {theirs}");
    }

    #[test]
    fn a_walled_pool_draws_only_its_surface() {
        // Nine wet tiles ringed by wall: nine top quads and nothing else.
        let world = grid([[POOL; 3]; 3], Solid::Cube, sky());
        assert_eq!(triangles(&world), 9 * 2);
    }

    #[test]
    fn one_tile_in_the_open_keeps_its_sides() {
        let mut patch = [[AIR; 3]; 3];
        patch[1][1] = POOL;
        let world = grid(patch, Solid::Empty, sky());
        // A top and four sides.
        assert_eq!(triangles(&world), 5 * 2);
    }

    #[test]
    fn adjacent_liquids_share_no_face() {
        let mut patch = [[AIR; 3]; 3];
        patch[1][1] = POOL;
        patch[1][2] = POOL;
        let world = grid(patch, Solid::Empty, sky());
        // Two tops and six sides: the face between the two is gone.
        assert_eq!(triangles(&world), 8 * 2);
    }

    #[test]
    fn water_under_water_has_no_surface() {
        let flooded = Voxel { water: 7, ..Default::default() };
        let world = grid([[POOL; 3]; 3], Solid::Cube, flooded);
        assert_eq!(triangles(&world), 0);
    }

    #[test]
    fn a_full_pool_under_a_floor_has_no_surface() {
        let lid = Voxel { solid: Solid::Floor, ..Default::default() };
        let world = grid([[POOL; 3]; 3], Solid::Cube, lid);
        assert_eq!(triangles(&world), 0);
    }
}
