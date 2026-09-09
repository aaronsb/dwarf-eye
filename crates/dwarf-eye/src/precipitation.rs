//! Rain and snow, as a particle volume the camera carries with it.
//!
//! Dwarf Fortress keeps a 5x5 grid of `weather_type` over the embark and the
//! protocol never sends it, so `dwarf_eye_world::weather` reads it through Lua.
//! The cell the character stands in says what is falling; the fraction of the
//! whole grid that is wet says how hard.
//!
//! One mesh holds every particle. Each is a quad rebuilt on the CPU per frame:
//! rain is a streak drawn along its own fall line, snow a camera-facing flake.
//! Their seeds are fixed, and the volume wraps around the camera per axis, so
//! the box travels without the particles sliding through it. Density is how
//! many of them are not collapsed to a point; the fade is the material's alpha.
//!
//! The same reading drives two surface terms in `shadow::ShadowUniform`: `wet`
//! darkens the ground and sharpens its specular while rain falls, and `snow`
//! whitens upward faces by DF's own snowfall at the embark's world tile,
//! blending over a minute so a thaw is not a cut.

use crate::shadow::TerrainMaterial;
use bevy::asset::RenderAssetUsages;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use dwarf_eye_world::weather::Precip;

/// How wide the volume is, in tiles. Wide enough to fill a normal framing,
/// narrow enough that a few thousand particles read as weather rather than
/// as scattered dots.
const RADIUS: f32 = 44.0;
/// How tall it is. Rain is only believable if it comes from above the frame.
const HEIGHT: f32 = 56.0;

/// Streaks at full intensity.
const RAIN_COUNT: usize = 4200;
const SNOW_COUNT: usize = 1800;
/// Quads the mesh always holds. The index buffer is built once for this many,
/// so the position buffer has to stay this long whatever is falling; the
/// surplus collapse to a point and draw nothing.
const TOTAL: usize = RAIN_COUNT;

/// Fall speed in tiles per second, and how long a streak is drawn.
const RAIN_SPEED: f32 = 34.0;
const RAIN_LENGTH: f32 = 3.2;
const RAIN_WIDTH: f32 = 0.028;
const SNOW_SPEED: f32 = 3.2;
const SNOW_SIZE: f32 = 0.16;

/// Sideways drift, in tiles per second. Snow takes the wind; rain barely does.
const RAIN_DRIFT: f32 = 2.5;
const SNOW_DRIFT: f32 = 2.2;

/// Seconds for the fall to arrive or clear.
const FADE: f32 = 3.0;
/// Seconds for lying snow to come and go. A thaw is slow.
const COVER_BLEND: f32 = 60.0;

/// How much the ground darkens under rain, and how far its roughness drops.
const WET_DARKEN: f32 = 0.22;
const WET_POLISH: f32 = 0.45;

/// What is falling, and how hard.
#[derive(Resource, Clone, Copy, Debug)]
pub struct Precipitation {
    /// What DF's grid says at the character's cell.
    pub kind: Precip,
    /// The fraction of the embark that is wet, 0..1.
    pub intensity: f32,
    /// False under a solid ceiling: the worker looks at the column above the
    /// camera in the loaded map.
    pub outdoors: bool,
    /// The kind actually being drawn, which lags `kind` across a change.
    drawn: Precip,
    /// How far in the fall has faded, 0..1.
    fade: f32,
}

impl Default for Precipitation {
    fn default() -> Self {
        Self {
            kind: Precip::None,
            intensity: 0.0,
            outdoors: true,
            drawn: Precip::None,
            fade: 0.0,
        }
    }
}

impl Precipitation {
    /// What is drawn and how much of it: kind, and 0..1 of full density.
    pub fn drawn(&self) -> (Precip, f32) {
        (self.drawn, self.fade * self.intensity.clamp(0.0, 1.0))
    }

    /// How wet the world reads, 0..1. Snow leaves the ground dry.
    pub fn wetness(&self) -> f32 {
        match self.drawn {
            Precip::Rain => self.fade * self.intensity.clamp(0.0, 1.0),
            _ => 0.0,
        }
    }

    pub fn describe(&self) -> String {
        let (kind, level) = self.drawn();
        match kind {
            Precip::None => "dry".into(),
            _ if !self.outdoors => format!("{} (sheltered)", kind.name()),
            _ => format!("{} {:.0}%", kind.name(), level * 100.0),
        }
    }

    /// `DWARF_EYE_WEATHER=clear|rain|snow` forces what falls, without asking
    /// the game. It wins over DF's own grid, which is what makes a shot
    /// repeatable; the `1` `2` `3` keys change the player's weather instead.
    pub fn override_from_env() -> Option<(Precip, f32)> {
        match std::env::var("DWARF_EYE_WEATHER").as_deref() {
            Ok("clear") => Some((Precip::None, 0.0)),
            Ok("rain") => Some((Precip::Rain, 0.8)),
            Ok("snow") => Some((Precip::Snow, 0.8)),
            _ => None,
        }
    }

    /// Takes a reading from the world, unless the environment has overridden it.
    pub fn report(&mut self, kind: Precip, intensity: f32, outdoors: bool) {
        let (kind, intensity, outdoors) =
            resolve((kind, intensity, outdoors), Self::override_from_env());
        self.kind = kind;
        self.intensity = intensity;
        self.outdoors = outdoors;
    }
}

/// What actually falls, given what the game reported and what the environment
/// forced. A forced sky also ignores the ceiling, so a shot framed anywhere
/// shows the weather it was asked for.
fn resolve(reported: (Precip, f32, bool), forced: Option<(Precip, f32)>) -> (Precip, f32, bool) {
    match forced {
        Some((kind, intensity)) => (kind, intensity, true),
        None => reported,
    }
}

/// Snow lying on the ground, on the coarse band's own scale.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct SnowCover {
    /// What DF's snowfall says, 0..1.
    pub target: f32,
    /// What is drawn, which crosses to the target over a minute.
    pub level: f32,
}

impl SnowCover {
    /// A covering forced from the environment, for a shot that must not wait
    /// for a winter. `DWARF_EYE_SNOW=0.7` sets it outright, and
    /// `DWARF_EYE_WEATHER=snow` lays one of its own, so a snow shot has snow on
    /// the ground in it.
    pub fn override_from_env() -> Option<f32> {
        if let Ok(level) = std::env::var("DWARF_EYE_SNOW") {
            return level.trim().parse::<f32>().ok().map(|v| v.clamp(0.0, 1.0));
        }
        matches!(std::env::var("DWARF_EYE_WEATHER").as_deref(), Ok("snow")).then_some(0.85)
    }
}

/// The one entity every particle lives in.
#[derive(Resource)]
struct Particles {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

pub struct PrecipitationPlugin;

impl Plugin for PrecipitationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Precipitation>()
            .init_resource::<SnowCover>()
            .add_systems(Startup, setup)
            .add_systems(Update, (drive, draw, drive_surfaces));
    }
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut precip: ResMut<Precipitation>,
    mut cover: ResMut<SnowCover>,
) {
    // A forced sky starts falling straight away rather than waiting on the
    // first weather poll, which is a dozen seconds and a map pass behind.
    if let Some((kind, intensity)) = Precipitation::override_from_env() {
        precip.kind = kind;
        precip.intensity = intensity;
    }
    if let Some(level) = SnowCover::override_from_env() {
        cover.target = level;
        cover.level = level;
    }

    let mesh = meshes.add(empty_mesh(TOTAL));
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.86, 0.90, 0.96, 0.0),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(material.clone()),
        // The volume is written in world space every frame, so its own
        // transform stays at the origin.
        Transform::IDENTITY,
        NotShadowCaster,
        NotShadowReceiver,
    ));
    commands.insert_resource(Particles { mesh, material });
}

/// A mesh of `count` quads, indexed once. Only the positions ever change.
fn empty_mesh(count: usize) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; count * 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; count * 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32, 0.0]; count * 4]);
    let mut indices = Vec::with_capacity(count * 6);
    for quad in 0..count as u32 {
        let base = quad * 4;
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Fades the fall in and out, and crosses the lying snow over.
fn drive(time: Res<Time>, mut precip: ResMut<Precipitation>, mut cover: ResMut<SnowCover>) {
    let dt = time.delta_secs();

    // A change of kind fades the old one out before the new one starts, so
    // rain never turns into snow mid-flight.
    let wanted = if precip.outdoors {
        precip.kind
    } else {
        Precip::None
    };
    let closing = wanted != precip.drawn;
    let step = dt / FADE;
    if closing {
        precip.fade = (precip.fade - step).max(0.0);
        if precip.fade <= 0.0 {
            precip.drawn = wanted;
        }
    } else if precip.drawn != Precip::None {
        precip.fade = (precip.fade + step).min(1.0);
    }

    let step = dt / COVER_BLEND;
    let delta = (cover.target - cover.level).clamp(-step, step);
    cover.level += delta;
}

/// Writes every particle's quad into the mesh.
fn draw(
    time: Res<Time>,
    precip: Res<Precipitation>,
    particles: Res<Particles>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    camera: Query<&Transform, With<Camera3d>>,
    mut drawn: Local<usize>,
) {
    let Ok(camera) = camera.single() else { return };
    let Some(mut mesh) = meshes.get_mut(&particles.mesh) else {
        return;
    };
    let (kind, level) = precip.drawn();

    let (count, alpha) = match kind {
        Precip::None => (0, 0.0),
        Precip::Rain => (RAIN_COUNT, 0.16),
        Precip::Snow => (SNOW_COUNT, 0.85),
    };
    let live = (count as f32 * level.clamp(0.0, 1.0)) as usize;
    if let Some(mut material) = materials.get_mut(&particles.material) {
        material.base_color = match kind {
            Precip::Snow => Color::srgba(0.97, 0.98, 1.0, alpha * precip.fade),
            _ => Color::srgba(0.72, 0.79, 0.90, alpha * precip.fade),
        };
    }
    // Nothing falling: clear the buffer once and leave it alone.
    if live == 0 && *drawn == 0 {
        return;
    }
    *drawn = live;

    let eye = camera.translation;
    let forward = *camera.forward();
    // The box is pushed along the view so most of it falls where the camera is
    // looking rather than behind it.
    let ahead = Vec3::new(forward.x, 0.0, forward.z).normalize_or_zero() * (RADIUS * 0.5);
    let (speed, drift, size) = match kind {
        Precip::Snow => (SNOW_SPEED, SNOW_DRIFT, SNOW_SIZE),
        _ => (RAIN_SPEED, RAIN_DRIFT, RAIN_WIDTH),
    };
    let wind = crate::clouds::WIND.normalize_or_zero() * drift;
    let fall = Vec3::new(wind.x, -speed, wind.y);
    let along = fall.normalize_or(Vec3::NEG_Y);
    // A streak drawn along the fall line needs a width across the view; a
    // flake is simply camera-facing. Both are the same for every particle, so
    // the cross products are taken once.
    let (edge_a, edge_b) = match kind {
        Precip::Snow => (*camera.right() * size, *camera.up() * size),
        _ => (
            along * (RAIN_LENGTH * 0.5),
            along.cross(forward).normalize_or_zero() * size,
        ),
    };

    let box_size = Vec3::new(RADIUS * 2.0, HEIGHT, RADIUS * 2.0);
    let elapsed = time.elapsed_secs();
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(TOTAL * 4);
    for index in 0..TOTAL {
        if index >= live {
            positions.extend_from_slice(&[[0.0; 3]; 4]);
            continue;
        }
        let seed = seed_point(index as u32) * box_size + fall * elapsed;
        let centre = wrap_around(seed, eye + ahead, box_size);
        positions.push((centre - edge_a - edge_b).to_array());
        positions.push((centre - edge_a + edge_b).to_array());
        positions.push((centre + edge_a + edge_b).to_array());
        positions.push((centre + edge_a - edge_b).to_array());
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
}

/// The particle's fixed place in the unit cube, from its index alone.
fn seed_point(index: u32) -> Vec3 {
    Vec3::new(hash(index * 3), hash(index * 3 + 1), hash(index * 3 + 2))
}

/// A cheap integer hash, 0..1.
fn hash(mut x: u32) -> f32 {
    x = x.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let word = ((x >> ((x >> 28).wrapping_add(4))) ^ x).wrapping_mul(277_803_737);
    ((word >> 22) ^ word) as f32 / u32::MAX as f32
}

/// Folds a point into the box the camera stands in the middle of.
///
/// Per axis, so a particle that leaves the top comes back at the bottom and the
/// volume travels with the camera without anything sliding through it.
fn wrap_around(point: Vec3, centre: Vec3, size: Vec3) -> Vec3 {
    let rel = point - centre + size * 0.5;
    Vec3::new(
        rel.x.rem_euclid(size.x),
        rel.y.rem_euclid(size.y),
        rel.z.rem_euclid(size.z),
    ) - size * 0.5
        + centre
}

/// Pushes the wet and snow terms into every terrain material.
///
/// The coarse horizon shades its own snow out of `RegionTile.snow`, so its
/// materials are left alone; whitening them here would count it twice and draw
/// a line at the seam.
fn drive_surfaces(
    precip: Res<Precipitation>,
    cover: Res<SnowCover>,
    mut materials: ResMut<Assets<TerrainMaterial>>,
    mut last: Local<(f32, f32)>,
) {
    let wet = precip.wetness();
    let snow = cover.level;
    if (wet - last.0).abs() < 0.002 && (snow - last.1).abs() < 0.002 {
        return;
    }
    *last = (wet, snow);
    let ids: Vec<_> = materials.ids().collect();
    for id in ids {
        let Some(mut material) = materials.get_mut(id) else {
            continue;
        };
        if material.extension.uniform.horizon > 0.5 {
            continue;
        }
        material.extension.uniform.wet = wet * WET_DARKEN;
        material.extension.uniform.polish = wet * WET_POLISH;
        material.extension.uniform.snow = snow;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(kind: Precip, intensity: f32, fade: f32) -> Precipitation {
        Precipitation {
            kind,
            intensity,
            outdoors: true,
            drawn: kind,
            fade,
        }
    }

    #[test]
    fn intensity_scales_what_is_drawn() {
        let light = state(Precip::Rain, 4.0 / 25.0, 1.0);
        let storm = state(Precip::Rain, 1.0, 1.0);
        assert!((light.drawn().1 - 0.16).abs() < 1e-6);
        assert_eq!(storm.drawn().1, 1.0);
        assert!(light.drawn().1 < storm.drawn().1);
    }

    #[test]
    fn a_half_faded_fall_draws_half_of_its_density() {
        let half = state(Precip::Rain, 1.0, 0.5);
        assert_eq!(half.drawn().1, 0.5);
    }

    #[test]
    fn only_rain_wets_the_ground() {
        assert!(state(Precip::Rain, 1.0, 1.0).wetness() > 0.0);
        assert_eq!(state(Precip::Snow, 1.0, 1.0).wetness(), 0.0);
        assert_eq!(state(Precip::None, 1.0, 1.0).wetness(), 0.0);
    }

    #[test]
    fn a_ceiling_stops_the_fall() {
        let mut precip = state(Precip::Rain, 1.0, 1.0);
        precip.outdoors = false;
        // `drive` reads the shelter, so the fall it wants is nothing.
        assert!(!precip.outdoors);
        assert_eq!(
            if precip.outdoors {
                precip.kind
            } else {
                Precip::None
            },
            Precip::None
        );
    }

    #[test]
    fn the_environment_override_beats_the_game_and_the_ceiling() {
        let reported = (Precip::Snow, 0.4, false);
        assert_eq!(
            resolve(reported, None),
            reported,
            "with nothing forced the game decides"
        );
        assert_eq!(
            resolve(reported, Some((Precip::Rain, 0.8))),
            (Precip::Rain, 0.8, true),
            "a forced sky ignores both the game's grid and the ceiling"
        );
        assert_eq!(
            resolve(reported, Some((Precip::None, 0.0))),
            (Precip::None, 0.0, true),
            "clear forces dry over the game's snow"
        );
    }

    #[test]
    fn the_volume_wraps_around_the_camera() {
        let size = Vec3::new(20.0, 10.0, 20.0);
        let eye = Vec3::new(100.0, 5.0, -30.0);
        for index in 0..500u32 {
            let point = seed_point(index) * size * 7.0 - Vec3::splat(400.0);
            let inside = wrap_around(point, eye, size) - eye;
            assert!(inside.x.abs() <= size.x * 0.5 + 1e-3, "{inside}");
            assert!(inside.y.abs() <= size.y * 0.5 + 1e-3, "{inside}");
            assert!(inside.z.abs() <= size.z * 0.5 + 1e-3, "{inside}");
        }
    }

    #[test]
    fn the_seeds_fill_the_cube() {
        let mut lo = Vec3::splat(1.0);
        let mut hi = Vec3::ZERO;
        for index in 0..3000u32 {
            let p = seed_point(index);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        assert!(
            lo.max_element() < 0.02,
            "{lo} leaves a corner of the box empty"
        );
        assert!(
            hi.min_element() > 0.98,
            "{hi} leaves a corner of the box empty"
        );
    }

    #[test]
    fn every_quad_is_indexed() {
        let mesh = empty_mesh(7);
        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("no indices")
        };
        assert_eq!(indices.len(), 7 * 6);
        assert_eq!(*indices.iter().max().unwrap(), 7 * 4 - 1);
    }
}
