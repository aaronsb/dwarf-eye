//! Scattering crowns over the coarse bands.
//!
//! A region tile names the species growing on it (`tree_materials`) and how
//! thickly (`vegetation`), which is everything a placement rule needs. The rule
//! is a pure function of the region tile's absolute coordinates, so a tree
//! keeps its spot as the window moves under it and is replaced in place, not
//! reshuffled, when the fine map arrives; and lowering the density only drops
//! the tail of the list, so the trees that remain do not move.
//!
//! Which stage of the tree chain an instance draws at is **not** decided here.
//! Every placed tree carries all three stages and Bevy swaps between them by
//! the camera's own distance to that tree (`main.rs:horizon_ranges`), the way
//! the canopy bands do: a region-sourced tree can stand a few tiles from the
//! camera, and deciding its stage from its distance to the window's centre
//! drew boxes the size of houses right in front of the eye. Past `REACH`
//! nothing is placed at all and the ground carries the canopy as colour
//! instead (`terrace.rs:canopy_at`).

use dwarf_eye_trees::Preset;

use super::field::hash;
use super::terrace::Terrain;
use super::{REGION_TILE, Window};

/// Vegetation at or below this yields no crowns: bare rock and tundra.
const FLOOR: f32 = 15.0;
/// Crowns per region tile at full vegetation, before the distance fade.
const PER_TILE: f32 = 20.0;
/// Full density inside this radius from the window centre, in tiles.
pub const NEAR: f32 = 720.0;
/// None at or beyond this radius, which is also the edge of the region details.
pub const REACH: f32 = 1920.0;
/// Size jitter around the height a Dwarf Fortress tree stands at.
const SCALE: (f32, f32) = (0.72, 1.35);
/// The height, in levels, a far tree is normalised to before the jitter. The
/// lab presets stand 13 to 22 tiles; the fine map shrinks them to the height
/// the game reports per tree, which runs about 4 to 10 levels, and the horizon
/// has no per-tree height, so it takes the middle of that range.
pub const DF_TREE_HEIGHT: f32 = 7.0;
/// Share of a patch that grows a noticeably taller emergent, and how much
/// taller: real woodland is not one storey.
const EMERGENT: f32 = 0.05;
const EMERGENT_SCALE: f32 = 1.8;
/// Share of a patch drawn from a neighbouring region tile's species mix, so a
/// biome boundary interleaves rather than switching on a 48-tile line.
const STRAY: f32 = 0.07;
/// Share drawn from a contrasting preset out past `CONTRAST_FROM`, so the far
/// band does not read as one habitat: a conifer in broadleaf country, and the
/// odd standing dead tree.
const CONTRAST: f32 = 0.03;
const CONTRAST_FROM: f32 = 1200.0;
const CONTRASTS: [Preset; 2] = [Preset::Spruce, Preset::DeadTree];
/// The stages of the far band's tree chain, nearest first. Each is one entity
/// per tree carrying its own `VisibilityRange`, so the swap is Bevy's and the
/// measure is the camera's distance to that tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// A grown tree at one voxel to a tile: `horizon::grown`.
    Grown,
    /// The canonical crown, a trunk under one to three boxes:
    /// `dwarf_eye_trees::crown`.
    Crown,
    /// Its bounding box in the crown's mean colour: `dwarf_eye_trees::crown_box`.
    Box,
}

/// Nearest first, which is the order `main.rs` builds the ranges in.
pub const STAGES: [Stage; 3] = [Stage::Grown, Stage::Crown, Stage::Box];

/// One placed tree, in render space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrownInstance {
    pub pos: [f32; 3],
    /// How tall this tree stands, in tiles. A stage's mesh has a height of its
    /// own, so the transform's scale is this over that.
    pub height: f32,
    pub yaw: f32,
    /// Which of the species' canonical growths the nearest stage draws.
    pub variant: u32,
}

/// A region tile's worth of forest: what grows there and how thickly.
pub struct Patch {
    /// Absolute region tile.
    pub rx: i32,
    pub ry: i32,
    pub vegetation: i32,
    /// The species mix, in the order DF listed the tile's tree materials.
    pub presets: Vec<Preset>,
    /// Species from the neighbouring region tiles that this tile does not
    /// carry itself: a few of them stray across.
    pub neighbours: Vec<Preset>,
}

/// How many crowns a region tile carries: linear in vegetation above the floor,
/// faded to nothing between `NEAR` and `REACH`.
pub fn crown_count(vegetation: i32, radius: f32) -> usize {
    let v = ((vegetation as f32 - FLOOR) / (100.0 - FLOOR)).clamp(0.0, 1.0);
    let fade = if radius <= NEAR {
        1.0
    } else {
        (1.0 - (radius - NEAR) / (REACH - NEAR)).clamp(0.0, 1.0)
    };
    (PER_TILE * v * fade).round() as usize
}

/// Places a region tile's crowns, skipping any that fall where the fine map
/// stands. Positions come out in a fixed order, so a smaller count is a prefix
/// of a larger one.
pub fn scatter(patch: &Patch, terrain: &Terrain, window: &Window, out: &mut Vec<(Preset, CrownInstance)>) {
    if patch.presets.is_empty() {
        return;
    }
    let (ox, oz) = (patch.rx * REGION_TILE - window.origin.0, patch.ry * REGION_TILE - window.origin.1);
    let radius = terrain.radius(ox + REGION_TILE / 2, oz + REGION_TILE / 2) as f32;
    let count = crown_count(patch.vegetation, radius);
    for k in 0..count {
        let seed = |salt: u32| hash(patch.rx, patch.ry, k as u32 * 8 + salt);
        let unit = |salt: u32| seed(salt) as f32 / u32::MAX as f32;
        let tx = ox + (unit(0) * REGION_TILE as f32) as i32;
        let tz = oz + (unit(1) * REGION_TILE as f32) as i32;
        // The fine map draws its own trees here.
        if terrain.fine.covers(tx, tz) {
            continue;
        }
        let Some(level) = terrain.level_at(tx, tz) else { continue };
        let preset = pick(patch, radius, unit(5), seed(2) as usize);
        let mut jitter = SCALE.0 + (SCALE.1 - SCALE.0) * unit(3);
        if unit(6) < EMERGENT {
            jitter *= EMERGENT_SCALE;
        }
        out.push((
            preset,
            CrownInstance {
                pos: [tx as f32 + 0.5, terrain.slab_top(level), tz as f32 + 0.5],
                height: DF_TREE_HEIGHT * jitter,
                yaw: unit(4) * std::f32::consts::TAU,
                variant: seed(7) % super::grown::VARIANTS,
            },
        ));
    }
}

/// Which species one instance is: mostly the tile's own mix, a few per cent
/// strayed in from a neighbouring tile, and out past `CONTRAST_FROM` a few per
/// cent of something that does not belong at all.
fn pick(patch: &Patch, radius: f32, roll: f32, index: usize) -> Preset {
    if radius >= CONTRAST_FROM && roll < CONTRAST {
        return CONTRASTS[index % CONTRASTS.len()];
    }
    if !patch.neighbours.is_empty() && roll < CONTRAST + STRAY {
        return patch.neighbours[index % patch.neighbours.len()];
    }
    patch.presets[index % patch.presets.len()]
}

/// The preset a DF tree material's name maps onto. The region tile names its
/// species outright, so the horizon takes DF's own mix rather than guessing one
/// from the biome.
pub fn preset_for(name: &str) -> Preset {
    let n = name.to_ascii_lowercase();
    let has = |k: &str| n.contains(k);
    if has("willow") || has("mangrove") {
        Preset::Willow
    } else if has("birch") || has("aspen") || has("poplar") || has("alder") {
        Preset::Birch
    } else if has("pine") {
        Preset::Pine
    } else if has("spruce") || has("fir") || has("cedar") || has("larch") || has("hemlock") || has("yew") || has("juniper") || has("cypress") || has("redwood") {
        Preset::Spruce
    } else if has("cap") || has("fungiwood") || has("tube") || has("spore") || has("thorn") || has("mushroom") || has("goblin") {
        Preset::MushroomTree
    } else if has("saguaro") || has("acacia") || has("bush") {
        Preset::Shrub
    } else {
        Preset::Oak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_follows_vegetation() {
        assert_eq!(crown_count(0, 0.0), 0);
        assert_eq!(crown_count(15, 0.0), 0);
        assert_eq!(crown_count(100, 0.0), PER_TILE as usize);
        // Monotone in vegetation.
        let mut last = 0;
        for v in 0..=100 {
            let n = crown_count(v, 0.0);
            assert!(n >= last, "vegetation {v} gave {n} after {last}");
            last = n;
        }
    }

    #[test]
    fn density_fades_with_distance() {
        assert_eq!(crown_count(100, NEAR), PER_TILE as usize);
        assert_eq!(crown_count(100, REACH), 0);
        assert_eq!(crown_count(100, REACH + 1000.0), 0);
        let mid = crown_count(100, (NEAR + REACH) * 0.5);
        assert!(mid > 0 && mid < PER_TILE as usize, "midway gave {mid}");
    }

    fn patch(presets: Vec<Preset>, neighbours: Vec<Preset>) -> Patch {
        Patch { rx: 0, ry: 0, vegetation: 60, presets, neighbours }
    }

    /// Nearly every tree is the tile's own species; a few per cent stray in
    /// from next door, and further out a few per cent are something else
    /// entirely.
    #[test]
    fn the_mix_is_mostly_local() {
        let p = patch(vec![Preset::Oak], vec![Preset::Birch]);
        let (mut local, mut stray, mut contrast) = (0, 0, 0);
        for i in 0..2000 {
            let roll = i as f32 / 2000.0;
            match pick(&p, CONTRAST_FROM, roll, i) {
                Preset::Oak => local += 1,
                Preset::Birch => stray += 1,
                _ => contrast += 1,
            }
        }
        assert!(local > 1700, "only {local} of 2000 were the tile's own");
        assert!((100..200).contains(&stray), "{stray} strays");
        assert!((40..80).contains(&contrast), "{contrast} contrasts");
        // Inside CONTRAST_FROM nothing contrasting appears at all.
        for i in 0..200 {
            let got = pick(&p, 0.0, i as f32 / 2000.0, i);
            assert!(matches!(got, Preset::Oak | Preset::Birch), "{got:?} near the window");
        }
    }

    #[test]
    fn species_names_map_to_presets() {
        assert_eq!(preset_for("willow plant"), Preset::Willow);
        assert_eq!(preset_for("birch plant"), Preset::Birch);
        assert_eq!(preset_for("Pine plant"), Preset::Pine);
        assert_eq!(preset_for("black cap plant"), Preset::MushroomTree);
        assert_eq!(preset_for("sand pear tree plant"), Preset::Oak);
    }
}
