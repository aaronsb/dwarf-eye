//! A voxel view of a live Dwarf Fortress map, fed by DFHack's
//! RemoteFortressReader plugin.
//!
//! Start Dwarf Fortress with DFHack, load a fort or an adventurer, then run this.

mod camera;
mod capture;
mod clouds;
mod god_rays;
mod noise;
mod polls;
mod precipitation;
mod shadow;
mod sky;
mod stars;
mod texture;
mod units;
mod walk;
mod worker;

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::prelude::*;
use bevy::camera::Exposure;
use bevy::camera::visibility::VisibilityRange;
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::light::{
    Atmosphere, AtmosphereEnvironmentMapLight, SunDisk, atmosphere::ScatteringMedium,
    light_consts::lux,
};
use bevy::light::NotShadowCaster;
use bevy::pbr::{AtmosphereMode, AtmosphereSettings};
use bevy::post_process::bloom::Bloom;
use camera::FlyCamera;
use clouds::Weather;
use shadow::{
    CloudShadow, ShadowUniform, TerrainMaterial as TerrainMat, canopy_sky, leaf_transmission,
};
use sky::Clock;
use dwarf_eye_trees as trees;
use bevy::render::batching::gpu_preprocessing::GpuPreprocessingSupport;
use bevy::render::occlusion_culling::OcclusionCulling;
use bevy::render::{RenderApp, RenderStartup};
use dwarf_eye_world::canopy::{BANDS, Band, CanopyMeshes, Coat, Surface};
use dwarf_eye_world::horizon::scatter::{STAGES as HORIZON_STAGES, Stage as TreeStage};
use dwarf_eye_world::{BLOCK, MeshData, MeshOptions, mesh::Z_SCALE};
use std::collections::HashMap;
use worker::{Bridge, ChunkKey, Command, Event};

/// Where the cut plane starts, relative to the player's z-level.
///
/// A tree stands several levels above the ground it grows from, so a plane just
/// overhead would shear the canopy off.
const CEILING_ABOVE_PLAYER: i32 = 16;

/// A sky held against what the game reports, from the environment.
///
/// `DWARF_EYE_CLOUDS=cumulus=0.5,fog=0.4` names the kinds; `DWARF_EYE_WEATHER=`
/// `clear|rain|snow` is the same three skies the `1` `2` `3` keys ask the game
/// for, without asking the game — the keys change the player's weather, this
/// only changes the picture.
fn forced_weather() -> Option<Weather> {
    let preset = match std::env::var("DWARF_EYE_WEATHER").as_deref() {
        Ok("clear") => Weather::default(),
        Ok("rain") => {
            Weather { cumulus: 0.35, stratus: 0.95, cirrus: 0.0, fog: 0.15, countdown: 0.4 }
        }
        Ok("snow") => {
            Weather { cumulus: 0.2, stratus: 0.8, cirrus: 0.0, fog: 0.45, countdown: 0.6 }
        }
        _ => return Weather::from_env(),
    };
    Some(preset)
}


/// One line naming the world and the game's date, printed once both are known,
/// so an unattended shot can be labelled with what it caught.
/// Where the camera looks relative to the player, in DF tiles and levels.
///
/// `DWARF_EYE_AIM=dx,dy,dz`. The camera starts looking at the character, which
/// is the right default and no use at all for a screenshot of a hall sixty
/// tiles away.
fn aim_offset() -> (f32, f32, f32) {
    let Ok(spec) = std::env::var("DWARF_EYE_AIM") else { return (0.0, 0.0, 0.0) };
    let n: Vec<f32> = spec.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    match n.as_slice() {
        [x, y, z] => (*x, *y, *z),
        [x, y] => (*x, *y, 0.0),
        _ => (0.0, 0.0, 0.0),
    }
}

fn log_scene(clock: Res<Clock>, status: Res<Status>, mut said: Local<bool>) {
    if *said || status.world.is_empty() || clock.year == 0 {
        return;
    }
    *said = true;
    info!("scene: world {} date {}", status.world, clock.describe());
}

fn main() {
    // `DWARF_EYE_GROUND=stepped` puts the terraces back, for a before-and-after
    // at one framing (`heightfield.rs:Ground`).
    println!("ground: {}", dwarf_eye_world::Ground::current().name());
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "dwarf-eye".into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(FrameTimeDiagnosticsPlugin::default())
        .add_plugins(shadow::CloudShadowPlugin)
        .add_plugins(clouds::CloudPlugin)
        .add_plugins(god_rays::GodRaysPlugin)
        .add_plugins(capture::CapturePlugin)
        .add_plugins(precipitation::PrecipitationPlugin)
        .insert_resource(ClearColor(Color::srgb(0.42, 0.58, 0.78)))
        .init_resource::<ViewSettings>()
        .init_resource::<Bands>()
        .init_resource::<ChunkEntities>()
        .init_resource::<PendingChunks>()
        .init_resource::<Status>()
        .init_resource::<NeedsFetch>()
        .init_resource::<Clock>()
        .init_resource::<SunAim>()
        .init_resource::<Crowd>()
        .init_resource::<walk::WalkMode>()
        .insert_resource(forced_weather().unwrap_or_default())
        .insert_non_send(Bridge::spawn())
        .add_systems(Startup, (setup, stars::setup))
        .add_systems(
            Update,
            (
                size_bands,
                drain_worker,
                upload_chunks,
                handle_input,
                camera::fly.run_if(walk::flying),
                sky::drive_lights,
                sky::drive_exposure,
                stars::drive,
                log_scene,
                request_blocks,
                refresh_mask,
                update_hud,
                pulse_magma,
            ),
        )
        .add_systems(
            Update,
            (
                walk::toggle,
                aim_blob_shadows,
                (poll_units, draw_units).chain(),
                (walk::receive, walk::walk).chain().run_if(walk::walking),
            ),
        )
        .add_plugins(report_culling)
        .run();
}

/// Says in the log whether this device can actually do the occlusion culling
/// the camera asks for: the two-phase pass needs GPU preprocessing with
/// culling, and Bevy quietly ignores `OcclusionCulling` where that is missing.
fn report_culling(app: &mut App) {
    let Some(render) = app.get_sub_app_mut(RenderApp) else { return };
    render.add_systems(
        RenderStartup,
        |support: Res<GpuPreprocessingSupport>| {
            info!(
                "GPU preprocessing {}; occlusion culling {}",
                if support.is_available() { "available" } else { "unavailable" },
                if !support.is_culling_supported() {
                    "unsupported on this device"
                } else if occlusion_culling() {
                    "on"
                } else {
                    "off (DWARF_EYE_OCCLUSION=0)"
                }
            );
        },
    );
}

#[derive(Resource)]
struct ViewSettings {
    z_ceiling: i32,
    show_hidden: bool,
    /// Set once the first map position arrives, so the camera starts on the map.
    placed: bool,
    /// The player's level at connect, where a cut plane starts from.
    player_z: i32,
}

impl Default for ViewSettings {
    fn default() -> Self {
        // Adventure mode leaves nearly the whole map undiscovered, so drawing
        // only what the player has seen shows almost nothing. Start revealed.
        Self { z_ceiling: i32::MAX, show_hidden: true, placed: false, player_z: 0 }
    }
}

impl ViewSettings {
    fn mesh_options(&self) -> MeshOptions {
        MeshOptions { z_ceiling: self.z_ceiling, show_hidden: self.show_hidden }
    }
}

/// Where the near canopy band ends, in tiles. Every later hand-off follows from
/// it (`canopy::Band::edge`).
///
/// Recomputed from the camera's lens and the window's height, since both
/// decide how many pixels a leaf voxel covers.
#[derive(Resource, Default)]
struct Bands {
    near: f32,
}

/// How much of the way to the neighbouring hand-off a crossfade reaches, in
/// log distance, either side of the edge.
///
/// The blend is a screen-space dither, which reads as a soft fade only if the
/// camera spends real distance inside it; straddling the edge rather than
/// starting at it means a band is already going before the one behind it is
/// gone. Under a half, so two neighbouring fades can never overlap however
/// close together their edges fall — Bevy requires a range's start margin to
/// end before its end margin begins.
const CROSSFADE: f32 = 0.45;

/// Where the last band stops: the camera's own far plane, which no chunk
/// retention ever reaches.
const BAND_FAR: f32 = 40000.0;

/// Where one band hands over to the next, in tiles, nearest first.
///
/// One edge per handover, each from the band's own leaf voxel
/// (`canopy::Band::edge`), so `canopy::BANDS` and this list grow together: a
/// coarser stage is one more entry there and nothing here. The last band runs
/// to the far plane and has no edge.
fn band_edges(near: f32) -> Vec<f32> {
    BANDS[..BANDS.len() - 1].iter().map(|band| band.edge(near)).collect()
}

/// The distances a hand-off dithers across, one per edge.
///
/// The fade straddles its edge and reaches [`CROSSFADE`] of the way to
/// whichever neighbouring edge is nearer, measured as a ratio, so a hand-off
/// gets as much room as its own gap allows and no more: the closer two bands
/// stand, the shorter their blend, and none of them ever runs into the next.
fn band_fades(edges: &[f32]) -> Vec<std::ops::Range<f32>> {
    edges
        .iter()
        .enumerate()
        .map(|(i, &at)| {
            let below = (i > 0).then(|| at / edges[i - 1]);
            let above = edges.get(i + 1).map(|&next| next / at);
            let gap = match (below, above) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                // One edge and nothing to crowd it: a fixed, generous blend.
                (None, None) => 2.0,
            };
            let reach = gap.powf(CROSSFADE);
            at / reach..at * reach
        })
        .collect()
}

/// One range per band, nearest first. Each band's end margin is the next one's
/// start margin, which is what Bevy crossfades across; the last runs to the far
/// plane.
fn band_ranges(near: f32) -> Vec<VisibilityRange> {
    let edges = band_edges(near);
    let fades = band_fades(&edges);
    let fade = |stage: usize| fades[stage].clone();
    (0..=edges.len())
        .map(|stage| VisibilityRange {
            start_margin: if stage == 0 { 0.0..0.0 } else { fade(stage - 1) },
            end_margin: if stage < edges.len() { fade(stage) } else { BAND_FAR..BAND_FAR },
            // Chunk meshes hold world-space vertices at an identity transform,
            // so the range has to measure from the mesh's own bounds, not its
            // origin.
            use_aabb: true,
        })
        .collect()
}

/// Which band a canopy entity belongs to, by its place in `canopy::BANDS`, so a
/// resized window can rewrite its range.
#[derive(Component, Clone, Copy, PartialEq)]
struct CanopyBand(usize);

/// Which stage of the far band's tree chain an instance is, by its place in
/// `horizon::scatter::STAGES`.
#[derive(Component, Clone, Copy, PartialEq)]
struct HorizonStage(usize);

/// How far the sun's shadow cascades reach, mirroring Bevy's own default
/// `CascadeShadowConfig`. Nothing inside this is allowed to be shadowless, so
/// it is the floor on where the box stage — the one stage that casts no shadow
/// — may begin.
const SHADOW_DISTANCE: f32 = 150.0;

/// How far the grown stage runs, as a multiple of the canopy's near band.
///
/// The projected-size rule would put it at four times that — a one-tile leaf
/// voxel is two pixels out to four times where a quarter-tile one is — and the
/// triangle budget will not carry it: a grown far tree is about eight hundred
/// triangles and the count goes with the square of the reach. Half again is
/// where a box crown starts reading as a box, which is what this stage exists
/// to push back.
const GROWN_REACH: f32 = 1.5;

/// How much of a hand-off the far band's tree stages dither across. Wider than
/// the canopy's, because the shapes either side differ more: a grown tree into
/// a handful of boxes wants a long fade, and there is no cutout to pay for.
const HORIZON_CROSSFADE: f32 = 0.35;

/// One range per stage of the far band's tree chain, nearest first.
///
/// Measured from the camera to the tree, not from the window's centre: a
/// region-sourced tree can stand a few tiles away, and the old rule drew it as
/// a box the size of a house. The grown stage ends where the canopy's near
/// band does, since it is the same growth at the same resolution; the crown
/// stage runs three times as far, and never stops short of the shadow
/// cascades, so the one stage that casts no shadow is wholly outside them.
fn horizon_ranges(near: f32) -> Vec<VisibilityRange> {
    let edges = [near * GROWN_REACH, (near * 3.0).max(SHADOW_DISTANCE)];
    let fade = |at: f32| at..at * (1.0 + HORIZON_CROSSFADE);
    (0..=edges.len())
        .map(|stage| VisibilityRange {
            start_margin: if stage == 0 { 0.0..0.0 } else { fade(edges[stage - 1]) },
            end_margin: match edges.get(stage) {
                Some(&at) => fade(at),
                None => BAND_FAR..BAND_FAR,
            },
            // A crown is one mesh at one transform, so its bounds are where it
            // stands: the measure is the camera's distance to that tree.
            use_aabb: true,
        })
        .collect()
}

/// A blob shadow: the ground shadow of a tree past the cascades, as one quad.
///
/// Beyond `SHADOW_DISTANCE` the shadow map has nothing left to resolve a tree
/// with, and a far band with no shadows at all reads as flat. One dark quad on
/// the ground per instance costs two triangles and re-aims when the sun moves.
#[derive(Component, Clone, Copy)]
struct BlobShadow {
    pos: [f32; 3],
    /// The crown's own half-width in tiles, after the instance's scale.
    radius: f32,
    /// How tall the tree stands, which is what the sun's slant multiplies.
    height: f32,
}

/// Where the blob sits above the ground it lies on, and how far it may stretch
/// as a multiple of the crown's width. Uncapped, a sun near the horizon would
/// throw a shadow across half the map.
const BLOB_LIFT: f32 = 0.06;
const BLOB_MAX_STRETCH: f32 = 4.0;

/// The direction the sun's light travels, kept so a blob spawned between two
/// aiming passes still points the right way.
#[derive(Resource, Clone, Copy)]
struct SunAim(Vec3);

impl Default for SunAim {
    fn default() -> Self {
        Self(Vec3::NEG_Y)
    }
}

/// The unit quad a blob shadow is drawn with, in the ground plane.
fn blob_quad() -> MeshData {
    let mut mesh = MeshData::default();
    mesh.push_quad(
        [[-0.5, 0.0, -0.5], [-0.5, 0.0, 0.5], [0.5, 0.0, 0.5], [0.5, 0.0, -0.5]],
        [0.0, 1.0, 0.0],
        [1.0, 1.0, 1.0, 1.0],
    );
    mesh
}

/// Lays one blob on the ground under its tree, turned to the sun's azimuth and
/// stretched along it by the tree's height over the tangent of the sun's
/// elevation.
fn blob_transform(shadow: &BlobShadow, aim: Vec3) -> Transform {
    let width = shadow.radius * 2.0;
    let flat = Vec2::new(aim.x, aim.z);
    let centre = Vec3::new(shadow.pos[0], shadow.pos[1] + BLOB_LIFT, shadow.pos[2]);
    let overhead = Transform::from_translation(centre).with_scale(Vec3::new(width, 1.0, width));
    if aim.y >= -0.02 || flat.length_squared() < 1e-8 {
        return overhead;
    }
    let dir = flat.normalize();
    let cast = (shadow.height * flat.length() / -aim.y).min(width * BLOB_MAX_STRETCH);
    Transform::from_translation(centre + Vec3::new(dir.x, 0.0, dir.y) * (cast * 0.5))
        .with_rotation(Quat::from_rotation_y(dir.x.atan2(dir.y)))
        .with_scale(Vec3::new(width, 1.0, width + cast))
}

/// Turns the blob shadows when the sun has moved more than a couple of degrees,
/// and fades them out as it sets.
fn aim_blob_shadows(
    sun: Query<&Transform, (With<sky::Sun>, Without<BlobShadow>)>,
    mut blobs: Query<(&BlobShadow, &mut Transform)>,
    mut aim: ResMut<SunAim>,
    mut materials: ResMut<Assets<TerrainMat>>,
    blob_material: Res<BlobShadowMaterial>,
) {
    let Ok(transform) = sun.single() else { return };
    let now = transform.forward().as_vec3().normalize_or(Vec3::NEG_Y);
    // Two degrees, so a slow noon does not rewrite ten thousand transforms a
    // frame and a sunset still swings the shadows visibly.
    if now.dot(aim.0) > 2.0f32.to_radians().cos() {
        return;
    }
    aim.0 = now;
    for (shadow, mut transform) in &mut blobs {
        *transform = blob_transform(shadow, now);
    }
    if let Some(mut m) = materials.get_mut(&blob_material.0) {
        // Nothing casts a shadow once the sun is down.
        let alpha = BLOB_ALPHA * ((-now.y - 0.02) / 0.12).clamp(0.0, 1.0);
        m.base.base_color.set_alpha(alpha);
    }
}

/// How dark a blob is at its strongest.
const BLOB_ALPHA: f32 = 0.35;

/// Everything spawning one horizon needs, in one parameter: a system may hold
/// only so many, and `drain_worker` was already at the limit.
#[derive(SystemParam)]
struct HorizonSpawn<'w, 's> {
    /// What the last horizon left behind, to despawn before the next lands.
    entities: Query<'w, 's, Entity, With<Horizon>>,
    ground: Res<'w, HorizonMaterial>,
    canopy: Res<'w, HorizonCanopyMaterial>,
    blob: Res<'w, BlobShadowMaterial>,
    bands: Res<'w, Bands>,
    aim: Res<'w, SunAim>,
}

fn occlusion_culling() -> bool {
    std::env::var("DWARF_EYE_OCCLUSION").map(|v| v != "0").unwrap_or(true)
}

/// Everyone on the map, and the two censuses the drawn positions sit between.
///
/// The poll is four a second and DF steps faster than that, so a capsule
/// placed at the last census would jump. Each unit is eased from where it was
/// to where it is over one poll period, which is the whole of the animation.
#[derive(Resource, Default)]
struct Crowd {
    previous: HashMap<i32, units::Unit>,
    current: HashMap<i32, units::Unit>,
    /// Seconds since the last census arrived.
    since: f32,
    entities: HashMap<i32, Entity>,
    /// One material per colour, and a brighter one for the adventurer.
    paint: HashMap<([u8; 3], bool), Handle<StandardMaterial>>,
    /// The capsule every unit is a scaled copy of.
    capsule: Option<Handle<Mesh>>,
}

/// Radius and half-length of the shared capsule, so a scale of one is a
/// creature one tile wide and two tall.
const CAPSULE_RADIUS: f32 = 0.5;
const CAPSULE_LENGTH: f32 = 1.0;

#[derive(Component)]
struct UnitTag(#[allow(dead_code)] i32);

/// Drains the census and starts a new easing window.
fn poll_units(bridge: NonSend<Bridge>, mut crowd: ResMut<Crowd>, time: Res<Time>) {
    let mut arrived = false;
    while let Ok(census) = bridge.units.rx.try_recv() {
        let next: HashMap<i32, units::Unit> = census.units.into_iter().map(|u| (u.id, u)).collect();
        crowd.previous = std::mem::replace(&mut crowd.current, next);
        arrived = true;
    }
    if arrived {
        crowd.since = 0.0;
    } else {
        crowd.since += time.delta_secs();
    }
}

/// Spawns, moves and retires the capsules.
///
/// The factory says a unit is drawn as a capsule and DF says how big it is;
/// nothing here decides either. A creature above the cut plane is hidden the
/// same way the tiles above it are.
fn draw_units(
    mut commands: Commands,
    mut crowd: ResMut<Crowd>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut transforms: Query<(&mut Transform, &mut Visibility), With<UnitTag>>,
    settings: Res<ViewSettings>,
    bridge: NonSend<Bridge>,
) {
    let Some(&origin) = bridge.origin.get() else { return };
    if crowd.current.is_empty() && crowd.entities.is_empty() {
        return;
    }
    let capsule = match crowd.capsule.clone() {
        Some(handle) => handle,
        None => {
            let handle = meshes.add(Capsule3d::new(CAPSULE_RADIUS, CAPSULE_LENGTH));
            crowd.capsule = Some(handle.clone());
            handle
        }
    };

    // Where between the two censuses we are. A unit that has just appeared has
    // no previous position and simply stands where it is.
    let t = (crowd.since / units::POLL.as_secs_f32()).clamp(0.0, 1.0);

    let live: Vec<units::Unit> = crowd.current.values().copied().collect();
    for unit in live {
        let was = crowd.previous.get(&unit.id).copied().unwrap_or(unit);
        // A jump of more than a few tiles is a teleport or a re-centring, not
        // a step; easing through it would be a long slide through the ground.
        let far = (was.at.0 - unit.at.0).abs().max((was.at.1 - unit.at.1).abs()) > 4.0
            || (was.at.2 - unit.at.2).abs() > 1.0;
        let at = if far {
            unit.at
        } else {
            (
                was.at.0 + (unit.at.0 - was.at.0) * t,
                was.at.1 + (unit.at.1 - was.at.1) * t,
                was.at.2 + (unit.at.2 - was.at.2) * t,
            )
        };
        // DF is x-east / y-south / z-up; Bevy is y-up, so y and z swap.
        let transform = Transform {
            translation: Vec3::new(
                at.0 - origin.0 as f32,
                (at.2 - origin.2 as f32) * Z_SCALE + unit.height * 0.5 * Z_SCALE,
                at.1 - origin.1 as f32,
            ),
            scale: Vec3::new(
                unit.radius * 2.0,
                unit.height / (CAPSULE_LENGTH + CAPSULE_RADIUS * 2.0),
                unit.radius * 2.0,
            ),
            ..default()
        };
        let seen = if (at.2 - origin.2 as f32) as i32 <= settings.z_ceiling {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };

        if let Some(&entity) = crowd.entities.get(&unit.id) {
            if let Ok((mut existing, mut visible)) = transforms.get_mut(entity) {
                *existing = transform;
                *visible = seen;
                continue;
            }
        }
        let key = (unit.color, unit.adventurer);
        let paint = match crowd.paint.get(&key) {
            Some(handle) => handle.clone(),
            None => {
                let base = Color::srgb_u8(unit.color[0], unit.color[1], unit.color[2]);
                let handle = materials.add(StandardMaterial {
                    base_color: base,
                    perceptual_roughness: 0.85,
                    // The player's own character, lit from inside so it is
                    // findable in a crowd of livestock.
                    emissive: if unit.adventurer {
                        LinearRgba::rgb(0.5, 0.45, 0.15)
                    } else {
                        LinearRgba::BLACK
                    },
                    ..default()
                });
                crowd.paint.insert(key, handle.clone());
                handle
            }
        };
        let entity = commands
            .spawn((
                Mesh3d(capsule.clone()),
                MeshMaterial3d(paint),
                transform,
                seen,
                UnitTag(unit.id),
            ))
            .id();
        crowd.entities.insert(unit.id, entity);
    }

    let gone: Vec<i32> = crowd
        .entities
        .keys()
        .filter(|id| !crowd.current.contains_key(id))
        .copied()
        .collect();
    for id in gone {
        if let Some(entity) = crowd.entities.remove(&id) {
            commands.entity(entity).despawn();
        }
    }
}

#[derive(Resource, Default)]
struct ChunkEntities(HashMap<ChunkKey, Spawned>);

/// What one chunk put on the GPU: its terrain, one entity per canopy material
/// per band, and what each band costs.
///
/// Only one band draws at a time — `VisibilityRange` swaps them — so the band
/// triangle counts are alternatives, not a sum.
struct Spawned {
    terrain: Option<Entity>,
    water: Option<Entity>,
    magma: Option<Entity>,
    /// One entry per canopy band, nearest first.
    bands: Vec<Stage>,
    /// Terrain and its liquids, which every band draws.
    base: usize,
    /// Middle of the chunk, for saying which band is drawing.
    centre: Vec3,
}

/// One chunk's crown at one detail band: its entities and what they cost.
struct Stage {
    entities: [Option<Entity>; 4],
    triangles: usize,
}

impl Spawned {
    fn entities(&self) -> impl Iterator<Item = Entity> + '_ {
        [self.terrain, self.water, self.magma]
            .into_iter()
            .chain(self.bands.iter().flat_map(|s| s.entities))
            .flatten()
    }

    fn held(&self) -> usize {
        self.base + self.bands.iter().map(|s| s.triangles).sum::<usize>()
    }

    /// What this chunk draws with the camera here: one band, never two.
    fn drawn(&self, eye: Vec3, edges: &[f32]) -> usize {
        let away = self.centre.distance(eye);
        let stage = edges.iter().position(|&edge| away < edge).unwrap_or(edges.len());
        self.base + self.bands.get(stage).map(|s| s.triangles).unwrap_or(0)
    }
}

/// What the loaded chunks draw from where the camera stands.
///
/// The band each chunk is in is read off its middle, which is what Bevy's own
/// range check does with the mesh's bounds.
fn drawn_triangles(entities: &ChunkEntities, near: f32, eye: Vec3) -> usize {
    let edges = band_edges(near);
    entities.0.values().map(|s| s.drawn(eye, &edges)).sum()
}

/// Meshes that have arrived from the worker and not yet reached the GPU.
/// Uploads are budgeted per frame, nearest to the camera first, so a burst of
/// chunks never stalls a frame. A later mesh for the same chunk replaces an
/// earlier one still waiting.
#[derive(Resource, Default)]
struct PendingChunks(HashMap<ChunkKey, (MeshData, Vec<CanopyMeshes>)>);

/// Chunk meshes uploaded per frame.
const UPLOAD_BUDGET: usize = 24;

/// Set when the cut plane moves, so the next frame reloads the new z range.
#[derive(Resource, Default, Deref, DerefMut)]
struct NeedsFetch(bool);

#[derive(Resource, Default)]
struct Status {
    world: String,
    detail: String,
    triangles: usize,
}

#[derive(Component)]
struct Hud;

/// The coarse terrain beyond the loaded map.
#[derive(Component)]
struct Horizon;

#[derive(Component)]
struct ChunkTag(#[allow(dead_code)] ChunkKey);

/// Reused by every chunk, since colour lives in the vertex data.
#[derive(Resource)]
pub struct TerrainMaterial(pub Handle<TerrainMat>);

/// The horizon's copy of the terrain material: the same shading, flagged to
/// yield wherever a fine chunk is loaded.
#[derive(Resource)]
pub struct HorizonMaterial(pub Handle<TerrainMat>);

/// The far band's trees: the canopy's opaque leaf shading with the canopy's own
/// leaf texture on it, and the horizon's block mask over it.
///
/// A crown out here is one mesh at one transform per tree, so its UVs cannot be
/// world-space in the vertex buffer the way a chunk's are — an instance scales
/// them with everything else. The shader derives them from the world position
/// instead, which puts the same texel density on a box crown as on the near
/// canopy whatever size the instance is (`cloud_shadow.wgsl:horizon_leaf`).
#[derive(Resource)]
pub struct HorizonCanopyMaterial {
    /// The grown stage: the canopy's own leaf shading, sky term and all, since
    /// a grown tree's faces carry no shading of their own.
    pub grown: Handle<TerrainMat>,
    /// The two box stages. `crown.rs` already bakes a lit top and darker sides
    /// into their vertex colours, and the sky term over that takes a box's
    /// sides to nearly black.
    pub boxes: Handle<TerrainMat>,
}

/// The blob shadows the box stage casts in place of a shadow-map shadow.
#[derive(Resource)]
pub struct BlobShadowMaterial(pub Handle<TerrainMat>);

/// Water surfaces: blended rather than masked, so the ground shows through the
/// shallows, and glossy enough to catch the sun. Its own material keeps the
/// terrain opaque, which is what lets the sorted transparent pass work at all.
#[derive(Resource)]
pub struct WaterMaterial(pub Handle<TerrainMat>);

/// Magma surfaces: blended like water, and emissive, so a magma sea lights
/// itself and blooms without a light per tile. `pulse_magma` breathes the
/// emissive strength; the colour and how opaque it is are vertex data
/// (`dwarf_eye_world::magma`).
#[derive(Resource)]
pub struct MagmaMaterial(pub Handle<TerrainMat>);

/// How bright magma glows, and how far the pulse takes that either way. The
/// scale is the renderer's own: the adventurer is lit from inside at 0.5, so
/// this is a surface several times brighter than anything else underground,
/// which is what makes it bloom.
const MAGMA_GLOW: f32 = 3.0;
const MAGMA_PULSE: f32 = 0.28;

/// The glow of magma, breathing.
///
/// Three slow sines with periods that share no factor read as a wandering
/// noise rather than a heartbeat: a sea brightens and dims over ten or twenty
/// seconds and never twice the same way. One material for the whole world, so
/// this is one asset write a frame.
fn pulse_magma(
    time: Res<Time>,
    magma: Res<MagmaMaterial>,
    mut materials: ResMut<Assets<TerrainMat>>,
) {
    let t = time.elapsed_secs();
    let wave = (t * 0.21).sin() * 0.5 + (t * 0.13 + 1.7).sin() * 0.3 + (t * 0.37 + 4.2).sin() * 0.2;
    let glow = MAGMA_GLOW * (1.0 + MAGMA_PULSE * wave);
    if let Some(mut material) = materials.get_mut(&magma.0) {
        material.base.emissive = LinearRgba::rgb(glow, glow * 0.34, glow * 0.06);
    }
}

/// Tree crowns. Their own material so they can be shaded as leaves rather than
/// as stone, and so cloud shadows still reach them: `clouds::bake_shadow`
/// updates every terrain material asset, and this is one of them. These four
/// are the only ones that carry the canopy sky term, which is what leaves a
/// crown with a lit side and a shaded one.
/// One material per canopy surface: bark, broadleaf cutout, needle cutout, and
/// the leaflet strip weeping strands hang from.
#[derive(Resource)]
pub struct CanopyMaterials {
    pub bark: Handle<TerrainMat>,
    pub broadleaf: Handle<TerrainMat>,
    pub needle: Handle<TerrainMat>,
    pub streamers: Handle<TerrainMat>,
    /// The far band's whole crown, bark and all: the leaf shading with no
    /// cutout, so it draws opaque, writes depth in the prepass and never
    /// discards.
    pub leaf: Handle<TerrainMat>,
}

impl CanopyMaterials {
    /// One material per mesh, in the order [`CanopyMeshes`] hands them over,
    /// as the band asks for them.
    fn each(&self, band: Band) -> [Handle<TerrainMat>; 4] {
        band.coats().map(|coat| match coat {
            Coat::Bark | Coat::Cutout(Surface::Bark) => self.bark.clone(),
            Coat::Cutout(Surface::Needle) => self.needle.clone(),
            Coat::Cutout(Surface::Broadleaf) => self.broadleaf.clone(),
            Coat::Strip => self.streamers.clone(),
            Coat::Leaf => self.leaf.clone(),
        })
    }
}

/// One texel per block, marking where fine chunks reach the ground. The worker
/// decides which blocks qualify; this only paints them.
#[derive(Resource)]
pub struct BlockMask {
    image: Handle<Image>,
    blocks: Vec<(i32, i32)>,
    dirty: bool,
}

fn setup(
    mut commands: Commands,
    mut materials: ResMut<Assets<TerrainMat>>,
    mut mediums: ResMut<Assets<ScatteringMedium>>,
    mut images: ResMut<Assets<Image>>,
) {
    // A physically-based atmosphere, so the sky colour follows the sun rather
    // than being painted on.
    commands.spawn(Atmosphere::earth(mediums.add(ScatteringMedium::earth(256, 256))));

    let camera = commands.spawn((
        Camera3d::default(),
        // Far enough to take in the outer terrain.
        Projection::Perspective(PerspectiveProjection { far: 40000.0, ..default() }),
        Transform::from_xyz(0.0, 40.0, 40.0).looking_at(Vec3::ZERO, Vec3::Y),
        AtmosphereSettings {
            // Raymarching integrates the sky directly, which removes the seams
            // the lookup textures leave and sharpens volumetric shadows.
            rendering_method: AtmosphereMode::Raymarched,
            sky_max_samples: 32,
            ..default()
        },
        // RAW_SUNLIGHT is pre-scattering, so the exposure has to be raised to
        // bring the scene back into range. The clock drives it from here on:
        // day sits where it always did, night opens five stops.
        Exposure { ev100: sky::ev100_override().unwrap_or(sky::DAY_EV100) },
        Tonemapping::AcesFitted,
        // A dark sky gradient bands badly at 8 bits; dithering breaks up the
        // steps that otherwise read as seams.
        DebandDither::Enabled,
        Bloom::NATURAL,
        // Sky-driven ambient: the sky lights the scene, which is what makes
        // dusk read as dusk. Raised above the physical default because there is
        // no bounce lighting to fill the shadows.
        AtmosphereEnvironmentMapLight { intensity: 2.6, size: UVec2::splat(1024), ..default() },
        // The cloud volume reads scene depth to stop its march at terrain.
        DepthPrepass,
        FlyCamera::default(),
    )).id();
    // Two-phase GPU occlusion culling, which rides on that depth prepass: a
    // forest hides most of itself behind its own front row, and this drops
    // those meshes before their vertices are transformed.
    // DWARF_EYE_OCCLUSION=0 leaves it off, for measuring what it is worth.
    if occlusion_culling() {
        commands.entity(camera).insert(OcclusionCulling);
    }

    commands.spawn((
        DirectionalLight {
            illuminance: lux::RAW_SUNLIGHT,
            shadow_maps_enabled: true,
            ..default()
        },
        // A real 32-arcminute disk, so it reads as the sun rather than a glare.
        SunDisk::EARTH,
        Transform::from_xyz(60.0, 120.0, 40.0).looking_at(Vec3::ZERO, Vec3::Y),
        sky::Sun,
    ));

    // The moon: dim, cool, on the far side of the sun's arc, and shadowless.
    // A second set of cascades would cost a whole shadow pass for light the
    // scene barely resolves.
    commands.spawn((
        DirectionalLight {
            illuminance: sky::MOON_ILLUMINANCE,
            color: sky::MOON_COLOUR,
            shadow_maps_enabled: false,
            ..default()
        },
        // The moon's disk is the sun's angular size; only its brightness differs.
        SunDisk { angular_size: SunDisk::EARTH.angular_size, intensity: sky::MOON_DISK_INTENSITY },
        Transform::from_xyz(-60.0, 120.0, -40.0).looking_at(Vec3::ZERO, Vec3::Y),
        sky::Moon,
    ));

    let mask = images.add(empty_mask());
    let shadow = images.add(clouds::flat_shadow(255));
    let terrain = |horizon: f32| TerrainMat {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 0.92,
            reflectance: 0.03,
            alpha_mode: AlphaMode::Mask(0.5),
            ..default()
        },
        extension: CloudShadow {
            uniform: ShadowUniform {
                horizon,
                mask_origin: Vec2::splat(-(shadow::MASK_BLOCKS as f32) / 2.0),
                ..default()
            },
            // Always bound: an unbound texture leaves the binding out of the
            // pipeline layout entirely.
            map: shadow.clone(),
            mask: mask.clone(),
        },
    };
    commands.insert_resource(TerrainMaterial(materials.add(terrain(0.0))));
    commands.insert_resource(HorizonMaterial(materials.add(terrain(1.0))));

    // Blend, which in Bevy also stops the surface writing depth, so what is
    // under the water still draws. Both sides, because the camera walks into
    // the pool. The colour and how opaque it is come from the vertex data,
    // which is where the depth of the water is worked out
    // (`dwarf_eye_world::water`).
    let mut water = terrain(0.0);
    water.base.alpha_mode = AlphaMode::Blend;
    water.base.double_sided = true;
    water.base.cull_mode = None;
    water.base.perceptual_roughness = 0.1;
    water.base.reflectance = 0.5;
    commands.insert_resource(WaterMaterial(materials.add(water)));

    // Magma: the same blended surface, lighting itself. Rougher and duller than
    // water, because a molten surface is a skin rather than a mirror, and no
    // point light anywhere — the emissive is the glow, and the bloom pass turns
    // it into light spilling onto the rock beside it.
    let mut magma = terrain(0.0);
    magma.base.alpha_mode = AlphaMode::Blend;
    magma.base.double_sided = true;
    magma.base.cull_mode = None;
    magma.base.perceptual_roughness = 0.65;
    magma.base.reflectance = 0.08;
    magma.base.emissive = LinearRgba::rgb(MAGMA_GLOW, MAGMA_GLOW * 0.34, MAGMA_GLOW * 0.06);
    commands.insert_resource(MagmaMaterial(materials.add(magma)));

    // Leaf faces carry a procedural cutout at Dwarf Fortress's own texel
    // density, which is what lets sky through the crown and dapples the shadow
    // under it. Holes mean back faces show, so the draw is double sided.
    let texels = tree_texels();
    // The species' own openness is per tree; these two cutouts are the fallback
    // the whole world shares, one leafy and one needled.
    let broadleaf = images.add(tree_texture(trees::texture::leaf_cutout(false, 0.34, texels)));
    let needle = images.add(tree_texture(trees::texture::leaf_cutout(true, 0.5, texels)));
    let strip = images.add(tree_texture(trees::texture::streamer_strip(texels)));
    let bark_texture = images.add(tree_texture(trees::texture::bark(texels)));

    let sky = canopy_sky();
    let cutout = |texture: Handle<Image>| {
        let mut leaves = terrain(0.0);
        leaves.extension.uniform.canopy = sky;
        leaves.base.base_color_texture = Some(texture);
        leaves.base.alpha_mode = AlphaMode::Mask(0.5);
        leaves.base.double_sided = true;
        leaves.base.cull_mode = None;
        // A leaf is a dull, matt surface: any sheen on it reads as wet plastic
        // and washes the shaded side back out.
        leaves.base.perceptual_roughness = 0.97;
        leaves.base.reflectance = 0.02;
        // Enough for a backlit leaf to glow when the sun is behind it, and no
        // more: transmission takes the sky's fill from every direction too,
        // which is what lit the far side of every crown.
        leaves.base.diffuse_transmission = leaf_transmission();
        leaves.base.thickness = 0.12;
        leaves
    };
    let mut bark = terrain(0.0);
    bark.extension.uniform.canopy = sky;
    bark.base.base_color_texture = Some(bark_texture);
    bark.base.alpha_mode = AlphaMode::Opaque;
    bark.base.perceptual_roughness = 0.95;
    // The mid band's leaves: the cutout's shading without its mask, so a crown
    // a long way off is a solid crown rather than a masked one.
    let mut solid_leaf = cutout(broadleaf.clone());
    solid_leaf.base.base_color_texture = None;
    // The cutout's own texels are a light grey, around 224 of 255, so dropping
    // the texture would leave the mid band a shade brighter than the near one
    // right where the two meet.
    solid_leaf.base.base_color = Color::srgb(0.88, 0.88, 0.88);
    solid_leaf.base.alpha_mode = AlphaMode::Opaque;
    solid_leaf.base.double_sided = false;
    solid_leaf.base.cull_mode = Some(bevy::render::render_resource::Face::Back);
    // The far band's crowns: the same opaque leaf shading, but wearing the leaf
    // cutout's own texels rather than one flat grey, so a box crown a hundred
    // tiles off still has foliage grain on it. Mipped and linearly minified,
    // because at that range the near band's nearest sampling is sparkle.
    let horizon_leaf = images.add(texture::tiled_image(
        texels,
        texels,
        trees::texture::leaf_cutout(false, 0.34, texels).rgba,
    ));
    let mut far_leaf = cutout(horizon_leaf);
    far_leaf.base.alpha_mode = AlphaMode::Opaque;
    far_leaf.base.double_sided = false;
    far_leaf.base.cull_mode = Some(bevy::render::render_resource::Face::Back);
    far_leaf.base.diffuse_transmission = 0.0;
    // 2: a horizon material whose UVs the shader derives from world position.
    far_leaf.extension.uniform.horizon = 2.0;
    // A canonical crown is boxes with a lit top and darker sides already in
    // their vertex colours; the sky term over that dims the sides twice and
    // leaves a far crown reading as a black block.
    let mut far_boxes = far_leaf.clone();
    far_boxes.extension.uniform.canopy = 0.0;
    commands.insert_resource(HorizonCanopyMaterial {
        grown: materials.add(far_leaf),
        boxes: materials.add(far_boxes),
    });

    let mut blob = terrain(1.0);
    blob.base.base_color = Color::srgba(0.05, 0.06, 0.04, BLOB_ALPHA);
    blob.base.alpha_mode = AlphaMode::Blend;
    blob.base.perceptual_roughness = 1.0;
    blob.base.reflectance = 0.0;
    commands.insert_resource(BlobShadowMaterial(materials.add(blob)));

    commands.insert_resource(CanopyMaterials {
        bark: materials.add(bark),
        broadleaf: materials.add(cutout(broadleaf)),
        needle: materials.add(cutout(needle)),
        streamers: materials.add(cutout(strip)),
        leaf: materials.add(solid_leaf),
    });
    commands.insert_resource(BlockMask { image: mask, blocks: Vec::new(), dirty: false });

    commands.spawn((
        Text::new("connecting to DFHack…"),
        TextFont { font_size: FontSize::Px(13.0), ..default() },
        Node { position_type: PositionType::Absolute, top: px(10), left: px(12), ..default() },
        // DWARF_EYE_HUD=off leaves the overlay out of the picture, for a shot
        // that is meant to show the world rather than the instrument.
        match std::env::var("DWARF_EYE_HUD").as_deref() {
            Ok("off") | Ok("0") | Ok("hidden") => Visibility::Hidden,
            _ => Visibility::Inherited,
        },
        Hud,
    ));
}

/// The three resources one weather reading lands in, gathered so the drain
/// system stays inside Bevy's limit on system parameters.
#[derive(SystemParam)]
struct WeatherState<'w> {
    weather: ResMut<'w, Weather>,
    precip: ResMut<'w, precipitation::Precipitation>,
    cover: ResMut<'w, precipitation::SnowCover>,
}

/// Pulls everything the worker has produced since the last frame.
fn drain_worker(
    mut commands: Commands,
    bridge: NonSend<Bridge>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<TerrainMat>>,
    material: Res<TerrainMaterial>,
    mut status: ResMut<Status>,
    mut settings: ResMut<ViewSettings>,
    mut clock: ResMut<Clock>,
    mut sky: WeatherState,
    mut ground: ResMut<clouds::GroundLevel>,
    mut camera: Query<(&mut Transform, &mut FlyCamera)>,
    far: HorizonSpawn,
    mut mask: ResMut<BlockMask>,
    mut pending: ResMut<PendingChunks>,
) {
    for event in bridge.rx.try_iter() {
        match event {
            Event::Weather(report) => {
                *sky.weather = forced_weather().unwrap_or(report.sky);
                sky.precip.report(report.precip, report.intensity, report.outdoors);
                sky.cover.target =
                    precipitation::SnowCover::override_from_env().unwrap_or(report.snow);
                // DF's own moon, which the protocol never sends. Absent, the
                // clock's 28-day derivation stays in charge.
                if report.moon.is_some() {
                    clock.moon = report.moon;
                }
            }
            Event::Clock { year, tick } => {
                // DWARF_EYE_HOUR pins the hour the view is lit at without
                // moving the game's own clock.
                *clock = Clock { year, tick, moon: clock.moon }.with_hour_override();
            }
            Event::Atlas { width, height, pixels } => {
                let handle = images.add(texture::atlas_image(width, height, pixels));
                if let Some(mut m) = materials.get_mut(&material.0) {
                    m.base.base_color_texture = Some(handle.clone());
                }
                // The coarse bands wear the same sprites, so the horizon reads
                // the same atlas; its own UVs name a cell rather than a point
                // and `cloud_shadow.wgsl` wraps the sprite across a slab.
                if let Some(mut m) = materials.get_mut(&far.ground.0) {
                    m.base.base_color_texture = Some(handle);
                }
            }
            Event::Connected { world_name, save, center, size } => {
                status.world = format!("{world_name} ({save})");
                status.detail = format!("map {} x {} x {} tiles", size.0, size.1, size.2);

                // Drop the camera just above and south of the player's position.
                if let Ok((mut transform, mut fly)) = camera.single_mut() {
                    capture::apply_view(&mut fly);
                    // DWARF_EYE_AIM=dx,dy,dz moves what the camera looks at, in
                    // DF tiles and levels off the player's position, for
                    // framing something the character is not standing in.
                    let aim = aim_offset();
                    let target = Vec3::new(
                        center.0 as f32 + aim.0,
                        (center.2 as f32 + aim.2) * Z_SCALE,
                        center.1 as f32 + aim.1,
                    );
                    // DWARF_EYE_CAM scales how far back the camera starts, for
                    // getting down among the tiles.
                    let back: f32 = std::env::var("DWARF_EYE_CAM")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1.0);
                    *transform =
                        Transform::from_translation(target + Vec3::new(0.0, 18.0, 28.0) * back)
                            .looking_at(target, Vec3::Y);
                }
                // No cut plane by default: the whole volume renders, so trees
                // are never sliced by the view. DWARF_EYE_Z_OFFSET starts one
                // relative to the player, for looking into the underground.
                settings.player_z = center.2;
                settings.z_ceiling = match std::env::var("DWARF_EYE_Z_OFFSET")
                    .ok()
                    .and_then(|v| v.parse::<i32>().ok())
                {
                    Some(offset) => center.2 + offset,
                    None => i32::MAX,
                };
                ground.0 = center.2 as f32 * Z_SCALE;
                settings.placed = true;
            }
            Event::Horizon(data) => {
                for entity in &far.entities {
                    commands.entity(entity).despawn();
                }
                let ranges = horizon_ranges(far.bands.near);
                let ground = data.mesh.triangle_count();
                let (trees, instances) = (data.instance_count(), data.entity_count());
                let per_stage: Vec<String> = HORIZON_STAGES
                    .iter()
                    .map(|s| format!("{:?} {}k", s, data.stage_triangles(*s) / 1000))
                    .collect();
                status.detail = format!(
                    "horizon: {ground} ground triangles, {trees} trees ({}), {instances} instances; \
                     fine map {:.2} canopy cover, {:.2} at the centre, {} clearings",
                    per_stage.join(" / "),
                    data.fine_density,
                    data.edge_density,
                    data.clearings,
                );
                commands.spawn((
                    Mesh3d(meshes.add(to_bevy_mesh(data.mesh))),
                    MeshMaterial3d(far.ground.0.clone()),
                    Transform::IDENTITY,
                    Horizon,
                ));
                // One mesh per species, growth variant and stage; one entity
                // per tree per stage. Bevy batches entities that share a mesh
                // and a material, so a whole forest costs a handful of draw
                // calls, and each entity's own `VisibilityRange` decides which
                // stage draws from the camera's distance to that tree.
                let blob = meshes.add(to_bevy_mesh(blob_quad()));
                for batch in data.crowns {
                    let stage = HORIZON_STAGES.iter().position(|s| *s == batch.stage).unwrap_or(0);
                    let range = ranges[stage].clone();
                    // The mesh's own size, so an instance is scaled to the
                    // height the scatter gave it and a blob is sized from the
                    // crown that casts it.
                    let unit = batch.mesh_height;
                    let radius = batch
                        .mesh
                        .positions
                        .iter()
                        .map(|p| p[0].abs().max(p[2].abs()))
                        .fold(0.5f32, f32::max);
                    let mesh = meshes.add(to_bevy_mesh(batch.mesh));
                    let coat = if batch.stage == TreeStage::Grown {
                        far.canopy.grown.clone()
                    } else {
                        far.canopy.boxes.clone()
                    };
                    for tree in &batch.instances {
                        let scale = tree.height / unit;
                        let mut entity = commands.spawn((
                            Mesh3d(mesh.clone()),
                            MeshMaterial3d(coat.clone()),
                            Transform::from_translation(Vec3::from(tree.pos))
                                .with_rotation(Quat::from_rotation_y(tree.yaw))
                                .with_scale(Vec3::splat(scale)),
                            range.clone(),
                            HorizonStage(stage),
                            Horizon,
                        ));
                        // Everything inside the sun's cascades casts: the two
                        // near stages are wholly inside them
                        // (`horizon_ranges` holds the box stage's near edge at
                        // or beyond `SHADOW_DISTANCE`), and the box stage is
                        // wholly outside, where a cascade would never have
                        // resolved it. There it casts a blob instead.
                        if batch.stage == TreeStage::Box {
                            entity.insert(NotShadowCaster);
                            commands.spawn((
                                Mesh3d(blob.clone()),
                                MeshMaterial3d(far.blob.0.clone()),
                                blob_transform(
                                    &BlobShadow {
                                        pos: tree.pos,
                                        radius: radius * scale,
                                        height: tree.height,
                                    },
                                    far.aim.0,
                                ),
                                range.clone(),
                                HorizonStage(stage),
                                BlobShadow {
                                    pos: tree.pos,
                                    radius: radius * scale,
                                    height: tree.height,
                                },
                                Horizon,
                                NotShadowCaster,
                            ));
                        }
                    }
                }
            }
            Event::Coverage(blocks) => {
                mask.blocks = blocks;
                mask.dirty = true;
            }
            Event::Chunks(batch) => {
                for (key, terrain, crowns) in batch {
                    pending.0.insert(key, (terrain, crowns));
                }
            }
            Event::Status(text) => status.detail = text,
            Event::Failed(err) => {
                status.detail = format!("DFHack error: {err}");
                error!("{err}");
            }
        }
    }
}

/// Sizes the canopy bands from the lens and the window, and rewrites the
/// ranges already on the GPU when either changes.
///
/// The rule is projected size, not distance: each band ends where its own leaf
/// voxel stops covering two pixels (`dwarf_eye_world::canopy::near_band` and
/// `Band::edge`), so a taller window or a longer lens pushes them all out.
/// `DWARF_EYE_LOD_NEAR` overrides the near edge, in blocks.
fn size_bands(
    mut bands: ResMut<Bands>,
    windows: Query<&Window>,
    camera: Query<&Projection, With<FlyCamera>>,
    mut ranged: Query<(&CanopyBand, &mut VisibilityRange), Without<HorizonStage>>,
    mut staged: Query<(&HorizonStage, &mut VisibilityRange), Without<CanopyBand>>,
) {
    let Ok(window) = windows.single() else { return };
    let fov = match camera.single() {
        Ok(Projection::Perspective(p)) => p.fov,
        _ => PerspectiveProjection::default().fov,
    };
    let near = dwarf_eye_world::canopy::near_band_override(fov, window.resolution.height());
    if (near - bands.near).abs() < 0.5 {
        return;
    }
    bands.near = near;
    let ranges = band_ranges(near);
    for (band, mut range) in &mut ranged {
        if let Some(wanted) = ranges.get(band.0) {
            *range = wanted.clone();
        }
    }
    let stages = horizon_ranges(near);
    for (stage, mut range) in &mut staged {
        if let Some(wanted) = stages.get(stage.0) {
            *range = wanted.clone();
        }
    }
    let edges = band_edges(near);
    let handovers: Vec<String> = band_fades(&edges)
        .iter()
        .zip(&edges)
        .map(|(fade, at)| format!("{at:.0} ({:.0}..{:.0})", fade.start, fade.end))
        .collect();
    info!(
        "canopy bands: near out to {near:.0} tiles ({:.1} blocks), hand-offs at {}, far beyond",
        near / BLOCK as f32,
        handovers.join(", ")
    );
}

/// Moves a budget of pending meshes onto the GPU, nearest the camera first.
fn upload_chunks(
    mut commands: Commands,
    mut pending: ResMut<PendingChunks>,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Res<TerrainMaterial>,
    water_material: Res<WaterMaterial>,
    magma_material: Res<MagmaMaterial>,
    canopy_materials: Res<CanopyMaterials>,
    mut entities: ResMut<ChunkEntities>,
    mut status: ResMut<Status>,
    bands: Res<Bands>,
    camera: Query<&Transform, With<FlyCamera>>,
) {
    if pending.0.is_empty() {
        return;
    }
    let eye = camera.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    let mut keys: Vec<ChunkKey> = pending.0.keys().copied().collect();
    let distance = |k: &ChunkKey| {
        let centre = Vec3::new(
            (k.0 * BLOCK + BLOCK / 2) as f32,
            k.2 as f32 * Z_SCALE,
            (k.1 * BLOCK + BLOCK / 2) as f32,
        );
        centre.distance_squared(eye)
    };
    keys.sort_by(|a, b| distance(a).total_cmp(&distance(b)));

    let ranges = band_ranges(bands.near);
    for key in keys.into_iter().take(UPLOAD_BUDGET) {
        let Some((mut data, crowns)) = pending.0.remove(&key) else { continue };
        // Water rides in with the terrain and splits off here: its own entity,
        // its own translucent material.
        let pool = data.take_water();
        let melt = data.take_magma();
        if let Some(old) = entities.0.remove(&key) {
            for entity in old.entities() {
                commands.entity(entity).despawn();
            }
        }
        if data.is_empty()
            && pool.is_empty()
            && melt.is_empty()
            && crowns.iter().all(CanopyMeshes::is_empty)
        {
            continue;
        }
        let base = data.triangle_count() + pool.triangle_count() + melt.triangle_count();
        let mut spawn = |mesh: MeshData, material: Handle<TerrainMat>, stage: Option<usize>| {
            (!mesh.is_empty()).then(|| {
                let mut entity = commands.spawn((
                    Mesh3d(meshes.add(to_bevy_mesh(mesh))),
                    MeshMaterial3d(material),
                    Transform::IDENTITY,
                    ChunkTag(key),
                ));
                // Terrain and water carry no range at all: they are drawn
                // wherever they are retained, and the bands beyond this one are
                // the heightfield's job, not theirs.
                if let Some(stage) = stage {
                    entity.insert((ranges[stage].clone(), CanopyBand(stage)));
                }
                entity.id()
            })
        };
        let terrain = spawn(data, material.0.clone(), None);
        let water = spawn(pool, water_material.0.clone(), None);
        let magma = spawn(melt, magma_material.0.clone(), None);
        let mut spawned_bands = Vec::with_capacity(crowns.len());
        for (stage, crown) in crowns.into_iter().enumerate() {
            let Some(&band) = BANDS.get(stage) else { continue };
            let triangles = crown.triangle_count();
            let meshes_of = [crown.bark, crown.broadleaf, crown.needle, crown.streamers];
            let mut entities = [None; 4];
            for (slot, (mesh, material)) in
                meshes_of.into_iter().zip(canopy_materials.each(band)).enumerate()
            {
                entities[slot] = spawn(mesh, material, Some(stage));
            }
            spawned_bands.push(Stage { entities, triangles });
        }
        let centre = Vec3::new(
            (key.0 * BLOCK + BLOCK / 2) as f32,
            key.2 as f32 * Z_SCALE,
            (key.1 * BLOCK + BLOCK / 2) as f32,
        );
        entities
            .0
            .insert(key, Spawned { terrain, water, magma, bands: spawned_bands, base, centre });
    }
    status.triangles = entities.0.values().map(Spawned::held).sum();
}

/// Texels per world tile for the tree surfaces. Dwarf Fortress's art is 32 to
/// a tile and the ground is drawn at that density, so trees match it by
/// default; `DWARF_EYE_TEXELS` overrides.
fn tree_texels() -> u32 {
    trees::texture::live_texels()
}

/// Wraps a generated surface. Canopy UVs are world-space and run well past
/// 0..1, so these wrap; nearest magnification keeps the cutout's edge hard.
fn tree_texture(texels: trees::texture::Texels) -> Image {
    let mut image = Image::new(
        Extent3d { width: texels.width, height: texels.height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        texels.rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Nearest,
        min_filter: ImageFilterMode::Nearest,
        ..default()
    });
    image
}

/// A mask with nothing loaded.
fn empty_mask() -> Image {
    let n = shadow::MASK_BLOCKS;
    Image::new(
        Extent3d { width: n, height: n, depth_or_array_layers: 1 },
        TextureDimension::D2,
        vec![0u8; (n * n) as usize],
        TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    )
}

/// Rewrites the block mask after the worker sent a new set of grounded blocks.
fn refresh_mask(mut mask: ResMut<BlockMask>, mut images: ResMut<Assets<Image>>) {
    if !mask.dirty {
        return;
    }
    let Some(mut image) = images.get_mut(&mask.image) else { return };
    mask.dirty = false;
    let n = shadow::MASK_BLOCKS as i32;
    let half = n / 2;
    let Some(data) = image.data.as_mut() else { return };
    data.fill(0);
    for &(bx, by) in &mask.blocks {
        let (x, y) = (bx + half, by + half);
        if x >= 0 && y >= 0 && x < n && y < n {
            data[(y * n + x) as usize] = 255;
        }
    }
}

fn to_bevy_mesh(data: MeshData) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, data.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, data.normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, data.colors);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, data.uvs);
    mesh.insert_indices(Indices::U32(data.indices));
    mesh
}

fn handle_input(
    keys: Res<ButtonInput<KeyCode>>,
    bridge: NonSend<Bridge>,
    clock: Res<Clock>,
    mut settings: ResMut<ViewSettings>,
    mut needs_fetch: ResMut<NeedsFetch>,
) {
    // Drive the game's own clock and weather, so lighting can be tested without
    // waiting for the world.
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let step = if shift { sky::TICKS_PER_DAY / 4 } else { sky::TICKS_PER_DAY / 24 };
    let nudge = keys.just_pressed(KeyCode::Period) as i32 - keys.just_pressed(KeyCode::Comma) as i32;
    // With the hour pinned from the environment the clock resource is the
    // viewer's own, so stepping it would drag the game somewhere it never was.
    if nudge != 0 && sky::hour_override().is_none() {
        let target = clock.tick + nudge * step;
        let _ = bridge.tx.send(Command::Run {
            command: "lua".into(),
            args: vec![dwarf_eye_world::clock::set_time(target)],
        });
    }

    for (key, weather) in [
        (KeyCode::Digit1, "clear"),
        (KeyCode::Digit2, "rain"),
        (KeyCode::Digit3, "snow"),
    ] {
        if keys.just_pressed(key) {
            let _ = bridge.tx.send(Command::Run {
                command: "weather".into(),
                args: vec![weather.into()],
            });
        }
    }

    let mut changed = false;

    // With no cut plane, the first press brings one in just above the player.
    if keys.just_pressed(KeyCode::BracketLeft) {
        settings.z_ceiling = if settings.z_ceiling == i32::MAX {
            settings.player_z + CEILING_ABOVE_PLAYER
        } else {
            settings.z_ceiling - 1
        };
        changed = true;
    }
    if keys.just_pressed(KeyCode::BracketRight) && settings.z_ceiling != i32::MAX {
        settings.z_ceiling += 1;
        changed = true;
    }
    if keys.just_pressed(KeyCode::KeyH) {
        settings.show_hidden = !settings.show_hidden;
        changed = true;
    }

    if changed {
        let _ = bridge.tx.send(Command::Remesh { opts: settings.mesh_options() });
        **needs_fetch = true;
    }
}

/// A travel vector held fixed by the environment: `DWARF_EYE_TRAVEL=east,south`
/// as two numbers, say `1,0`. It is how the preload order is checked without
/// walking anybody: the fetch centre stays where it is and only the order the
/// blocks are asked for, meshed and kept in changes.
fn pinned_travel() -> Option<Vec2> {
    let spec = std::env::var("DWARF_EYE_TRAVEL").ok()?;
    let (east, south) = spec.split_once(',')?;
    let v = Vec2::new(east.trim().parse().ok()?, south.trim().parse().ok()?);
    (v != Vec2::ZERO).then(|| v.normalize())
}

/// Asks for map around the camera whenever it moves into a new block.
fn request_blocks(
    time: Res<Time>,
    bridge: NonSend<Bridge>,
    settings: Res<ViewSettings>,
    camera: Query<(&Transform, &FlyCamera)>,
    walk: Res<walk::WalkMode>,
    mut last: Local<Option<(i32, i32, i32)>>,
    mut cooldown: Local<f32>,
    mut primed: Local<bool>,
    mut needs_fetch: ResMut<NeedsFetch>,
) {
    if !settings.placed {
        return;
    }
    *cooldown -= time.delta_secs();

    let Ok((transform, fly)) = camera.single() else { return };

    // Load around the cut plane, not the camera's altitude: the camera normally
    // floats well above the slice it is looking at.
    let z = if settings.z_ceiling == i32::MAX {
        (transform.translation.y / Z_SCALE) as i32
    } else {
        settings.z_ceiling
    };
    let center = (transform.translation.x as i32, transform.translation.z as i32, z);
    let block = (center.0.div_euclid(BLOCK), center.1.div_euclid(BLOCK), center.2);

    // Re-request on entering a new block, and otherwise once a second so the
    // view keeps up with the game world changing underneath it.
    let moved = *last != Some(block) || **needs_fetch;
    if !moved && *cooldown > 0.0 {
        return;
    }
    **needs_fetch = false;

    // The first request has to force, so DFHack sends the map as it stands
    // rather than only what has changed since.
    let force = !*primed;
    *primed = true;
    *last = Some(block);
    // Walking crosses a tile every fraction of a second, so the land ahead has
    // to stream in at the character's pace rather than the flier's.
    *cooldown = if walk.active { 0.3 } else { 1.0 };

    // Which way the view is going, on the ground plan. The character's own run
    // of tile crossings comes first — it is the world the game is streaming —
    // and the flier's eased keys stand in when nobody is walking.
    let heading = pinned_travel()
        .or_else(|| walk.travel())
        .or_else(|| if walk.active { None } else { fly.travelling() })
        .map(|v| (v.x, v.y));

    let _ = bridge.tx.send(Command::Fetch {
        center,
        opts: settings.mesh_options(),
        force,
        heading,
    });
}

fn update_hud(
    diagnostics: Res<DiagnosticsStore>,
    clock: Res<Clock>,
    weather: Res<Weather>,
    precip: Res<precipitation::Precipitation>,
    status: Res<Status>,
    settings: Res<ViewSettings>,
    entities: Res<ChunkEntities>,
    bands: Res<Bands>,
    rays: Res<god_rays::GodRays>,
    walk: Res<walk::WalkMode>,
    camera: Query<(&Transform, &FlyCamera)>,
    mut hud: Query<&mut Text, With<Hud>>,
) {
    let Ok(mut text) = hud.single_mut() else { return };
    let Ok((transform, fly)) = camera.single() else { return };

    let ceiling = if settings.z_ceiling == i32::MAX {
        "none".to_string()
    } else {
        settings.z_ceiling.to_string()
    };

    text.0 = format!(
        "{}\n{}\n{}  {}\n\
         camera  tile ({:.0}, {:.0}, {:.0})   speed {:.0}\n\
         {}\n\
         chunks  {}   triangles {} of {} held   {:.0} fps\n\
         z-ceiling {ceiling}   hidden tiles {}   sky {}   falling {}   light shafts {}   \
         haze: {}\n\
         \n\
         WASD move   QE up/down   shift boost   right-drag look   wheel speed\n\
         tab  fly / walk sync (walk: WASD steps the character, QE on stairs)\n\
         [ ]  cut plane    H  undiscovered tiles    G  light shafts    F  haze level\n\
         , .  step the game clock (shift: six hours)    1 2 3  clear / rain / snow",
        status.world,
        status.detail,
        clock.describe(),
        if clock.is_daylight() { "daylight" } else { "night" },
        transform.translation.x,
        transform.translation.z,
        transform.translation.y / Z_SCALE,
        fly.speed,
        if walk.active { walk.line.as_str() } else { "fly     tab to walk with the character" },
        entities.0.len(),
        drawn_triangles(&entities, bands.near, transform.translation),
        status.triangles,
        diagnostics
            .get(&FrameTimeDiagnosticsPlugin::FPS)
            .and_then(|d| d.smoothed())
            .unwrap_or(0.0),
        if settings.show_hidden { "shown" } else { "hidden" },
        weather.describe(),
        precip.describe(),
        if rays.enabled { "on" } else { "off" },
        rays.haze.describe(rays.derived),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the near band ends for a 45-degree lens in a 720-tall window.
    fn near() -> f32 {
        dwarf_eye_world::canopy::near_band(std::f32::consts::FRAC_PI_4, 720.0)
    }

    #[test]
    fn a_hand_off_dithers_across_a_distance_and_not_a_line() {
        let edges = band_edges(near());
        assert_eq!(edges.len(), BANDS.len() - 1, "one hand-off per gap");
        for (fade, at) in band_fades(&edges).iter().zip(&edges) {
            assert!(fade.start < *at && fade.end > *at, "{fade:?} does not straddle {at}");
            // The dither is screen-space, so it only reads as a fade if the
            // camera spends real distance inside it.
            assert!((fade.end - fade.start) / at > 0.2, "{fade:?} is barely wider than {at}");
        }
    }

    #[test]
    fn no_two_hand_offs_overlap() {
        // Bevy wants a range's start margin over before its end margin begins,
        // and a band caught fading in and out at once would flicker.
        let fades = band_fades(&band_edges(near()));
        assert!(fades.windows(2).all(|w| w[0].end < w[1].start), "{fades:?} run into each other");
        for range in band_ranges(near()) {
            assert!(
                range.start_margin.end <= range.end_margin.start,
                "a band fades in over {:?} and out over {:?} at once",
                range.start_margin,
                range.end_margin
            );
        }
    }

    #[test]
    fn the_ranges_run_nearest_first_and_meet_at_the_hand_offs() {
        let ranges = band_ranges(near());
        assert_eq!(ranges.len(), BANDS.len(), "one range per band");
        assert_eq!(ranges[0].start_margin, 0.0..0.0, "the near band starts at the camera");
        for pair in ranges.windows(2) {
            assert_eq!(
                pair[0].end_margin, pair[1].start_margin,
                "a band's end margin is the next one's start margin"
            );
        }
        assert_eq!(ranges[BANDS.len() - 1].end_margin, BAND_FAR..BAND_FAR);
    }
}
