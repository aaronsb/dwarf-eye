//! A voxel view of the fortress, assembled from DFHack map blocks.

use crate::palette::{Palette, Rgb, Solid, solid_for_shape};
use dfhack_remote::rfr::{BlockList, MapBlock};
use std::collections::HashMap;

/// DF map blocks are 16x16 tiles on a single z-level.
pub const BLOCK: i32 = 16;
pub const TILES_PER_BLOCK: usize = (BLOCK * BLOCK) as usize;

/// One tile, reduced to what the renderer needs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Voxel {
    pub solid: Solid,
    pub color: Rgb,
    /// Not yet discovered by the player.
    pub hidden: bool,
    pub outside: bool,
    /// Fill level 0-7.
    pub water: u8,
    /// Fill level 0-7.
    pub magma: u8,
}

/// A decoded 16x16x1 slab, addressed by block coordinates.
pub struct Chunk {
    pub block_x: i32,
    pub block_y: i32,
    pub z: i32,
    pub voxels: Vec<Voxel>,
}

impl Chunk {
    pub fn get(&self, x: i32, y: i32) -> Voxel {
        self.voxels[(y * BLOCK + x) as usize]
    }

    /// Tile coordinate of this chunk's lower corner.
    pub fn origin(&self) -> (i32, i32, i32) {
        (self.block_x * BLOCK, self.block_y * BLOCK, self.z)
    }
}

/// A bounding box of blocks in x/y and z-levels in z, matching `BlockRequest`.
#[derive(Clone, Copy, Debug)]
pub struct BlockBounds {
    pub min_x: i32,
    pub max_x: i32,
    pub min_y: i32,
    pub max_y: i32,
    pub min_z: i32,
    pub max_z: i32,
}

impl BlockBounds {
    /// A box reaching `depth` z-levels below a cut plane at `z_top`.
    ///
    /// Levels above the cut plane are never drawn, so there is no reason to pay
    /// for fetching them.
    pub fn under_ceiling(x: i32, y: i32, z_top: i32, radius: i32, depth: i32) -> Self {
        let (bx, by) = (x.div_euclid(BLOCK), y.div_euclid(BLOCK));
        Self {
            min_x: bx - radius,
            max_x: bx + radius + 1,
            min_y: by - radius,
            max_y: by + radius + 1,
            min_z: z_top - depth,
            max_z: z_top + 2,
        }
    }

    pub fn contains_block(&self, block_x: i32, block_y: i32, z: i32) -> bool {
        (self.min_x..self.max_x).contains(&block_x)
            && (self.min_y..self.max_y).contains(&block_y)
            && (self.min_z..self.max_z).contains(&z)
    }

    /// A box of `radius` blocks and `depth` z-levels around a tile position.
    pub fn around_tile(x: i32, y: i32, z: i32, radius: i32, depth: i32) -> Self {
        let (bx, by) = (x / BLOCK, y / BLOCK);
        Self {
            min_x: bx - radius,
            max_x: bx + radius + 1,
            min_y: by - radius,
            max_y: by + radius + 1,
            min_z: z - depth,
            max_z: z + depth + 1,
        }
    }
}

/// The decoded map, keyed by block coordinates.
pub struct World {
    pub palette: Palette,
    chunks: HashMap<(i32, i32, i32), Chunk>,
}

impl World {
    pub fn new(palette: Palette) -> Self {
        Self { palette, chunks: HashMap::new() }
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn chunks(&self) -> impl Iterator<Item = &Chunk> {
        self.chunks.values()
    }

    pub fn chunk(&self, block_x: i32, block_y: i32, z: i32) -> Option<&Chunk> {
        self.chunks.get(&(block_x, block_y, z))
    }

    /// Looks up a single tile by absolute tile coordinates.
    pub fn voxel(&self, x: i32, y: i32, z: i32) -> Option<Voxel> {
        let chunk = self.chunk(x.div_euclid(BLOCK), y.div_euclid(BLOCK), z)?;
        Some(chunk.get(x.rem_euclid(BLOCK), y.rem_euclid(BLOCK)))
    }

    /// Drops chunks outside `bounds`, returning the keys that went away so the
    /// renderer can retire their meshes.
    pub fn retain_within(&mut self, bounds: BlockBounds) -> Vec<(i32, i32, i32)> {
        let dropped: Vec<_> = self
            .chunks
            .keys()
            .filter(|&&(bx, by, z)| !bounds.contains_block(bx, by, z))
            .copied()
            .collect();
        for key in &dropped {
            self.chunks.remove(key);
        }
        dropped
    }

    /// Folds a block list into the world, replacing any chunks it covers.
    pub fn absorb(&mut self, list: BlockList) -> usize {
        let count = list.map_blocks.len();
        for block in list.map_blocks {
            let chunk = self.decode(&block);
            self.chunks
                .insert((chunk.block_x, chunk.block_y, chunk.z), chunk);
        }
        count
    }

    fn decode(&self, block: &MapBlock) -> Chunk {
        let mut voxels = vec![Voxel::default(); TILES_PER_BLOCK];

        // Every parallel array is either full length or absent, so index
        // defensively rather than assuming the server filled all of them.
        for i in 0..TILES_PER_BLOCK {
            let Some(&tile_id) = block.tiles.get(i) else { continue };
            let shape = self.palette.shape(tile_id);
            let color = match block.materials.get(i) {
                Some(pair) => self.palette.color(tile_id, pair),
                None => self.palette.color(tile_id, &Default::default()),
            };
            voxels[i] = Voxel {
                solid: solid_for_shape(shape),
                color,
                hidden: block.hidden.get(i).copied().unwrap_or(false),
                outside: block.outside.get(i).copied().unwrap_or(false),
                water: block.water.get(i).copied().unwrap_or(0).clamp(0, 7) as u8,
                magma: block.magma.get(i).copied().unwrap_or(0).clamp(0, 7) as u8,
            };
        }

        Chunk {
            // Responses carry tile coordinates in x/y; requests use block units.
            block_x: block.map_x.div_euclid(BLOCK),
            block_y: block.map_y.div_euclid(BLOCK),
            z: block.map_z,
            voxels,
        }
    }
}
