//! Parses the graphics raws that say where each tile's sprite lives.
//!
//! Two file kinds matter:
//!
//! - `tile_page_*.txt` declares sheets:
//!   `[TILE_PAGE:TREE_WILLOW] [FILE:images/tree_willow.png] [TILE_DIM:32:32]`
//! - `graphics_individual_trees.txt` indexes them per species:
//!   `[PLANT_GRAPHICS:WILLOW] [TREE_TILE:TREE_TRUNK_PILLAR:TREE_WILLOW:11:12]`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Direction bits, matching the order DFHack reports in `Tiletype::direction`.
pub const NORTH: u8 = 1;
pub const SOUTH: u8 = 2;
pub const WEST: u8 = 4;
pub const EAST: u8 = 8;

/// A tile part, split into its family and the neighbours it connects to.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TileKey {
    pub family: String,
    pub dirs: u8,
}

/// One sheet of sprites.
#[derive(Clone, Debug)]
pub struct TilePage {
    pub name: String,
    pub file: PathBuf,
    pub tile_w: u32,
    pub tile_h: u32,
}

/// Where a single sprite sits on its sheet.
#[derive(Clone, Copy, Debug)]
pub struct SpriteRef {
    pub page: usize,
    pub col: u32,
    pub row: u32,
}

/// Seasonal suffixes that name a variant rather than a direction.
const SEASONS: &[&str] = &["SPRING", "SUMMER", "AUTUMN", "WINTER"];

/// The build stage a workshop tile is drawn at when the shop is finished.
/// DF counts down from it as the shop is put up.
const FINISHED: u32 = 3;

fn is_direction_group(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| matches!(c, 'N' | 'S' | 'W' | 'E'))
}

/// The generic sheets spell out absent connections in lowercase, so
/// `TREE_TRUNK_S_nwe` is the same tile as the per-species `TREE_TRUNK_S`.
fn is_absent_group(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| matches!(c, 'n' | 's' | 'w' | 'e'))
}

fn direction_bits(token: &str) -> u8 {
    token.chars().fold(0, |acc, c| {
        acc | match c {
            'N' => NORTH,
            'S' => SOUTH,
            'W' => WEST,
            'E' => EAST,
            _ => 0,
        }
    })
}

/// Splits a raw part name such as `TREE_TRUNK_THICK_NE` into family and dirs.
///
/// Season suffixes come off first, then at most one trailing direction group,
/// so `TREE_TRUNK_THICK_INTERIOR` keeps `INTERIOR` as part of its family.
pub fn parse_part(part: &str) -> TileKey {
    let mut tokens: Vec<&str> = part.split('_').collect();
    while tokens
        .last()
        .is_some_and(|t| SEASONS.contains(t) || is_absent_group(t))
    {
        tokens.pop();
    }
    let mut dirs = 0;
    if tokens.len() > 1 && tokens.last().is_some_and(|t| is_direction_group(t)) {
        dirs = direction_bits(tokens.pop().unwrap());
    }
    TileKey { family: tokens.join("_"), dirs }
}

/// Turns a DFHack tiletype name such as `TreeCapWallThickSE` into a family.
///
/// Trailing single-letter direction tokens are dropped: DFHack reports the
/// connection set separately in `Tiletype::direction`, and its letter order does
/// not always match the raws'.
pub fn family_from_tiletype(name: &str) -> String {
    let mut tokens: Vec<String> = Vec::new();
    let mut previous_digit = false;
    for c in name.chars() {
        // The raws separate trailing variant numbers, so `TreeCapFloor1` has to
        // become `TREE_CAP_FLOOR_1` rather than `TREE_CAP_FLOOR1`.
        let digit = c.is_ascii_digit();
        if tokens.is_empty() || c.is_uppercase() || (digit && !previous_digit) {
            tokens.push(String::new());
        }
        tokens.last_mut().unwrap().push(c.to_ascii_uppercase());
        previous_digit = digit;
    }
    while tokens.len() > 1
        && tokens.last().is_some_and(|t| t.len() == 1 && is_direction_group(t))
    {
        tokens.pop();
    }
    tokens.join("_")
}

/// Reads DFHack's eight-character direction string, e.g. `"N-S-W-E-"`.
pub fn direction_mask(direction: &str) -> u8 {
    let mut bits = 0;
    for c in direction.chars() {
        bits |= match c {
            'N' => NORTH,
            'S' => SOUTH,
            'W' => WEST,
            'E' => EAST,
            _ => 0,
        };
    }
    bits
}

/// The variant of `family` whose connection set is closest to `dirs`.
///
/// Ties break on the lowest direction mask, so a given tile always resolves to
/// the same sprite across runs.
fn nearest(
    table: &HashMap<TileKey, SpriteRef>,
    family: &str,
    dirs: u8,
) -> Option<SpriteRef> {
    table
        .iter()
        .filter(|(k, _)| k.family == family)
        .min_by_key(|(k, _)| ((k.dirs ^ dirs).count_ones(), k.dirs))
        .map(|(_, v)| *v)
}

/// Every `[TAG:a:b:c]` token on a line, as its colon-separated fields.
fn tags(line: &str) -> impl Iterator<Item = Vec<&str>> {
    line.split('[').skip(1).filter_map(|chunk| {
        let body = chunk.split(']').next()?;
        Some(body.split(':').collect())
    })
}

/// The parsed contents of a graphics directory.
#[derive(Default)]
pub struct GraphicsIndex {
    pub pages: Vec<TilePage>,
    page_by_name: HashMap<String, usize>,
    /// plant raw id -> tile key -> sprite location.
    pub plants: HashMap<String, HashMap<TileKey, SpriteRef>>,
    /// Species-independent tiles, from `TILE_GRAPHICS` entries. Only 20 of the
    /// 72 tree species ship their own sheet; the rest land here.
    pub generic: HashMap<TileKey, SpriteRef>,
}

impl GraphicsIndex {
    pub fn page(&self, index: usize) -> &TilePage {
        &self.pages[index]
    }

    /// Looks up a tile, narrowing from exact match to the species' undirected
    /// variant to the generic sheet.
    pub fn sprite(&self, plant_id: &str, family: &str, dirs: u8) -> Option<SpriteRef> {
        let exact = TileKey { family: family.to_string(), dirs };
        let plain = TileKey { family: family.to_string(), dirs: 0 };

        if let Some(tiles) = self.plants.get(plant_id) {
            if let Some(found) = tiles.get(&exact).or_else(|| tiles.get(&plain)) {
                return Some(*found);
            }
        }
        if let Some(found) = self.generic.get(&exact).or_else(|| self.generic.get(&plain)) {
            return Some(*found);
        }

        // Some families, such as `ROOT_WALL`, only ship directional variants.
        // Take the closest one rather than dropping the tile to a plain block.
        nearest(&self.generic, family, dirs)
            .or_else(|| self.plants.get(plant_id).and_then(|t| nearest(t, family, dirs)))
    }

    /// A species-independent tile such as `SHRUB` or `SAPLING`.
    pub fn generic_sprite(&self, family: &str) -> Option<SpriteRef> {
        self.generic
            .get(&TileKey { family: family.to_string(), dirs: 0 })
            .copied()
    }

    pub fn has_plant(&self, plant_id: &str) -> bool {
        self.plants.contains_key(plant_id)
    }

    fn add_page(&mut self, page: TilePage) {
        self.page_by_name.insert(page.name.clone(), self.pages.len());
        self.pages.push(page);
    }

    /// Scans one `graphics/` directory: sheets first, then the tile indices.
    pub fn load_dir(&mut self, dir: &Path) -> anyhow::Result<()> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "txt"))
            .collect();
        // Tile pages must land before the entries that reference them.
        files.sort_by_key(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            (!name.starts_with("tile_page"), name)
        });

        for file in files {
            let text = std::fs::read_to_string(&file)?;
            self.read_file(dir, &text);
        }
        Ok(())
    }

    fn read_file(&mut self, dir: &Path, text: &str) {
        let mut page: Option<TilePage> = None;
        let mut plant: Option<String> = None;

        for line in text.lines() {
            for fields in tags(line) {
                match fields.as_slice() {
                    ["TILE_PAGE", name] => {
                        if let Some(done) = page.take() {
                            self.add_page(done);
                        }
                        page = Some(TilePage {
                            name: (*name).to_string(),
                            file: PathBuf::new(),
                            tile_w: 32,
                            tile_h: 32,
                        });
                    }
                    ["FILE", rel] => {
                        if let Some(p) = page.as_mut() {
                            p.file = dir.join(rel);
                        }
                    }
                    ["TILE_DIM", w, h] => {
                        if let Some(p) = page.as_mut() {
                            p.tile_w = w.parse().unwrap_or(32);
                            p.tile_h = h.parse().unwrap_or(32);
                        }
                    }
                    ["PLANT_GRAPHICS", id] => {
                        if let Some(done) = page.take() {
                            self.add_page(done);
                        }
                        plant = Some((*id).to_string());
                    }
                    ["TREE_TILE", part, page_name, col, row] => {
                        let (Some(plant_id), Some(&page_index)) =
                            (plant.as_ref(), self.page_by_name.get(*page_name))
                        else {
                            continue;
                        };
                        let (Ok(col), Ok(row)) = (col.parse(), row.parse()) else { continue };
                        self.plants
                            .entry(plant_id.clone())
                            .or_default()
                            .insert(parse_part(part), SpriteRef { page: page_index, col, row });
                    }
                    ["TILE_GRAPHICS", page_name, col, row, part] => {
                        let Some(&page_index) = self.page_by_name.get(*page_name) else {
                            continue;
                        };
                        let (Ok(col), Ok(row)) = (col.parse(), row.parse()) else { continue };
                        self.generic
                            .insert(parse_part(part), SpriteRef { page: page_index, col, row });
                    }
                    // A workshop is one sprite per tile of its footprint, and
                    // DF spells the tile out after the name: a build stage,
                    // then the offset inside the shop. Only the finished stage
                    // is kept, and each tile lands under its own family so the
                    // mesher can ask for one square of a three-square shop.
                    ["TILE_GRAPHICS", page_name, col, row, part, stage, sub_x, sub_y] => {
                        let Some(&page_index) = self.page_by_name.get(*page_name) else {
                            continue;
                        };
                        let (Ok(col), Ok(row)) = (col.parse(), row.parse()) else { continue };
                        let (Ok(stage), Ok(sub_x), Ok(sub_y)) =
                            (stage.parse::<u32>(), sub_x.parse::<u32>(), sub_y.parse::<u32>())
                        else {
                            continue;
                        };
                        if stage != FINISHED {
                            continue;
                        }
                        let key = parse_part(&format!("{part}_{sub_x}_{sub_y}"));
                        self.generic.insert(key, SpriteRef { page: page_index, col, row });
                    }
                    _ => {}
                }
            }
        }
        if let Some(done) = page.take() {
            self.add_page(done);
        }
    }
}

/// A species' tree growth tokens, from the plant object raws.
///
/// DF grows every tree from these, and reading them is how a renderer can put
/// limbs where the species would put them instead of where a generic rule
/// would. Radii are DF's own units; density is a percentage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeGrowth {
    pub max_trunk_height: u32,
    pub trunk_branching: u32,
    pub branch_density: u32,
    pub branch_radius: u32,
    pub heavy_branch_density: u32,
    pub heavy_branch_radius: u32,
    pub max_trunk_diameter: u32,
}

/// Reads the growth tokens for every plant under a `data/vanilla/*/objects`
/// tree, keyed by plant raw id.
pub fn load_growth(vanilla: &Path) -> HashMap<String, TreeGrowth> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(vanilla) else { return out };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path().join("objects")))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let Ok(files) = std::fs::read_dir(&dir) else { continue };
        let mut paths: Vec<PathBuf> = files
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("plant_") && n.ends_with(".txt"))
            })
            .collect();
        paths.sort();
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            read_growth(&text, &mut out);
        }
    }
    out
}

fn read_growth(text: &str, out: &mut HashMap<String, TreeGrowth>) {
    let mut plant: Option<String> = None;
    for line in text.lines() {
        for fields in tags(line) {
            let value = |v: &str| v.parse::<u32>().unwrap_or(0);
            match fields.as_slice() {
                ["PLANT", id] => plant = Some((*id).to_string()),
                [name, v] => {
                    let Some(id) = plant.as_ref() else { continue };
                    let slot = out.entry(id.clone()).or_default();
                    match *name {
                        "MAX_TRUNK_HEIGHT" => slot.max_trunk_height = value(v),
                        "TRUNK_BRANCHING" => slot.trunk_branching = value(v),
                        "BRANCH_DENSITY" => slot.branch_density = value(v),
                        "BRANCH_RADIUS" => slot.branch_radius = value(v),
                        "HEAVY_BRANCH_DENSITY" => slot.heavy_branch_density = value(v),
                        "HEAVY_BRANCH_RADIUS" => slot.heavy_branch_radius = value(v),
                        "MAX_TRUNK_DIAMETER" => slot.max_trunk_diameter = value(v),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod growth_tests {
    use super::*;

    #[test]
    fn reads_the_tokens_that_shape_a_tree() {
        let mut out = HashMap::new();
        read_growth(
            "[PLANT:WILLOW]\n\t[BRANCH_DENSITY:60]\n\t[BRANCH_RADIUS:3]\n[PLANT:PINE]\n\t[BRANCH_RADIUS:2]\n",
            &mut out,
        );
        assert_eq!(out["WILLOW"].branch_density, 60);
        assert_eq!(out["WILLOW"].branch_radius, 3);
        assert_eq!(out["PINE"].branch_radius, 2);
        assert_eq!(out["PINE"].branch_density, 0);
    }
}
