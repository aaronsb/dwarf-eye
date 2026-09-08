//! Ground colour for the coarse bands, from the same fields DF's own world map
//! is drawn with.

use crate::palette::Rgb;

use super::field::{Cell, hash01};

pub const WATER: Rgb = [60, 110, 190];
/// Rivers read as the same water, a touch brighter so a thin strip still
/// reads against the darker ground shadow it sits on.
pub const RIVER: Rgb = [70, 122, 198];
pub const GRASS: Rgb = [96, 142, 62];
/// Rainfall drives the grass hue between these two, so the midpoint lands on
/// `GRASS` and the fine map's tone is matched on average.
const DRY_GRASS: Rgb = [150, 156, 90];
const WET_GRASS: Rgb = [58, 138, 52];
pub const CANOPY: Rgb = [66, 106, 52];
const SNOW: Rgb = [226, 232, 240];
pub const SOIL: Rgb = [134, 96, 67];
/// Bare rock, above the treeline.
const ROCK: Rgb = [142, 136, 126];
/// A site building's walls, where its stone says nothing.
pub const BUILDING: Rgb = [128, 112, 96];

/// Elevation above which the ground fades from grass to bare rock: DF's
/// world-tile scale runs 0-99 ocean, 100-149 normal biomes, 150+ mountains.
const ROCK_ELEVATION: f32 = 150.0;
const ROCK_ELEVATION_FULL: f32 = 200.0;

pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}

/// Brightens or dims a colour by up to `amount`, keyed on absolute tile
/// coordinates so a patch of ground keeps its shade as the window moves.
pub fn jitter(rgb: Rgb, ax: i32, az: i32, amount: f32) -> Rgb {
    let factor = 1.0 + (hash01(ax, az, 7) * 2.0 - 1.0) * amount;
    [
        (rgb[0] as f32 * factor).round().clamp(0.0, 255.0) as u8,
        (rgb[1] as f32 * factor).round().clamp(0.0, 255.0) as u8,
        (rgb[2] as f32 * factor).round().clamp(0.0, 255.0) as u8,
    ]
}

pub fn to_linear(rgb: Rgb) -> [f32; 4] {
    let f = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    [f(rgb[0]), f(rgb[1]), f(rgb[2]), 1.0]
}

/// The surface material, pulled toward its own luminance the way the tile
/// mesher pulls substance colours: DF's material colours are strong, and a
/// hillside of them reads as paint.
pub fn ground_color(cell: &Cell) -> Rgb {
    let ground = cell.ground.unwrap_or(SOIL);
    let luma = 0.2126 * ground[0] as f32 + 0.7152 * ground[1] as f32 + 0.0722 * ground[2] as f32;
    [
        (luma + (ground[0] as f32 - luma) * 0.35) as u8,
        (luma + (ground[1] as f32 - luma) * 0.35) as u8,
        (luma + (ground[2] as f32 - luma) * 0.35) as u8,
    ]
}

/// The top of a coarse cell: the surface material under grass, canopy where
/// vegetation is dense and rainfall high, bare rock above the treeline, snow
/// on top, water where the land sits below the water table.
///
/// `canopy` is how much of the tile's foliage the ground has to carry itself,
/// 1 near the horizon where no crown is scattered and 0 where crowns stand.
pub fn top_color(cell: &Cell, canopy: f32) -> Rgb {
    if cell.underwater() {
        return WATER;
    }
    let ground = ground_color(cell);
    let r = (cell.rainfall / 100.0).clamp(0.0, 1.0);
    let grass = mix(DRY_GRASS, WET_GRASS, r);
    // The detailed map is grass at vegetation 30, so bare ground shows only
    // where vegetation is sparse; denser growth darkens toward canopy, most
    // strongly where it's also wet.
    let v = (cell.vegetation / 100.0).clamp(0.0, 1.0);
    let mut color = mix(ground, grass, (v / 0.2).clamp(0.0, 1.0));
    let shade = ((v - 0.3) / 0.7).clamp(0.0, 1.0) * (0.25 + 0.75 * r) * 0.6;
    color = mix(color, CANOPY, shade * canopy.clamp(0.0, 1.0));
    let rock_t = ((cell.elevation - ROCK_ELEVATION) / (ROCK_ELEVATION_FULL - ROCK_ELEVATION)).clamp(0.0, 1.0);
    color = mix(color, ROCK, rock_t);
    if cell.snow > 0.0 {
        color = mix(color, SNOW, (cell.snow / 100.0).clamp(0.0, 1.0));
    }
    color
}

/// The face of a terrace step: the soil or stone under the turf, dimmed,
/// because a riser is a cut bank rather than a lit top.
pub fn riser_color(cell: &Cell, top: Rgb) -> Rgb {
    let ground = mix(ground_color(cell), SOIL, 0.35);
    let rock_t = ((cell.elevation - ROCK_ELEVATION) / (ROCK_ELEVATION_FULL - ROCK_ELEVATION)).clamp(0.0, 1.0);
    let color = mix(ground, ROCK, rock_t);
    // Kept close to the turf above it: a terrace edge should read as a shaded
    // slope, not as a stripe of a different substance.
    mix(mix(top, color, 0.55), [0, 0, 0], 0.18)
}
