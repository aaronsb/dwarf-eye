//! Resolves a tile to a cached voxel model built from DF's own sprite.

use crate::canopy::CanopyPart;
use crate::factory::{self, Plan};
use crate::mesh::MeshData;
use crate::model::{Caps, RenderMode, build_flat_tile, build_model};
use crate::palette::SandHue;
use crate::ramp;
use crate::wall;
use anyhow::Result;
use dfhack_remote::rfr::{
    PlantRawList, TiletypeList, TiletypeMaterial, TiletypeShape, TiletypeSpecial,
};
use dwarf_eye_art::atlas::{Atlas, Rect};
use dwarf_eye_art::{Art, Sprite, find_install, raws};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Sub-voxels per tile edge. Twelve keeps a trunk visibly round while leaving
/// the triangle count survivable; `DWARF_EYE_GRID` overrides it.
pub const DEFAULT_GRID: u32 = 12;

/// A cached tile mesh, plus whether it is a pattern awaiting a material colour.
#[derive(Clone)]
pub struct Model {
    pub mesh: Arc<MeshData>,
    /// True when the sprite was near-grey and should be multiplied by the
    /// tile's material colour.
    pub tint: bool,
}

/// Where a wall's faces sample the atlas.
///
/// The top is DF's own picture of the tile for this set of neighbouring walls;
/// the sides share one strip cut from the same sheet, because DF draws none.
#[derive(Clone, Copy)]
pub struct WallSkin {
    pub top: Rect,
    pub side: Rect,
    /// True when the sheet is a near-grey pattern for the material to colour.
    pub tint: bool,
}

/// One packed building sprite: which building, and which of its faces.
///
/// A workshop keys on its subtype and the square of its footprint; everything
/// else keys on the material sheet it was found on, and carries [`NOT_A_SHOP`]
/// where a shop would carry its offset.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BuildingCell {
    building: i32,
    subtype: i32,
    at: u8,
}

/// The `at` a building that is not a workshop carries.
const NOT_A_SHOP: u8 = 0xff;

/// How many workshop and furnace subtypes are asked for. DF has 25 and 8;
/// asking past them costs a failed lookup and nothing else.
const WORKSHOP_SUBTYPES: i32 = 32;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ModelKey {
    tile: i32,
    plant: i32,
    caps: Caps,
    /// Orientation, for geometry the mesher works out from neighbours.
    dirs: u8,
    /// The tile's own sand hue, where it has one.
    sand: Option<SandHue>,
}

struct TileInfo {
    family: String,
    /// Ground families to try in order; empty for everything else.
    candidates: Vec<String>,
    /// Ground to draw beneath a free-standing object.
    beneath: Option<&'static str>,
    /// Which of DF's ramp sheets a ramp tile draws from.
    ramp: Option<&'static str>,
    /// Whether this tiletype's material class is `Soil`: the gate for trying
    /// a sand hue's sheet in place of the generic soil one, on a floor, a wall
    /// or a ramp alike.
    soil: bool,
    dirs: u8,
    /// Neighbours a branch tile joins, read from DFHack's direction string.
    links: u8,
    mode: RenderMode,
    /// Species-independent tiles look straight at the generic sheet.
    generic: bool,
    /// Part of a tree's crown, meshed as a merged volume rather than a model.
    canopy: Option<CanopyPart>,
    /// Trunk or root: the woody column a crown hangs on.
    trunk: bool,
    /// What the entity factory makes of this tiletype: what it is, and how much
    /// room it has. Decided once here, so a mesher asking per tile pays one
    /// lookup rather than a chain of special cases.
    plan: Plan,
}

/// Decides how a tiletype's mask becomes geometry.
///
/// DF's shape enum already separates a flat scatter of pebbles from a boulder
/// sitting on top of one, so nothing has to be inferred from neighbours.
fn mode_for(shape: TiletypeShape, name: &str) -> Option<RenderMode> {
    use TiletypeShape as S;
    let tree = name.starts_with("Tree");
    match shape {
        // A sloping trunk is still a trunk, and roots are trunks underground.
        S::Wall | S::Ramp | S::TrunkBranch if tree => Some(RenderMode::Extrude),
        S::Branch | S::Twig => Some(RenderMode::ThinExtrude),
        S::Sapling | S::Shrub | S::Boulder => Some(RenderMode::Billboard),
        // Ground cover, including the walkable surface of a treetop.
        S::Floor | S::Pebbles => Some(RenderMode::FlatTile),
        S::Ramp => Some(RenderMode::Ramp),
        _ => None,
    }
}

/// Whether a tiletype belongs to a tree's crown.
///
/// Branches and twigs are leaves outright. The cap tiles are the solid part of
/// a treetop — floor where it is walkable, wall around its rim — and DF gives
/// them a wall or floor shape, so only the name separates them from masonry.
fn canopy_part(shape: TiletypeShape, name: &str) -> Option<CanopyPart> {
    use TiletypeShape as S;
    match shape {
        // A trunk branch is a heavy limb inside the crown. The graphics raws
        // call it TREE_HEAVY_BRANCH and DFHack calls it TreeTrunkBranch, so it
        // resolves to no sprite at all; folding it into the volume is both
        // right and what keeps it from drawing as a bare block among leaves.
        S::Branch | S::TrunkBranch => Some(CanopyPart::Branch),
        S::Twig => Some(CanopyPart::Twig),
        _ if name.starts_with("Tree") && name.contains("Cap") => Some(CanopyPart::Cap),
        _ => None,
    }
}

/// Below this mean saturation a sprite is a pattern to tint, not a colour.
const PATTERN_SATURATION: f32 = 0.22;

/// Foliage colour for a species whose sheets carry no twigs.
const DEFAULT_FOLIAGE: [u8; 3] = [82, 138, 58];

/// Wood colour for a species whose sheets carry no trunk.
const DEFAULT_BARK: [u8; 3] = [96, 72, 48];

/// `n` shades of one colour, dark to light, for when a sprite offers none.
fn spread(base: [u8; 3], n: usize) -> Vec<[u8; 3]> {
    (0..n)
        .map(|i| {
            let f = 0.72 + 0.56 * (i as f32 + 0.5) / n.max(1) as f32;
            [
                (base[0] as f32 * f).min(255.0) as u8,
                (base[1] as f32 * f).min(255.0) as u8,
                (base[2] as f32 * f).min(255.0) as u8,
            ]
        })
        .collect()
}

/// Height of a ground slab, as a fraction of a z-level.
const FLOOR_HEIGHT: f32 = 0.12;

/// The ground a free-standing object stands on.
///
/// A shrub or a boulder occupies its tile outright — DFHack reports no separate
/// floor underneath — so without this the tile is a hole with sky behind it.
fn ground_under(material: TiletypeMaterial) -> &'static str {
    use TiletypeMaterial as M;
    match material {
        M::GrassLight | M::GrassDark | M::GrassDry | M::GrassDead | M::Plant | M::Mushroom => {
            "GRASS_5"
        }
        M::Stone | M::Mineral | M::LavaStone | M::Feature | M::Construction => "STONE_FLOOR_5",
        // DF's natural ice floor is `ROUGH_ICE_FLOOR`, on the `FLOOR_ICE`
        // page. There is no `FROZEN_*` sprite at all, so asking for one left
        // every ice ramp and boulder standing on nothing.
        M::FrozenLiquid => "ROUGH_ICE_FLOOR",
        _ => "DIRT_FLOOR_5",
    }
}

/// Reconciles DFHack's tiletype names with the graphics raws' family names.
///
/// The two vocabularies drifted: DFHack says `TreeBranches`, the raws say
/// `TREE_BRANCH`; roots live under the environment sheet's `ROOT_WALL`.
fn alias(family: &str) -> &str {
    match family {
        "TREE_BRANCHES" | "TREE_BRANCHES_SMOOTH" => "TREE_BRANCH",
        "TREE_ROOTS" => "ROOT_WALL",
        "TREE_TRUNK_SLOPING" => "TREE_TRUNK_SLOPE",
        other => other,
    }
}

/// Ground families the environment sheets name differently from DFHack.
///
/// The numbered families are not variants of a texture — they are the nine
/// slices of one interlocking 3x3 edge pattern, and only the centre (`_5`) is
/// fully opaque. DF picks a slice per tile from its neighbours to make grass
/// interlock; a voxel view wants the solid centre everywhere.
///
/// The centre's own four variants (`_5`, `_5B`, `_5C`, `_5D`) line up with
/// DFHack's four floor variants, so the variety survives.
///
/// Returns candidates in preference order.
fn ground_alias(family: &str) -> Vec<String> {
    const SUFFIX: [&str; 4] = ["", "B", "C", "D"];

    let centre = |base: &str, n: &str| -> Vec<String> {
        let index = n.parse::<usize>().unwrap_or(1).saturating_sub(1).min(3);
        vec![
            format!("{base}_5{}", SUFFIX[index]),
            format!("{base}_5"),
        ]
    };

    for shade in [
        "GRASS_LIGHT_FLOOR_",
        "GRASS_DARK_FLOOR_",
        "GRASS_DRY_FLOOR_",
        "GRASS_DEAD_FLOOR_",
    ] {
        if let Some(n) = family.strip_prefix(shade) {
            return centre("GRASS", n);
        }
    }
    if let Some(n) = family.strip_prefix("SOIL_FLOOR_") {
        return centre("DIRT_FLOOR", n);
    }
    if let Some(n) = family.strip_prefix("STONE_FLOOR_") {
        return centre("STONE_FLOOR", n);
    }
    if let Some(n) = family.strip_prefix("STONE_PEBBLES_") {
        return centre("PEBBLES_FLOOR", n);
    }
    if family == "FURROWED_SOIL" {
        return vec!["FURROWED_SOIL_1".to_string()];
    }
    vec![family.to_string()]
}

/// The floor sheet a sand hue draws from. Tan is the unlabelled sheet DF
/// ships alongside the four named ones.
fn sand_floor_family(hue: SandHue) -> &'static str {
    match hue {
        SandHue::Tan => "SAND_FLOOR",
        SandHue::Yellow => "SAND_YELLOW_FLOOR",
        SandHue::White => "SAND_WHITE_FLOOR",
        SandHue::Black => "SAND_BLACK_FLOOR",
        SandHue::Red => "SAND_RED_FLOOR",
    }
}

/// A soil floor's sand counterpart for one hue, mirroring `ground_alias`'s
/// numbered-family reduction onto DF's sand floor sheets: `SoilFloor2`'s
/// `SOIL_FLOOR_2` becomes `SAND_YELLOW_FLOOR_5B` rather than `DIRT_FLOOR_5B`.
/// `None` when `family` is not one of DFHack's numbered soil floors.
fn sand_floor_alias(family: &str, hue: SandHue) -> Option<Vec<String>> {
    const SUFFIX: [&str; 4] = ["", "B", "C", "D"];
    let n = family.strip_prefix("SOIL_FLOOR_")?;
    let index = n.parse::<usize>().unwrap_or(1).saturating_sub(1).min(3);
    let base = sand_floor_family(hue);
    Some(vec![format!("{base}_5{}", SUFFIX[index]), format!("{base}_5")])
}

/// The single ground-beneath sheet a sand hue draws from — the fixed centre
/// variant `ground_under`'s `DIRT_FLOOR_5` names for every other soil, used
/// under a billboard and on a flat or unsheeted ramp.
fn sand_ground_family(hue: SandHue) -> &'static str {
    match hue {
        SandHue::Tan => "SAND_FLOOR_5",
        SandHue::Yellow => "SAND_YELLOW_FLOOR_5",
        SandHue::White => "SAND_WHITE_FLOOR_5",
        SandHue::Black => "SAND_BLACK_FLOOR_5",
        SandHue::Red => "SAND_RED_FLOOR_5",
    }
}

/// Which of DF's floor sheets a built floor draws from, or `None` for ground
/// the numbered families already name.
///
/// A construction's tiletype says only that somebody built it: `ConstructedFloor`,
/// the four `ShoddyConstructedFloor` cuts and the sixteen `ConstructedFloorTrack`
/// variants all report material `Construction` and nothing about the item they
/// were raised from. That item lives in the block's `construction_items` list,
/// and a voxel carries a material *index* without the material *type* that
/// separates a plank from a slab, so nothing here can tell a wooden roof from a
/// stone one. Every built floor therefore wears the block sheet and takes its
/// material's colour, the bargain a constructed wall already makes with
/// `ROCK_BLOCKS_WALL` ([walls.md]). DF ships `WOOD_FLOOR`, `METAL_FLOOR` and the
/// three `GLASS_*_FLOOR` sheets against the day a voxel carries the type too.
///
/// The rim of the slab is the wall sheet's own side strip, so a roof's edge is
/// the masonry it sits on rather than a white skirt.
///
/// [walls.md]: ../../../docs/architecture/textures/walls.md
fn construction_floor(material: TiletypeMaterial, shape: TiletypeShape) -> Option<&'static str> {
    matches!(
        (material, shape),
        (TiletypeMaterial::Construction, TiletypeShape::Floor)
    )
    .then_some("FLOOR_STONE_BLOCK")
}

/// A stable atlas key for one packed sprite.
fn atlas_key(family: &str, dirs: u8, plant_id: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    (family, dirs, plant_id).hash(&mut hasher);
    hasher.finish()
}

fn plant_index_of(plants: &[String], id: &str) -> i32 {
    plants.iter().position(|p| p == id).map(|i| i as i32).unwrap_or(-1)
}

/// Mean colour of a sprite's opaque pixels.
fn sprite_mean(sprite: &dwarf_eye_art::Sprite) -> Option<[u8; 3]> {
    let (mut sum, mut n) = ([0u32; 3], 0u32);
    for p in sprite.pixels.iter().filter(|p| p[3] >= 128) {
        for i in 0..3 {
            sum[i] += p[i] as u32;
        }
        n += 1;
    }
    (n > 0).then(|| [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8])
}


/// Sprite-derived tile models, built once per tiletype, species and cap pair.
pub struct TileLibrary {
    art: Art,
    tiles: HashMap<i32, TileInfo>,
    plants: Vec<String>,
    grid: u32,
    cache: HashMap<ModelKey, Option<Model>>,
    misses: HashSet<String>,
    /// Ground sprites packed into one texture, filled once at load so the
    /// renderer can upload it and never revisit it.
    atlas: Atlas,
    /// (tiletype, species) -> where its sprite sits in the atlas. Species is
    /// -1 for ground that does not vary by plant.
    flat_uv: HashMap<(i32, i32), Rect>,
    flat_tint: HashMap<(i32, i32), bool>,
    /// Ground families packed for use beneath free-standing objects.
    under_uv: HashMap<&'static str, (Rect, bool)>,
    under_model: HashMap<&'static str, Option<Model>>,
    /// (soil floor tiletype, sand hue) -> where that hue's floor sprite sits
    /// in the atlas, for the floors `flat_uv` would otherwise send to
    /// `DIRT_FLOOR` regardless of which sand the tile actually is.
    sand_flat_uv: HashMap<(i32, SandHue), (Rect, bool)>,
    /// Species index -> the colour its crown is painted, cached on first use.
    canopy_color: HashMap<i32, [u8; 3]>,
    /// Species index -> the colour of its wood.
    bark: HashMap<i32, [u8; 3]>,
    /// Species index -> where its leaf cutout sits in the atlas.
    leaf_uv: HashMap<i32, Rect>,
    /// Species index -> the tones its leaves and its wood are painted in.
    leaf_tones: HashMap<i32, Vec<[u8; 3]>>,
    bark_tones: HashMap<i32, Vec<[u8; 3]>>,
    /// Tiletype -> which of DF's wall sheets it draws from.
    walls: HashMap<i32, &'static str>,
    /// (wall family, neighbour mask) -> the top face for that set.
    wall_top: HashMap<(&'static str, u8), Rect>,
    /// Wall family -> the strip its four sides share, and whether the sheet is
    /// a pattern the material colours.
    wall_side: HashMap<&'static str, (Rect, bool)>,
    /// Built floor -> the wall family whose side strip skirts its slab.
    floor_rim: HashMap<i32, &'static str>,
    /// Building sprites, by which building wears them.
    building_uv: HashMap<BuildingCell, (Rect, bool)>,
    /// DF's ramp sprites, by full sprite name.
    ramp_uv: HashMap<String, (Rect, bool)>,
    /// Ramp geometry, by tiletype, the eight-neighbour wall mask and the
    /// tile's sand hue, where it has one.
    ramp_model: HashMap<(i32, u8, Option<SandHue>), Option<Model>>,
    /// Tiletypes the factory calls built work. Kept apart from `tiles`, which
    /// holds only what has a sprite treatment: a constructed wall is drawn as a
    /// plain block and would not be in there, and a tree still has to know not
    /// to grow into one.
    built: HashSet<i32>,
}

impl TileLibrary {
    pub fn load(tiletypes: &TiletypeList, plants: &PlantRawList) -> Result<Self> {
        let art = Art::load(&find_install()?)?;
        let grid = std::env::var("DWARF_EYE_GRID")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_GRID)
            .clamp(4, 32);

        let mut tiles = HashMap::new();
        let mut built = HashSet::new();
        let mut walls = HashMap::new();
        let mut floor_rim = HashMap::new();
        for t in &tiletypes.tiletype_list {
            let name = t.name();
            let entity = factory::Tile {
                shape: t.shape(),
                material: t.material(),
                special: t.special(),
                name,
                plant: "",
            };
            if factory::built(factory::classify(entity, factory::Near::default())) {
                built.insert(t.id);
            }
            // A wall is a cube the mesher draws itself, not a model: DF's wall
            // sheets are pictures of a tile seen from above, and voxelising one
            // would throw away what makes it worth having.
            if matches!(t.shape(), TiletypeShape::Wall | TiletypeShape::Fortification) {
                if let Some(family) = wall::family_for(t.material(), t.special(), name) {
                    walls.insert(t.id, family);
                }
            }
            let Some(mode) = mode_for(t.shape(), name) else { continue };
            let generic = matches!(mode, RenderMode::Billboard);
            let family = if generic {
                match t.shape() {
                    TiletypeShape::Sapling => "SAPLING".to_string(),
                    TiletypeShape::Shrub => "SHRUB".to_string(),
                    _ => "BOULDER".to_string(),
                }
            } else {
                alias(&raws::family_from_tiletype(name)).to_string()
            };
            // A built floor's own family names nothing on any sheet — there is
            // no `CONSTRUCTED_FLOOR` — so it goes straight to the block sheet.
            let built_floor = construction_floor(t.material(), t.shape());
            let candidates = match (mode, built_floor) {
                (RenderMode::FlatTile, Some(sheet)) => vec![sheet.to_string()],
                (RenderMode::FlatTile, None) => ground_alias(&family),
                _ => Vec::new(),
            };
            if built_floor.is_some() {
                if let Some(rim) = wall::family_for(t.material(), TiletypeSpecial::Normal, name) {
                    floor_rim.insert(t.id, rim);
                }
            }
            // Billboards need ground under them; ramps need it on their slope.
            let beneath = matches!(mode, RenderMode::Billboard | RenderMode::Ramp)
                .then(|| ground_under(t.material()));
            tiles.insert(
                t.id,
                TileInfo {
                    family,
                    candidates,
                    beneath,
                    ramp: (mode == RenderMode::Ramp)
                        .then(|| ramp::family_for(t.material()))
                        .flatten(),
                    soil: t.material() == TiletypeMaterial::Soil,
                    // The block sheet has no directional cuts, so the sixteen
                    // track floors would otherwise pack one sprite sixteen
                    // times under sixteen atlas keys.
                    dirs: if built_floor.is_some() {
                        0
                    } else {
                        raws::direction_mask(t.direction())
                    },
                    links: crate::skeleton::links_from_direction(t.direction()),
                    mode,
                    generic,
                    canopy: canopy_part(t.shape(), name),
                    trunk: mode == RenderMode::Extrude && canopy_part(t.shape(), name).is_none(),
                    // A cap tile wears an ordinary floor or wall shape, and
                    // DF's own name for it is the link to the tree.
                    plan: factory::plan(
                        entity,
                        factory::Near { in_tree: canopy_part(t.shape(), name).is_some() },
                    ),
                },
            );
        }

        // Plant materials index this list directly, so hold it dense.
        let mut ids = vec![String::new(); plants.plant_raws.len()];
        for p in &plants.plant_raws {
            let index = p.index() as usize;
            if index < ids.len() {
                ids[index] = p.id().to_string();
            }
        }

        let mut library = Self {
            art,
            tiles,
            plants: ids,
            grid,
            cache: HashMap::new(),
            misses: HashSet::new(),
            atlas: Atlas::new(),
            flat_uv: HashMap::new(),
            flat_tint: HashMap::new(),
            under_uv: HashMap::new(),
            under_model: HashMap::new(),
            sand_flat_uv: HashMap::new(),
            canopy_color: HashMap::new(),
            bark: HashMap::new(),
            leaf_uv: HashMap::new(),
            leaf_tones: HashMap::new(),
            bark_tones: HashMap::new(),
            walls,
            wall_top: HashMap::new(),
            wall_side: HashMap::new(),
            floor_rim,
            building_uv: HashMap::new(),
            ramp_uv: HashMap::new(),
            ramp_model: HashMap::new(),
            built,
        };
        library.pack_leaves();
        library.pack_ground();
        library.pack_under();
        library.pack_ramps();
        library.pack_walls();
        library.pack_buildings();
        Ok(library)
    }

    /// Packs every ground sprite into the atlas up front.
    ///
    /// The set is small and knowable — a few dozen ground families, plus canopy
    /// floors for the twenty species that ship their own sheet — so the atlas
    /// can be finished before the first frame and shipped as one texture.
    fn pack_ground(&mut self) {
        let flat: Vec<(i32, Vec<String>, u8, bool)> = self
            .tiles
            .iter()
            .filter(|(_, info)| info.mode == RenderMode::FlatTile)
            .map(|(id, info)| {
                (
                    *id,
                    info.candidates.clone(),
                    info.dirs,
                    info.family.starts_with("TREE"),
                )
            })
            .collect();

        let species: Vec<String> = self.art.index.plants.keys().cloned().collect();

        for (tile, candidates, dirs, per_species) in flat {
            let subjects: Vec<(i32, String)> = if per_species {
                species
                    .iter()
                    .enumerate()
                    .map(|(i, id)| (i as i32, id.clone()))
                    .collect()
            } else {
                vec![(-1, String::new())]
            };

            for (_, plant_id) in subjects {
                // Take the first candidate that exists, so a missing centre
                // variant falls back to the plain one.
                let found = candidates.iter().find_map(|family| {
                    self.art
                        .tree_sprite(&plant_id, family, dirs)
                        .cloned()
                        .map(|s| (family.clone(), s))
                });
                let Some((family, sprite)) = found else {
                    if let Some(first) = candidates.first() {
                        self.misses.insert(first.clone());
                    }
                    continue;
                };
                let pattern = sprite.saturation() < PATTERN_SATURATION;
                // Ground must be solid, so a sparse scatter fills its gaps.
                let backdrop = if pattern { Some([255, 255, 255]) } else { sprite_mean(&sprite) };
                let key = atlas_key(&family, dirs, &plant_id);
                let Some(rect) = self.atlas.insert(key, &sprite, backdrop) else { continue };

                // Species share ground sprites, so index every species at the
                // same rect rather than packing duplicates.
                let plant_index = if per_species { plant_index_of(&self.plants, &plant_id) } else { -1 };
                self.flat_uv.insert((tile, plant_index), rect);
                self.flat_tint.insert((tile, plant_index), pattern);
            }
        }

        self.pack_sand_floors();
    }

    /// Packs every sand hue's floor sheet for each soil floor tiletype.
    ///
    /// A specific tile's material tells sand from clay, not its tiletype, so
    /// this cannot fold into the loop above: every soil floor tiletype gets
    /// all five hues packed regardless of which (if any) actually turns up on
    /// the map, the same bargain `pack_ramps` and `pack_walls` make for the
    /// sheets they hold ready before the first block arrives.
    fn pack_sand_floors(&mut self) {
        let soil_floors: Vec<(i32, String, u8)> = self
            .tiles
            .iter()
            .filter(|(_, info)| info.mode == RenderMode::FlatTile && info.soil)
            .map(|(id, info)| (*id, info.family.clone(), info.dirs))
            .collect();
        for (tile, family, dirs) in soil_floors {
            for hue in SandHue::ALL {
                let Some(candidates) = sand_floor_alias(&family, hue) else { continue };
                let found = candidates.iter().find_map(|name| {
                    self.art.tree_sprite("", name, dirs).cloned().map(|s| (name.clone(), s))
                });
                let Some((name, sprite)) = found else {
                    self.misses.insert(candidates[0].clone());
                    continue;
                };
                let pattern = sprite.saturation() < PATTERN_SATURATION;
                let backdrop = if pattern { Some([255, 255, 255]) } else { sprite_mean(&sprite) };
                let key = atlas_key(&name, dirs, "");
                let Some(rect) = self.atlas.insert(key, &sprite, backdrop) else { continue };
                self.sand_flat_uv.insert((tile, hue), (rect, pattern));
            }
        }
    }

    /// A ramp's sloped surface for one eight-neighbour wall mask.
    ///
    /// The mask keys the cache, so all 256 neighbour sets resolve — and because
    /// DF names its ramp sprites by the same set, the picture on the slope
    /// always agrees with the shape underneath it.
    ///
    /// Falls back to a flat slab of ground when no neighbour is a wall and
    /// there is nothing for the tile to climb toward. `sand` is the specific
    /// tile's own hue, not the tiletype's: only a soil ramp with one draws
    /// from the sand sheets, everything else ignores it.
    pub fn ramp(&mut self, tile: i32, mask: u8, sand: Option<SandHue>) -> Option<Model> {
        let cache_key = (tile, mask, sand);
        if let Some(found) = self.ramp_model.get(&cache_key) {
            return found.clone();
        }

        let info = self.tiles.get(&tile)?;
        let hue = sand.filter(|_| info.soil);
        // The ground a flat or unsheeted sloped ramp wears: the hue's own
        // sand sheet where the tile has one, else the family's generic
        // ground, both packed by `pack_under`.
        let beneath = |lib: &Self| -> Option<(Rect, bool)> {
            hue.and_then(|h| lib.under_uv.get(sand_ground_family(h)).copied())
                .or_else(|| info.beneath.and_then(|g| lib.under_uv.get(g).copied()))
        };
        let built = if ramp::is_flat(mask) {
            beneath(self).map(|(rect, tint)| Model {
                mesh: Arc::new(build_flat_tile(rect, FLOOR_HEIGHT)),
                tint,
            })
        } else {
            // A slope with a sheet of its own wears DF's sprite for this wall
            // set, greyed to a pattern at pack time (or, for sand, kept in
            // its own full colour). The rest — soil, grass, ice, everything
            // whose sheet bakes in a shadow or does not exist — wear the flat
            // ground beside them, lit by the renderer.
            let uv = match hue.map(ramp::sand_family).or(info.ramp) {
                Some(family) => self.ramp_uv.get(&ramp::sprite_name(family, mask)).copied(),
                None => beneath(self),
            };
            uv.map(|(rect, tint)| Model {
                mesh: Arc::new(ramp::build_ramp(rect, mask, FLOOR_HEIGHT)),
                tint,
            })
        };
        self.ramp_model.insert(cache_key, built.clone());
        built
    }

    /// A ground slab for a tile that holds a free-standing object, or `None`
    /// when the tile needs no floor of its own.
    pub fn ground_beneath(&mut self, tile: i32) -> Option<Model> {
        let info = self.tiles.get(&tile)?;
        if info.mode != RenderMode::Billboard {
            return None;
        }
        let family = info.beneath?;
        if let Some(found) = self.under_model.get(family) {
            return found.clone();
        }

        let built = self.under_uv.get(family).copied().map(|(rect, tint)| Model {
            mesh: Arc::new(build_flat_tile(rect, FLOOR_HEIGHT)),
            tint,
        });
        self.under_model.insert(family, built.clone());
        built
    }

    /// Per-tiletype ground packing result: family, and whether it found a cell.
    pub fn ground_report(&self) -> Vec<(i32, String, bool)> {
        let mut rows: Vec<_> = self
            .tiles
            .iter()
            .filter(|(_, info)| info.mode == RenderMode::FlatTile)
            .map(|(id, info)| {
                let packed = self.flat_uv.keys().any(|(t, _)| t == id);
                (*id, info.family.clone(), packed)
            })
            .collect();
        rows.sort();
        rows
    }

    /// Packs the handful of ground families used beneath objects.
    ///
    /// Every sand hue's own single-cell ground sheet is packed alongside the
    /// generic `DIRT_FLOOR_5` whenever a soil tile needs ground beneath it —
    /// a billboard standing on sand, or a flat or unsheeted sand ramp — since
    /// which hue a specific tile is comes from its material, not its
    /// tiletype, and is not known until a voxel asks.
    fn pack_under(&mut self) {
        let mut wanted: Vec<&'static str> = {
            let mut v: Vec<_> = self.tiles.values().filter_map(|t| t.beneath).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        if self.tiles.values().any(|t| t.soil && t.beneath.is_some()) {
            wanted.extend(SandHue::ALL.iter().map(|hue| sand_ground_family(*hue)));
        }
        for family in wanted {
            let Some(sprite) = self.art.tree_sprite("", family, 0).cloned() else {
                self.misses.insert(family.to_string());
                continue;
            };
            let pattern = sprite.saturation() < PATTERN_SATURATION;
            let backdrop = if pattern { Some([255, 255, 255]) } else { sprite_mean(&sprite) };
            let key = atlas_key(family, 0, "");
            if let Some(rect) = self.atlas.insert(key, &sprite, backdrop) {
                self.under_uv.insert(family, (rect, pattern));
            }
        }
    }

    /// Packs DF's own ramp sheets, whole.
    ///
    /// Each family holds 47 sprites — one per distinct wall set — and the set
    /// is small and knowable, so it can be finished before the first frame like
    /// the ground is. The raws parser eats a trailing direction group off every
    /// part name, so a lookup has to be spelled the same way the index was
    /// filled.
    fn pack_ramps(&mut self) {
        let wanted: Vec<&'static str> = {
            let mut v: Vec<_> = self.tiles.values().filter_map(|t| t.ramp).collect();
            v.sort_unstable();
            v.dedup();
            v
        };

        for family in wanted {
            // A ramp follows the colour rule of the ground it runs into. DF's
            // grass ramps are already green; its stone ramps are a pattern
            // shaded in a blue nobody's granite is.
            let tinted = self.tiles.values().any(|t| {
                t.ramp == Some(family)
                    && t.beneath
                        .and_then(|g| self.under_uv.get(g))
                        .is_some_and(|(_, pattern)| *pattern)
            });
            self.pack_ramp_family(family, tinted);
        }

        // A sand ramp's own sheet is full colour, unlike `STONE_RAMP`: never
        // neutralised, and packed for every hue regardless of which (if any)
        // this map actually has, the same as `pack_walls` does for
        // `SOIL_WALL`. Which hue a specific tile is comes from its material,
        // not from `TileInfo::ramp` above.
        if self.tiles.values().any(|t| t.mode == RenderMode::Ramp && t.soil) {
            for hue in SandHue::ALL {
                self.pack_ramp_family(ramp::sand_family(hue), false);
            }
        }
    }

    /// Packs one ramp family's 47 sprites, neutralised to a pattern first
    /// when `tinted`.
    fn pack_ramp_family(&mut self, family: &str, tinted: bool) {
        for name in ramp::sprite_names(family) {
            let key = raws::parse_part(&name);
            let Some(sprite) = self.art.tree_sprite("", &key.family, key.dirs).cloned() else {
                self.misses.insert(name);
                continue;
            };
            let sprite = if tinted { ramp::neutralise(&sprite) } else { sprite };
            let pattern = tinted || sprite.saturation() < PATTERN_SATURATION;
            let backdrop = sprite_mean(&sprite);
            if let Some(rect) = self.atlas.insert(atlas_key(&name, 0, ""), &sprite, backdrop) {
                self.ramp_uv.insert(name, (rect, pattern));
            }
        }
    }

    /// One wall sprite, taking the first spelling the sheet answers to.
    fn wall_sprite(&mut self, family: &str, mask: u8) -> Option<Sprite> {
        wall::sprite_names(family, mask).into_iter().find_map(|name| {
            // The raws parser eats a trailing direction group, so a lookup has
            // to be spelled the way the index was filled.
            let key = raws::parse_part(&name);
            self.art.tree_sprite("", &key.family, key.dirs).cloned()
        })
    }

    /// Packs DF's wall sheets: one top face per neighbour set, one side strip.
    ///
    /// Sixteen cells per family, and the families are knowable from the
    /// tiletype list, so the whole set is finished before the first frame like
    /// the ground and the ramps are.
    fn pack_walls(&mut self) {
        let mut wanted: Vec<&'static str> = {
            let mut v: Vec<_> = self.walls.values().copied().collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        // A sand wall's own sheet is full colour, packed for every hue
        // regardless of which (if any) this map actually has, the same as
        // the ramps do — which hue a specific wall is comes from its
        // material, not from `walls` above.
        if wanted.contains(&"SOIL_WALL") {
            wanted.extend(SandHue::ALL.iter().map(|hue| wall::sand_family(*hue)));
        }

        for family in wanted {
            // The fully connected sprite is the only one that covers the tile,
            // so it is both the backdrop the others are drawn over and the
            // stock the side strip is cut from.
            let Some(base) = self.wall_sprite(family, wall::ALL) else {
                self.misses.insert(family.to_string());
                continue;
            };
            let pattern = base.saturation() < PATTERN_SATURATION;
            let backdrop = wall::backdrop(&base);
            // DF leaves the middle of an enclosed wall dark, which reads as a
            // hole once the tile has a top face to stand on.
            let solid = wall::fill_hole(&base);

            let strip = wall::side_strip(&base, backdrop);
            if let Some(rect) = self.atlas.insert(atlas_key(family, 0, "SIDE"), &strip, None) {
                self.wall_side.insert(family, (rect, pattern));
            }

            for mask in wall::masks() {
                let Some(sprite) = self.wall_sprite(family, mask) else {
                    self.misses.insert(wall::sprite_names(family, mask)[1].clone());
                    continue;
                };
                let top = wall::opaque(&wall::over(&sprite, &solid), backdrop);
                if let Some(rect) = self.atlas.insert(atlas_key(family, mask, "TOP"), &top, None) {
                    self.wall_top.insert((family, mask), rect);
                }
            }
        }
    }

    /// Packs one sprite per building look DF's raws actually carry.
    ///
    /// Every building type is asked for once per material sheet, and every
    /// workshop and furnace once per square of its footprint, so the set is
    /// knowable before the first block arrives and the atlas can be finished
    /// with the rest of it. A stem the raws do not carry costs one failed
    /// lookup and no cell.
    fn pack_buildings(&mut self) {
        use factory::{BUILDING_SHEETS, BuildingKind, WORKSHOP_SPAN, building_families};
        let mut wanted: Vec<(BuildingCell, Vec<String>)> = Vec::new();
        for building in 0..=factory::building_type::OFFERING_PLACE {
            let kind = factory::building_kind(building);
            if !kind.drawn() {
                continue;
            }
            if kind == BuildingKind::Workshop {
                for subtype in 0..WORKSHOP_SUBTYPES {
                    for y in 0..WORKSHOP_SPAN {
                        for x in 0..WORKSHOP_SPAN {
                            let cell = BuildingCell {
                                building,
                                subtype,
                                at: ((x as u8) << 4) | y as u8,
                            };
                            let families = building_families(building, subtype, 0, (x, y));
                            if !families.is_empty() {
                                wanted.push((cell, families));
                            }
                        }
                    }
                }
                continue;
            }
            for sheet in 0..BUILDING_SHEETS.len() {
                let cell = BuildingCell { building, subtype: sheet as i32, at: NOT_A_SHOP };
                let families = building_families(building, -1, sheet, (0, 0));
                if !families.is_empty() {
                    wanted.push((cell, families));
                }
            }
        }

        for (cell, families) in wanted {
            let found = families.iter().find_map(|family| {
                let key = raws::parse_part(family);
                self.art.tree_sprite("", &key.family, key.dirs).cloned().map(|s| (family.clone(), s))
            });
            let Some((family, sprite)) = found else { continue };
            let pattern = sprite.saturation() < PATTERN_SATURATION;
            if let Some(rect) = self.atlas.insert(atlas_key(&family, 0, ""), &sprite, None) {
                self.building_uv.insert(cell, (rect, pattern));
            }
        }
    }

    /// Where a building tile's lid samples the atlas, and whether that sheet is
    /// a near-grey pattern for the material colour to carry.
    ///
    /// `sheet` is the material sheet the decoder picked from DF's name for the
    /// material; a building whose own sheet is missing falls through the rest
    /// in order, so a wax door is a wooden one rather than nothing.
    pub fn building_cell(
        &self,
        building: i32,
        subtype: i32,
        sheet: u8,
        at: (i32, i32),
    ) -> Option<(Rect, bool)> {
        if factory::building_kind(building) == factory::BuildingKind::Workshop {
            let span = factory::WORKSHOP_SPAN;
            let at = ((at.0.clamp(0, span - 1) as u8) << 4) | at.1.clamp(0, span - 1) as u8;
            return self.building_uv.get(&BuildingCell { building, subtype, at }).copied();
        }
        let first = sheet as usize;
        std::iter::once(first)
            .chain((0..factory::BUILDING_SHEETS.len()).filter(move |&s| s != first))
            .find_map(|sheet| {
                self.building_uv
                    .get(&BuildingCell { building, subtype: sheet as i32, at: NOT_A_SHOP })
                    .copied()
            })
    }

    /// How many atlas cells the building sheets took.
    pub fn building_cells(&self) -> usize {
        self.building_uv.len()
    }

    /// Whether a tiletype draws from one of DF's wall sheets.
    pub fn is_wall(&self, tile: i32) -> bool {
        self.walls.contains_key(&tile)
    }

    /// Where a wall's faces sample the atlas, for a set of neighbouring walls.
    ///
    /// `sand` is the specific tile's own hue; only a `SOIL_WALL` tile with one
    /// is redirected to its sand sheet, falling back to plain soil if that
    /// hue somehow was not packed.
    pub fn wall_skin(&self, tile: i32, mask: u8, sand: Option<SandHue>) -> Option<WallSkin> {
        let base = *self.walls.get(&tile)?;
        let family = sand
            .filter(|_| base == "SOIL_WALL")
            .map(wall::sand_family)
            .filter(|f| self.wall_side.contains_key(f))
            .unwrap_or(base);
        let (side, tint) = *self.wall_side.get(family)?;
        let top = *self.wall_top.get(&(family, wall::variant(mask)))?;
        Some(WallSkin { top, side, tint })
    }

    /// Where a built floor's rim samples the atlas, and whether that sheet is a
    /// pattern the material colours.
    ///
    /// A roof is the lid of the wall under it, so its edge takes the wall
    /// sheet's own side strip rather than a blank slab side.
    pub fn floor_rim(&self, tile: i32) -> Option<(Rect, bool)> {
        let family = *self.floor_rim.get(&tile)?;
        self.wall_side.get(family).copied()
    }

    /// How many atlas cells the wall sheets took.
    pub fn wall_cells(&self) -> usize {
        self.wall_top.len() + self.wall_side.len()
    }

    /// How many atlas cells the ramp sheets took in total, and how many of
    /// those are the five sand hues' own sheets.
    pub fn ramp_cells(&self) -> (usize, usize) {
        let sand = self.ramp_uv.keys().filter(|name| name.starts_with("SAND_")).count();
        (self.ramp_uv.len(), sand)
    }

    /// How many atlas cells a soil floor's sand hues took.
    pub fn sand_floor_cells(&self) -> usize {
        self.sand_flat_uv.len()
    }

    /// Per-family wall packing result: family, whether it is tinted by the
    /// tile's material, and how many of its sixteen cells landed.
    pub fn wall_report(&self) -> Vec<(&'static str, bool, usize)> {
        let mut rows: Vec<_> = self
            .wall_side
            .iter()
            .map(|(family, (_, tint))| {
                let cells = 1 + self.wall_top.keys().filter(|(f, _)| f == family).count();
                (*family, *tint, cells)
            })
            .collect();
        rows.sort();
        rows
    }

    /// Where a ground family sits in the atlas, and whether its sheet is a
    /// near-grey pattern for the material to colour.
    ///
    /// Read-only, and the horizon's only way in: the coarse bands wear the
    /// same sprites the fine tiles do, so a slab beside a floor shows the
    /// texture continuing. Families packed for use beneath objects answer
    /// first; the rest are found through whichever tiletype packed them.
    pub fn ground_cell(&self, family: &str) -> Option<(Rect, bool)> {
        if let Some(found) = self.under_uv.get(family) {
            return Some(*found);
        }
        let mut best: Option<(i32, Rect, bool)> = None;
        for (id, info) in &self.tiles {
            if info.mode != RenderMode::FlatTile || !info.candidates.iter().any(|c| c == family) {
                continue;
            }
            let Some(rect) = self.flat_uv.get(&(*id, -1)) else { continue };
            if best.is_none_or(|(seen, _, _)| *id < seen) {
                best = Some((*id, *rect, self.flat_tint.get(&(*id, -1)).copied().unwrap_or(false)));
            }
        }
        best.map(|(_, rect, tint)| (rect, tint))
    }

    /// The strip a wall family's four sides share, for geometry that wants a
    /// cut bank rather than a floor.
    pub fn wall_side_cell(&self, family: &str) -> Option<(Rect, bool)> {
        self.wall_side.get(family).copied()
    }

    /// The packed ground texture, for the renderer to upload.
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    pub fn grid_size(&self) -> u32 {
        self.grid
    }

    pub fn model_count(&self) -> usize {
        self.cache.values().filter(|m| m.is_some()).count()
            + self.ramp_model.values().filter(|m| m.is_some()).count()
    }

    pub fn missing(&self) -> usize {
        self.misses.len()
    }

    /// Returns the geometry for a tile, or `None` when it has no sprite and the
    /// caller should fall back to a plain block.
    ///
    /// `sand` is the specific tile's own hue; only a soil floor with one is
    /// redirected to that hue's sheet, so a black sand beach and a clay floor
    /// of the same tiletype still resolve to different cells.
    pub fn model(&mut self, tile: i32, mat_index: i32, sand: Option<SandHue>, caps: Caps) -> Option<Model> {
        let info = self.tiles.get(&tile)?;
        let plant = if info.generic { -1 } else { mat_index };
        let hue = sand.filter(|_| info.soil);
        let key = ModelKey { tile, plant, caps, dirs: 0, sand: hue };
        if let Some(found) = self.cache.get(&key) {
            return found.clone();
        }

        let plant_id = if info.generic {
            ""
        } else {
            self.plants.get(mat_index.max(0) as usize).map(String::as_str).unwrap_or("")
        };

        let mode = info.mode;

        // Ground was packed at load; look up its atlas cell instead of
        // voxelising a picture.
        if mode == RenderMode::FlatTile {
            let uv = hue
                .and_then(|h| self.sand_flat_uv.get(&(tile, h)))
                .copied()
                .or_else(|| {
                    let rect = self
                        .flat_uv
                        .get(&(tile, mat_index))
                        .or_else(|| self.flat_uv.get(&(tile, -1)))
                        .copied()?;
                    let tint = self
                        .flat_tint
                        .get(&(tile, mat_index))
                        .or_else(|| self.flat_tint.get(&(tile, -1)))
                        .copied()
                        .unwrap_or(true);
                    Some((rect, tint))
                });
            let built = uv.map(|(rect, tint)| Model {
                mesh: Arc::new(build_flat_tile(rect, FLOOR_HEIGHT)),
                tint,
            });
            self.cache.insert(key, built.clone());
            return built;
        }

        let built = match self.art.tree_sprite(plant_id, &info.family, info.dirs) {
            Some(sprite) => {
                let grid = sprite.downsample(self.grid);
                (grid.solid_count() > 0).then(|| Model {
                    mesh: Arc::new(build_model(&grid, mode, caps)),
                    tint: false,
                })
            }
            None => {
                self.misses.insert(info.family.clone());
                None
            }
        };
        self.cache.insert(key, built.clone());
        built
    }

    /// Which neighbours a branch tile joins.
    pub fn branch_links(&self, tile: i32) -> u8 {
        self.tiles.get(&tile).map(|t| t.links).unwrap_or(0)
    }

    /// The colour of a species' wood, from the mean of its trunk sprite.
    pub fn bark_color(&mut self, mat_index: i32) -> [u8; 3] {
        if let Some(&found) = self.bark.get(&mat_index) {
            return found;
        }
        let plant_id = self.plant_id(mat_index);
        // Most trunk sprites are near-grey patterns that DF tints with the
        // tile's material, so a grey mean says nothing about the wood. Only a
        // sprite with real colour in it is worth reading.
        let color = ["TREE_TRUNK_PILLAR", "TREE_TRUNK", "TREE_BRANCH"]
            .into_iter()
            .find_map(|family| {
                let sprite = self.art.tree_sprite(&plant_id, family, 0)?;
                (sprite.saturation() >= PATTERN_SATURATION)
                    .then(|| sprite_mean(sprite))
                    .flatten()
            })
            .unwrap_or(DEFAULT_BARK);
        self.bark.insert(mat_index, color);
        color
    }

    /// How many species have a leaf cutout packed.
    pub fn leaf_cells(&self) -> usize {
        self.leaf_uv.len()
    }

    /// Where a species' leaf cutout sits in the atlas.
    pub fn leaf_uv(&self, mat_index: i32) -> Option<Rect> {
        self.leaf_uv
            .get(&mat_index)
            .or_else(|| self.leaf_uv.get(&-1))
            .copied()
    }

    /// Packs one leaf cutout per species up front.
    ///
    /// Alpha is kept rather than flattened, because the holes in a twig sprite
    /// are the point: a leaf face masked by it lets sky and sun through at texel
    /// scale, and the shadow it casts comes out finely dappled. The atlas is
    /// uploaded once after load, so this cannot wait until a tree is meshed.
    fn pack_leaves(&mut self) {
        let species: Vec<String> = self.art.index.plants.keys().cloned().collect();
        let mut subjects: Vec<(i32, String)> = species
            .iter()
            .map(|id| (plant_index_of(&self.plants, id), id.clone()))
            .collect();
        subjects.push((-1, String::new()));
        subjects.sort();

        for (index, plant_id) in subjects {
            let found = ["TREE_TWIGS", "TREE_BRANCH", "TREE_CAP_FLOOR_1"]
                .into_iter()
                .find_map(|family| {
                    let sprite = self.art.tree_sprite(&plant_id, family, 0)?;
                    // A sprite with almost nothing in it would mask the whole
                    // face away, so pass over it.
                    (sprite.coverage() > 0.12).then(|| (family, sprite.clone()))
                });
            let Some((family, sprite)) = found else { continue };
            let key = atlas_key(family, 0, &plant_id);
            if let Some(rect) = self.atlas.insert(key, &sprite, None) {
                self.leaf_uv.insert(index, rect);
            }
        }
    }

    /// The tones a species' leaves are drawn in, darkest first.
    ///
    /// DF's own twig sprite already holds the greens the species is drawn with,
    /// so taking its dominant tones keeps a pine dark and an apricot pale
    /// without a table.
    pub fn leaf_tones(&mut self, mat_index: i32, n: usize) -> Vec<[u8; 3]> {
        if let Some(found) = self.leaf_tones.get(&mat_index) {
            return found.clone();
        }
        let plant_id = self.plant_id(mat_index);
        let tones = ["TREE_TWIGS", "TREE_BRANCH"]
            .into_iter()
            .find_map(|family| {
                let found = self.art.tree_sprite(&plant_id, family, 0)?.tones(n);
                (!found.is_empty()).then_some(found)
            })
            .unwrap_or_else(|| spread(DEFAULT_FOLIAGE, n));
        self.leaf_tones.insert(mat_index, tones.clone());
        tones
    }

    /// The tones a species' wood is drawn in, darkest first.
    ///
    /// Most trunk sprites are near-grey patterns DF tints with the tile's
    /// material, and reading those gives grey wood, so a pattern falls back to
    /// shades of plain bark.
    pub fn bark_tones(&mut self, mat_index: i32, n: usize) -> Vec<[u8; 3]> {
        if let Some(found) = self.bark_tones.get(&mat_index) {
            return found.clone();
        }
        let plant_id = self.plant_id(mat_index);
        let tones = ["TREE_TRUNK_PILLAR", "TREE_TRUNK", "TREE_BRANCH"]
            .into_iter()
            .find_map(|family| {
                let sprite = self.art.tree_sprite(&plant_id, family, 0)?;
                if sprite.saturation() < PATTERN_SATURATION {
                    return None;
                }
                let found = sprite.tones(n);
                (!found.is_empty()).then_some(found)
            })
            .unwrap_or_else(|| spread(DEFAULT_BARK, n));
        self.bark_tones.insert(mat_index, tones.clone());
        tones
    }

    /// How a species grows, from the plant object raws. All zero when the
    /// species is not a tree or its raws were not found.
    pub fn growth(&self, mat_index: i32) -> raws::TreeGrowth {
        self.plants
            .get(mat_index.max(0) as usize)
            .and_then(|id| self.art.growth.get(id))
            .copied()
            .unwrap_or_default()
    }

    /// The plant's raw id, so a species can be recognised by name where its
    /// growth tokens do not say enough.
    pub fn plant_id(&self, mat_index: i32) -> String {
        self.plants.get(mat_index.max(0) as usize).cloned().unwrap_or_default()
    }

    /// What the factory makes of a tiletype, or `None` when DF has never
    /// mentioned it.
    pub fn plan(&self, tile: i32) -> Option<Plan> {
        self.tiles.get(&tile).map(|t| t.plan)
    }

    /// Whether a tiletype is built work: a wall, floor or building someone
    /// raised, which no plant may grow into.
    pub fn is_built(&self, tile: i32) -> bool {
        self.built.contains(&tile)
    }

    /// Whether a tiletype is part of a multi-tile tree, living or dead.
    pub fn of_tree(&self, tile: i32) -> bool {
        self.plan(tile).is_some_and(|p| p.of_tree())
    }

    /// Whether this tiletype is a tree's woody column.
    pub fn is_trunk(&self, tile: i32) -> bool {
        self.tiles.get(&tile).is_some_and(|t| t.trunk)
    }

    /// Which part of a tree's crown this tiletype is, if any.
    pub fn canopy_part(&self, tile: i32) -> Option<CanopyPart> {
        self.tiles.get(&tile).and_then(|t| t.canopy)
    }

    /// The colour of a species' foliage: the mean of its twig sprite, falling
    /// back to its branches and then to a generic leaf green.
    pub fn canopy_color(&mut self, mat_index: i32) -> [u8; 3] {
        if let Some(&found) = self.canopy_color.get(&mat_index) {
            return found;
        }
        let plant_id = self.plant_id(mat_index);
        let color = ["TREE_TWIGS", "TREE_BRANCH"]
            .into_iter()
            .find_map(|family| {
                self.art.tree_sprite(&plant_id, family, 0).and_then(sprite_mean)
            })
            .unwrap_or(DEFAULT_FOLIAGE);
        self.canopy_color.insert(mat_index, color);
        color
    }

    /// Whether this tiletype has a sprite treatment at all.
    pub fn handles(&self, tile: i32) -> bool {
        self.tiles.contains_key(&tile)
    }

    pub fn mode(&self, tile: i32) -> Option<RenderMode> {
        self.tiles.get(&tile).map(|t| t.mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_soil_floor_maps_onto_the_matching_sand_variant() {
        // SoilFloor2's numbered suffix (5B) survives onto whichever hue the
        // specific tile turns out to be, the way `ground_alias` maps it onto
        // `DIRT_FLOOR_5B` for every other soil.
        assert_eq!(
            sand_floor_alias("SOIL_FLOOR_2", SandHue::Yellow),
            Some(vec!["SAND_YELLOW_FLOOR_5B".to_string(), "SAND_YELLOW_FLOOR_5".to_string()])
        );
        assert_eq!(
            sand_floor_alias("SOIL_FLOOR_1", SandHue::Tan),
            Some(vec!["SAND_FLOOR_5".to_string(), "SAND_FLOOR_5".to_string()])
        );
        // Furrowed soil and anything not a numbered soil floor has no sand
        // counterpart to try.
        assert_eq!(sand_floor_alias("FURROWED_SOIL", SandHue::Red), None);
        assert_eq!(sand_floor_alias("STONE_FLOOR_1", SandHue::Red), None);
    }

    #[test]
    fn every_hue_names_a_distinct_floor_and_ground_family() {
        let floor: HashSet<_> = SandHue::ALL.iter().map(|h| sand_floor_family(*h)).collect();
        let ground: HashSet<_> = SandHue::ALL.iter().map(|h| sand_ground_family(*h)).collect();
        assert_eq!(floor.len(), 5);
        assert_eq!(ground.len(), 5);
    }

    #[test]
    fn the_five_hues_worth_of_sheets_fit_the_atlas_budget() {
        // Ramps: 5 hues x 47 sprites. Walls: 5 hues x (15 masks + 1 side).
        // Floors: a handful of soil floor tiletypes x 5 hues, small next to
        // the rest. None of it is worth the atlas's 1024 cells (32x32) on its
        // own, but the ramp and wall additions are the two large blocks, so a
        // budget check pins them rather than the whole atlas, which needs a
        // DF install to build.
        let ramp_cells = 5 * 47;
        let wall_cells = 5 * (15 + 1);
        assert_eq!(ramp_cells, 235);
        assert_eq!(wall_cells, 80);
        assert!(ramp_cells + wall_cells < 32 * 32);
    }

    #[test]
    fn a_built_floor_takes_dfs_block_sheet() {
        use TiletypeMaterial as M;
        use TiletypeShape as S;
        let f = construction_floor;
        // ConstructedFloor, the four Shoddy cuts and the sixteen track floors
        // are one tiletype class as far as the sheets are concerned.
        assert_eq!(f(M::Construction, S::Floor), Some("FLOOR_STONE_BLOCK"));
        // Everything else a construction can be is somebody else's: a wall and
        // a fortification wear the wall sheet, a ramp and a stair have their
        // own geometry.
        assert_eq!(f(M::Construction, S::Wall), None);
        assert_eq!(f(M::Construction, S::Fortification), None);
        assert_eq!(f(M::Construction, S::Ramp), None);
        assert_eq!(f(M::Construction, S::StairUpdown), None);
        // Natural ground keeps the numbered families it already resolves to.
        assert_eq!(f(M::Stone, S::Floor), None);
        assert_eq!(f(M::Soil, S::Floor), None);
        assert_eq!(f(M::GrassLight, S::Floor), None);
        assert_eq!(f(M::FrozenLiquid, S::Floor), None);
    }

    #[test]
    fn a_built_floors_rim_is_the_masonry_of_a_built_wall() {
        let wall = wall::family_for(TiletypeMaterial::Construction, TiletypeSpecial::Normal, "");
        assert_eq!(wall, Some("ROCK_BLOCKS_WALL"));
        // The rim is looked up through `wall_side`, so the wall sheet has to be
        // one `pack_walls` fills: it is, because the tiletype list always holds
        // constructed walls whether or not the map does.
        assert!(construction_floor(TiletypeMaterial::Construction, TiletypeShape::Floor).is_some());
    }

    #[test]
    fn the_floor_sheets_survive_the_raws_parser() {
        // `parse_part` eats a trailing direction group, so a sheet whose name
        // ends in N, S, W or E letters would be indexed under a shorter family
        // than it was asked for.
        for name in ["FLOOR_STONE_BLOCK", "WOOD_FLOOR", "METAL_FLOOR", "GLASS_GREEN_FLOOR"] {
            let key = raws::parse_part(name);
            assert_eq!(key.family, name);
            assert_eq!(key.dirs, 0);
        }
    }

    #[test]
    fn a_built_floor_does_not_go_through_the_ground_families() {
        // DFHack's names give families no sheet answers to, which is what left
        // a roof blank. The construction path has to come first.
        for name in ["ConstructedFloor", "ShoddyConstructedFloor1", "ConstructedFloorTrackNSEW"] {
            let family = alias(&raws::family_from_tiletype(name)).to_string();
            assert_eq!(ground_alias(&family), vec![family.clone()]);
            assert!(family.starts_with("CONSTRUCTED") || family.starts_with("SHODDY"));
        }
    }
}
