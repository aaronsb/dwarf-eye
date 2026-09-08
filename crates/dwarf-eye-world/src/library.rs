//! Resolves a tile to a cached voxel model built from DF's own sprite.

use crate::mesh::MeshData;
use crate::model::{Caps, RenderMode, build_model};
use anyhow::Result;
use dfhack_remote::rfr::{PlantRawList, TiletypeList, TiletypeShape};
use dwarf_eye_art::{Art, find_install, raws};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Sub-voxels per tile edge. Twelve keeps a trunk visibly round while leaving
/// the triangle count survivable; `DWARF_EYE_GRID` overrides it.
pub const DEFAULT_GRID: u32 = 12;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ModelKey {
    tile: i32,
    plant: i32,
    caps: Caps,
}

struct TileInfo {
    family: String,
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
    match shape {
        S::Wall if name.starts_with("Tree") => Some(RenderMode::Extrude),
        S::Branch | S::Twig => Some(RenderMode::ThinExtrude),
        S::Sapling | S::Shrub | S::Boulder => Some(RenderMode::Billboard),
        _ => None,
    }
}

/// Sprite-derived tile models, built once per tiletype, species and cap pair.
pub struct TileLibrary {
    art: Art,
    tiles: HashMap<i32, TileInfo>,
    plants: Vec<String>,
    grid: u32,
    cache: HashMap<ModelKey, Option<Arc<MeshData>>>,
    misses: HashSet<String>,
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
                raws::family_from_tiletype(name)
            };
            tiles.insert(
                t.id,
                TileInfo { family, dirs: raws::direction_mask(t.direction()), mode, generic },
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

        Ok(Self { art, tiles, plants: ids, grid, cache: HashMap::new(), misses: HashSet::new() })
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
    pub fn model(&mut self, tile: i32, mat_index: i32, caps: Caps) -> Option<Arc<MeshData>> {
        let info = self.tiles.get(&tile)?;
        let plant = if info.generic { -1 } else { mat_index };
        let key = ModelKey { tile, plant, caps };
        if let Some(found) = self.cache.get(&key) {
            return found.clone();
        }

        let plant_id = if info.generic {
            ""
        } else {
            self.plants.get(mat_index.max(0) as usize).map(String::as_str).unwrap_or("")
        };

        let built = match self.art.tree_sprite(plant_id, &info.family, info.dirs) {
            Some(sprite) => {
                let grid = sprite.downsample(self.grid);
                (grid.solid_count() > 0)
                    .then(|| Arc::new(build_model(&grid, info.mode, caps)))
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
