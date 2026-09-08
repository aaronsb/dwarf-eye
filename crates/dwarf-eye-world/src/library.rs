//! Resolves a tile to a cached voxel model built from DF's own sprite.

use crate::mesh::MeshData;
use crate::model::{Caps, RenderMode, build_flat_tile, build_model, build_ramp};
use anyhow::Result;
use dfhack_remote::rfr::{PlantRawList, TiletypeList, TiletypeMaterial, TiletypeShape};
use dwarf_eye_art::atlas::{Atlas, Rect};
use dwarf_eye_art::{Art, find_install, raws};
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ModelKey {
    tile: i32,
    plant: i32,
    caps: Caps,
    /// Orientation, for geometry the mesher works out from neighbours.
    dirs: u8,
}

struct TileInfo {
    family: String,
    /// Ground families to try in order; empty for everything else.
    candidates: Vec<String>,
    /// Ground to draw beneath a free-standing object.
    beneath: Option<&'static str>,
    dirs: u8,
    mode: RenderMode,
    /// Species-independent tiles look straight at the generic sheet.
    generic: bool,
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
        S::Wall | S::Ramp if tree => Some(RenderMode::Extrude),
        S::Branch | S::Twig => Some(RenderMode::ThinExtrude),
        S::Sapling | S::Shrub | S::Boulder => Some(RenderMode::Billboard),
        // Ground cover, including the walkable surface of a treetop.
        S::Floor | S::Pebbles => Some(RenderMode::FlatTile),
        S::Ramp => Some(RenderMode::Ramp),
        _ => None,
    }
}

/// Below this mean saturation a sprite is a pattern to tint, not a colour.
const PATTERN_SATURATION: f32 = 0.22;

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
        M::FrozenLiquid => "FROZEN_FLOOR_5",
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
        for t in &tiletypes.tiletype_list {
            let name = t.name();
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
            let candidates = if mode == RenderMode::FlatTile {
                ground_alias(&family)
            } else {
                Vec::new()
            };
            // Billboards need ground under them; ramps need it on their slope.
            let beneath = matches!(mode, RenderMode::Billboard | RenderMode::Ramp)
                .then(|| ground_under(t.material()));
            tiles.insert(
                t.id,
                TileInfo {
                    family,
                    candidates,
                    beneath,
                    dirs: raws::direction_mask(t.direction()),
                    mode,
                    generic,
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
        };
        library.pack_ground();
        library.pack_under();
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
    }

    /// A wedge for a ramp tile, rising toward `high`.
    ///
    /// Falls back to a flat slab when no neighbouring wall says which way it
    /// should climb.
    pub fn ramp(&mut self, tile: i32, high: u8) -> Option<Model> {
        let family = self.tiles.get(&tile)?.beneath?;
        let key = ModelKey { tile, plant: -1, caps: Caps::BOTH, dirs: high };
        if let Some(found) = self.cache.get(&key) {
            return found.clone();
        }

        let built = self.under_uv.get(family).copied().map(|(rect, tint)| Model {
            mesh: Arc::new(if high == 0 {
                build_flat_tile(rect, FLOOR_HEIGHT)
            } else {
                build_ramp(rect, high, FLOOR_HEIGHT)
            }),
            tint,
        });
        self.cache.insert(key, built.clone());
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
    fn pack_under(&mut self) {
        let wanted: Vec<&'static str> = {
            let mut v: Vec<_> = self.tiles.values().filter_map(|t| t.beneath).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
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

    /// The packed ground texture, for the renderer to upload.
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    pub fn grid_size(&self) -> u32 {
        self.grid
    }

    pub fn model_count(&self) -> usize {
        self.cache.values().filter(|m| m.is_some()).count()
    }

    pub fn missing(&self) -> usize {
        self.misses.len()
    }

    /// Returns the geometry for a tile, or `None` when it has no sprite and the
    /// caller should fall back to a plain block.
    pub fn model(&mut self, tile: i32, mat_index: i32, caps: Caps) -> Option<Model> {
        let info = self.tiles.get(&tile)?;
        let plant = if info.generic { -1 } else { mat_index };
        let key = ModelKey { tile, plant, caps, dirs: 0 };
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
            let uv = self
                .flat_uv
                .get(&(tile, mat_index))
                .or_else(|| self.flat_uv.get(&(tile, -1)))
                .copied();
            let built = uv.map(|rect| Model {
                mesh: Arc::new(build_flat_tile(rect, FLOOR_HEIGHT)),
                tint: self
                    .flat_tint
                    .get(&(tile, mat_index))
                    .or_else(|| self.flat_tint.get(&(tile, -1)))
                    .copied()
                    .unwrap_or(true),
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

    /// Whether this tiletype has a sprite treatment at all.
    pub fn handles(&self, tile: i32) -> bool {
        self.tiles.contains_key(&tile)
    }

    pub fn mode(&self, tile: i32) -> Option<RenderMode> {
        self.tiles.get(&tile).map(|t| t.mode)
    }
}
