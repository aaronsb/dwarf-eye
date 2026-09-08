//! A voxel view of a live Dwarf Fortress map, fed by DFHack's
//! RemoteFortressReader plugin.
//!
//! Start Dwarf Fortress with DFHack, load a fort or an adventurer, then run this.

mod camera;
mod worker;

use bevy::asset::RenderAssetUsages;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use camera::FlyCamera;
use dwarf_eye_world::{BLOCK, MeshData, MeshOptions, mesh::Z_SCALE};
use std::collections::HashMap;
use worker::{Bridge, ChunkKey, Command, Event};

/// How far around the camera to keep map loaded, in 16-tile blocks.
const LOAD_RADIUS: i32 = 5;
/// How many z-levels above and below the camera to keep loaded.
const LOAD_DEPTH: i32 = 12;

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
        .insert_resource(ClearColor(Color::srgb(0.42, 0.58, 0.78)))
        .init_resource::<ViewSettings>()
        .init_resource::<ChunkEntities>()
        .init_resource::<Status>()
        .init_resource::<NeedsFetch>()
        .insert_non_send(Bridge::spawn())
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                drain_worker,
                handle_input,
                camera::fly,
                request_blocks,
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

#[derive(Component)]
struct ChunkTag(#[allow(dead_code)] ChunkKey);

/// Reused by every chunk, since colour lives in the vertex data.
#[derive(Resource)]
struct TerrainMaterial(Handle<StandardMaterial>);

fn setup(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 40.0, 40.0).looking_at(Vec3::ZERO, Vec3::Y),
        AmbientLight { brightness: 260.0, ..default() },
        FlyCamera::default(),
    ));

    commands.spawn((
        DirectionalLight { illuminance: 9000.0, shadow_maps_enabled: true, ..default() },
        Transform::from_xyz(60.0, 120.0, 40.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands.insert_resource(TerrainMaterial(materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.92,
        reflectance: 0.03,
        ..default()
    })));

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
    material: Res<TerrainMaterial>,
    mut entities: ResMut<ChunkEntities>,
    mut status: ResMut<Status>,
    mut settings: ResMut<ViewSettings>,
    mut camera: Query<&mut Transform, With<FlyCamera>>,
) {
    for event in bridge.rx.try_iter() {
        match event {
            Event::Connected { world_name, save, center, size } => {
                status.world = format!("{world_name} ({save})");
                status.detail = format!("map {} x {} x {} tiles", size.0, size.1, size.2);

                // Drop the camera just above and south of the player's position.
                if let Ok(mut transform) = camera.single_mut() {
                    let target = Vec3::new(
                        center.0 as f32,
                        center.2 as f32 * Z_SCALE,
                        center.1 as f32,
                    );
                    *transform = Transform::from_translation(target + Vec3::new(0.0, 18.0, 28.0))
                        .looking_at(target, Vec3::Y);
                }
                // DWARF_EYE_Z_OFFSET drops the starting cut plane, for looking
                // straight into the underground rather than at the surface.
                let offset: i32 = std::env::var("DWARF_EYE_Z_OFFSET")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(2);
                settings.z_ceiling = center.2 + offset;
                settings.placed = true;
            }
            Event::Chunks(batch) => {
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

fn to_bevy_mesh(data: MeshData) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, data.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, data.normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, data.colors);
    mesh.insert_indices(Indices::U32(data.indices));
    mesh
}

fn handle_input(
    keys: Res<ButtonInput<KeyCode>>,
    bridge: NonSend<Bridge>,
    mut settings: ResMut<ViewSettings>,
    mut needs_fetch: ResMut<NeedsFetch>,
) {
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
        radius: LOAD_RADIUS,
        depth: LOAD_DEPTH,
        opts: settings.mesh_options(),
        force,
    });
}

fn update_hud(
    diagnostics: Res<DiagnosticsStore>,
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
        "{}\n{}\n\
         camera  tile ({:.0}, {:.0}, {:.0})   speed {:.0}\n\
         chunks  {}   triangles {}   {:.0} fps\n\
         z-ceiling {ceiling}   hidden tiles {}\n\
         \n\
         WASD move   QE up/down   shift boost   right-drag look   wheel speed\n\
         [ ]  lower/raise the cut plane      H  toggle undiscovered tiles",
        status.world,
        status.detail,
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
    );
}
