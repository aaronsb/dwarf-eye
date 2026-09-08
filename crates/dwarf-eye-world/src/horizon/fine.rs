//! Where the fine map stands, and how high its ground is.
//!
//! This is the horizon's half of the arbitration between tiers. `worker.rs`
//! sends the renderer the same set of grounded block columns as a GPU mask,
//! which discards horizon fragments over them; here the columns are read off
//! the same chunks so the coarse bands can leave a hole and, at the rim of it,
//! snap to the fine ground's own height instead of guessing.
//!
//! The height is a z-level rather than a distance, so a coarse cell at the rim
//! and the fine floor slab beside it land on exactly the same plane.

use crate::world::{BLOCK, World};
use crate::Solid;
use std::collections::{HashMap, HashSet};

/// Tiles of a block column that must be solid at its lowest loaded chunk for
/// the column to count as grounded. A sparse lowest chunk is canopy with no
/// ground under it, and the coarse band must still draw there.
const GROUNDED_FRACTION: f32 = 0.5;

/// Percentile of the column's surface levels taken as its top: low enough that
/// a crown standing in the column does not pull it up, high enough to sit on
/// the ground rather than in a pit.
const SURFACE_PERCENTILE: usize = 30;

/// The fine map's ground: one entry per 16-tile block column that reaches it,
/// and one per tile inside those columns.
///
/// The column entry is the arbitration — it is the same set `worker.rs` ships
/// as the GPU block mask, so `covers` and the shader agree. The **per-tile**
/// entry is what the stitch is built from: a column's 30th percentile is a
/// statistic, and a coarse slab snapped to it stands two or three levels off
/// the floor it is supposed to meet wherever the ground inside that block is
/// not flat. Keys and levels are render-local, matching `World`'s own chunk
/// coordinates.
#[derive(Default)]
pub struct FineSurface {
    tops: HashMap<(i32, i32), i32>,
    tiles: HashMap<(i32, i32), i32>,
    trees: HashMap<(i32, i32), usize>,
}

/// Tiles in a 16-tile block column, which is what a tree count is per.
const PER_COLUMN: f32 = (BLOCK * BLOCK) as f32;

impl FineSurface {
    pub fn survey(world: &World) -> Self {
        let mut lowest: HashMap<(i32, i32), i32> = HashMap::new();
        let mut levels: HashMap<(i32, i32), (i32, i32)> = HashMap::new();
        for chunk in world.chunks() {
            let key = (chunk.block_x, chunk.block_y);
            let entry = lowest.entry(key).or_insert(chunk.z);
            *entry = (*entry).min(chunk.z);
            let span = levels.entry(key).or_insert((chunk.z, chunk.z));
            span.0 = span.0.min(chunk.z);
            span.1 = span.1.max(chunk.z);
        }

        let mut tops = HashMap::new();
        let mut tiles = HashMap::new();
        let mut trees = HashMap::new();
        for (key, floor) in lowest {
            let Some(chunk) = world.chunk(key.0, key.1, floor) else { continue };
            let filled = chunk.voxels.iter().filter(|v| !v.solid.is_empty()).count();
            if (filled as f32) < chunk.voxels.len() as f32 * GROUNDED_FRACTION {
                continue;
            }
            let (z_lo, z_hi) = levels[&key];
            let mut surface = Vec::with_capacity((BLOCK * BLOCK) as usize);
            let mut found = Vec::with_capacity((BLOCK * BLOCK) as usize);
            let mut origins: HashSet<(i32, i32, i32)> = HashSet::new();
            for y in 0..BLOCK {
                for x in 0..BLOCK {
                    let (tx, ty) = (key.0 * BLOCK + x, key.1 * BLOCK + y);
                    for z in (z_lo..=z_hi).rev() {
                        let Some(v) = world.voxel(tx, ty, z) else { continue };
                        if v.solid == Solid::Empty {
                            continue;
                        }
                        // A tile inside a tree is canopy, not ground: taking
                        // the first solid from the top would put the stitch on
                        // the treetops. The offsets are zero only at the
                        // tree's own base tile, which does stand on the floor.
                        if v.tree_dx != 0 || v.tree_dy != 0 || v.tree_dz != 0 {
                            origins.insert(world_tree_origin(tx, ty, z, &v));
                            continue;
                        }
                        surface.push(z);
                        found.push(((tx, ty), z));
                        break;
                    }
                }
            }
            if surface.is_empty() {
                continue;
            }
            tiles.extend(found);
            trees.insert(key, origins.len());
            surface.sort_unstable();
            tops.insert(key, surface[surface.len() * SURFACE_PERCENTILE / 100]);
        }
        Self { tops, tiles, trees }
    }

    /// Whether the fine map covers the block column holding a render-local
    /// tile, and so whether the coarse bands must stand out of the way.
    pub fn covers(&self, tx: i32, tz: i32) -> bool {
        self.tops.contains_key(&(tx.div_euclid(BLOCK), tz.div_euclid(BLOCK)))
    }

    /// Whether one 16-tile block column is grounded.
    pub fn covers_block(&self, bx: i32, bz: i32) -> bool {
        self.tops.contains_key(&(bx, bz))
    }

    /// The fine ground's z-level in the block column holding a render-local
    /// tile.
    pub fn level(&self, tx: i32, tz: i32) -> Option<i32> {
        self.tops.get(&(tx.div_euclid(BLOCK), tz.div_euclid(BLOCK))).copied()
    }

    /// The top solid z of one fine tile, where the fine map has one.
    ///
    /// This is what the stitch snaps to, tile by tile, rather than the
    /// column's percentile: a coarse cell then meets the floor it actually
    /// abuts rather than the average of the block that floor is in.
    pub fn tile_level(&self, tx: i32, tz: i32) -> Option<i32> {
        self.tiles.get(&(tx, tz)).copied()
    }

    /// The lowest fine tile on the far side of a boundary, over the `pitch`
    /// tiles a coarse cell's side spans, or `None` where that side faces no
    /// fine ground.
    ///
    /// The lowest, because a coarse cell must never ride over fine ground; and
    /// the whole side rather than one probe, because a cell six tiles across
    /// can abut six different levels.
    pub fn border_level(&self, x0: i32, z0: i32, (dx, _dz): (i32, i32), pitch: i32) -> Option<i32> {
        let mut lowest: Option<i32> = None;
        for i in 0..pitch {
            let (tx, tz) = if dx == 0 { (x0 + i, z0) } else { (x0, z0 + i) };
            if let Some(l) = self.tile_level(tx, tz) {
                lowest = Some(lowest.map_or(l, |s: i32| s.min(l)));
            }
        }
        lowest
    }

    pub fn columns(&self) -> usize {
        self.tops.len()
    }

    pub fn tiles(&self) -> usize {
        self.tiles.len()
    }

    /// What the fine map actually grows near a coarse point, and how far off
    /// that fine ground is.
    ///
    /// The far band's density comes from a region tile's `vegetation`, which is
    /// a 48-tile average and says nothing about the clearing the character is
    /// standing in. This is the cue that does: the trees per block column the
    /// fine map holds nearest this point, and the distance to it, so a scatter
    /// can blend from what is really there at the window's edge to what the
    /// survey says further out.
    ///
    /// Returned as crowns per `PER_COLUMN` tiles and a distance in tiles. The
    /// search is a widening ring of block columns and stops at `reach` blocks.
    pub fn nearby_density(&self, tx: i32, tz: i32, reach: i32) -> Option<(f32, f32)> {
        if self.trees.is_empty() {
            return None;
        }
        let (bx, bz) = (tx.div_euclid(BLOCK), tz.div_euclid(BLOCK));
        for r in 0..=reach {
            let (mut sum, mut n) = (0usize, 0usize);
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs() != r && dz.abs() != r {
                        continue;
                    }
                    if let Some(count) = self.trees.get(&(bx + dx, bz + dz)) {
                        sum += *count;
                        n += 1;
                    }
                }
            }
            if n > 0 {
                let distance = ((r - 1).max(0) * BLOCK) as f32;
                return Some((sum as f32 / n as f32 / PER_COLUMN, distance));
            }
        }
        None
    }

    /// Trees per tile over the whole fine map, for a report.
    pub fn mean_density(&self) -> f32 {
        if self.trees.is_empty() {
            return 0.0;
        }
        self.trees.values().sum::<usize>() as f32 / self.trees.len() as f32 / PER_COLUMN
    }

    #[cfg(test)]
    pub fn from_columns(columns: &[((i32, i32), i32)]) -> Self {
        let mut tiles = HashMap::new();
        for ((bx, by), level) in columns.iter().copied() {
            for y in 0..BLOCK {
                for x in 0..BLOCK {
                    tiles.insert((bx * BLOCK + x, by * BLOCK + y), level);
                }
            }
        }
        Self { tops: columns.iter().copied().collect(), tiles, trees: HashMap::new() }
    }

    #[cfg(test)]
    pub fn with_trees(mut self, trees: &[((i32, i32), usize)]) -> Self {
        self.trees = trees.iter().copied().collect();
        self
    }

    #[cfg(test)]
    pub fn with_tiles(columns: &[((i32, i32), i32)], tiles: &[((i32, i32), i32)]) -> Self {
        let mut surface = Self::from_columns(columns);
        surface.tiles.extend(tiles.iter().copied());
        surface
    }
}

/// The absolute origin tile of the tree a voxel belongs to.
fn world_tree_origin(tx: i32, ty: i32, z: i32, v: &crate::world::Voxel) -> (i32, i32, i32) {
    (tx - v.tree_dx as i32, ty - v.tree_dy as i32, z - v.tree_dz as i32)
}
