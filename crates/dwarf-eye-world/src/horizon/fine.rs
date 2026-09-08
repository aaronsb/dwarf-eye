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
use std::collections::HashMap;

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
}

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
        for (key, floor) in lowest {
            let Some(chunk) = world.chunk(key.0, key.1, floor) else { continue };
            let filled = chunk.voxels.iter().filter(|v| !v.solid.is_empty()).count();
            if (filled as f32) < chunk.voxels.len() as f32 * GROUNDED_FRACTION {
                continue;
            }
            let (z_lo, z_hi) = levels[&key];
            let mut surface = Vec::with_capacity((BLOCK * BLOCK) as usize);
            let mut found = Vec::with_capacity((BLOCK * BLOCK) as usize);
            for y in 0..BLOCK {
                for x in 0..BLOCK {
                    let (tx, ty) = (key.0 * BLOCK + x, key.1 * BLOCK + y);
                    for z in (z_lo..=z_hi).rev() {
                        if let Some(v) = world.voxel(tx, ty, z)
                            && v.solid != Solid::Empty
                        {
                            surface.push(z);
                            found.push(((tx, ty), z));
                            break;
                        }
                    }
                }
            }
            if surface.is_empty() {
                continue;
            }
            tiles.extend(found);
            surface.sort_unstable();
            tops.insert(key, surface[surface.len() * SURFACE_PERCENTILE / 100]);
        }
        Self { tops, tiles }
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
        Self { tops: columns.iter().copied().collect(), tiles }
    }

    #[cfg(test)]
    pub fn with_tiles(columns: &[((i32, i32), i32)], tiles: &[((i32, i32), i32)]) -> Self {
        let mut surface = Self::from_columns(columns);
        surface.tiles.extend(tiles.iter().copied());
        surface
    }
}
