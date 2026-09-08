//! What the coarse bands are painted with.
//!
//! The far band used to be flat vertex colour, so the fine window read as a
//! darker, greener patch in the middle of it and the seam showed as a change of
//! colour rather than of detail. Here a coarse slab wears the **same Dwarf
//! Fortress ground sprite** a fine floor wears, and a riser wears the same wall
//! side strip a fine wall does; the vertex colour only carries how far the
//! biome pulls that sprite from its own tone.
//!
//! One quad still covers a whole cell, six to forty-eight tiles across, so the
//! repeat cannot be done with UVs: the atlas has no room around a cell to tile
//! into. Instead a vertex carries the **cell's centre UV** and nothing else,
//! and `cloud_shadow.wgsl` recovers the cell from it and wraps the sprite
//! across the surface at one sprite per world tile, which is Dwarf Fortress's
//! own density and the fine map's. See [`Skins::uv`].

use dwarf_eye_art::atlas::{Atlas, Rect, WHITE_UV};

use crate::library::TileLibrary;
use crate::palette::Rgb;

use super::field::Cell;
use super::shade::{self, to_linear};

/// The ground sprite a coarse cell wears. These are exactly the families the
/// fine mesher packs for the ground under a standing object, so a coarse slab
/// and the floor beside it come out of the same atlas cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ground {
    Grass,
    Soil,
    Stone,
    Ice,
}

/// The wall sheet a riser is cut from: a cut bank is soil where the land is
/// wet and low, stone where it is drained or high.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Soil,
    Stone,
}

impl Ground {
    pub fn family(self) -> &'static str {
        match self {
            Ground::Grass => "GRASS_5",
            Ground::Soil => "DIRT_FLOOR_5",
            Ground::Stone => "STONE_FLOOR_5",
            Ground::Ice => "ROUGH_ICE_FLOOR",
        }
    }
}

impl Side {
    pub fn family(self) -> &'static str {
        match self {
            Side::Soil => "SOIL_WALL",
            Side::Stone => "STONE_WALL",
        }
    }
}

/// Above this vegetation the ground is turf rather than bare earth. The fine
/// map is grass from about a fifth of the scale up, which is where
/// [`shade::top_color`] already stops showing the surface material.
const TURF: f32 = 20.0;
/// Drainage at or above which a cut bank is stone rather than soil.
const DRAINED: f32 = 60.0;

/// The sprite a cell's top wears.
pub fn ground_of(cell: &Cell) -> Ground {
    if cell.snow > 60.0 {
        return Ground::Ice;
    }
    if cell.elevation >= shade::ROCK_ELEVATION {
        return Ground::Stone;
    }
    if cell.vegetation >= TURF {
        return Ground::Grass;
    }
    Ground::Soil
}

/// The sheet a cell's risers are cut from: soil where the land is wet and low,
/// stone where it is well drained or up in the mountains.
pub fn side_of(cell: &Cell) -> Side {
    if cell.drainage >= DRAINED || cell.elevation >= shade::ROCK_ELEVATION {
        Side::Stone
    } else {
        Side::Soil
    }
}

/// One packed cell: where it sits, and the mean of the light it reflects.
///
/// The mean is measured off the packed atlas rather than assumed. It is the
/// whole of the brightness match: the shader multiplies the sprite by the
/// vertex colour, so dividing the colour the biome asks for by the sprite's own
/// mean makes the surface average to exactly that colour whatever the sheet
/// under it is — a near-white ice floor, a mid-grey stone pattern or green turf
/// alike. Assuming instead that a near-grey sheet could be multiplied by the
/// colour outright, the way the fine mesher does, turned every snowy cell
/// white.
#[derive(Clone, Copy)]
struct Cellry {
    uv: [f32; 2],
    mean: [f32; 3],
}

impl Default for Cellry {
    fn default() -> Self {
        Self { uv: WHITE_UV, mean: [1.0; 3] }
    }
}

/// The mean of one packed cell, in linear light, weighted by opacity: a
/// transparent texel reflects nothing and must not drag the mean toward black.
fn cell_mean(atlas: &Atlas, rect: Rect) -> [f32; 3] {
    let f = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let x0 = (rect.u0 * atlas.width as f32).round() as u32;
    let x1 = (rect.u1 * atlas.width as f32).round() as u32;
    let y0 = (rect.v0 * atlas.height as f32).round() as u32;
    let y1 = (rect.v1 * atlas.height as f32).round() as u32;
    let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
    for y in y0..y1.max(y0 + 1) {
        for x in x0..x1.max(x0 + 1) {
            let i = ((y * atlas.width + x) * 4) as usize;
            let Some(px) = atlas.pixels.get(i..i + 4) else { continue };
            let alpha = px[3] as f32 / 255.0;
            for c in 0..3 {
                sum[c] += f(px[c]) * alpha;
            }
            n += alpha;
        }
    }
    if n <= 0.0 { [1.0; 3] } else { std::array::from_fn(|c| (sum[c] / n).max(1e-3)) }
}


/// Where every surface the coarse bands wear sits in the atlas.
///
/// Built once per horizon from the sprite library, and defaulting to the white
/// cell throughout, so a build with no Dwarf Fortress art behaves exactly as
/// the flat-coloured band did.
#[derive(Clone, Copy)]
pub struct Skins {
    grass: Cellry,
    soil: Cellry,
    stone: Cellry,
    ice: Cellry,
    soil_side: Cellry,
    stone_side: Cellry,
}

impl Default for Skins {
    fn default() -> Self {
        Self::none()
    }
}

impl Skins {
    /// Every surface on the white cell: the flat-coloured band, for a build
    /// with no Dwarf Fortress art behind it.
    pub const fn none() -> Self {
        let white = Cellry { uv: WHITE_UV, mean: [1.0; 3] };
        Self {
            grass: white,
            soil: white,
            stone: white,
            ice: white,
            soil_side: white,
            stone_side: white,
        }
    }

    pub fn from_library(library: &TileLibrary) -> Self {
        let atlas = library.atlas();
        let ground = |g: Ground| match library.ground_cell(g.family()) {
            Some((rect, _)) => Cellry { uv: rect.point(), mean: cell_mean(atlas, rect) },
            None => Cellry::default(),
        };
        let side = |s: Side| match library.wall_side_cell(s.family()) {
            Some((rect, _)) => Cellry { uv: rect.point(), mean: cell_mean(atlas, rect) },
            None => Cellry::default(),
        };
        Self {
            grass: ground(Ground::Grass),
            soil: ground(Ground::Soil),
            stone: ground(Ground::Stone),
            ice: ground(Ground::Ice),
            soil_side: side(Side::Soil),
            stone_side: side(Side::Stone),
        }
    }

    fn cell(&self, ground: Ground) -> Cellry {
        match ground {
            Ground::Grass => self.grass,
            Ground::Soil => self.soil,
            Ground::Stone => self.stone,
            Ground::Ice => self.ice,
        }
    }

    fn side_cell(&self, side: Side) -> Cellry {
        match side {
            Side::Soil => self.soil_side,
            Side::Stone => self.stone_side,
        }
    }

    /// The UV every vertex of a surface wearing this sprite carries: the atlas
    /// cell's own centre. The shader reads the cell out of it and wraps the
    /// sprite across the surface by world position, so one quad however wide
    /// still shows one sprite per world tile.
    pub fn uv(&self, ground: Ground) -> [f32; 2] {
        self.cell(ground).uv
    }

    pub fn side_uv(&self, side: Side) -> [f32; 2] {
        self.side_cell(side).uv
    }

    /// The vertex colour a coarse top takes, given the colour the biome asks
    /// for: that colour over the sprite's own mean, so the surface averages to
    /// it whatever the sheet under it looks like. With no sprite at all the
    /// mean is white and the colour comes through unchanged, which is the flat
    /// band this replaces.
    pub fn tint(&self, ground: Ground, want: Rgb) -> [f32; 4] {
        ratio(want, self.cell(ground).mean)
    }

    /// The same for a riser.
    pub fn side_tint(&self, side: Side, want: Rgb) -> [f32; 4] {
        ratio(want, self.side_cell(side).mean)
    }
}

/// The multiplier that makes a sheet of mean `mean` average to `want`, in
/// linear light. Only the top is held: a very dark sheet would otherwise ask
/// for a multiplier that blows its highlights out, while there is nothing
/// wrong with a coarse cell that wants to be nearly black.
fn ratio(want: Rgb, mean: [f32; 3]) -> [f32; 4] {
    let a = to_linear(want);
    let f = |i: usize| (a[i] / mean[i].max(1e-3)).min(6.0);
    [f(0), f(1), f(2), 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(vegetation: f32, elevation: f32, snow: f32, drainage: f32) -> Cell {
        Cell {
            elevation,
            water: elevation - 20.0,
            vegetation,
            rainfall: 50.0,
            drainage,
            snow,
            ground: None,
            detail: true,
        }
    }

    #[test]
    fn the_sprite_follows_the_biome() {
        assert_eq!(ground_of(&cell(80.0, 120.0, 0.0, 40.0)), Ground::Grass);
        assert_eq!(ground_of(&cell(4.0, 120.0, 0.0, 40.0)), Ground::Soil);
        assert_eq!(ground_of(&cell(80.0, 190.0, 0.0, 40.0)), Ground::Stone);
        // Snow covers whatever is under it, mountain or meadow.
        assert_eq!(ground_of(&cell(80.0, 120.0, 90.0, 40.0)), Ground::Ice);
        assert_eq!(ground_of(&cell(80.0, 190.0, 90.0, 40.0)), Ground::Ice);
    }

    #[test]
    fn a_cut_bank_is_soil_when_wet_and_low() {
        assert_eq!(side_of(&cell(80.0, 120.0, 0.0, 10.0)), Side::Soil);
        assert_eq!(side_of(&cell(80.0, 120.0, 0.0, 90.0)), Side::Stone);
        assert_eq!(side_of(&cell(80.0, 190.0, 0.0, 10.0)), Side::Stone);
    }

    /// `cloud_shadow.wgsl` recovers a cell from the UV by flooring it into the
    /// atlas grid, and rebuilds the cell's texel origin from the layout it
    /// hardcodes. Both halves have to agree, and neither can see the other.
    #[test]
    fn the_shader_can_recover_the_cell_from_the_uv() {
        use dwarf_eye_art::atlas::{Atlas, CELL, GRID, PAD, SIDE, STRIDE};
        // The constants `cloud_shadow.wgsl` spells out.
        assert_eq!((GRID, CELL, PAD, STRIDE, SIDE), (32, 32, 16, 64, 2048));

        let atlas = Atlas::new();
        // Slot zero is the white cell, and untextured horizon geometry rides
        // on it: it has to floor to (0, 0) like any other.
        for (uv, want) in [(atlas.white().point(), (0.0, 0.0)), (WHITE_UV, (0.0, 0.0))] {
            let slot = ((uv[0] * GRID as f32).floor(), (uv[1] * GRID as f32).floor());
            assert_eq!(slot, want, "uv {uv:?} floored to {slot:?}");
        }
    }

    /// With no art loaded every surface points at the white cell and the
    /// colour comes through as the flat band's own.
    #[test]
    fn no_art_leaves_the_colour_alone() {
        let skins = Skins::default();
        assert_eq!(skins.uv(Ground::Grass), WHITE_UV);
        assert_eq!(skins.tint(Ground::Grass, shade::GRASS), to_linear(shade::GRASS));
    }

    /// The whole of the brightness match: sheet times tint averages to the
    /// colour the biome asked for, whether the sheet is near-white ice, a
    /// mid-grey stone pattern or green turf.
    #[test]
    fn a_sheet_times_its_tint_is_the_colour_asked_for() {
        for mean in [[0.86f32, 0.88, 0.93], [0.18, 0.18, 0.19], [0.09, 0.21, 0.05]] {
            let skins = Skins { grass: Cellry { uv: [0.5, 0.5], mean }, ..Default::default() };
            for want in [shade::GRASS, shade::SNOW, [48, 71, 31], shade::SOIL] {
                let tint = skins.tint(Ground::Grass, want);
                let wanted = to_linear(want);
                for i in 0..3 {
                    // Past the cap the sheet is simply too dark to reach the
                    // colour asked for, and is taken as bright as it goes.
                    if wanted[i] / mean[i] > 6.0 {
                        assert!((tint[i] - 6.0).abs() < 1e-4, "channel {i} ran past the cap");
                        continue;
                    }
                    let got = tint[i] * mean[i];
                    assert!(
                        (got - wanted[i]).abs() < 1e-3,
                        "mean {mean:?} want {want:?}: channel {i} came out {got}, not {}",
                        wanted[i]
                    );
                }
            }
        }
    }

    /// A snowy cell wearing a near-white ice sheet is not white: it is the
    /// colour the survey asked for, which is what the flat band drew.
    #[test]
    fn a_near_white_sheet_does_not_bleach_the_ground() {
        let mean = [0.86f32, 0.88, 0.93];
        let skins = Skins { ice: Cellry { uv: [0.5, 0.5], mean }, ..Default::default() };
        let want = shade::mix(shade::GRASS, shade::SNOW, 0.65);
        let tint = skins.tint(Ground::Ice, want);
        for i in 0..3 {
            let got = tint[i] * mean[i];
            assert!(got < to_linear(shade::SNOW)[i], "channel {i} is {got}, as bright as snow");
        }
    }
}
