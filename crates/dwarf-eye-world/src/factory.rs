//! What a Dwarf Fortress entity is, and who draws it.
//!
//! Dwarf Fortress hands us one undifferentiated stream of tiles. Deciding what
//! each one is was scattered across the mesher as early-outs — a check for a
//! trunk here, a canopy part there, a render mode somewhere else — so a new
//! treatment meant editing three places and hoping nothing else matched first.
//!
//! This is the one place that decides. [`classify`] says what an entity is,
//! [`extent`] says how much of the map it is allowed to fill, and [`resolve`]
//! says who draws it: the sprite library that has always drawn everything, the
//! vegetation crate, or a crossed billboard. Nothing here knows about meshes
//! or chunks, so it can be tested on its own.
//!
//! Two rules hold for every override:
//!
//! - Dwarf Fortress gives the *bounds*, never the shape. A tree gets the height
//!   and reach of its tiles; a one-tile plant gets its tile. What grows inside
//!   that is the species preset's own, the same one the tree lab draws.
//! - A plant's seed is [`seed`]: a hash of the absolute tile it stands on and
//!   its species. Never render coordinates, never the tile configuration, so a
//!   plant is the same plant across a reload and wherever the origin sits.

use crate::tree::DETAIL;
use dfhack_remote::rfr::{TiletypeMaterial, TiletypeShape, TiletypeSpecial};
use dwarf_eye_trees as trees;
use dwarf_eye_trees::{TreeParams, VegetationKind};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::OnceLock;

/// What Dwarf Fortress says about one tile: everything the classifier reads.
#[derive(Clone, Copy, Debug)]
pub struct Tile<'a> {
    pub shape: TiletypeShape,
    pub material: TiletypeMaterial,
    pub special: TiletypeSpecial,
    /// DFHack's own name for the tiletype, such as `TreeDeadTrunkPillar`.
    pub name: &'a str,
    /// The raw id of the species standing here, empty when there is none.
    pub plant: &'a str,
}

impl Default for Tile<'_> {
    fn default() -> Self {
        Self {
            shape: TiletypeShape::NoShape,
            material: TiletypeMaterial::NoMaterial,
            special: TiletypeSpecial::NoSpecial,
            name: "",
            plant: "",
        }
    }
}

/// What the tiles around one tile say about it.
///
/// Only what a treatment cannot work out from the tile alone belongs here. A
/// cap tile is a floor or a wall as far as its shape goes, and DF's own tree
/// link is what separates a treetop from masonry.
#[derive(Clone, Copy, Debug, Default)]
pub struct Near {
    /// DF links this tile to a multi-tile plant standing somewhere else.
    pub in_tree: bool,
}

/// What an entity is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// Any part of a living multi-tile tree: trunk, limb, twig, cap.
    Tree,
    /// A low woody plant filling one tile.
    Shrub,
    /// A young tree, one tile of it so far.
    Sapling,
    /// Dead vegetation, whatever size: bare wood, no foliage.
    DeadTree,
    /// A tuft of blades.
    TallGrass,
    /// A rock lying on the floor.
    Boulder,
    /// Built work: a constructed wall or floor, or a building's tile. Nothing
    /// grows into it, and no crown is allowed to fill it.
    Built,
    /// One tile of a Dwarf Fortress building, carrying DF's own building type.
    /// Built work as far as vegetation is concerned, and drawn on top of the
    /// tile rather than instead of it.
    Building(i32),
    /// A creature standing in a tile: the adventurer, a dwarf, a pig.
    Unit,
    /// Enough loose items in one tile to read as a pile rather than as litter.
    ItemPile,
    /// Ground, walls, everything the sprite library already draws well.
    Other,
}

/// What one tile of terrain does to the ground surface.
///
/// The heightfield draws [`Footing::Ground`] and [`Footing::Slope`] as one
/// smoothed sheet; [`Footing::Cliff`] is where that sheet stops and holds its
/// height, and [`Footing::Tile`] is everything that keeps the geometry the
/// sprite library has always given it — constructed floors, stairs, buildings,
/// a tree's own wood.
///
/// Decided from the tiletype alone, so it is cached with the rest of the plan
/// and a mesher pays one lookup. Whether a *particular* cliff has a ramp
/// against it is the mesher's question, not this one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Footing {
    /// Natural ground: soil, sand, grass, a natural stone floor, and the tile
    /// a shrub or a boulder stands in, which DF reports with no floor of its
    /// own.
    Ground,
    /// A natural ramp: the slope that connects two levels of natural ground.
    Slope,
    /// A natural wall the ground stops against.
    Cliff,
    /// Keeps its tile geometry.
    #[default]
    Tile,
}

/// Whether a material is ground that formed rather than ground someone laid.
fn natural(material: TiletypeMaterial) -> bool {
    use TiletypeMaterial as M;
    matches!(
        material,
        M::Soil
            | M::Stone
            | M::Feature
            | M::LavaStone
            | M::Mineral
            | M::FrozenLiquid
            | M::GrassLight
            | M::GrassDark
            | M::GrassDry
            | M::GrassDead
            | M::Plant
            | M::Mushroom
            | M::Ashes
            | M::Driftwood
            | M::Pool
            | M::Brook
            | M::River
            | M::Hfs
    )
}

/// Whether somebody worked this tile: smoothed it, carved a track into it,
/// ploughed it. Worked ground is built work as far as the surface goes.
fn worked(special: TiletypeSpecial) -> bool {
    matches!(
        special,
        TiletypeSpecial::Smooth
            | TiletypeSpecial::SmoothDead
            | TiletypeSpecial::Track
            | TiletypeSpecial::Furrowed
    )
}

/// What a tile does to the ground surface.
pub fn footing(tile: Tile, class: Class) -> Footing {
    use TiletypeShape as S;
    // A tree's wood, built work, a building's tile: all of them draw
    // themselves, and none of them is ground.
    if matches!(class, Class::Tree | Class::Built | Class::Building(_) | Class::Unit | Class::ItemPile)
        || !natural(tile.material)
        || worked(tile.special)
    {
        return Footing::Tile;
    }
    match tile.shape {
        S::Floor | S::Pebbles | S::BrookTop | S::BrookBed => Footing::Ground,
        // A plant or a boulder fills its tile outright and DF reports no floor
        // under it, so the ground it stands on is the heightfield's to draw.
        S::Shrub | S::Sapling | S::Boulder => Footing::Ground,
        S::Ramp => Footing::Slope,
        S::Wall | S::Fortification => Footing::Cliff,
        _ => Footing::Tile,
    }
}

/// How much of the map an entity may fill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Extent {
    /// One tile, floor to ceiling. A shrub never leans into its neighbour.
    Tile,
    /// The tiles DF reports for a tree, read by `tree::Envelope`.
    Tree,
}

/// Who draws an entity.
#[derive(Clone, Debug)]
pub enum Treatment {
    /// The sprite library: DF's own art, extruded or laid flat.
    Sprite,
    /// Grown by `dwarf-eye-trees` from a species preset, inside the entity's
    /// extent.
    Grown(VegetationKind, Box<TreeParams>),
    /// Two crossed planes of sprite. The old treatment for standing plants,
    /// kept for comparison.
    Billboard,
    /// A box filling the entity's footprint, this fraction of a z-level tall,
    /// wearing the entity's own sprite on its lid where the raws name one.
    /// The placeholder for a building until a `.vox` prefab replaces it.
    Massing(f32),
    /// A capsule standing in a tile, at the size DF gives the creature.
    Capsule,
}

impl Treatment {
    pub fn is_grown(&self) -> bool {
        matches!(self, Treatment::Grown(..))
    }
}

// ---------------------------------------------------------------------------
// The treatment is a chain
// ---------------------------------------------------------------------------

/// Sub-voxels to a tile a standing plant is cut into. The chain's own copy of
/// `canopy::DEFAULT_PLANT_DETAIL`, so a chain can be read without a mesher.
pub const PLANT_DETAIL: i32 = 2;

/// How far the sun's shadow cascades reach, in tiles, mirroring Bevy's default
/// `CascadeShadowConfig`.
///
/// Nothing inside this may be shadowless, so it is the floor under the last
/// stage that still casts: the crown stage never hands over to the box stage
/// nearer than this, whatever the projected-size rule asks for.
pub const SHADOW_DISTANCE: f32 = 150.0;

/// How far the canonical crown runs, as a multiple of the near band.
///
/// Twice the reach of the one-voxel cut it replaces, which is the ratio the far
/// band shipped with — its crown stage ran to `3 N` behind a grown stage that
/// stopped at `1.5 N`. The cut it stands behind now ends where its own voxel
/// falls under the pixel floor, at `4 N`, so the crown ends at `8 N`.
pub const CROWN_REACH: f32 = 8.0;

/// How one stage of a [`Chain`] is drawn.
///
/// This is the whole of a stage's identity as far as a mesh cache is concerned:
/// two stages that differ only in where they hand over draw the same geometry.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Detail {
    /// Rasterised voxels, `per_tile` of them to a tile edge. `cutout` is
    /// whether the leaves keep their alpha mask, `undergrowth` whether standing
    /// plants and tufts come with them, `strands` whether a weeping crown's
    /// curtains do.
    Voxels { per_tile: i32, cutout: bool, undergrowth: bool, strands: bool },
    /// One canonical crown per preset: a trunk under one to three boxes
    /// (`dwarf_eye_trees::crown`).
    Crown,
    /// A box: a crown's own bounds (`dwarf_eye_trees::crown_box`), or a
    /// building's footprint.
    Box,
    /// Two crossed planes of sprite.
    Billboard,
    /// A voxel prefab instance, such as vox-uristi's `.vox` buildings.
    Prefab,
    /// A liquid's own surface, as the mesher draws it.
    Surface,
    /// One flat tinted quad over a whole body of water.
    Quad,
    /// Nothing of its own: the colour is baked into the coarse heightfield.
    Baked,
}

impl Detail {
    /// Sub-voxels to a tile, for the stages that are rasterised. The stages
    /// that are not have no leaf voxel, and so no projected-size rule of their
    /// own.
    pub fn per_tile(self) -> Option<i32> {
        match self {
            Detail::Voxels { per_tile, .. } => Some(per_tile),
            _ => None,
        }
    }

    /// Whether this cut keeps the leaf cutout. A cutout costs a masked pass, a
    /// discard in the depth prepass and the overdraw behind every hole, which
    /// is worth paying while a hole is still about a pixel across.
    pub fn cutout(self) -> bool {
        matches!(self, Detail::Voxels { cutout: true, .. })
    }

    /// Whether the ground cover comes with this cut: standing plants and tufts.
    pub fn undergrowth(self) -> bool {
        matches!(self, Detail::Voxels { undergrowth: true, .. })
    }

    /// Whether a weeping crown's strands come with this cut. They are quads the
    /// growth crate has already meshed, so carrying them one stage further out
    /// costs a copy rather than a rasterisation.
    pub fn strands(self) -> bool {
        matches!(self, Detail::Voxels { strands: true, .. })
    }
}

/// Where a stage hands over to the next.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum EdgeRule {
    /// The projected-size rule on this stage's own leaf voxel: it holds until
    /// that voxel stops covering the pixel floor, which is `DETAIL / per_tile`
    /// times as far out as the near band.
    Projected,
    /// A fixed multiple of the near band, never nearer than `at_least` tiles.
    /// For a stage the projected rule would reach further with than the
    /// triangle budget allows, or one something else sets a floor under.
    Reach { of_near: f32, at_least: f32 },
    /// The last stage of a chain: it runs to the camera's far plane, and its
    /// edge is never asked for.
    Far,
    /// The builder does not exist yet, so where the stage would hand over is
    /// not a number anyone has measured. Consumers skip it.
    Unbuilt,
}

/// One link of a [`Chain`]: a builder, and the projected size it holds down to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stage {
    pub detail: Detail,
    pub edge: EdgeRule,
}

impl Stage {
    /// Where this stage hands over to the next, in tiles, given where the near
    /// band ends.
    ///
    /// The one edge function: the window's canopy bands and the horizon's
    /// instances both come through here, so the two hand over by the same
    /// numbers. A stage with no edge — the last of a chain, or one nobody can
    /// build yet — never hands over, and says so with infinity.
    pub fn edge(self, near: f32) -> f32 {
        match self.edge {
            EdgeRule::Projected => {
                let per_tile = self.detail.per_tile().unwrap_or(DETAIL).max(1);
                near * DETAIL as f32 / per_tile as f32
            }
            EdgeRule::Reach { of_near, at_least } => (near * of_near).max(at_least),
            EdgeRule::Far | EdgeRule::Unbuilt => f32::INFINITY,
        }
    }

    /// What a [`StageCache`] keys this stage by.
    pub fn key(self) -> Detail {
        self.detail
    }

    /// Whether anything can build this stage today. A chain lists the stages the
    /// design calls for; a consumer skips the ones that have no builder.
    pub fn built(self) -> bool {
        self.edge != EdgeRule::Unbuilt
    }
}

/// Who draws a class at every range, coarsening outward.
///
/// [`resolve`] is the head of this — the one treatment a consumer with no level
/// of detail of its own uses. The chain is the same answer as a list, so the
/// window's chunk spawner and the horizon's scatter take their stages from one
/// place and their hand-off distances from one rule.
#[derive(Clone, Debug)]
pub struct Chain {
    pub stages: Vec<Stage>,
}

impl Chain {
    /// Every stage, nearest first.
    pub fn stages(&self) -> &[Stage] {
        &self.stages
    }

    /// The stages a chunk mesher builds: the rasterised cuts at the head of the
    /// chain. The window holds fine data per entity, so it draws the entity
    /// itself rather than a canonical one.
    pub fn window(&self) -> &[Stage] {
        &self.stages[..self.cuts()]
    }

    /// The stages an instanced consumer draws: all of them.
    ///
    /// The horizon's scatter has no per-tree data to preserve, so it draws a
    /// handful of canonical growths per species instead — but at the same cuts,
    /// at the same distances. Detail is the camera's distance to a tree and
    /// never which survey the tree came from, which is what keeps the window's
    /// boundary out of the canopy.
    pub fn instanced(&self) -> &[Stage] {
        &self.stages
    }

    /// How many rasterised cuts the chain opens with.
    fn cuts(&self) -> usize {
        self.stages.iter().take_while(|s| s.detail.per_tile().is_some()).count()
    }
}

/// One hand-off distance per gap in a run of stages, nearest first.
///
/// The last stage a consumer draws runs to its own far plane and has no edge,
/// which is why this is one shorter than the run. Unbuilt stages are the
/// caller's to drop first: their edges are not numbers.
pub fn edges(stages: &[Stage], near: f32) -> Vec<f32> {
    let last = stages.len().saturating_sub(1);
    stages[..last].iter().map(|s| s.edge(near)).collect()
}

/// The chain a class is drawn by, coarsening outward.
///
/// Written out in full even where only the first stage has a builder: the list
/// is the design, and a stage nobody can build yet carries [`EdgeRule::Unbuilt`]
/// rather than an invented distance.
pub fn chain(class: Class, style: Style) -> Chain {
    let cut = |per_tile, cutout, undergrowth, strands| Detail::Voxels {
        per_tile,
        cutout,
        undergrowth,
        strands,
    };
    let projected = |detail| Stage { detail, edge: EdgeRule::Projected };
    let reach =
        |detail, of_near, at_least| Stage { detail, edge: EdgeRule::Reach { of_near, at_least } };
    let last = |detail| Stage { detail, edge: EdgeRule::Far };
    let unbuilt = |detail| Stage { detail, edge: EdgeRule::Unbuilt };
    let stages = match class {
        // Full voxels, three coarser cuts, the canonical crown, its box. The
        // window draws the four cuts — they are all a chunk mesher can build —
        // and the horizon draws the whole list, so a tree just outside the
        // window is cut exactly as coarsely as one just inside it at the same
        // distance and no more.
        Class::Tree | Class::DeadTree => vec![
            projected(cut(DETAIL, true, true, true)),
            projected(cut(DETAIL - 1, true, false, true)),
            projected(cut(DETAIL / 2, true, false, true)),
            projected(cut(DETAIL / 4, false, false, false)),
            reach(Detail::Crown, CROWN_REACH, SHADOW_DISTANCE),
            last(Detail::Box),
        ],
        // Grown, then a billboard, then nothing. Only the growth is built: a
        // standing plant is dropped past the near band today rather than handed
        // to a coarser stage.
        Class::Shrub | Class::Sapling | Class::TallGrass => match style {
            Style::Grown => vec![
                projected(cut(PLANT_DETAIL, true, true, false)),
                unbuilt(Detail::Billboard),
                unbuilt(Detail::Baked),
            ],
            Style::Billboard => vec![projected(Detail::Billboard), unbuilt(Detail::Baked)],
        },
        Class::Boulder => vec![projected(Detail::Billboard), unbuilt(Detail::Baked)],
        // Fine cubes, the prefab that replaces them, the footprint box, nothing.
        // Only the cubes the sprite mesher already draws are built.
        Class::Built | Class::Building(_) | Class::ItemPile => vec![
            projected(cut(1, false, false, false)),
            unbuilt(Detail::Prefab),
            unbuilt(Detail::Box),
            unbuilt(Detail::Baked),
        ],
        Class::Unit | Class::Other => vec![projected(cut(1, false, false, false))],
    };
    Chain { stages }
}

/// Water's own chain: the surface the mesher draws, then one flat tinted quad
/// over the whole body. Water is not a [`Class`] — it rides on a tile rather
/// than being one — so it has its own entry point.
pub fn water_chain() -> Chain {
    Chain {
        stages: vec![
            Stage { detail: Detail::Surface, edge: EdgeRule::Projected },
            Stage { detail: Detail::Quad, edge: EdgeRule::Unbuilt },
        ],
    }
}

/// The tree chain, resolved once. Its stages are what both the window's canopy
/// bands and the horizon's instances are cut from.
pub fn tree_chain() -> &'static Chain {
    static CHAIN: OnceLock<Chain> = OnceLock::new();
    CHAIN.get_or_init(|| chain(Class::Tree, Style::Grown))
}

/// Meshes built once per key and stage, and kept.
///
/// The key is whatever varies inside a stage: a tree's origin tile for the
/// window, where every tree is its own; a preset and growth variant for the
/// horizon, where a handful of canonical shapes serve thousands of instances.
/// The stage is [`Stage::key`], so a coarser cut of the same tree is its own
/// entry and never overwrites the fine one.
pub struct StageCache<K, V> {
    entries: HashMap<(K, Detail), V>,
}

impl<K, V> Default for StageCache<K, V> {
    fn default() -> Self {
        Self { entries: HashMap::new() }
    }
}

impl<K: Eq + Hash + Copy, V> StageCache<K, V> {
    pub fn get(&self, key: K, stage: Detail) -> Option<&V> {
        self.entries.get(&(key, stage))
    }

    pub fn insert(&mut self, key: K, stage: Detail, value: V) {
        self.entries.insert((key, stage), value);
    }

    pub fn contains(&self, key: K, stage: Detail) -> bool {
        self.entries.contains_key(&(key, stage))
    }

    /// Keeps the entries the predicate accepts, key and stage together.
    pub fn retain(&mut self, mut keep: impl FnMut(K, Detail) -> bool) {
        self.entries.retain(|&(key, stage), _| keep(key, stage));
    }

    /// Every entry, as key, stage and value.
    pub fn iter(&self) -> impl Iterator<Item = (K, Detail, &V)> {
        self.entries.iter().map(|(&(key, stage), value)| (key, stage, value))
    }

    /// The values held for one stage.
    pub fn at(&self, stage: Detail) -> impl Iterator<Item = &V> {
        self.entries.iter().filter(move |((_, s), _)| *s == stage).map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// What kind of thing a DF building is, as far as a treatment cares.
///
/// DF has fifty-five building types and they want about ten looks between
/// them; this is the grouping, and it is what decides how tall a massing box
/// stands and which sheet its lid comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuildingKind {
    /// Fills the tile it stands in: a door, a floodgate, wall bars.
    Door,
    /// A lid on the floor: a hatch, a floor grate, floor bars.
    Hatch,
    /// A chair, a table, a bed, a cabinet, a box, a rack.
    Furniture,
    /// Nearly a tile tall and worth reading as such.
    Statue,
    Well,
    /// A multi-tile shop or furnace, drawn as its top-down sprite on a slab.
    Workshop,
    /// A bridge or a road: a floor someone laid.
    Bridge,
    /// Gears, axles, wheels, pumps.
    Machine,
    /// A stockpile or an activity zone: a designation, not a thing.
    Zone,
    Other,
}

impl BuildingKind {
    /// Whether anything is drawn for a building of this kind.
    ///
    /// A zone is a rectangle the player drew on the floor, and a road is
    /// already a constructed floor in the tile stream; drawing either would
    /// only bury the ground.
    pub fn drawn(self) -> bool {
        !matches!(self, BuildingKind::Zone)
    }

    /// How tall its massing box stands, as a fraction of a z-level.
    pub fn height(self) -> f32 {
        match self {
            BuildingKind::Door => 1.0,
            BuildingKind::Hatch => 0.14,
            BuildingKind::Bridge => 0.18,
            BuildingKind::Furniture => 0.5,
            BuildingKind::Statue => 0.92,
            BuildingKind::Well => 0.8,
            BuildingKind::Workshop => 0.3,
            BuildingKind::Machine => 0.6,
            BuildingKind::Zone => 0.0,
            BuildingKind::Other => 0.45,
        }
    }
}

/// DF's `building_type` values, as of 53.16. Only the ones with a treatment
/// are named; the rest fall through to [`BuildingKind::Other`].
pub mod building_type {
    pub const CHAIR: i32 = 0;
    pub const BED: i32 = 1;
    pub const TABLE: i32 = 2;
    pub const COFFIN: i32 = 3;
    pub const FARM_PLOT: i32 = 4;
    pub const FURNACE: i32 = 5;
    pub const DOOR: i32 = 8;
    pub const FLOODGATE: i32 = 9;
    pub const BOX: i32 = 10;
    pub const WEAPON_RACK: i32 = 11;
    pub const ARMOR_STAND: i32 = 12;
    pub const WORKSHOP: i32 = 13;
    pub const CABINET: i32 = 14;
    pub const STATUE: i32 = 15;
    pub const WELL: i32 = 18;
    pub const BRIDGE: i32 = 19;
    pub const ROAD_DIRT: i32 = 20;
    pub const ROAD_PAVED: i32 = 21;
    pub const SUPPORT: i32 = 25;
    pub const ARCHERY_TARGET: i32 = 26;
    pub const CAGE: i32 = 28;
    pub const STOCKPILE: i32 = 29;
    pub const CIVZONE: i32 = 30;
    pub const SCREW_PUMP: i32 = 33;
    pub const CONSTRUCTION: i32 = 34;
    pub const HATCH: i32 = 35;
    pub const GRATE_WALL: i32 = 36;
    pub const GRATE_FLOOR: i32 = 37;
    pub const BARS_VERTICAL: i32 = 38;
    pub const BARS_FLOOR: i32 = 39;
    pub const GEAR_ASSEMBLY: i32 = 40;
    pub const AXLE_HORIZONTAL: i32 = 41;
    pub const AXLE_VERTICAL: i32 = 42;
    pub const WATER_WHEEL: i32 = 43;
    pub const WINDMILL: i32 = 44;
    pub const TRACTION_BENCH: i32 = 45;
    pub const SLAB: i32 = 46;
    pub const NEST_BOX: i32 = 48;
    pub const HIVE: i32 = 49;
    pub const BOOKCASE: i32 = 52;
    pub const DISPLAY_FURNITURE: i32 = 53;
    pub const OFFERING_PLACE: i32 = 54;
}

/// Which look a DF building type takes.
pub fn building_kind(building_type: i32) -> BuildingKind {
    use BuildingKind as K;
    use self::building_type as T;
    match building_type {
        T::DOOR | T::FLOODGATE | T::GRATE_WALL | T::BARS_VERTICAL => K::Door,
        T::HATCH | T::GRATE_FLOOR | T::BARS_FLOOR | T::SLAB => K::Hatch,
        T::CHAIR | T::BED | T::TABLE | T::COFFIN | T::BOX | T::WEAPON_RACK | T::ARMOR_STAND
        | T::CABINET | T::CAGE | T::TRACTION_BENCH | T::NEST_BOX | T::HIVE | T::BOOKCASE
        | T::DISPLAY_FURNITURE | T::OFFERING_PLACE | T::ARCHERY_TARGET => K::Furniture,
        T::STATUE | T::SUPPORT => K::Statue,
        T::WELL => K::Well,
        T::WORKSHOP | T::FURNACE => K::Workshop,
        T::BRIDGE => K::Bridge,
        T::GEAR_ASSEMBLY | T::AXLE_HORIZONTAL | T::AXLE_VERTICAL | T::WATER_WHEEL
        | T::WINDMILL | T::SCREW_PUMP => K::Machine,
        // Zones and roads are designations and floors; the tile stream already
        // carries anything they changed on the ground.
        T::STOCKPILE | T::CIVZONE | T::CONSTRUCTION | T::ROAD_DIRT | T::ROAD_PAVED
        | T::FARM_PLOT => K::Zone,
        _ => K::Other,
    }
}

/// DF's workshop and furnace subtypes under the names the graphics raws give
/// them. `WORKSHOP_CARPENTER`, not `Carpenters`.
const WORKSHOPS: [&str; 25] = [
    "WORKSHOP_CARPENTER",
    "WORKSHOP_FARMER",
    "WORKSHOP_MASON",
    "WORKSHOP_CRAFTS",
    "WORKSHOP_JEWELER",
    "WORKSHOP_METALSMITH",
    "WORKSHOP_METALSMITH_LAVA",
    "WORKSHOP_BOWYER",
    "WORKSHOP_MECHANIC",
    "WORKSHOP_SIEGE",
    "WORKSHOP_BUTCHER",
    "WORKSHOP_LEATHER",
    "WORKSHOP_TANNER",
    "WORKSHOP_CLOTHES",
    "WORKSHOP_FISHERY",
    "WORKSHOP_STILL",
    "WORKSHOP_LOOM",
    "WORKSHOP_QUERN",
    "WORKSHOP_KENNEL",
    "WORKSHOP_KITCHEN",
    "WORKSHOP_ASHERY",
    "WORKSHOP_DYER",
    "WORKSHOP_MILLSTONE",
    "WORKSHOP_CUSTOM",
    "WORKSHOP_TOOL",
];

const FURNACES: [&str; 8] = [
    "FURNACE_WOOD",
    "FURNACE_SMELTER",
    "FURNACE_GLASS",
    "FURNACE_KILN",
    "FURNACE_SMELTER_LAVA",
    "FURNACE_GLASS_LAVA",
    "FURNACE_KILN_LAVA",
    "FURNACE_CUSTOM",
];

/// DF's per-material sprite sheets for a thing, in the order the library
/// packs them. The empty sheet is for families that have no material variants.
pub const BUILDING_SHEETS: [&str; 5] = ["_STONE", "_WOOD", "_METAL", "_GLASS", ""];

/// Which sheets to try for a material, best first, as indices into
/// [`BUILDING_SHEETS`].
///
/// Nothing in DF's material pair says "this is a metal"; the name is what the
/// game itself puts on the screen, so it is what the sheets are chosen by. The
/// rest follow so a material with no sheet of its own still lands somewhere.
pub fn sheet_order(material: &str) -> [usize; 5] {
    const METALS: [&str; 14] = [
        "iron", "steel", "copper", "bronze", "brass", "silver", "gold", "platinum", "nickel",
        "bismuth", "adamantine", "aluminum", "lead", "tin",
    ];
    let name = material.to_ascii_lowercase();
    let first = if name.contains("wood") || name.contains("log") {
        1
    } else if name.contains("glass") || name.contains("crystal") {
        3
    } else if METALS.iter().any(|m| name.contains(m)) {
        2
    } else {
        0
    };
    let mut order = [first, 0, 0, 0, 0];
    let mut n = 1;
    for other in 0..BUILDING_SHEETS.len() {
        if other != first {
            order[n] = other;
            n += 1;
        }
    }
    order
}

/// How wide a workshop's sprite footprint is, in tiles.
pub const WORKSHOP_SPAN: i32 = 3;

/// The sprite families to try for one building tile, best first.
///
/// A shop is one sprite per square of its footprint; everything else is one
/// sprite per material sheet, under a handful of state suffixes, because DF
/// draws a closed door and an open one apart.
pub fn building_families(
    building_type: i32,
    subtype: i32,
    sheet: usize,
    sub: (i32, i32),
) -> Vec<String> {
    use self::building_type as T;
    if building_type == T::WORKSHOP || building_type == T::FURNACE {
        let table: &[&str] = if building_type == T::WORKSHOP { &WORKSHOPS } else { &FURNACES };
        let Some(name) = usize::try_from(subtype).ok().and_then(|i| table.get(i)) else {
            return Vec::new();
        };
        // The sheets carry a row above the shop for anything that sticks up
        // over it, so the footprint starts at row one.
        return vec![format!("{name}_{}_{}", sub.0, sub.1 + 1)];
    }
    let Some(stem) = building_stem(building_type) else { return Vec::new() };
    let Some(suffix) = BUILDING_SHEETS.get(sheet) else { return Vec::new() };
    ["", "_CLOSED", "_EMPTY", "_BLANK"].iter().map(|v| format!("{stem}{suffix}{v}")).collect()
}

/// The sprite family stem for a building type, before its material.
fn building_stem(building_type: i32) -> Option<&'static str> {
    use self::building_type as T;
    Some(match building_type {
        T::CHAIR => "ITEM_CHAIR",
        T::BED => "ITEM_BED",
        T::TABLE => "ITEM_TABLE",
        T::COFFIN => "ITEM_COFFIN",
        T::DOOR => "ITEM_DOOR",
        T::FLOODGATE => "ITEM_FLOODGATE",
        T::BOX => "ITEM_BOX",
        T::WEAPON_RACK => "ITEM_WEAPON_RACK",
        T::ARMOR_STAND => "ITEM_ARMOR_STAND",
        T::CABINET => "ITEM_CABINET",
        T::STATUE | T::DISPLAY_FURNITURE => "ITEM_STATUE",
        T::WELL => "BLD_WELL",
        T::SUPPORT => "BLD_SUPPORT",
        T::ARCHERY_TARGET => "BLD_ARCHERY_TARGET",
        T::CAGE => "ITEM_CAGE",
        T::HATCH => "ITEM_HATCH_COVER",
        T::GRATE_WALL => "ITEM_GRATE",
        T::GRATE_FLOOR => "ITEM_GRATE",
        T::BARS_VERTICAL => "BLD_VERTICAL_BARS",
        T::BARS_FLOOR => "BLD_FLOOR_BARS",
        T::SLAB => "ITEM_SLAB",
        T::TRACTION_BENCH => "ITEM_TRACTION_BENCH",
        T::NEST_BOX => "ITEM_NEST_BOX",
        T::HIVE => "ITEM_HIVE",
        T::BOOKCASE => "ITEM_BOOKCASE",
        T::BRIDGE => "BLD_BRIDGE",
        _ => return None,
    })
}

/// Which treatment one-tile plants get. Trees are always grown.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Style {
    /// Grown from the presets, the same way the lab grows them.
    #[default]
    Grown,
    /// The crossed sprite planes, for comparing against.
    Billboard,
}

impl Style {
    /// `DWARF_EYE_PLANTS=billboard` puts standing plants back on sprites.
    ///
    /// Read once. The mesher asks per tile, and the answer cannot change while
    /// the process runs.
    pub fn current() -> Self {
        static STYLE: std::sync::OnceLock<Style> = std::sync::OnceLock::new();
        *STYLE.get_or_init(|| match std::env::var("DWARF_EYE_PLANTS").as_deref() {
            Ok("billboard") | Ok("sprite") => Style::Billboard,
            _ => Style::Grown,
        })
    }
}

/// What an entity is, from its tile and what surrounds it.
pub fn classify(tile: Tile, near: Near) -> Class {
    use TiletypeShape as S;
    let dead = matches!(tile.special, TiletypeSpecial::Dead | TiletypeSpecial::SmoothDead)
        || tile.name.contains("Dead");
    let woody = tile.name.starts_with("Tree") || near.in_tree;

    // Built work first: someone raised it, so whatever shape it wears it is not
    // vegetation and nothing vegetal may grow into it.
    if tile.material == TiletypeMaterial::Construction {
        return Class::Built;
    }

    match tile.shape {
        // Limbs and twigs are only ever a tree's.
        S::Branch | S::TrunkBranch | S::Twig => {
            if dead {
                Class::DeadTree
            } else {
                Class::Tree
            }
        }
        S::Sapling => {
            if dead {
                Class::DeadTree
            } else {
                Class::Sapling
            }
        }
        // DF calls a tuft of blades a shrub too; its material is the giveaway.
        S::Shrub if grassy(tile.material) => Class::TallGrass,
        S::Shrub => {
            if dead {
                Class::DeadTree
            } else {
                Class::Shrub
            }
        }
        S::Boulder => Class::Boulder,
        // A trunk, a root, or the solid top of a cap tree: DF gives all three
        // an ordinary wall, ramp or floor shape, and only the name or the tree
        // link says otherwise.
        _ if woody && matches!(tile.material, TiletypeMaterial::TreeMaterial | TiletypeMaterial::Root)
            || woody && tile.name.starts_with("Tree") =>
        {
            if dead {
                Class::DeadTree
            } else {
                Class::Tree
            }
        }
        _ => Class::Other,
    }
}

/// Whether a material is one of DF's grasses.
fn grassy(material: TiletypeMaterial) -> bool {
    matches!(
        material,
        TiletypeMaterial::GrassLight
            | TiletypeMaterial::GrassDark
            | TiletypeMaterial::GrassDry
            | TiletypeMaterial::GrassDead
    )
}

/// How much room an entity has: its own tile, or the tiles DF gives its tree.
pub fn extent(tile: Tile) -> Extent {
    use TiletypeShape as S;
    match tile.shape {
        S::Sapling | S::Shrub | S::Boulder => Extent::Tile,
        _ => Extent::Tree,
    }
}

/// Who draws a class, and with what.
///
/// The parameters are the preset: the look the tree lab shows. Callers then
/// override the size from DF's bounds and the palette from the species' own
/// colours, and nothing else.
pub fn resolve(class: Class, style: Style) -> Treatment {
    let grown = |kind, params: TreeParams| Treatment::Grown(kind, Box::new(params));
    let standing = |kind, params: TreeParams| match style {
        Style::Grown => grown(kind, params),
        Style::Billboard => Treatment::Billboard,
    };
    match class {
        Class::Tree => grown(VegetationKind::Tree, trees::oak()),
        Class::DeadTree => grown(VegetationKind::DeadTree, trees::dead_tree()),
        Class::Shrub => standing(VegetationKind::Shrub, trees::shrub()),
        Class::Sapling => standing(VegetationKind::Sapling, trees::sapling()),
        Class::TallGrass => standing(VegetationKind::TallGrass, trees::tall_grass()),
        Class::Boulder => Treatment::Billboard,
        // A massing box until the `.vox` prefabs land: DF's own extent, DF's
        // own sprite on the lid, nothing invented inside it.
        Class::Building(t) => Treatment::Massing(building_kind(t).height()),
        Class::ItemPile => Treatment::Massing(PILE_HEIGHT),
        Class::Unit => Treatment::Capsule,
        // Built work keeps DF's own art until the building prefabs land.
        Class::Built | Class::Other => Treatment::Sprite,
    }
}

/// How tall a pile of loose items stands, as a fraction of a z-level.
pub const PILE_HEIGHT: f32 = 0.22;

/// How many items have to share a tile before they read as a pile.
///
/// One dropped sock is litter and costs a box for nothing; a stockpile square
/// is stacked, and that is the only item worth a voxel at this range.
pub const PILE_ITEMS: u8 = 3;

/// What a creature standing in a tile is.
///
/// Units never come out of the tile stream — DF reports them on their own —
/// so this is here to say that the factory owns the answer, not the poller.
pub fn classify_unit() -> Class {
    Class::Unit
}

/// What a building instance's tile is, from DF's own building type.
pub fn classify_building(building_type: i32) -> Class {
    Class::Building(building_type)
}

/// What a tile holding `items` loose items is.
pub fn classify_items(items: u8) -> Class {
    if items >= PILE_ITEMS { Class::ItemPile } else { Class::Other }
}

/// What DF says about an entity, decided once per tiletype.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
    pub class: Class,
    pub extent: Extent,
    /// What this tile does to the ground surface.
    pub footing: Footing,
}

impl Plan {
    /// Whether this entity is part of a multi-tile tree, whose bounds are read
    /// from the tiles DF reports for it rather than from this one tile.
    pub fn of_tree(self) -> bool {
        self.extent == Extent::Tree && matches!(self.class, Class::Tree | Class::DeadTree)
    }

    /// Whether this entity is grown rather than drawn from its sprite, so the
    /// tile mesher leaves it alone.
    ///
    /// Trees are always grown; the style only decides what happens to the
    /// plants that stand in a single tile.
    pub fn grown(self, style: Style) -> bool {
        match self.extent {
            Extent::Tree => self.of_tree(),
            Extent::Tile => {
                style == Style::Grown
                    && matches!(
                        self.class,
                        Class::Shrub | Class::Sapling | Class::TallGrass | Class::DeadTree
                    )
            }
        }
    }
}

/// The plan for one tile: what it is and how much room it has.
pub fn plan(tile: Tile, near: Near) -> Plan {
    let class = classify(tile, near);
    Plan { class, extent: extent(tile), footing: footing(tile, class) }
}

/// A plant's seed: the absolute tile it stands on, and what it is.
///
/// Absolute tiles rather than render coordinates, so a plant keeps its shape
/// when the origin moves; and not the tile configuration, so it does not change
/// as neighbouring tiles arrive.
pub fn seed(x: i32, y: i32, z: i32, species: i32) -> u64 {
    trees::rng::hash3(x, y, z, species as u64)
}

/// Whether a class is built work that vegetation may not grow into.
///
/// A building's footprint counts: DF puts no tree tile inside a workshop, but
/// a crown's envelope is a cylinder of the tree's whole reach, and without
/// this a tree beside a hall grows through its roof.
pub fn built(class: Class) -> bool {
    matches!(class, Class::Built | Class::Building(_))
}

/// Paints a preset in the colour Dwarf Fortress gives this plant.
///
/// The preset carries the shape and the shading between its tones; DF's own
/// colour for the tile — which for a plant is the species' colour out of its
/// raws — carries the hue, so a rhubarb is not an oak-green shrub and a dead
/// stem is not a living one. The tones stay a spread rather than becoming one
/// flat colour, because a single-colour plant reads as a cardboard cut-out.
pub fn recolour(params: &mut TreeParams, class: Class, hint: [u8; 3]) {
    let hint = trees::Rgb(hint[0], hint[1], hint[2]);
    if class == Class::DeadTree {
        // Nothing but wood on a dead plant, so the hue belongs to the bark.
        for bark in &mut params.palette.bark {
            *bark = bark.lerp(hint, 0.5);
        }
        return;
    }
    for leaf in &mut params.palette.leaf {
        *leaf = leaf.lerp(hint, 0.6);
    }
    params.palette.tip = params.palette.tip.lerp(hint, 0.45);
}

/// How tall a plant standing in one tile grows before it is fitted into that
/// tile, in the preset's own units.
///
/// The preset's proportions are the look, so a plant is grown at the size the
/// grammar expects and then scaled into the tile DF gave it. A dead plant is
/// the exception: the dead-tree preset is a thirteen-tile snag, and a dead
/// shrub is a handful of bare stems.
pub fn standing_height(class: Class, params: &TreeParams) -> f32 {
    match class {
        Class::DeadTree => 2.4,
        _ => params.height,
    }
}

/// Whether a species hangs its foliage on streamers.
///
/// Nothing in DF's growth tokens says weeping; only the raw id does.
pub fn weeping(plant_id: &str) -> bool {
    plant_id.contains("WILLOW")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile<'a>(shape: TiletypeShape, name: &'a str) -> Tile<'a> {
        Tile { shape, name, ..Default::default() }
    }

    #[test]
    fn standing_plants_classify_by_shape() {
        assert_eq!(classify(tile(TiletypeShape::Shrub, "Shrub"), Near::default()), Class::Shrub);
        assert_eq!(
            classify(tile(TiletypeShape::Sapling, "Sapling"), Near::default()),
            Class::Sapling
        );
        assert_eq!(classify(tile(TiletypeShape::Boulder, "Boulder"), Near::default()), Class::Boulder);
        assert_eq!(
            classify(tile(TiletypeShape::Floor, "GrassLightFloor1"), Near::default()),
            Class::Other
        );
    }

    #[test]
    fn a_tree_is_every_part_of_one() {
        for (shape, name) in [
            (TiletypeShape::Wall, "TreeTrunkPillar"),
            (TiletypeShape::Ramp, "TreeTrunkSlopeNS"),
            (TiletypeShape::TrunkBranch, "TreeTrunkBranchN"),
            (TiletypeShape::Branch, "TreeBranchesNSEW"),
            (TiletypeShape::Twig, "TreeTwigs"),
            (TiletypeShape::Floor, "TreeCapFloor1"),
        ] {
            assert_eq!(classify(tile(shape, name), Near::default()), Class::Tree, "{name}");
            assert_eq!(extent(tile(shape, name)), Extent::Tree, "{name}");
        }
    }

    #[test]
    fn a_wall_is_masonry_until_df_links_it_to_a_tree() {
        let wall = Tile {
            shape: TiletypeShape::Wall,
            material: TiletypeMaterial::Stone,
            ..Default::default()
        };
        assert_eq!(classify(wall, Near::default()), Class::Other);
        let cap = Tile { material: TiletypeMaterial::TreeMaterial, ..wall };
        assert_eq!(classify(cap, Near { in_tree: true }), Class::Tree);
    }

    #[test]
    fn dead_vegetation_keeps_its_own_extent() {
        let dead_shrub = Tile {
            shape: TiletypeShape::Shrub,
            special: TiletypeSpecial::Dead,
            name: "ShrubDead",
            ..Default::default()
        };
        assert_eq!(classify(dead_shrub, Near::default()), Class::DeadTree);
        // Bare twigs in one tile, not a thirteen-tile snag.
        assert_eq!(extent(dead_shrub), Extent::Tile);

        let dead_trunk = tile(TiletypeShape::Wall, "TreeDeadTrunkPillar");
        assert_eq!(classify(dead_trunk, Near::default()), Class::DeadTree);
        assert_eq!(extent(dead_trunk), Extent::Tree);
    }

    #[test]
    fn a_grassy_shrub_is_a_tuft() {
        let grass = Tile {
            shape: TiletypeShape::Shrub,
            material: TiletypeMaterial::GrassLight,
            ..Default::default()
        };
        assert_eq!(classify(grass, Near::default()), Class::TallGrass);
    }

    #[test]
    fn built_work_is_never_vegetation() {
        // A constructed floor of the same stone as a cave floor, and a wall
        // someone raised: both are built, whatever shape DF gives them.
        for shape in [TiletypeShape::Floor, TiletypeShape::Wall, TiletypeShape::Fortification] {
            let built = Tile {
                shape,
                material: TiletypeMaterial::Construction,
                name: "ConstructedFloor",
                ..Default::default()
            };
            assert_eq!(classify(built, Near::default()), Class::Built);
            assert!(super::built(classify(built, Near::default())));
            assert!(matches!(resolve(Class::Built, Style::Grown), Treatment::Sprite));
        }
    }

    #[test]
    fn treatments_follow_the_class() {
        assert!(matches!(resolve(Class::Other, Style::Grown), Treatment::Sprite));
        assert!(matches!(resolve(Class::Boulder, Style::Grown), Treatment::Billboard));
        for (class, kind) in [
            (Class::Tree, VegetationKind::Tree),
            (Class::DeadTree, VegetationKind::DeadTree),
            (Class::Shrub, VegetationKind::Shrub),
            (Class::Sapling, VegetationKind::Sapling),
            (Class::TallGrass, VegetationKind::TallGrass),
        ] {
            match resolve(class, Style::Grown) {
                Treatment::Grown(got, params) => {
                    assert_eq!(got, kind, "{class:?}");
                    assert_eq!(params.kind, kind, "{class:?} preset");
                }
                other => panic!("{class:?} resolved to {other:?}"),
            }
        }
    }

    #[test]
    fn billboard_style_only_moves_the_standing_plants() {
        for class in [Class::Shrub, Class::Sapling, Class::TallGrass] {
            assert!(matches!(resolve(class, Style::Billboard), Treatment::Billboard));
        }
        // A tree is grown whatever the style says: the billboard path never
        // drew one in the first place.
        assert!(resolve(Class::Tree, Style::Billboard).is_grown());
    }

    #[test]
    fn a_plan_says_who_meshes_a_tile() {
        let shrub = plan(tile(TiletypeShape::Shrub, "Shrub"), Near::default());
        assert!(shrub.grown(Style::Grown));
        assert!(!shrub.grown(Style::Billboard), "the mesher draws the sprite again");
        let trunk = plan(tile(TiletypeShape::Wall, "TreeTrunkPillar"), Near::default());
        assert!(trunk.grown(Style::Billboard), "a tree is grown either way");
        let floor = plan(tile(TiletypeShape::Floor, "SoilFloor1"), Near::default());
        assert!(!floor.grown(Style::Grown));
    }

    #[test]
    fn a_seed_is_the_tile_and_the_species() {
        // Two plants of one species in neighbouring tiles are two plants.
        assert_ne!(seed(10, 20, 30, 4), seed(11, 20, 30, 4));
        assert_ne!(seed(10, 20, 30, 4), seed(10, 21, 30, 4));
        assert_ne!(seed(10, 20, 30, 4), seed(10, 20, 31, 4));
        // One species apart from another on the same tile.
        assert_ne!(seed(10, 20, 30, 4), seed(10, 20, 30, 5));
        // And the same plant every time it is asked for.
        assert_eq!(seed(-7, 3, 128, 9), seed(-7, 3, 128, 9));
    }

    #[test]
    fn a_building_is_massed_inside_one_level() {
        use building_type as T;
        for kind in [T::DOOR, T::TABLE, T::STATUE, T::WORKSHOP, T::BRIDGE, T::HATCH, T::WELL] {
            let class = classify_building(kind);
            assert!(built(class), "{kind} is built work");
            match resolve(class, Style::Grown) {
                Treatment::Massing(height) => {
                    assert!(height > 0.0 && height <= 1.0, "{kind} stands {height} of a level")
                }
                other => panic!("building {kind} resolved to {other:?}"),
            }
        }
    }

    #[test]
    fn a_zone_is_a_designation_and_draws_nothing() {
        use building_type as T;
        for kind in [T::STOCKPILE, T::CIVZONE, T::ROAD_PAVED, T::CONSTRUCTION, T::FARM_PLOT] {
            assert!(!building_kind(kind).drawn(), "{kind}");
            assert_eq!(building_kind(kind).height(), 0.0, "{kind}");
        }
        assert!(building_kind(T::DOOR).drawn());
    }

    #[test]
    fn a_unit_is_a_capsule_and_a_pile_is_a_low_box() {
        assert!(matches!(resolve(classify_unit(), Style::Grown), Treatment::Capsule));
        assert_eq!(classify_items(PILE_ITEMS - 1), Class::Other);
        assert_eq!(classify_items(PILE_ITEMS), Class::ItemPile);
        match resolve(Class::ItemPile, Style::Grown) {
            Treatment::Massing(h) => assert!(h < 0.5, "a pile is low"),
            other => panic!("a pile resolved to {other:?}"),
        }
    }

    #[test]
    fn a_material_picks_its_own_sheet_first_and_then_the_rest() {
        let sheet = |m: &str| BUILDING_SHEETS[sheet_order(m)[0]];
        assert_eq!(sheet("oaken wood"), "_WOOD");
        assert_eq!(sheet("green glass"), "_GLASS");
        assert_eq!(sheet("steel"), "_METAL");
        assert_eq!(sheet("granite"), "_STONE");
        // Every sheet is reachable, once each, whatever the material.
        let mut order = sheet_order("wax");
        order.sort();
        assert_eq!(order, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_shop_asks_for_one_sprite_per_square_of_its_footprint() {
        use building_type as T;
        let families: Vec<String> = (0..WORKSHOP_SPAN)
            .flat_map(|y| (0..WORKSHOP_SPAN).map(move |x| (x, y)))
            .flat_map(|at| building_families(T::WORKSHOP, 0, 0, at))
            .collect();
        assert_eq!(families.len(), 9);
        // DF's sheets keep row zero for whatever sticks up over the shop.
        assert!(families.contains(&"WORKSHOP_CARPENTER_0_1".to_string()));
        assert!(families.contains(&"WORKSHOP_CARPENTER_2_3".to_string()));
        assert!(building_families(T::WORKSHOP, 99, 0, (0, 0)).is_empty(), "no such shop");
    }

    #[test]
    fn furniture_asks_for_its_material_sheet_and_its_states() {
        let doors = building_families(building_type::DOOR, -1, 1, (0, 0));
        assert!(doors.contains(&"ITEM_DOOR_WOOD".to_string()));
        assert!(doors.contains(&"ITEM_DOOR_WOOD_CLOSED".to_string()));
        // A type with no sheet of its own asks for nothing rather than guessing.
        assert!(building_families(999, -1, 0, (0, 0)).is_empty());
    }

    #[test]
    fn only_willows_weep() {
        assert!(weeping("WILLOW"));
        assert!(weeping("BLACK_WILLOW"));
        assert!(!weeping("OAK"));
    }

    // ------------------------------------------------------------ the chain

    /// Where the near band ends for a 45-degree lens in a 720-tall window: the
    /// figure every edge below is a multiple of.
    const NEAR: f32 = 109.0;

    fn tree_stages() -> Vec<Stage> {
        tree_chain().stages().to_vec()
    }

    /// The window run is the four cuts the canopy bands ship with, in order,
    /// carrying what each one carries.
    #[test]
    fn the_window_run_is_the_four_bands_that_shipped() {
        let window: Vec<Detail> = tree_chain().window().iter().map(|s| s.detail).collect();
        assert_eq!(
            window,
            vec![
                Detail::Voxels { per_tile: 4, cutout: true, undergrowth: true, strands: true },
                Detail::Voxels { per_tile: 3, cutout: true, undergrowth: false, strands: true },
                Detail::Voxels { per_tile: 2, cutout: true, undergrowth: false, strands: true },
                Detail::Voxels { per_tile: 1, cutout: false, undergrowth: false, strands: false },
            ]
        );
    }

    /// The four band edges reproduce exactly: the near band's own distance, then
    /// its leaf voxel's rule on each coarser cut. The last band the window draws
    /// runs to the far plane and has no edge.
    #[test]
    fn the_band_edges_reproduce_bit_for_bit() {
        let want = [NEAR, NEAR * 4.0 / 3.0, NEAR * 2.0];
        assert_eq!(edges(tree_chain().window(), NEAR), want.to_vec());
    }

    /// The horizon draws the same stages at the same distances: its first three
    /// hand-offs are the window's, to the bit. Detail is the camera's distance
    /// and never the survey a tree came from.
    #[test]
    fn the_horizon_opens_on_the_window_s_own_edges() {
        let window = edges(tree_chain().window(), NEAR);
        let horizon = edges(tree_chain().instanced(), NEAR);
        assert_eq!(&horizon[..window.len()], &window[..]);
        assert_eq!(horizon.len(), tree_chain().stages().len() - 1);
        // Then the one-voxel cut on the same rule, and the crown, held out past
        // the cascades however small the window.
        assert_eq!(horizon[3], NEAR * 4.0);
        assert_eq!(horizon[4], (NEAR * CROWN_REACH).max(SHADOW_DISTANCE));
        assert_eq!(edges(tree_chain().instanced(), 1.0)[4], SHADOW_DISTANCE);
    }

    /// A chain coarsens outward: every stage is coarser than the one before it
    /// and hands over further out.
    #[test]
    fn a_chain_is_ordered_coarse_outward() {
        let stages = tree_stages();
        let cuts: Vec<i32> = stages.iter().filter_map(|s| s.detail.per_tile()).collect();
        assert!(cuts.windows(2).all(|w| w[0] > w[1]), "{cuts:?} does not coarsen");
        let at = edges(&stages, NEAR);
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?} is not monotone");
        // The last stage runs to the far plane and never hands over.
        assert_eq!(stages.last().unwrap().edge(NEAR), f32::INFINITY);
    }

    /// A cut never carries more than the cut before it: the ground cover goes
    /// first, then the strands, then the cutout.
    #[test]
    fn a_coarser_cut_never_carries_more() {
        let carried: Vec<(bool, bool, bool)> = tree_chain()
            .window()
            .iter()
            .map(|s| (s.detail.cutout(), s.detail.undergrowth(), s.detail.strands()))
            .collect();
        assert!(
            carried.windows(2).all(|w| w[0].0 >= w[1].0 && w[0].1 >= w[1].1 && w[0].2 >= w[1].2),
            "{carried:?}"
        );
    }

    /// Every stage of a chain is its own cache key, so a coarse copy never
    /// stands in for a fine one at the key they share.
    #[test]
    fn cache_keys_are_distinct_per_stage() {
        let mut cache: StageCache<u32, &'static str> = StageCache::default();
        for stage in tree_stages() {
            cache.insert(7, stage.key(), "held");
        }
        assert_eq!(cache.len(), tree_stages().len(), "two stages shared a cache key");
        for stage in tree_stages() {
            cache.insert(8, stage.key(), "held");
        }
        assert_eq!(cache.len(), 2 * tree_stages().len(), "two keys shared a stage");
        cache.retain(|key, _| key != 8);
        assert_eq!(cache.len(), tree_stages().len(), "retiring a key has to take every stage");
    }

    /// The chains the issue lists are written out in full, and a stage nobody
    /// can build yet says so rather than carrying an invented distance.
    #[test]
    fn every_chain_is_written_out_whole() {
        let plant: Vec<Detail> =
            chain(Class::Shrub, Style::Grown).stages().iter().map(|s| s.detail).collect();
        assert_eq!(
            plant,
            vec![
                Detail::Voxels {
                    per_tile: PLANT_DETAIL,
                    cutout: true,
                    undergrowth: true,
                    strands: false
                },
                Detail::Billboard,
                Detail::Baked,
            ]
        );
        let built: Vec<Detail> =
            chain(Class::Built, Style::Grown).stages().iter().map(|s| s.detail).collect();
        assert_eq!(
            built,
            vec![
                Detail::Voxels { per_tile: 1, cutout: false, undergrowth: false, strands: false },
                Detail::Prefab,
                Detail::Box,
                Detail::Baked,
            ]
        );
        let water: Vec<Detail> = water_chain().stages().iter().map(|s| s.detail).collect();
        assert_eq!(water, vec![Detail::Surface, Detail::Quad]);
        // Only the head of each is built today; the tree chain is built whole.
        for class in [Class::Shrub, Class::Built] {
            let resolved = chain(class, Style::Grown);
            let is_built: Vec<bool> = resolved.stages().iter().map(|s| s.built()).collect();
            let want: Vec<bool> = (0..is_built.len()).map(|i| i == 0).collect();
            assert_eq!(is_built, want, "{class:?}");
        }
        assert!(tree_chain().stages().iter().all(|s| s.built()));
    }
}
