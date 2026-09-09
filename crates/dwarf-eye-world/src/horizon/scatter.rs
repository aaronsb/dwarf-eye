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
//! Every placed tree carries every stage and Bevy swaps between them by the
//! camera's own distance to that tree (`main.rs:horizon_ranges`), the way the
//! canopy bands do: a region-sourced tree can stand a few tiles from the
//! camera, and deciding its stage from its distance to the window's centre
//! drew boxes the size of houses right in front of the eye. Past `REACH`
//! nothing is placed at all and the ground carries the canopy as colour
//! instead (`terrace.rs:canopy_at`).
//!
//! How a stage is *drawn* is [`super::batch`]'s: the cheap stages are baked
//! into one mesh per cell, so the swap is per cell rather than per tree, and
//! the rasterised cuts stay one entity a tree but only where the camera can
//! reach them. That is why an instance carries its own tile and reach.

use dwarf_eye_trees::Preset;

use crate::factory;
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
/// How one stage of the far band's tree chain is drawn: the factory's own
/// vocabulary, so the window's bands and these instances name the same things.
pub type Stage = factory::Detail;

/// The stages of the far band's tree chain, nearest first: the whole of the
/// factory's tree chain, which opens with the whole of the window's.
///
/// Each is one entity per tree carrying its own `VisibilityRange`, so the swap
/// is Bevy's and the measure is the camera's distance to that tree. Detail is
/// that distance and nothing else — never which survey the tree came from —
/// which is what keeps the window's boundary out of the canopy.
pub fn stages() -> &'static [factory::Stage] {
    factory::tree_chain().instanced()
}

/// One placed tree, in render space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrownInstance {
    pub pos: [f32; 3],
    /// How tall this tree stands, in tiles. A stage's mesh has a height of its
    /// own, so the transform's scale is this over that.
    pub height: f32,
    pub yaw: f32,
    /// Which of the species' canonical growths the rasterised stages draw.
    pub variant: u32,
    /// The absolute region tile this tree grew on, which is the cell its cheap
    /// stages are merged over (`batch::assemble`).
    pub tile: (i32, i32),
    /// The nearest the camera can come to this tree's own region tile while it
    /// stands anywhere in the live window (`batch::reach`). A stage that hands
    /// over inside this can never be asked for and is never spawned.
    pub reach: f32,
}

/// A region tile's worth of forest: what grows there and how thickly.
pub struct Patch {
    /// Absolute region tile.
    pub rx: i32,
    pub ry: i32,
    pub vegetation: i32,
    /// The tile's elevation on DF's own scale, for the tree line.
    pub elevation: f32,
    /// The species mix, in the order DF listed the tile's tree materials.
    pub presets: Vec<Preset>,
    /// Species from the neighbouring region tiles that this tile does not
    /// carry itself: a few of them stray across.
    pub neighbours: Vec<Preset>,
}

/// Over how many tiles the density blends from what the fine map actually
/// grows at the window's edge to what the region survey says.
pub const BLEND: f32 = 200.0;
/// How far out the fine map is searched for that cue, in 16-tile blocks: a
/// little past the blend, so a region tile inside it always finds one.
pub const BLEND_BLOCKS: i32 = 14;
/// How far a crown keeps clear of a site building's footprint, in tiles. A
/// fortress or a tower stands in a clearing, not in a thicket.
pub const CLEARING: f32 = 6.0;

/// Where the tree line is, on DF's own elevation scale: 0 to 99 ocean, 100 to
/// 149 normal biomes, 150 and up mountains
/// (`docs/dfhack-horizon-notes.md`). Nothing grows at or above it, and the
/// density tapers to nothing over the levels below so the line is soft rather
/// than a ring drawn round a peak.
pub const TREELINE: f32 = 150.0;
pub const TREELINE_TAPER: f32 = 10.0;

/// How much of a region tile's density survives its elevation.
pub fn treeline(elevation: f32) -> f32 {
    ((TREELINE - elevation) / TREELINE_TAPER).clamp(0.0, 1.0)
}

/// How many crowns a region tile carries: linear in vegetation above the floor,
/// faded to nothing between `NEAR` and `REACH`.
pub fn crown_count(vegetation: i32, radius: f32) -> usize {
    crown_count_near(vegetation, 100.0, radius, None)
}

/// The same, blended toward what the fine map is actually growing nearby.
///
/// `observed` is `(canopy cover 0..1 seen in the fine map, distance to that
/// fine ground in tiles)` from `FineSurface::nearby_density`. A region tile's
/// `vegetation` is a 48-tile average and says nothing about the clearing the
/// character is standing in, so right at the window's edge the fine map wins
/// outright and the survey takes over across [`BLEND`]: a treeless window edge
/// gives a treeless surround, a dense one continues the forest.
///
/// Full cover maps to the survey's own full density, so the two ends of the
/// blend are on one scale and neither can run away with the count.
///
/// Only the count changes, and the count is a prefix of the seeded list, so no
/// tree moves.
pub fn crown_count_near(
    vegetation: i32,
    elevation: f32,
    radius: f32,
    observed: Option<(f32, f32)>,
) -> usize {
    let v = ((vegetation as f32 - FLOOR) / (100.0 - FLOOR)).clamp(0.0, 1.0);
    let survey = PER_TILE * v;
    let wanted = match observed {
        Some((cover, distance)) => {
            let seen = PER_TILE * cover.clamp(0.0, 1.0);
            let t = (distance / BLEND).clamp(0.0, 1.0);
            seen + (survey - seen) * t
        }
        None => survey,
    };
    let fade = if radius <= NEAR {
        1.0
    } else {
        (1.0 - (radius - NEAR) / (REACH - NEAR)).clamp(0.0, 1.0)
    };
    (wanted.clamp(0.0, PER_TILE) * fade * treeline(elevation)).round() as usize
}

/// A site building's footprint, in render-local tiles, that crowns keep out of.
#[derive(Clone, Copy, Debug)]
pub struct Clearing {
    pub x0: f32,
    pub z0: f32,
    pub x1: f32,
    pub z1: f32,
}

impl Clearing {
    /// Whether a point is inside the footprint or within [`CLEARING`] of it.
    pub fn holds(&self, x: f32, z: f32) -> bool {
        x >= self.x0 - CLEARING
            && x <= self.x1 + CLEARING
            && z >= self.z0 - CLEARING
            && z <= self.z1 + CLEARING
    }
}

/// Places a region tile's crowns, skipping any that fall where the fine map
/// stands. Positions come out in a fixed order, so a smaller count is a prefix
/// of a larger one.
pub fn scatter(
    patch: &Patch,
    terrain: &Terrain,
    window: &Window,
    clearings: &[Clearing],
    out: &mut Vec<(Preset, CrownInstance)>,
) {
    if patch.presets.is_empty() {
        return;
    }
    let (ox, oz) = (patch.rx * REGION_TILE - window.origin.0, patch.ry * REGION_TILE - window.origin.1);
    let (cx, cz) = (ox + REGION_TILE / 2, oz + REGION_TILE / 2);
    let radius = terrain.radius(cx, cz) as f32;
    let tile = (patch.rx, patch.ry);
    let reach = super::batch::reach(tile, (window.origin.0, window.origin.1), window.live);
    let observed = terrain.fine.nearby_density(cx, cz, BLEND_BLOCKS);
    let count = crown_count_near(patch.vegetation, patch.elevation, radius, observed);
    for k in 0..count {
        let seed = |salt: u32| hash(patch.rx, patch.ry, k as u32 * 8 + salt);
        let unit = |salt: u32| seed(salt) as f32 / u32::MAX as f32;
        let tx = ox + (unit(0) * REGION_TILE as f32) as i32;
        let tz = oz + (unit(1) * REGION_TILE as f32) as i32;
        // The fine map draws its own trees here.
        if terrain.fine.covers(tx, tz) {
            continue;
        }
        // A site stands in a clearing: nothing grows on its footprint or in
        // the ground it keeps around itself.
        if clearings.iter().any(|c| c.holds(tx as f32 + 0.5, tz as f32 + 0.5)) {
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
                tile,
                reach,
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
        Patch { rx: 0, ry: 0, vegetation: 60, elevation: 120.0, presets, neighbours }
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

    /// A treeless window edge gives a treeless surround: what the fine map
    /// actually grows wins over the region tile's 48-tile average right at the
    /// boundary, and the survey takes over across `BLEND`.
    #[test]
    fn the_fine_map_sets_the_density_at_its_own_edge() {
        // A well vegetated region tile, so the survey alone would fill it.
        let survey = crown_count(90, 0.0);
        assert!(survey > 15, "{survey}");
        // Fine ground right here, with nothing growing on it.
        assert_eq!(crown_count_near(90, 100.0, 0.0, Some((0.0, 0.0))), 0);
        // Half way out the two are mixed, and past the blend the survey is
        // back in charge.
        let midway = crown_count_near(90, 100.0, 0.0, Some((0.0, BLEND * 0.5)));
        assert!(midway > 0 && midway < survey, "{midway} of {survey}");
        assert_eq!(crown_count_near(90, 100.0, 0.0, Some((0.0, BLEND))), survey);
        assert_eq!(crown_count_near(90, 100.0, 0.0, Some((0.0, BLEND * 4.0))), survey);
        // A dense window edge continues the forest through a thin survey.
        let thin = crown_count_near(20, 100.0, 0.0, None);
        let dense = crown_count_near(20, 100.0, 0.0, Some((0.9, 0.0)));
        assert!(dense > thin * 4, "{dense} against {thin}");
        // And the cue can never ask for more than the survey's own full
        // density, however wooded the window is.
        assert!(crown_count_near(20, 100.0, 0.0, Some((5.0, 0.0))) <= PER_TILE as usize);
    }

    /// Nothing grows on a site's footprint or in the ground it keeps clear
    /// around itself.
    #[test]
    fn a_site_footprint_is_a_clearing() {
        let clearing = Clearing { x0: 10.0, z0: 20.0, x1: 30.0, z1: 40.0 };
        assert!(clearing.holds(20.0, 30.0), "inside the footprint");
        assert!(clearing.holds(10.0 - CLEARING + 0.5, 30.0), "just inside the margin");
        assert!(!clearing.holds(10.0 - CLEARING - 1.0, 30.0), "clear of the margin");
        assert!(!clearing.holds(20.0, 40.0 + CLEARING + 1.0), "clear to the south");
    }

    /// Nothing grows at the tree line, and the density tapers into it rather
    /// than stopping on a contour.
    #[test]
    fn the_tree_line_is_soft() {
        let at = |e: f32| crown_count_near(90, e, 0.0, None);
        let lowland = at(100.0);
        assert!(lowland > 0);
        assert_eq!(at(TREELINE), 0, "trees grew on the mountain");
        assert_eq!(at(TREELINE + 40.0), 0, "trees grew above the mountain");
        assert_eq!(at(TREELINE - TREELINE_TAPER), lowland, "the taper reached too low");
        let midway = at(TREELINE - TREELINE_TAPER * 0.5);
        assert!(midway > 0 && midway < lowland, "{midway} of {lowland} halfway up");
    }

    /// A region tile that names no tree materials grows nothing, whatever its
    /// vegetation says: `vegetation` counts grass and shrubs too.
    #[test]
    fn a_tile_with_no_species_grows_nothing() {
        let field = crate::horizon::field::Field::default();
        let fine = crate::horizon::fine::FineSurface::default();
        let window =
            Window { x0: 0, y0: 0, origin: (0, 0, 100), centre: (72, 72), live: (0, 0, 144, 144) };
        let skins = crate::horizon::skin::Skins::none();
        let terrain = Terrain {
            field: &field,
            fine: &fine,
            window: &window,
            skins: &skins,
            crown_near: NEAR,
            crown_reach: REACH,
        };
        let mut out = Vec::new();
        scatter(
            &Patch {
                rx: 0,
                ry: 0,
                vegetation: 100,
                elevation: 120.0,
                presets: Vec::new(),
                // Even with wooded neighbours, a tile that names no species of
                // its own stays bare.
                neighbours: vec![Preset::Oak],
            },
            &terrain,
            &window,
            &[],
            &mut out,
        );
        assert!(out.is_empty(), "{} crowns grew where DF names no trees", out.len());
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
