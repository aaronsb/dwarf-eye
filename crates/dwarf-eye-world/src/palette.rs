//! Lookup tables that turn DF tile ids and material pairs into voxel appearance.

use dfhack_remote::rfr::{
    MatPair, MaterialList, Tiletype, TiletypeList, TiletypeMaterial, TiletypeShape,
};
use std::collections::HashMap;

pub type Rgb = [u8; 3];

/// The geometry a tile contributes to the mesh.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Solid {
    #[default]
    Empty,
    /// Fills the whole cell.
    Cube,
    /// A thin slab at the bottom of the cell.
    Floor,
    /// A wedge rising toward the adjacent wall.
    Ramp,
    /// Stepped geometry connecting two z-levels.
    Stair,
    /// A cube with slits cut through it.
    Fortification,
    /// Sparse foliage that reads better as a cross-billboard than a cube.
    Foliage,
}

impl Solid {
    pub fn is_empty(self) -> bool {
        self == Solid::Empty
    }

    /// Whether a neighbouring face can be culled against this cell.
    pub fn occludes(self) -> bool {
        matches!(self, Solid::Cube)
    }
}

/// Maps a DF tile shape onto voxel geometry.
pub fn solid_for_shape(shape: TiletypeShape) -> Solid {
    use TiletypeShape as S;
    match shape {
        S::Empty | S::NoShape | S::EndlessPit | S::RampTop => Solid::Empty,
        S::Wall | S::Boulder | S::TreeShape | S::TrunkBranch => Solid::Cube,
        S::Floor | S::Pebbles | S::BrookTop => Solid::Floor,
        S::Ramp => Solid::Ramp,
        S::StairUp | S::StairDown | S::StairUpdown => Solid::Stair,
        S::Fortification => Solid::Fortification,
        S::BrookBed => Solid::Floor,
        S::Sapling | S::Shrub | S::Branch | S::Twig => Solid::Foliage,
    }
}

/// How a tile of this material class looks as terrain.
///
/// DF's `state_color` describes a material as a *substance* — loam is grey,
/// grass plants are brown — which is right for an item and wrong for ground.
fn terrain_color(material: TiletypeMaterial) -> Rgb {
    use TiletypeMaterial as M;
    match material {
        M::Air | M::NoMaterial => [0, 0, 0],
        M::Soil => [134, 96, 67],
        M::Stone => [128, 128, 128],
        M::Feature => [150, 120, 160],
        M::LavaStone => [70, 55, 55],
        M::Mineral => [140, 130, 150],
        M::FrozenLiquid => [190, 225, 240],
        M::Construction => [170, 165, 155],
        M::GrassLight => [126, 176, 78],
        M::GrassDark => [92, 148, 62],
        M::GrassDry => [178, 162, 96],
        M::GrassDead => [150, 130, 92],
        M::Plant => [96, 152, 74],
        M::Hfs => [180, 60, 200],
        M::Campfire | M::Fire => [255, 140, 40],
        M::Ashes => [90, 88, 85],
        M::Magma => [255, 90, 20],
        M::Driftwood => [150, 130, 105],
        M::Pool | M::Brook | M::River => [60, 110, 190],
        M::Root => [95, 70, 45],
        M::TreeMaterial => [110, 80, 50],
        M::Mushroom => [190, 175, 140],
        M::UnderworldGate => [120, 40, 140],
    }
}

/// Whether the tiletype's material class describes the look better than the
/// concrete material does.
///
/// Stone, ore and constructions keep their own colour, so granite still reads
/// differently from marble; ground cover and liquids do not.
fn class_wins(material: TiletypeMaterial) -> bool {
    use TiletypeMaterial as M;
    matches!(
        material,
        M::Air
            | M::NoMaterial
            | M::Soil
            | M::GrassLight
            | M::GrassDark
            | M::GrassDry
            | M::GrassDead
            | M::Plant
            | M::Root
            | M::TreeMaterial
            | M::Driftwood
            | M::Mushroom
            | M::Magma
            | M::Fire
            | M::Campfire
            | M::Ashes
            | M::Pool
            | M::Brook
            | M::River
    )
}

/// Leaves, wherever they hang.
const FOLIAGE: Rgb = [82, 138, 58];

/// A hand's depth of water, and water deep enough that the bottom is gone.
///
/// DF paints a murky pool teal and shades its shallows lighter, which is the
/// look; the tiletype's own `Pool` blue is the colour of the tile, not of the
/// liquid standing in it.
pub const WATER_SHALLOW: Rgb = [122, 186, 184];
pub const WATER_DEEP: Rgb = [18, 68, 92];

/// How deep water has to stand before it reads as fully deep, in tiles.
const WATER_RANGE: f32 = 2.5;

/// Colour and opacity of a water surface over `depth` tiles of water.
///
/// A shore is nearly clear so the ground under it shows through; a metre or
/// two out the bottom has gone. Depth is the surface height plus whatever is
/// stacked below, so it climbs both across a pool and down a shaft.
pub fn water_color(depth: f32) -> (Rgb, f32) {
    let t = (depth / WATER_RANGE).clamp(0.0, 1.0).powf(0.7);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    let rgb = [
        mix(WATER_SHALLOW[0], WATER_DEEP[0]),
        mix(WATER_SHALLOW[1], WATER_DEEP[1]),
        mix(WATER_SHALLOW[2], WATER_DEEP[2]),
    ];
    (rgb, 0.22 + 0.62 * t)
}

/// A film of magma over its own crust, and magma deep enough to be all glow.
///
/// The thin edge of a flow has cooled and lets the rock under it through; a sea
/// is opaque and near white-hot. The material's emissive is what makes either
/// of them light (`main.rs:MagmaMaterial`); these are the colours it multiplies.
pub const MAGMA_THIN: Rgb = [168, 46, 16];
pub const MAGMA_DEEP: Rgb = [255, 148, 42];

/// How deep magma has to stand before it reads as a sea, in tiles.
const MAGMA_RANGE: f32 = 2.0;

/// Colour and opacity of a magma surface over `depth` tiles of magma.
///
/// Unlike water, magma darkens *outward*: the shallows are cooling crust rather
/// than clear liquid, so they are dimmer and thinner and the deep is bright and
/// nearly opaque.
pub fn magma_color(depth: f32) -> (Rgb, f32) {
    let t = (depth / MAGMA_RANGE).clamp(0.0, 1.0).powf(0.6);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    let rgb = [
        mix(MAGMA_THIN[0], MAGMA_DEEP[0]),
        mix(MAGMA_THIN[1], MAGMA_DEEP[1]),
        mix(MAGMA_THIN[2], MAGMA_DEEP[2]),
    ];
    (rgb, 0.62 + 0.33 * t)
}

/// The sand hue a soil material draws from, for the five sand sheets DF ships
/// (`SAND_FLOOR`, `SAND_YELLOW_FLOOR`, `SAND_WHITE_FLOOR`, `SAND_BLACK_FLOOR`,
/// `SAND_RED_FLOOR`, and one ramp and wall sheet per hue). Every other soil
/// material — clay, loam, silt, peat — has no sheet of its own and is `None`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SandHue {
    Tan,
    Yellow,
    White,
    Black,
    Red,
}

impl SandHue {
    /// Every hue DF ships a sheet for, tan first, matching the unlabelled
    /// sheet's own place as the default.
    pub const ALL: [SandHue; 5] =
        [SandHue::Tan, SandHue::Yellow, SandHue::White, SandHue::Black, SandHue::Red];
}

/// The same classification from a material's display name, for the rare
/// reply that carries a name but no id. Matched whole, case-insensitively,
/// against DF's own `STATE_NAME_ADJ` text — "sand", "yellow sand", "white
/// sand", "black sand", "red sand" — so "sandy loam" does not qualify.
fn sand_hue_from_name(name: &str) -> Option<SandHue> {
    match name.to_ascii_lowercase().as_str() {
        "sand" | "tan sand" => Some(SandHue::Tan),
        "yellow sand" => Some(SandHue::Yellow),
        "white sand" => Some(SandHue::White),
        "black sand" => Some(SandHue::Black),
        "red sand" => Some(SandHue::Red),
        _ => None,
    }
}

/// Classifies a material's raw id into the sand hue DF ships a sheet for, or
/// `None` for every other soil material.
///
/// Matched by exact id, not a substring: `SANDY_LOAM` and `SANDY_CLAY` both
/// contain "SAND" and are ordinary soil, not sand; only the five materials
/// `[SOIL_SAND]` tags in `inorganic_stone_soil.txt` — `SAND` (also spelled
/// `SAND_TAN`), `SAND_YELLOW`, `SAND_WHITE`, `SAND_BLACK`, `SAND_RED` — are.
/// DFHack's material id is sometimes qualified with its object type
/// (`INORGANIC:SAND_BLACK`); only the token after the last colon is matched.
pub fn sand_hue_from_id(id: &str) -> Option<SandHue> {
    match id.rsplit(':').next().unwrap_or(id) {
        "SAND" | "SAND_TAN" => Some(SandHue::Tan),
        "SAND_YELLOW" => Some(SandHue::Yellow),
        "SAND_WHITE" => Some(SandHue::White),
        "SAND_BLACK" => Some(SandHue::Black),
        "SAND_RED" => Some(SandHue::Red),
        _ => None,
    }
}

/// Everything needed to give a tile a shape and a colour.
pub struct Palette {
    tiletypes: HashMap<i32, Tiletype>,
    colors: HashMap<(i32, i32), Rgb>,
    names: HashMap<(i32, i32), String>,
    /// Material pairs classified as one of DF's five sand hues, resolved once
    /// here from the raw id rather than re-parsed per tile.
    sand: HashMap<(i32, i32), SandHue>,
}

impl Palette {
    pub fn new(tiletypes: TiletypeList, materials: MaterialList) -> Self {
        let tiletypes = tiletypes
            .tiletype_list
            .into_iter()
            .map(|t| (t.id, t))
            .collect();

        let mut colors = HashMap::new();
        let mut names = HashMap::new();
        let mut sand = HashMap::new();
        for m in materials.material_list {
            let key = (m.mat_pair.mat_type, m.mat_pair.mat_index);
            if let Some(c) = m.state_color {
                colors.insert(key, [clamp_channel(c.red), clamp_channel(c.green), clamp_channel(c.blue)]);
            }
            if let Some(n) = &m.name {
                names.insert(key, n.clone());
            }
            let hue = m
                .id
                .as_deref()
                .and_then(sand_hue_from_id)
                .or_else(|| m.name.as_deref().and_then(sand_hue_from_name));
            if let Some(hue) = hue {
                sand.insert(key, hue);
            }
        }

        Self { tiletypes, colors, names, sand }
    }

    /// The sand hue this material pair draws from, if it is one of DF's five
    /// named sands. `None` covers both "not soil" and "soil but not sand" —
    /// clay, loam, silt and peat included.
    pub fn sand_hue(&self, pair: &MatPair) -> Option<SandHue> {
        self.sand.get(&(pair.mat_type, pair.mat_index)).copied()
    }

    pub fn tiletype(&self, id: i32) -> Option<&Tiletype> {
        self.tiletypes.get(&id)
    }

    pub fn shape(&self, id: i32) -> TiletypeShape {
        self.tiletypes
            .get(&id)
            .and_then(|t| t.shape)
            .and_then(|s| TiletypeShape::try_from(s).ok())
            .unwrap_or(TiletypeShape::NoShape)
    }

    pub fn tile_material(&self, id: i32) -> TiletypeMaterial {
        self.tiletypes
            .get(&id)
            .and_then(|t| t.material)
            .and_then(|m| TiletypeMaterial::try_from(m).ok())
            .unwrap_or(TiletypeMaterial::NoMaterial)
    }

    /// The material's own colour, if DF reports one.
    pub fn material_color(&self, pair: &MatPair) -> Option<Rgb> {
        self.colors.get(&(pair.mat_type, pair.mat_index)).copied()
    }

    pub fn material_name(&self, pair: &MatPair) -> Option<&str> {
        self.names.get(&(pair.mat_type, pair.mat_index)).map(String::as_str)
    }

    /// Colour for a tile.
    ///
    /// Ground cover and liquids take their look from the tiletype's material
    /// class; stone, ore and sand take theirs from the material itself — sand
    /// keeps its own five colours the way stone keeps its own, so a black
    /// sand sheet that turned out to need tinting (`library.rs:pack_walls`)
    /// tints black rather than the generic soil brown every other soil
    /// material shares.
    pub fn color(&self, tile_id: i32, pair: &MatPair) -> Rgb {
        use TiletypeShape as S;
        if matches!(self.shape(tile_id), S::Branch | S::Twig | S::Shrub | S::Sapling) {
            return FOLIAGE;
        }

        let class = self.tile_material(tile_id);
        if class == TiletypeMaterial::Soil
            && self.sand_hue(pair).is_some()
            && let Some(c) = self.colors.get(&(pair.mat_type, pair.mat_index))
        {
            return *c;
        }
        if class_wins(class) {
            return terrain_color(class);
        }
        self.colors
            .get(&(pair.mat_type, pair.mat_index))
            .copied()
            .unwrap_or_else(|| terrain_color(class))
    }

    pub fn tiletype_count(&self) -> usize {
        self.tiletypes.len()
    }

    pub fn material_count(&self) -> usize {
        self.colors.len()
    }
}

fn clamp_channel(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[cfg(test)]
mod sand_tests {
    use super::*;

    #[test]
    fn the_five_named_sands_classify_by_id() {
        assert_eq!(sand_hue_from_id("SAND"), Some(SandHue::Tan));
        assert_eq!(sand_hue_from_id("SAND_TAN"), Some(SandHue::Tan));
        assert_eq!(sand_hue_from_id("SAND_YELLOW"), Some(SandHue::Yellow));
        assert_eq!(sand_hue_from_id("SAND_WHITE"), Some(SandHue::White));
        assert_eq!(sand_hue_from_id("SAND_BLACK"), Some(SandHue::Black));
        assert_eq!(sand_hue_from_id("SAND_RED"), Some(SandHue::Red));
    }

    #[test]
    fn a_qualified_id_is_matched_on_its_own_token() {
        assert_eq!(sand_hue_from_id("INORGANIC:SAND_BLACK"), Some(SandHue::Black));
    }

    #[test]
    fn other_soil_materials_do_not_contain_sand() {
        // These all contain the substring "SAND" and are not sand: only an
        // exact-id match keeps them off the sand sheets.
        for id in ["SANDY_LOAM", "SANDY_CLAY", "SANDY_CLAY_LOAM", "SANDSTONE"] {
            assert_eq!(sand_hue_from_id(id), None, "{id} misclassified as sand");
        }
        for id in ["CLAY", "CLAY_LOAM", "LOAM", "SILT", "PEAT"] {
            assert_eq!(sand_hue_from_id(id), None, "{id} misclassified as sand");
        }
    }

    #[test]
    fn the_name_fallback_matches_dfs_adjectives() {
        assert_eq!(sand_hue_from_name("sand"), Some(SandHue::Tan));
        assert_eq!(sand_hue_from_name("yellow sand"), Some(SandHue::Yellow));
        assert_eq!(sand_hue_from_name("Red Sand"), Some(SandHue::Red));
        assert_eq!(sand_hue_from_name("sandy loam"), None);
    }

    #[test]
    fn a_palette_resolves_sand_hue_from_the_material_list() {
        use dfhack_remote::rfr::MaterialDefinition;
        let pair = MatPair { mat_type: 0, mat_index: 5 };
        let other = MatPair { mat_type: 0, mat_index: 6 };
        let materials = MaterialList {
            material_list: vec![
                MaterialDefinition {
                    mat_pair: pair,
                    id: Some("SAND_BLACK".to_string()),
                    ..Default::default()
                },
                MaterialDefinition {
                    mat_pair: other,
                    id: Some("CLAY_LOAM".to_string()),
                    ..Default::default()
                },
            ],
        };
        let palette = Palette::new(TiletypeList::default(), materials);
        assert_eq!(palette.sand_hue(&pair), Some(SandHue::Black));
        assert_eq!(palette.sand_hue(&other), None);
    }

    #[test]
    fn a_sand_tile_keeps_its_own_colour_instead_of_the_generic_soil_brown() {
        use dfhack_remote::rfr::{ColorDefinition, MaterialDefinition, Tiletype};
        let pair = MatPair { mat_type: 0, mat_index: 9 };
        let materials = MaterialList {
            material_list: vec![MaterialDefinition {
                mat_pair: pair,
                id: Some("SAND_BLACK".to_string()),
                state_color: Some(ColorDefinition { red: 10, green: 10, blue: 10 }),
                ..Default::default()
            }],
        };
        let tile = Tiletype {
            id: 1,
            material: Some(TiletypeMaterial::Soil as i32),
            ..Default::default()
        };
        let tiletypes = TiletypeList { tiletype_list: vec![tile] };
        let palette = Palette::new(tiletypes, materials);
        assert_eq!(palette.color(1, &pair), [10, 10, 10]);
        // A clay tile of the same class still reads as the generic soil.
        let clay = MatPair { mat_type: 0, mat_index: 10 };
        assert_eq!(palette.color(1, &clay), terrain_color(TiletypeMaterial::Soil));
    }
}
