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

/// Everything needed to give a tile a shape and a colour.
pub struct Palette {
    tiletypes: HashMap<i32, Tiletype>,
    colors: HashMap<(i32, i32), Rgb>,
    names: HashMap<(i32, i32), String>,
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
        for m in materials.material_list {
            let key = (m.mat_pair.mat_type, m.mat_pair.mat_index);
            if let Some(c) = m.state_color {
                colors.insert(key, [clamp_channel(c.red), clamp_channel(c.green), clamp_channel(c.blue)]);
            }
            if let Some(n) = m.name {
                names.insert(key, n);
            }
        }

        Self { tiletypes, colors, names }
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
    /// class; stone and ore take theirs from the material itself.
    pub fn color(&self, tile_id: i32, pair: &MatPair) -> Rgb {
        use TiletypeShape as S;
        if matches!(self.shape(tile_id), S::Branch | S::Twig | S::Shrub | S::Sapling) {
            return FOLIAGE;
        }

        let class = self.tile_material(tile_id);
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
