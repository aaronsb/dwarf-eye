//! A voxel view of the fortress, assembled from DFHack map blocks.

use crate::palette::{Palette, Rgb, Solid, solid_for_shape};
use dfhack_remote::rfr::{BlockList, MapBlock};
use std::collections::HashMap;

/// DF map blocks are 16x16 tiles on a single z-level.
pub const BLOCK: i32 = 16;
pub const TILES_PER_BLOCK: usize = (BLOCK * BLOCK) as usize;

/// One tile, reduced to what the renderer needs.
#[derive(Clone, Copy, Debug)]
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
    /// DF's `building_type` for the building standing here, or -1 for none.
    pub building: i16,
    /// The building's subtype: which workshop, which furnace. -1 for none.
    pub building_sub: i16,
    /// Where this tile sits inside its building's footprint, x in the high
    /// nibble and y in the low one, so a three-square shop can wear the right
    /// square of its sprite.
    pub building_at: u8,
}

impl Default for Voxel {
    fn default() -> Self {
        Self {
            solid: Solid::default(),
            tile_id: 0,
            mat_index: 0,
            color: Rgb::default(),
            hidden: false,
            outside: false,
            water: 0,
            magma: 0,
            tree_dx: 0,
            tree_dy: 0,
            tree_dz: 0,
            building: NO_BUILDING,
            building_sub: NO_BUILDING,
            building_at: 0,
        }
    }
}

/// What `Voxel::building` holds where nobody built anything.
pub const NO_BUILDING: i16 = -1;

/// What a building standing in a tile is made of.
///
/// The identity of a building lives in the voxel and is cached with it; what
/// it is made of does not, because a chunk read back from disk is land the
/// game has moved on from and a colour is cheap to fetch again. A restored
/// chunk draws its buildings in stone until the game sends the block.
#[derive(Clone, Copy, Debug, Default)]
pub struct Built {
    pub color: Rgb,
    /// Which of DF's per-material sprite sheets it belongs on, an index into
    /// `factory::BUILDING_SHEETS`.
    pub sheet: u8,
}

/// A tile's worth of loose items, kept apart from the voxels.
///
/// Items move every few seconds and are worth nothing once the character has
/// walked away, so they are never written to the disk cache: a restored chunk
/// simply has none until the game sends the block again.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pile {
    /// How many items share the tile, saturating.
    pub items: u8,
    /// The colour of the topmost item's material.
    pub color: Rgb,
}

impl Voxel {
    /// Whether a building stands in this tile.
    pub fn built_over(&self) -> bool {
        self.building != NO_BUILDING
    }

    /// Where this tile sits inside its building's footprint.
    pub fn building_offset(&self) -> (i32, i32) {
        ((self.building_at >> 4) as i32, (self.building_at & 0x0f) as i32)
    }

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
#[derive(Default)]
pub struct Chunk {
    pub block_x: i32,
    pub block_y: i32,
    pub z: i32,
    pub voxels: Vec<Voxel>,
    /// Loose items, by tile index, for the few tiles that hold enough of them
    /// to read as a pile. Sparse and never cached.
    pub piles: HashMap<u8, Pile>,
    /// What each building tile is made of, by tile index. Sparse and never
    /// cached; the building itself is in the voxel.
    pub built: HashMap<u8, Built>,
}

impl Chunk {
    pub fn get(&self, x: i32, y: i32) -> Voxel {
        self.voxels[(y * BLOCK + x) as usize]
    }

    /// The pile of items standing in a tile, if there is one worth drawing.
    pub fn pile(&self, x: i32, y: i32) -> Option<Pile> {
        if self.piles.is_empty() {
            return None;
        }
        self.piles.get(&((y * BLOCK + x) as u8)).copied()
    }

    /// What the building in a tile is made of. Stone for one read back from
    /// the cache, which carries the building but not its material.
    pub fn built(&self, x: i32, y: i32) -> Built {
        self.built.get(&((y * BLOCK + x) as u8)).copied().unwrap_or(Built {
            color: [150, 146, 138],
            sheet: 0,
        })
    }

    /// Tile coordinate of this chunk's lower corner.
    pub fn origin(&self) -> (i32, i32, i32) {
        (self.block_x * BLOCK, self.block_y * BLOCK, self.z)
    }
}

/// A bounding box of blocks in x/y and z-levels in z, matching `BlockRequest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// One building instance, placed in render tiles and levels.
///
/// DFHack does not attach a building to the block it stands in. Every
/// `GetBlockList` reply carries every building whose footprint falls inside
/// the box that was asked for, hung off one arbitrary block of the reply, in
/// the same window-local tiles the blocks themselves are placed by. So a
/// reply's buildings are stamped onto the chunks of that same reply, after
/// they land: the box is the same box, so the coverage matches, and a chunk
/// decoded afresh loses its buildings and gets them straight back.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    pub building: i16,
    pub subtype: i16,
    pub min: (i32, i32, i32),
    pub max: (i32, i32, i32),
    pub color: Rgb,
    /// Which of DF's per-material sprite sheets it belongs on.
    pub sheet: u8,
}

/// DF's `building_flags.exists`. A building without it is a plan.
const BUILDING_EXISTS: u32 = 0x1;

/// The decoded map, keyed by block coordinates.
pub struct World {
    pub palette: Palette,
    chunks: HashMap<(i32, i32, i32), Chunk>,
    /// How many buildings the last reply carried, for the pass report.
    buildings: usize,
}

impl World {
    pub fn new(palette: Palette) -> Self {
        Self { palette, chunks: HashMap::new(), buildings: 0 }
    }

    /// How many buildings the last reply carried.
    pub fn building_count(&self) -> usize {
        self.buildings
    }

    /// Stamps a reply's buildings onto the chunks that hold their footprints.
    ///
    /// Cheap enough to run after every reply: the work is one pass over the
    /// buildings, not over the map. Nothing is cleared first, because a chunk
    /// that has just been decoded carries no building and one that has not
    /// arrived is not this reply's business.
    fn stamp_buildings(&mut self, buildings: &[Placed]) {
        for placed in buildings {
            for z in placed.min.2..=placed.max.2 {
                for y in placed.min.1..=placed.max.1 {
                    for x in placed.min.0..=placed.max.0 {
                        let key = (x.div_euclid(BLOCK), y.div_euclid(BLOCK), z);
                        let Some(chunk) = self.chunks.get_mut(&key) else { continue };
                        let index =
                            (y.rem_euclid(BLOCK) * BLOCK + x.rem_euclid(BLOCK)) as usize;
                        let Some(voxel) = chunk.voxels.get_mut(index) else { continue };
                        voxel.building = placed.building;
                        voxel.building_sub = placed.subtype;
                        // A nibble each, which is ten more than any DF building
                        // is wide; a wagon at five tiles is the largest there is.
                        voxel.building_at = (((x - placed.min.0).clamp(0, 15) as u8) << 4)
                            | ((y - placed.min.1).clamp(0, 15) as u8);
                        chunk.built.insert(
                            index as u8,
                            Built { color: placed.color, sheet: placed.sheet },
                        );
                    }
                }
            }
        }
    }

    /// Reads a reply's building list into render space.
    ///
    /// The same instance can be repeated across blocks, so the index is what
    /// makes a building one building.
    fn read_buildings(&self, list: &BlockList, shift: (i32, i32, i32)) -> Vec<Placed> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for block in &list.map_blocks {
            for b in &block.buildings {
                let Some(kind) = b.building_type.as_ref() else { continue };
                if b.building_flags() & BUILDING_EXISTS == 0 || !seen.insert(b.index) {
                    continue;
                }
                // A zone is a rectangle the player drew on the floor, and it
                // can cover a meadow. Stamping one would mark every tile under
                // it as built work, which is what stops a tree growing there.
                if !crate::factory::building_kind(kind.building_type).drawn() {
                    continue;
                }
                let material = b.material.as_ref();
                let color = material
                    .and_then(|m| self.palette.material_color(m))
                    .unwrap_or([150, 146, 138]);
                let sheet = crate::factory::sheet_order(
                    material.and_then(|m| self.palette.material_name(m)).unwrap_or(""),
                )[0] as u8;
                let (dx, dy, dz) = (shift.0 * BLOCK, shift.1 * BLOCK, shift.2);
                out.push(Placed {
                    building: kind.building_type.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                    subtype: kind
                        .building_subtype
                        .clamp(i16::MIN as i32, i16::MAX as i32)
                        as i16,
                    min: (b.pos_x_min() + dx, b.pos_y_min() + dy, b.pos_z_min() + dz),
                    max: (b.pos_x_max() + dx, b.pos_y_max() + dy, b.pos_z_max() + dz),
                    color,
                    sheet,
                });
            }
        }
        out
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
        self.chunks.insert(
            key,
            Chunk {
                block_x: key.0,
                block_y: key.1,
                z: key.2,
                voxels,
                piles: HashMap::new(),
                built: HashMap::new(),
            },
        );
        true
    }

    /// Drops one chunk. True when there was one to drop.
    pub fn remove(&mut self, key: (i32, i32, i32)) -> bool {
        self.chunks.remove(&key).is_some()
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
        let buildings = self.read_buildings(&list, shift);
        self.buildings = buildings.len();
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
        self.stamp_buildings(&buildings);
        absorbed
    }

    fn decode(&self, block: &MapBlock) -> Chunk {
        let origin = (block.map_x.div_euclid(BLOCK) * BLOCK, block.map_y.div_euclid(BLOCK) * BLOCK);
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
                ..Voxel::default()
            };
        }

        Chunk {
            // Responses carry tile coordinates in x/y; requests use block units.
            block_x: block.map_x.div_euclid(BLOCK),
            block_y: block.map_y.div_euclid(BLOCK),
            z: block.map_z,
            voxels,
            piles: read_piles(block, origin, &self.palette),
            built: HashMap::new(),
        }
    }
}

/// Reads the loose items in a block, counted per tile.
///
/// Only tiles carrying enough items to read as a pile are kept: the rest is
/// litter, and a box per dropped sock is a box for nothing.
fn read_piles(block: &MapBlock, origin: (i32, i32), palette: &Palette) -> HashMap<u8, Pile> {
    let mut piles: HashMap<u8, Pile> = HashMap::new();
    for item in &block.items {
        let Some(pos) = item.pos.as_ref() else { continue };
        if pos.z() != block.map_z {
            continue;
        }
        let (dx, dy) = (pos.x() - origin.0, pos.y() - origin.1);
        if !(0..BLOCK).contains(&dx) || !(0..BLOCK).contains(&dy) {
            continue;
        }
        let color = item
            .material
            .as_ref()
            .and_then(|m| palette.material_color(m))
            .unwrap_or([180, 170, 150]);
        let entry = piles.entry((dy * BLOCK + dx) as u8).or_default();
        entry.items = entry.items.saturating_add(1);
        entry.color = color;
    }
    piles.retain(|_, pile| {
        crate::factory::classify_items(pile.items) == crate::factory::Class::ItemPile
    });
    piles
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory;
    use dfhack_remote::rfr;

    fn empty_world() -> World {
        World::new(Palette::new(Default::default(), Default::default()))
    }

    /// One block at a block origin, with `tiles` filled so `absorb` keeps it.
    fn block(map_x: i32, map_y: i32, map_z: i32) -> rfr::MapBlock {
        rfr::MapBlock {
            map_x,
            map_y,
            map_z,
            tiles: vec![0; TILES_PER_BLOCK],
            ..Default::default()
        }
    }

    fn building(
        kind: i32,
        subtype: i32,
        min: (i32, i32, i32),
        max: (i32, i32, i32),
    ) -> rfr::BuildingInstance {
        rfr::BuildingInstance {
            index: 0,
            pos_x_min: Some(min.0),
            pos_y_min: Some(min.1),
            pos_z_min: Some(min.2),
            pos_x_max: Some(max.0),
            pos_y_max: Some(max.1),
            pos_z_max: Some(max.2),
            building_type: Some(rfr::BuildingType {
                building_type: kind,
                building_subtype: subtype,
                building_custom: -1,
            }),
            building_flags: Some(BUILDING_EXISTS),
            ..Default::default()
        }
    }

    /// The whole map's buildings ride on one arbitrary block, so a reply is a
    /// block with tiles and a list hung off it.
    fn reply(blocks: Vec<rfr::MapBlock>) -> rfr::BlockList {
        rfr::BlockList { map_blocks: blocks, ..Default::default() }
    }

    #[test]
    fn a_workshop_paints_its_whole_footprint_and_nothing_else() {
        let mut world = empty_world();
        let mut b = block(16, 16, 5);
        b.buildings.push(building(
            factory::building_type::WORKSHOP,
            0,
            (20, 21, 5),
            (22, 23, 5),
        ));
        world.absorb(reply(vec![b]), (0, 0, 0));

        for y in 21..=23 {
            for x in 20..=22 {
                let v = world.voxel(x, y, 5).expect("a tile inside the footprint");
                assert!(v.built_over(), "{x},{y} is the shop");
                assert_eq!(v.building as i32, factory::building_type::WORKSHOP);
                assert_eq!(v.building_sub, 0);
                assert_eq!(v.building_offset(), (x - 20, y - 21), "{x},{y} sub-tile");
            }
        }
        assert!(!world.voxel(19, 21, 5).unwrap().built_over(), "one tile west is not the shop");
        assert!(!world.voxel(23, 23, 5).unwrap().built_over(), "one tile east is not the shop");
    }

    #[test]
    fn a_building_is_painted_on_every_level_it_reaches_and_no_other() {
        let door = |z| {
            let mut b = block(0, 0, z);
            b.buildings.push(building(factory::building_type::DOOR, -1, (3, 4, 7), (3, 4, 8)));
            b
        };
        let mut world = empty_world();
        world.absorb(
            reply(vec![door(6), door(7), door(8)]),
            (0, 0, 0),
        );
        assert!(!world.voxel(3, 4, 6).unwrap().built_over(), "below the door");
        assert!(world.voxel(3, 4, 7).unwrap().built_over());
        assert!(world.voxel(3, 4, 8).unwrap().built_over());
    }

    #[test]
    fn a_footprint_is_built_work_no_tree_may_grow_into() {
        let mut world = empty_world();
        let mut b = block(0, 0, 0);
        b.buildings.push(building(factory::building_type::STATUE, -1, (1, 1, 0), (1, 1, 0)));
        world.absorb(reply(vec![b]), (0, 0, 0));

        let statue = world.voxel(1, 1, 0).unwrap();
        // `tree.rs:Envelope::read` blocks a crown wherever this holds.
        assert!(statue.built_over());
        assert!(factory::built(factory::classify_building(statue.building as i32)));
        assert!(!world.voxel(2, 1, 0).unwrap().built_over(), "the tile beside it is open");
    }

    #[test]
    fn a_zone_never_reaches_a_tile() {
        // An activity zone can cover a meadow. Marking every tile under it as
        // built work would stop a tree growing there, so it never lands.
        let mut world = empty_world();
        let mut b = block(0, 0, 0);
        b.buildings.push(building(factory::building_type::CIVZONE, -1, (0, 0, 0), (4, 4, 0)));
        b.buildings.push(building(factory::building_type::STOCKPILE, -1, (6, 6, 0), (9, 9, 0)));
        world.absorb(reply(vec![b]), (0, 0, 0));
        assert_eq!(world.building_count(), 0, "neither is worth a tile");
        assert!(!world.voxel(2, 2, 0).unwrap().built_over());
        assert!(!world.voxel(7, 7, 0).unwrap().built_over());
    }

    #[test]
    fn a_plan_is_not_a_building() {
        let mut world = empty_world();
        let mut b = block(0, 0, 0);
        let mut planned = building(factory::building_type::TABLE, -1, (2, 2, 0), (2, 2, 0));
        planned.building_flags = Some(0);
        b.buildings.push(planned);
        world.absorb(reply(vec![b]), (0, 0, 0));
        assert!(!world.voxel(2, 2, 0).unwrap().built_over());
    }

    #[test]
    fn only_a_stacked_tile_counts_as_a_pile() {
        let mut world = empty_world();
        let mut b = block(0, 0, 0);
        let item = |x, y| rfr::Item {
            pos: Some(rfr::Coord { x: Some(x), y: Some(y), z: Some(0) }),
            ..Default::default()
        };
        // One sock at 1,1; a stockpile square at 2,2.
        b.items.push(item(1, 1));
        for _ in 0..factory::PILE_ITEMS {
            b.items.push(item(2, 2));
        }
        world.absorb(reply(vec![b]), (0, 0, 0));
        let chunk = world.chunk(0, 0, 0).unwrap();
        assert!(chunk.pile(1, 1).is_none(), "one item is litter");
        assert_eq!(chunk.pile(2, 2).map(|p| p.items), Some(factory::PILE_ITEMS));
    }

    #[test]
    fn a_restored_chunk_keeps_its_buildings_and_loses_their_material() {
        // What the cache carries: the voxels, not the side tables.
        let mut world = empty_world();
        let mut voxels = vec![Voxel::default(); TILES_PER_BLOCK];
        voxels[0].building = factory::building_type::TABLE as i16;
        assert!(world.restore((0, 0, 0), voxels));
        let chunk = world.chunk(0, 0, 0).unwrap();
        assert!(chunk.get(0, 0).built_over());
        assert_eq!(chunk.built(0, 0).sheet, 0, "stone until the game sends the block");
    }
}
