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
    /// DFHack tiletype id, for looking up a sprite-derived model.
    pub tile_id: i32,
    /// Material index of the tile, which for plants selects the species.
    pub mat_index: i32,
    pub color: Rgb,
    /// Not yet discovered by the player.
    pub hidden: bool,
    pub outside: bool,
    /// Fill level 0-7.
    pub water: u8,
    /// Fill level 0-7.
    pub magma: u8,
    /// Where this tile sits inside its tree, from `tree_x`/`tree_y`/`tree_z`.
    /// Zero for everything that is not part of a tree.
    pub tree_dx: i8,
    pub tree_dy: i8,
    pub tree_dz: i8,
}

impl Voxel {
    /// The tile position of the tree this tile belongs to.
    ///
    /// DFHack reports the offset from the origin on x and y and the offset to
    /// it on z, so the two axes are combined the opposite way round.
    pub fn tree_origin(&self, x: i32, y: i32, z: i32) -> (i32, i32, i32) {
        (
            x - self.tree_dx as i32,
            y - self.tree_dy as i32,
            z + self.tree_dz as i32,
        )
    }
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

    /// Places a chunk that came from somewhere other than the game, such as
    /// the disk cache. Never replaces one the game has already supplied.
    pub fn restore(&mut self, key: (i32, i32, i32), voxels: Vec<Voxel>) -> bool {
        if self.chunks.contains_key(&key) {
            return false;
        }
        self.chunks.insert(key, Chunk { block_x: key.0, block_y: key.1, z: key.2, voxels });
        true
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
    /// `shift` moves the reply's window-local block coordinates and levels
    /// into render space. Returns the keys of the chunks that arrived.
    ///
    /// An incremental reply carries blocks whose hash changed for any reason —
    /// a unit moved, a liquid shifted — and those can arrive with no tile array
    /// at all. Absorbing one of those would replace a decoded chunk with an
    /// empty one, so a block with no tiles is left to stand on what is already
    /// known.
    pub fn absorb(&mut self, list: BlockList, shift: (i32, i32, i32)) -> Vec<(i32, i32, i32)> {
        let mut absorbed = Vec::new();
        for block in list.map_blocks {
            if block.tiles.is_empty() {
                continue;
            }
            let mut chunk = self.decode(&block);
            chunk.block_x += shift.0;
            chunk.block_y += shift.1;
            chunk.z += shift.2;
            let key = (chunk.block_x, chunk.block_y, chunk.z);
            self.chunks.insert(key, chunk);
            absorbed.push(key);
        }
        absorbed
    }

    fn decode(&self, block: &MapBlock) -> Chunk {
        let mut voxels = vec![Voxel::default(); TILES_PER_BLOCK];

        // Every parallel array is either full length or absent, so index
        // defensively rather than assuming the server filled all of them.
        // Tree extents never approach a hundred tiles, so a byte holds an
        // offset with room to spare.
        let offset = |list: &[i32], i: usize| list.get(i).map(|&v| v.clamp(-127, 127) as i8).unwrap_or(0);

        for i in 0..TILES_PER_BLOCK {
            let Some(&tile_id) = block.tiles.get(i) else { continue };
            let shape = self.palette.shape(tile_id);
            let color = match block.materials.get(i) {
                Some(pair) => self.palette.color(tile_id, pair),
                None => self.palette.color(tile_id, &Default::default()),
            };
            voxels[i] = Voxel {
                solid: solid_for_shape(shape),
                tile_id,
                mat_index: block.materials.get(i).map(|m| m.mat_index).unwrap_or(-1),
                color,
                hidden: block.hidden.get(i).copied().unwrap_or(false),
                outside: block.outside.get(i).copied().unwrap_or(false),
                water: block.water.get(i).copied().unwrap_or(0).clamp(0, 7) as u8,
                magma: block.magma.get(i).copied().unwrap_or(0).clamp(0, 7) as u8,
                tree_dx: offset(&block.tree_x, i),
                tree_dy: offset(&block.tree_y, i),
                tree_dz: offset(&block.tree_z, i),
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
