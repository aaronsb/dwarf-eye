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

/// The fine map's ground, one entry per 16-tile block column that reaches it.
/// Keys and levels are render-local, matching `World`'s own chunk coordinates.
#[derive(Default)]
pub struct FineSurface {
    tops: HashMap<(i32, i32), i32>,
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
        for (key, floor) in lowest {
            let Some(chunk) = world.chunk(key.0, key.1, floor) else { continue };
            let filled = chunk.voxels.iter().filter(|v| !v.solid.is_empty()).count();
            if (filled as f32) < chunk.voxels.len() as f32 * GROUNDED_FRACTION {
                continue;
            }
            let (z_lo, z_hi) = levels[&key];
            let mut surface = Vec::with_capacity((BLOCK * BLOCK) as usize);
            for y in 0..BLOCK {
                for x in 0..BLOCK {
                    let (tx, ty) = (key.0 * BLOCK + x, key.1 * BLOCK + y);
                    for z in (z_lo..=z_hi).rev() {
                        if let Some(v) = world.voxel(tx, ty, z)
                            && v.solid != Solid::Empty
                        {
                            surface.push(z);
                            break;
                        }
                    }
                }
            }
            if surface.is_empty() {
                continue;
            }
            surface.sort_unstable();
            tops.insert(key, surface[surface.len() * SURFACE_PERCENTILE / 100]);
        }
        Self { tops }
    }

    /// Whether the fine map covers the block column holding a render-local
    /// tile, and so whether the coarse bands must stand out of the way.
    pub fn covers(&self, tx: i32, tz: i32) -> bool {
        self.tops.contains_key(&(tx.div_euclid(BLOCK), tz.div_euclid(BLOCK)))
    }

    /// The fine ground's z-level in the block column holding a render-local
    /// tile.
    pub fn level(&self, tx: i32, tz: i32) -> Option<i32> {
        self.tops.get(&(tx.div_euclid(BLOCK), tz.div_euclid(BLOCK))).copied()
    }

    pub fn columns(&self) -> usize {
        self.tops.len()
    }

    #[cfg(test)]
    pub fn from_columns(columns: &[((i32, i32), i32)]) -> Self {
        Self { tops: columns.iter().copied().collect() }
    }
}
