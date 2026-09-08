//! A voxel view of a live Dwarf Fortress map, fed by DFHack's
//! RemoteFortressReader plugin.
//!
//! Start Dwarf Fortress with DFHack, load a fort or an adventurer, then run this.

mod camera;
mod capture;
mod clouds;
mod god_rays;
mod noise;
mod shadow;
mod sky;
mod stars;
mod texture;
mod walk;
mod worker;

use bevy::asset::RenderAssetUsages;
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
use bevy::pbr::{AtmosphereMode, AtmosphereSettings};
use bevy::post_process::bloom::Bloom;
use camera::FlyCamera;
use clouds::Weather;
use shadow::{
    CloudShadow, ShadowUniform, TerrainMaterial as TerrainMat, canopy_sky, leaf_transmission,
};
use sky::Clock;
use dwarf_eye_trees as trees;
use bevy::render::occlusion_culling::OcclusionCulling;
use dwarf_eye_world::canopy::{Band, CanopyMeshes, Coat, Surface};
use dwarf_eye_world::{BLOCK, MeshData, MeshOptions, mesh::Z_SCALE};
use std::collections::HashMap;
use worker::{Bridge, ChunkKey, Command, Event};

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
        .add_plugins(god_rays::GodRaysPlugin)
        .add_plugins(capture::CapturePlugin)
        .insert_resource(ClearColor(Color::srgb(0.42, 0.58, 0.78)))
        .init_resource::<ViewSettings>()
        .init_resource::<Bands>()
        .init_resource::<ChunkEntities>()
        .init_resource::<PendingChunks>()
        .init_resource::<Status>()
        .init_resource::<NeedsFetch>()
        .init_resource::<Clock>()
        .init_resource::<walk::WalkMode>()
        .insert_resource(Weather::from_env().unwrap_or_default())
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
                poll_weather,
                poll_clock,
                request_blocks,
                refresh_mask,
                update_hud,
            ),
        )
        .add_systems(
            Update,
            (
                walk::toggle,
                (walk::receive, walk::walk).chain().run_if(walk::walking),
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

/// Where the near canopy band gives way to the mid one, in tiles.
///
/// Recomputed from the camera's lens and the window's height, since both
/// decide how many pixels a leaf voxel covers.
#[derive(Resource, Default)]
struct Bands {
    near: f32,
}

/// How much of the near band's range the crossfade takes, so one band dithers
/// into the other rather than popping.
const CROSSFADE: f32 = 0.15;

/// Where the mid band stops: the camera's own far plane, which no chunk
/// retention ever reaches.
const BAND_FAR: f32 = 40000.0;

/// The two ranges, near then mid. The near band's end margin is the mid band's
/// start margin, which is what Bevy crossfades across.
fn band_ranges(near: f32) -> [VisibilityRange; 2] {
    let fade = near..near * (1.0 + CROSSFADE);
    [
        // Chunk meshes hold world-space vertices at an identity transform, so
        // the range has to measure from the mesh's own bounds, not its origin.
        VisibilityRange { start_margin: 0.0..0.0, end_margin: fade.clone(), use_aabb: true },
        VisibilityRange {
            start_margin: fade,
            end_margin: BAND_FAR..BAND_FAR,
            use_aabb: true,
        },
    ]
}

/// Which band a canopy entity belongs to, so a resized window can rewrite its
/// range.
#[derive(Component, Clone, Copy, PartialEq)]
struct CanopyBand(Band);

fn occlusion_culling() -> bool {
    std::env::var("DWARF_EYE_OCCLUSION").map(|v| v != "0").unwrap_or(true)
}

#[derive(Resource, Default)]
struct ChunkEntities(HashMap<ChunkKey, Spawned>);

/// What one chunk put on the GPU: its terrain, one entity per canopy material
/// per band, and what each band costs.
///
/// Only one band draws at a time — `VisibilityRange` swaps them — so the two
/// triangle counts are alternatives, not a sum.
struct Spawned {
    terrain: Option<Entity>,
    water: Option<Entity>,
    canopy: [Option<Entity>; 4],
    mid: [Option<Entity>; 4],
    /// Terrain and water, which every band draws.
    base: usize,
    near: usize,
    mid_triangles: usize,
    /// Middle of the chunk, for saying which band is drawing.
    centre: Vec3,
}

impl Spawned {
    fn held(&self) -> usize {
        self.base + self.near + self.mid_triangles
    }

    /// What this chunk draws with the camera here: one band, never both.
    fn drawn(&self, eye: Vec3, near: f32) -> usize {
        self.base
            + if self.centre.distance(eye) < near { self.near } else { self.mid_triangles }
    }
}

/// What the loaded chunks draw from where the camera stands.
///
/// The band each chunk is in is read off its middle, which is what Bevy's own
/// range check does with the mesh's bounds.
fn drawn_triangles(entities: &ChunkEntities, near: f32, eye: Vec3) -> usize {
    entities.0.values().map(|s| s.drawn(eye, near)).sum()
}

/// Meshes that have arrived from the worker and not yet reached the GPU.
/// Uploads are budgeted per frame, nearest to the camera first, so a burst of
/// chunks never stalls a frame. A later mesh for the same chunk replaces an
/// earlier one still waiting.
#[derive(Resource, Default)]
struct PendingChunks(HashMap<ChunkKey, (MeshData, CanopyMeshes, CanopyMeshes)>);

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

/// Water surfaces: blended rather than masked, so the ground shows through the
/// shallows, and glossy enough to catch the sun. Its own material keeps the
/// terrain opaque, which is what lets the sorted transparent pass work at all.
#[derive(Resource)]
pub struct WaterMaterial(pub Handle<TerrainMat>);

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
    /// The mid band's whole crown, bark and all: the leaf shading with no
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
    info!("occlusion culling {}", if occlusion_culling() { "on" } else { "off" });

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
    solid_leaf.base.alpha_mode = AlphaMode::Opaque;
    solid_leaf.base.double_sided = false;
    solid_leaf.base.cull_mode = Some(bevy::render::render_resource::Face::Back);
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
    mut status: ResMut<Status>,
    mut settings: ResMut<ViewSettings>,
    mut clock: ResMut<Clock>,
    mut weather: ResMut<Weather>,
    mut ground: ResMut<clouds::GroundLevel>,
    mut camera: Query<(&mut Transform, &mut FlyCamera)>,
    horizon: Query<Entity, With<Horizon>>,
    horizon_material: Res<HorizonMaterial>,
    mut mask: ResMut<BlockMask>,
    mut pending: ResMut<PendingChunks>,
) {
    for event in bridge.rx.try_iter() {
        match event {
            Event::Weather(reported) => {
                *weather = Weather::from_env().unwrap_or(reported);
            }
            Event::Clock { year, tick } => {
                // DWARF_EYE_HOUR pins the hour the view is lit at without
                // moving the game's own clock.
                *clock = Clock { year, tick }.with_hour_override();
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
            Event::Coverage(blocks) => {
                mask.blocks = blocks;
                mask.dirty = true;
            }
            Event::Chunks(batch) => {
                for (key, terrain, crown, mid) in batch {
                    pending.0.insert(key, (terrain, crown, mid));
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
/// The rule is projected size, not distance: the near band ends where a
/// near-detail leaf voxel stops covering two pixels
/// (`dwarf_eye_world::canopy::near_band`), so a taller window or a longer lens
/// pushes it out. `DWARF_EYE_LOD_NEAR` overrides it, in blocks.
fn size_bands(
    mut bands: ResMut<Bands>,
    windows: Query<&Window>,
    camera: Query<&Projection, With<FlyCamera>>,
    mut ranged: Query<(&CanopyBand, &mut VisibilityRange)>,
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
        *range = match band.0 {
            Band::Near => ranges[0].clone(),
            Band::Mid => ranges[1].clone(),
        };
    }
    info!(
        "canopy bands: near out to {near:.0} tiles ({:.1} blocks), mid beyond",
        near / BLOCK as f32
    );
}

/// Moves a budget of pending meshes onto the GPU, nearest the camera first.
fn upload_chunks(
    mut commands: Commands,
    mut pending: ResMut<PendingChunks>,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Res<TerrainMaterial>,
    water_material: Res<WaterMaterial>,
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
        let Some((mut data, crown, mid)) = pending.0.remove(&key) else { continue };
        // Water rides in with the terrain and splits off here: its own entity,
        // its own translucent material.
        let pool = data.take_water();
        if let Some(old) = entities.0.remove(&key) {
            for entity in [old.terrain, old.water]
                .into_iter()
                .chain(old.canopy)
                .chain(old.mid)
                .flatten()
            {
                commands.entity(entity).despawn();
            }
        }
        if data.is_empty() && pool.is_empty() && crown.is_empty() && mid.is_empty() {
            continue;
        }
        let base = data.triangle_count() + pool.triangle_count();
        let (near, mid_triangles) = (crown.triangle_count(), mid.triangle_count());
        let mut spawn = |mesh: MeshData, material: Handle<TerrainMat>, band: Option<Band>| {
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
                if let Some(band) = band {
                    let range = match band {
                        Band::Near => ranges[0].clone(),
                        Band::Mid => ranges[1].clone(),
                    };
                    entity.insert((range, CanopyBand(band)));
                }
                entity.id()
            })
        };
        let terrain = spawn(data, material.0.clone(), None);
        let water = spawn(pool, water_material.0.clone(), None);
        let mut canopy = [None; 4];
        let crowns = [crown.bark, crown.broadleaf, crown.needle, crown.streamers];
        for (slot, (mesh, material)) in
            crowns.into_iter().zip(canopy_materials.each(Band::Near)).enumerate()
        {
            canopy[slot] = spawn(mesh, material, Some(Band::Near));
        }
        let mut coarse = [None; 4];
        let coarse_meshes = [mid.bark, mid.broadleaf, mid.needle, mid.streamers];
        for (slot, (mesh, material)) in
            coarse_meshes.into_iter().zip(canopy_materials.each(Band::Mid)).enumerate()
        {
            coarse[slot] = spawn(mesh, material, Some(Band::Mid));
        }
        let centre = Vec3::new(
            (key.0 * BLOCK + BLOCK / 2) as f32,
            key.2 as f32 * Z_SCALE,
            (key.1 * BLOCK + BLOCK / 2) as f32,
        );
        entities.0.insert(
            key,
            Spawned { terrain, water, canopy, mid: coarse, base, near, mid_triangles, centre },
        );
    }
    status.triangles = entities.0.values().map(Spawned::held).sum();
}

/// Texels per world tile for the tree surfaces. Dwarf Fortress's art is 32 to
/// a tile and the ground is drawn at that density, so trees match it by
/// default; `DWARF_EYE_TEXELS` overrides.
fn tree_texels() -> u32 {
    std::env::var("DWARF_EYE_TEXELS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(trees::texture::DEFAULT_TEXELS)
        .clamp(4, 128)
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

/// Asks for map around the camera whenever it moves into a new block.
fn request_blocks(
    time: Res<Time>,
    bridge: NonSend<Bridge>,
    settings: Res<ViewSettings>,
    camera: Query<&Transform, With<FlyCamera>>,
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
    // Walking crosses a tile every fraction of a second, so the land ahead has
    // to stream in at the character's pace rather than the flier's.
    *cooldown = if walk.active { 0.3 } else { 1.0 };

    let _ = bridge.tx.send(Command::Fetch {
        center,
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
         z-ceiling {ceiling}   hidden tiles {}   sky {}   light shafts {}\n\
         \n\
         WASD move   QE up/down   shift boost   right-drag look   wheel speed\n\
         tab  fly / walk sync (walk: WASD steps the character, QE on stairs)\n\
         [ ]  cut plane    H  undiscovered tiles    G  light shafts\n\
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
        if rays.enabled { "on" } else { "off" },
    );
}
