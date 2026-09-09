//! Magma as a translucent, glowing surface rather than an opaque brick.
//!
//! DF reports magma exactly as it reports water — a fill level 0-7 per tile —
//! so the sheet is the sheet `water.rs` already draws: the level becomes a
//! height at each *corner*, averaged over the four tiles that touch it, and no
//! face is drawn between two magma tiles. What differs is what it is made of.
//! Magma is nearly opaque where it is deep, thin and dark over its own crust at
//! the edge, and it lights itself: the material carries the glow
//! (`main.rs:MagmaMaterial`), so a magma sea is bright without a light per tile.

use crate::mesh::{MeshData, MeshOptions, Z_SCALE, shade, to_linear};
use crate::palette::{Solid, magma_color};
use crate::water::{EPSILON, Liquid, corners_of, depth_below, wall};
use crate::world::{BLOCK, Chunk, World};

/// The four corner heights of a magma tile, indexed `[cx][cy]`.
pub fn corners(world: &World, x: i32, y: i32, z: i32, own: u8) -> [[f32; 2]; 2] {
    corners_of(world, x, y, z, own, Liquid::Magma)
}

/// Vertex colour for a point on the surface over `depth` tiles of magma.
fn tint(depth: f32, face: f32) -> [f32; 4] {
    let (rgb, alpha) = magma_color(depth);
    shade(to_linear(rgb, alpha), face)
}

/// Meshes every magma tile of one chunk into `mesh`.
///
/// Faces: the top of each column, and a side only where the tile beside it is
/// open air. Nothing between two magma tiles, nothing underneath.
pub fn build_chunk(world: &World, chunk: &Chunk, opts: MeshOptions, mesh: &mut MeshData) {
    let (ox, oy, oz) = chunk.origin();
    if oz > opts.z_ceiling {
        return;
    }

    for ly in 0..BLOCK {
        for lx in 0..BLOCK {
            let voxel = chunk.get(lx, ly);
            if voxel.magma == 0 || (voxel.hidden && !opts.show_hidden) {
                continue;
            }
            let (x, y, z) = (ox + lx, oy + ly, oz);
            // DF is x-east / y-south / z-up; Bevy is y-up, so y and z swap.
            let fx = x as f32;
            let fz = y as f32;
            let fy = z as f32 * Z_SCALE;

            // Above the cut plane there is nothing, so a buried tile still
            // shows its surface when the view slices into the magma.
            let above = (z + 1 <= opts.z_ceiling)
                .then(|| world.voxel(x, y, z + 1))
                .flatten();
            let submerged = above.is_some_and(|n| n.magma > 0);
            let h = if submerged {
                [[Z_SCALE; 2]; 2]
            } else {
                corners(world, x, y, z, voxel.magma)
            };

            let depth = depth_below(world, x, y, z, Liquid::Magma);
            let corner_depth = |cx: usize, cy: usize| depth + h[cx][cy] / Z_SCALE;

            // A lid rests on a full surface, so the two would fight for the
            // same plane; anything shallower still shows its top.
            let full = h.iter().all(|c| c.iter().all(|&v| v >= Z_SCALE - EPSILON));
            let lid = above.is_some_and(|n| wall(n.solid) || n.solid == Solid::Floor) && full;

            if !submerged && !lid {
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

            // A side shows only against open air: never between two magma
            // tiles, never into a wall, and never where an unloaded neighbour
            // might yet turn out to be more magma.
            let open = |dx: i32, dy: i32| match world.voxel(x + dx, y + dy, z) {
                None => false,
                Some(n) => n.magma == 0 && !wall(n.solid),
            };
            // A side of a glowing liquid is lit by the liquid, not by the sun,
            // so it keeps more of its colour than a water side does.
            let mut side = |corners: [[f32; 3]; 4], normal: [f32; 3], face: f32, d: [f32; 2]| {
                if corners[1][1] - corners[0][1] < EPSILON
                    && corners[2][1] - corners[3][1] < EPSILON
                {
                    return;
                }
                let low = tint(depth, face);
                mesh.push_quad_colors(corners, normal, [low, tint(d[0], face), tint(d[1], face), low]);
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
                    0.92,
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
                    0.92,
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
                    0.86,
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
                    0.86,
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
            for (dx, &(magma, solid)) in row.iter().enumerate() {
                let i = (dy as i32 + 1) * BLOCK + dx as i32 + 1;
                voxels[i as usize] = Voxel { solid, magma, ..Default::default() };
            }
        }
        world.restore((0, 0, 0), voxels);
        world.restore((0, 0, 1), vec![over; TILES_PER_BLOCK]);
        world
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
    fn a_walled_sea_is_flat() {
        let world = grid([[POOL; 3]; 3], Solid::Cube, Voxel::default());
        assert_eq!(corners(&world, 2, 2, 0, 7), [[1.0; 2]; 2]);
    }

    #[test]
    fn a_dry_bank_pulls_the_corner_down() {
        let world = grid([[DRY, POOL, DRY]; 3], Solid::Cube, Voxel::default());
        let h = corners(&world, 2, 2, 0, 7);
        assert!((h[0][0] - 0.5).abs() < 1e-6, "{h:?}");
        assert!((h[1][1] - 0.5).abs() < 1e-6, "{h:?}");
    }

    #[test]
    fn a_corner_is_the_mean_of_the_four_tiles_that_touch_it() {
        let mut patch = [[DRY; 3]; 3];
        patch[1][1] = POOL;
        patch[1][2] = (3, Solid::Floor);
        let world = grid(patch, Solid::Cube, Voxel::default());
        let h = corners(&world, 2, 2, 0, 7)[1][0];
        assert!((h - (7.0 + 3.0) / 4.0 / 7.0).abs() < 1e-6, "{h}");
    }

    #[test]
    fn water_is_not_magma() {
        // A pool of water leaves the magma sheet empty.
        let mut world = grid([[AIR; 3]; 3], Solid::Empty, Voxel::default());
        let mut voxels = vec![Voxel::default(); TILES_PER_BLOCK];
        voxels[(BLOCK + 1) as usize] = Voxel { water: 7, ..Default::default() };
        world.restore((0, 0, 0), voxels);
        assert_eq!(triangles(&world), 0);
    }

    #[test]
    fn one_tile_in_the_open_keeps_its_sides() {
        let mut patch = [[AIR; 3]; 3];
        patch[1][1] = POOL;
        let world = grid(patch, Solid::Empty, Voxel::default());
        // A top and four sides.
        assert_eq!(triangles(&world), 5 * 2);
    }

    #[test]
    fn adjacent_magma_shares_no_face() {
        let mut patch = [[AIR; 3]; 3];
        patch[1][1] = POOL;
        patch[1][2] = POOL;
        let world = grid(patch, Solid::Empty, Voxel::default());
        // Two tops and six sides: the face between the two is gone.
        assert_eq!(triangles(&world), 8 * 2);
    }

    #[test]
    fn magma_under_magma_has_no_surface() {
        let flooded = Voxel { magma: 7, ..Default::default() };
        let world = grid([[POOL; 3]; 3], Solid::Cube, flooded);
        assert_eq!(triangles(&world), 0);
    }
}
