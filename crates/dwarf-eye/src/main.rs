//! A voxel view of a live Dwarf Fortress map, fed by DFHack's
//! RemoteFortressReader plugin.
//!
//! Start Dwarf Fortress with DFHack, load a fort or an adventurer, then run this.

mod camera;
mod capture;
mod clouds;
mod noise;
mod shadow;
mod sky;
mod stars;
mod texture;
mod worker;

use bevy::asset::RenderAssetUsages;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::prelude::*;
use bevy::camera::Exposure;
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::light::{
    Atmosphere, AtmosphereEnvironmentMapLight, SunDisk, atmosphere::ScatteringMedium,
    light_consts::lux,
};
use bevy::pbr::{AtmosphereMode, AtmosphereSettings};
use bevy::post_process::bloom::Bloom;
use camera::FlyCamera;
use clouds::Weather;
use shadow::{CloudShadow, ShadowUniform, TerrainMaterial as TerrainMat};
use sky::Clock;
use dwarf_eye_world::{BLOCK, MeshData, MeshOptions, mesh::Z_SCALE};
use std::collections::HashMap;
use worker::{Bridge, ChunkKey, Command, Event};

/// How far around the camera to keep map loaded, in 16-tile blocks.
/// `DWARF_EYE_RADIUS` overrides it.
const LOAD_RADIUS: i32 = 5;

fn load_radius() -> i32 {
    std::env::var("DWARF_EYE_RADIUS").ok().and_then(|v| v.parse().ok()).unwrap_or(LOAD_RADIUS)
}
/// How many z-levels below the cut plane to keep loaded.
const LOAD_DEPTH: i32 = 22;

/// Where the cut plane starts, relative to the player's z-level.
///
/// A tree stands several levels above the ground it grows from, so a plane just
/// overhead would shear the canopy off.
const CEILING_ABOVE_PLAYER: i32 = 16;

fn main() {
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
        .add_plugins(capture::CapturePlugin)
        .insert_resource(ClearColor(Color::srgb(0.42, 0.58, 0.78)))
        .init_resource::<ViewSettings>()
        .init_resource::<ChunkEntities>()
        .init_resource::<Status>()
        .init_resource::<NeedsFetch>()
        .init_resource::<Clock>()
        .insert_resource(Weather::from_env().unwrap_or_default())
        .insert_non_send(Bridge::spawn())
        .add_systems(Startup, (setup, stars::setup))
        .add_systems(
            Update,
            (
                drain_worker,
                handle_input,
                camera::fly,
                sky::drive_sun,
                stars::drive,
                poll_weather,
                poll_clock,
                request_blocks,
                refresh_mask,
                update_hud,
            ),
        )
        .run();
}

#[derive(Resource)]
struct ViewSettings {
    z_ceiling: i32,
    show_hidden: bool,
    /// Set once the first map position arrives, so the camera starts on the map.
    placed: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        // Adventure mode leaves nearly the whole map undiscovered, so drawing
        // only what the player has seen shows almost nothing. Start revealed.
        Self { z_ceiling: i32::MAX, show_hidden: true, placed: false }
    }
}

impl ViewSettings {
    fn mesh_options(&self) -> MeshOptions {
        MeshOptions { z_ceiling: self.z_ceiling, show_hidden: self.show_hidden }
    }
}

#[derive(Resource, Default)]
struct ChunkEntities(HashMap<ChunkKey, (Entity, usize)>);

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

/// One texel per block, marking where fine chunks are loaded. Rebuilt from the
/// chunk set whenever it changes.
#[derive(Resource)]
pub struct BlockMask {
    image: Handle<Image>,
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

    commands.spawn((
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
        // bring the scene back into range.
        // DWARF_EYE_EV100 overrides it, for finding the right stop.
        Exposure {
            ev100: std::env::var("DWARF_EYE_EV100").ok().and_then(|v| v.parse().ok()).unwrap_or(13.0),
        },
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
    ));

    commands.spawn((
        DirectionalLight {
            illuminance: lux::RAW_SUNLIGHT,
            shadow_maps_enabled: true,
            ..default()
        },
        // A real 32-arcminute disk, so it reads as the sun rather than a glare.
        SunDisk::EARTH,
        Transform::from_xyz(60.0, 120.0, 40.0).looking_at(Vec3::ZERO, Vec3::Y),
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
    commands.insert_resource(BlockMask { image: mask, dirty: false });

    commands.spawn((
        Text::new("connecting to DFHack…"),
        TextFont { font_size: FontSize::Px(13.0), ..default() },
        Node { position_type: PositionType::Absolute, top: px(10), left: px(12), ..default() },
        Hud,
    ));
}

/// Pulls everything the worker has produced since the last frame.
fn drain_worker(
    mut commands: Commands,
    bridge: NonSend<Bridge>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<TerrainMat>>,
    material: Res<TerrainMaterial>,
    mut entities: ResMut<ChunkEntities>,
    mut status: ResMut<Status>,
    mut settings: ResMut<ViewSettings>,
    mut clock: ResMut<Clock>,
    mut weather: ResMut<Weather>,
    mut ground: ResMut<clouds::GroundLevel>,
    mut camera: Query<(&mut Transform, &mut FlyCamera)>,
    horizon: Query<Entity, With<Horizon>>,
    horizon_material: Res<HorizonMaterial>,
    mut mask: ResMut<BlockMask>,
) {
    for event in bridge.rx.try_iter() {
        match event {
            Event::Weather(reported) => {
                *weather = Weather::from_env().unwrap_or(reported);
            }
            Event::Clock { year, tick } => {
                *clock = Clock { year, tick };
            }
            Event::Atlas { width, height, pixels } => {
                let handle = images.add(texture::atlas_image(width, height, pixels));
                if let Some(mut m) = materials.get_mut(&material.0) {
                    m.base.base_color_texture = Some(handle);
                }
            }
            Event::Connected { world_name, save, center, size } => {
                status.world = format!("{world_name} ({save})");
                status.detail = format!("map {} x {} x {} tiles", size.0, size.1, size.2);

                // Drop the camera just above and south of the player's position.
                if let Ok((mut transform, mut fly)) = camera.single_mut() {
                    capture::apply_view(&mut fly);
                    let target = Vec3::new(
                        center.0 as f32,
                        center.2 as f32 * Z_SCALE,
                        center.1 as f32,
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
                // DWARF_EYE_Z_OFFSET drops the starting cut plane, for looking
                // straight into the underground rather than at the surface.
                let offset: i32 = std::env::var("DWARF_EYE_Z_OFFSET")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(CEILING_ABOVE_PLAYER);
                settings.z_ceiling = center.2 + offset;
                ground.0 = center.2 as f32 * Z_SCALE;
                settings.placed = true;
            }
            Event::Horizon(data) => {
                for entity in &horizon {
                    commands.entity(entity).despawn();
                }
                let triangles = data.indices.len() / 3;
                status.detail = format!("horizon: {triangles} triangles");
                commands.spawn((
                    Mesh3d(meshes.add(to_bevy_mesh(data))),
                    MeshMaterial3d(horizon_material.0.clone()),
                    Transform::IDENTITY,
                    Horizon,
                ));
            }
            Event::Chunks(batch) => {
                mask.dirty = true;
                for (key, data) in batch {
                    if let Some((entity, _)) = entities.0.remove(&key) {
                        commands.entity(entity).despawn();
                    }
                    if data.is_empty() {
                        continue;
                    }
                    let triangles = data.triangle_count();
                    let handle = meshes.add(to_bevy_mesh(data));
                    let entity = commands
                        .spawn((
                            Mesh3d(handle),
                            MeshMaterial3d(material.0.clone()),
                            Transform::IDENTITY,
                            ChunkTag(key),
                        ))
                        .id();
                    entities.0.insert(key, (entity, triangles));
                }
                status.triangles = entities.0.values().map(|(_, t)| t).sum();
            }
            Event::Status(text) => status.detail = text,
            Event::Failed(err) => {
                status.detail = format!("DFHack error: {err}");
                error!("{err}");
            }
        }
    }
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

/// Rewrites the block mask from the chunk set after it changed.
fn refresh_mask(
    mut mask: ResMut<BlockMask>,
    entities: Res<ChunkEntities>,
    mut images: ResMut<Assets<Image>>,
) {
    if !mask.dirty {
        return;
    }
    let Some(mut image) = images.get_mut(&mask.image) else { return };
    mask.dirty = false;
    let n = shadow::MASK_BLOCKS as i32;
    let half = n / 2;
    let Some(data) = image.data.as_mut() else { return };
    data.fill(0);
    for &(bx, by, _) in entities.0.keys() {
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
    if nudge != 0 {
        let target = clock.tick + nudge * step;
        let _ = bridge.tx.send(Command::Run {
            command: "lua".into(),
            args: vec![format!("df.global.cur_year_tick = {}", target.max(0))],
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

    if keys.just_pressed(KeyCode::BracketLeft) {
        settings.z_ceiling -= 1;
        changed = true;
    }
    if keys.just_pressed(KeyCode::BracketRight) {
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

/// Asks for map around the camera whenever it moves into a new block.
fn request_blocks(
    time: Res<Time>,
    bridge: NonSend<Bridge>,
    settings: Res<ViewSettings>,
    camera: Query<&Transform, With<FlyCamera>>,
    mut last: Local<Option<(i32, i32, i32)>>,
    mut cooldown: Local<f32>,
    mut primed: Local<bool>,
    mut needs_fetch: ResMut<NeedsFetch>,
) {
    if !settings.placed {
        return;
    }
    *cooldown -= time.delta_secs();

    let Ok(transform) = camera.single() else { return };

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
    *cooldown = 1.0;

    let _ = bridge.tx.send(Command::Fetch {
        center,
        radius: load_radius(),
        depth: LOAD_DEPTH,
        opts: settings.mesh_options(),
        force,
    });
}

/// Asks for the world's cloud cover now and then. The map message is large and
/// the sky changes slowly, so this is deliberately infrequent.
fn poll_weather(time: Res<Time>, bridge: NonSend<Bridge>, mut next: Local<f32>) {
    *next -= time.delta_secs();
    if *next > 0.0 {
        return;
    }
    *next = 12.0;
    let _ = bridge.tx.send(Command::Weather);
}

/// Asks for the game's calendar a few times a second.
fn poll_clock(time: Res<Time>, bridge: NonSend<Bridge>, mut next: Local<f32>) {
    *next -= time.delta_secs();
    if *next > 0.0 {
        return;
    }
    *next = 0.5;
    let _ = bridge.tx.send(Command::Clock);
}

fn update_hud(
    diagnostics: Res<DiagnosticsStore>,
    clock: Res<Clock>,
    weather: Res<Weather>,
    status: Res<Status>,
    settings: Res<ViewSettings>,
    entities: Res<ChunkEntities>,
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
         chunks  {}   triangles {}   {:.0} fps\n\
         z-ceiling {ceiling}   hidden tiles {}   sky {}\n\
         \n\
         WASD move   QE up/down   shift boost   right-drag look   wheel speed\n\
         [ ]  cut plane    H  undiscovered tiles\n\
         , .  step the game clock (shift: six hours)    1 2 3  clear / rain / snow",
        status.world,
        status.detail,
        clock.describe(),
        if clock.is_daylight() { "daylight" } else { "night" },
        transform.translation.x,
        transform.translation.z,
        transform.translation.y / Z_SCALE,
        fly.speed,
        entities.0.len(),
        status.triangles,
        diagnostics
            .get(&FrameTimeDiagnosticsPlugin::FPS)
            .and_then(|d| d.smoothed())
            .unwrap_or(0.0),
        if settings.show_hidden { "shown" } else { "hidden" },
        weather.describe(),
    );
}
