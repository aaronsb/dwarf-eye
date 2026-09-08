//! Volumetric clouds, shaped by Dwarf Fortress's own weather.
//!
//! DF reports a cloud *kind* per world tile — cumulus, stratus, cirrus and fog —
//! rather than a coverage number. The kinds set a weather map (how much sky is
//! covered, and by what), and a Perlin-Worley field carves the clouds out of it.
//! The GPU raymarches that field for the picture; the CPU marches the same field
//! toward the sun to bake the shadow map the terrain reads. `density` here and
//! `density` in `cloud_volume.wgsl` are the same function and change together.

use crate::noise::{self, Cover, Sheet, Volume};
use bevy::asset::{RenderAssetUsages, embedded_asset};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, CompareFunction, Extent3d, Face, RenderPipelineDescriptor, ShaderType,
    SpecializedMeshPipelineError, TextureDimension, TextureFormat,
};
use bevy::shader::ShaderRef;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

/// The cloud layer's base and top, above the terrain's z-level.
pub const LAYER_BOTTOM: f32 = 70.0;
pub const LAYER_TOP: f32 = 200.0;
/// The cirrus sheet, well above the main layer.
pub const CIRRUS_HEIGHT: f32 = 290.0;
/// How far the ray is followed, in tiles.
pub const MAX_DISTANCE: f32 = 2600.0;
/// The whole field repeats at this period, in tiles.
pub const WEATHER_PERIOD: f32 = 2048.0;
pub const BASE_PERIOD: f32 = 256.0;
pub const DETAIL_PERIOD: f32 = 24.0;
/// Wind, in tiles per second. `DWARF_EYE_WIND=x,z` overrides it.
pub const WIND: Vec2 = Vec2::new(0.9, 0.32);

fn wind() -> Vec2 {
    std::env::var("DWARF_EYE_WIND")
        .ok()
        .and_then(|spec| {
            let (x, z) = spec.split_once(',')?;
            Some(Vec2::new(x.trim().parse().ok()?, z.trim().parse().ok()?))
        })
        .unwrap_or(WIND)
}

const BASE_RESOLUTION: usize = 128;
const DETAIL_RESOLUTION: usize = 32;
const WEATHER_RESOLUTION: usize = 256;
pub const SHADOW_RESOLUTION: usize = 1024;

/// The z-level the terrain sits at, so the layer can hold a fixed altitude.
///
/// Tying the layer to the camera instead would put the viewer inside it.
#[derive(Resource, Default)]
pub struct GroundLevel(pub f32);

/// Cloud cover as Dwarf Fortress reports it, reduced to what a sky needs.
#[derive(Resource, Clone, Copy, PartialEq, Debug, Default)]
pub struct Weather {
    /// Puffy and tall: little ground cover, a lot of vertical extent.
    pub cumulus: f32,
    /// A flat sheet: nearly total cover, very little depth.
    pub stratus: f32,
    /// High and thin.
    pub cirrus: f32,
    /// Sits on the ground rather than above it.
    pub fog: f32,
}

impl Weather {
    /// A sky forced from the environment, for testing without waiting on the
    /// world. `DWARF_EYE_CLOUDS=cumulus=0.8,cirrus=0.4`.
    pub fn from_env() -> Option<Self> {
        let spec = std::env::var("DWARF_EYE_CLOUDS").ok()?;
        let mut weather = Weather::default();
        for part in spec.split(',').filter(|p| !p.trim().is_empty()) {
            let (name, amount) = match part.split_once('=') {
                Some((n, v)) => (n.trim(), v.trim().parse().unwrap_or(0.6)),
                None => (part.trim(), 0.6f32),
            };
            match name {
                "cumulus" => weather.cumulus = amount,
                "stratus" => weather.stratus = amount,
                "cirrus" => weather.cirrus = amount,
                "fog" => weather.fog = amount,
                _ => {}
            }
        }
        Some(weather)
    }

    pub fn is_clear(self) -> bool {
        self.cumulus + self.stratus + self.cirrus + self.fog < 0.01
    }

    fn cover(self) -> Cover {
        Cover { cumulus: self.cumulus, stratus: self.stratus, cirrus: self.cirrus }
    }

    pub fn describe(self) -> String {
        if self.is_clear() {
            return "clear".into();
        }
        let mut parts = Vec::new();
        for (name, amount) in [
            ("cumulus", self.cumulus),
            ("stratus", self.stratus),
            ("cirrus", self.cirrus),
            ("fog", self.fog),
        ] {
            if amount > 0.01 {
                parts.push(format!("{name} {:.0}%", amount * 100.0));
            }
        }
        parts.join(", ")
    }
}

/// Knobs that shape and light the field. Overridable from the environment for
/// tuning without a rebuild: `DWARF_EYE_CLOUD_TUNE=sigma=0.12,gain=3`.
#[derive(Clone, Copy, Debug, Reflect, ShaderType)]
pub struct Tuning {
    /// Extinction per tile of fully dense cloud.
    pub sigma: f32,
    /// How deeply the detail noise erodes the edges, 0..1.
    pub detail: f32,
    /// Multiplier on sunlight scattered toward the eye. Real clouds owe most of
    /// their brightness to multiple scattering the march cannot afford.
    pub sun_gain: f32,
    /// Multiplier on sky light.
    pub ambient_gain: f32,
    /// Distance haze: fraction of the way to the sky colour per tile.
    pub haze: f32,
    /// Opacity of the cirrus sheet.
    pub cirrus_opacity: f32,
    /// Fraction of light a fully opaque cloud takes from the ground.
    pub shadow_strength: f32,
    pub steps: f32,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            sigma: 0.10,
            detail: 0.32,
            sun_gain: 3.0,
            ambient_gain: 1.0,
            haze: 0.0006,
            cirrus_opacity: 0.5,
            shadow_strength: 0.85,
            steps: 64.0,
        }
    }
}

impl Tuning {
    pub fn from_env() -> Self {
        let mut tuning = Self::default();
        let Ok(spec) = std::env::var("DWARF_EYE_CLOUD_TUNE") else { return tuning };
        for part in spec.split(',') {
            let Some((name, value)) = part.split_once('=') else { continue };
            let Ok(value) = value.trim().parse::<f32>() else { continue };
            match name.trim() {
                "sigma" => tuning.sigma = value,
                "detail" => tuning.detail = value,
                "gain" => tuning.sun_gain = value,
                "ambient" => tuning.ambient_gain = value,
                "haze" => tuning.haze = value,
                "cirrus" => tuning.cirrus_opacity = value,
                "shadow" => tuning.shadow_strength = value,
                "steps" => tuning.steps = value,
                _ => {}
            }
        }
        tuning
    }
}

/// Everything the shader needs beyond the textures.
#[derive(Clone, Copy, Debug, Reflect, ShaderType)]
pub struct CloudParams {
    pub bottom: f32,
    pub top: f32,
    pub cirrus_height: f32,
    pub max_distance: f32,
    pub weather_period: f32,
    pub base_period: f32,
    pub detail_period: f32,
    pub enabled: f32,
    /// Wind offset in tiles. The field is sampled at world + wind.
    pub wind: Vec2,
    pub padding: Vec2,
    pub tuning: Tuning,
}

impl Default for CloudParams {
    fn default() -> Self {
        Self {
            bottom: LAYER_BOTTOM,
            top: LAYER_TOP,
            cirrus_height: CIRRUS_HEIGHT,
            max_distance: MAX_DISTANCE,
            weather_period: WEATHER_PERIOD,
            base_period: BASE_PERIOD,
            detail_period: DETAIL_PERIOD,
            enabled: 0.0,
            wind: Vec2::ZERO,
            padding: Vec2::ZERO,
            tuning: Tuning::default(),
        }
    }
}

/// The raymarched cloud slab.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct CloudVolumeMaterial {
    #[uniform(0)]
    pub params: CloudParams,
    #[texture(1, dimension = "3d")]
    #[sampler(2)]
    pub base: Handle<Image>,
    #[texture(3, dimension = "3d")]
    #[sampler(4)]
    pub detail: Handle<Image>,
    #[texture(5)]
    #[sampler(6)]
    pub weather: Handle<Image>,
}

const SHADER: &str = "embedded://dwarf_eye/cloud_volume.wgsl";

impl Material for CloudVolumeMaterial {
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }

    /// The shader writes integrated light and coverage, so the blend is
    /// premultiplied.
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Premultiplied
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    /// Draw the slab's far faces with no depth test. The camera is always
    /// inside the slab horizontally, and the march reads the prepass depth
    /// itself, so terrain poking into the clouds still occludes them.
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = Some(Face::Front);
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_compare = Some(CompareFunction::Always);
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

#[derive(Component)]
pub struct CloudSlab;

/// The CPU copies of the field, shared with the shadow bake thread.
pub struct Field {
    pub base: Volume,
    pub detail: Volume,
}

impl Field {
    /// Cloud density at a point in the field, 0..1. `p.xz` already carries the
    /// wind offset. `cheap` skips the detail erosion.
    ///
    /// Mirrors `density` in `cloud_volume.wgsl`.
    pub fn density(&self, weather: &Sheet, params: &CloudParams, p: Vec3, cheap: bool) -> f32 {
        let h = (p.y - params.bottom) / (params.top - params.bottom);
        if h <= 0.0 || h >= 1.0 {
            return 0.0;
        }
        let w = weather.sample(Vec2::new(p.x, p.z) / params.weather_period);
        let coverage = w.x;
        if coverage <= 0.005 {
            return 0.0;
        }
        let kind = w.y;

        // Stratus: a thin sheet near the base. Cumulus: a tall column with a
        // flat bottom and a tapering crown.
        let stratus = remap(h, 0.0, 0.07, 0.0, 1.0).clamp(0.0, 1.0)
            * remap(h, 0.18, 0.30, 1.0, 0.0).clamp(0.0, 1.0);
        let cumulus = remap(h, 0.0, 0.10, 0.0, 1.0).clamp(0.0, 1.0)
            * remap(h, 0.55, 0.95, 1.0, 0.0).clamp(0.0, 1.0);
        let gradient = stratus + (cumulus - stratus) * kind;

        let n = self.base.sample(p / params.base_period);
        let low = n.y * 0.625 + n.z * 0.25 + n.w * 0.125;
        let base = remap(n.x, low - 1.0, 1.0, 0.0, 1.0).clamp(0.0, 1.0) * gradient;
        let mut shape = remap(base, 1.0 - coverage, 1.0, 0.0, 1.0).clamp(0.0, 1.0) * coverage;

        if !cheap && shape > 0.0 {
            let d = self.detail.sample(p / params.detail_period);
            let fbm = d.x * 0.625 + d.y * 0.25 + d.z * 0.125;
            // Wispy at the base, billowy at the crown.
            let erode = fbm + ((1.0 - fbm) - fbm) * (h * 8.0).clamp(0.0, 1.0);
            shape = remap(shape, erode * params.tuning.detail, 1.0, 0.0, 1.0).clamp(0.0, 1.0);
        }
        shape
    }
}

fn remap(v: f32, lo: f32, hi: f32, new_lo: f32, new_hi: f32) -> f32 {
    new_lo + (v - lo) / (hi - lo) * (new_hi - new_lo)
}

/// Shared state between the visible clouds and the shadow bake.
#[derive(Resource)]
pub struct CloudState {
    pub field: Arc<Field>,
    pub weather: Arc<Sheet>,
    pub params: CloudParams,
    pub material: Handle<CloudVolumeMaterial>,
    /// The weather the current map was built for.
    pub built_for: Option<Weather>,
}

/// One shadow bake in flight, and the sun it was baked for.
#[derive(Resource, Default)]
pub struct ShadowBake {
    /// Behind a mutex only because a receiver is not `Sync`.
    pending: Option<Mutex<Receiver<Baked>>>,
    pub baked_sun: Vec3,
    pub baked_for: Option<Weather>,
    pub baked_ground: f32,
    since: f32,
}

pub struct Baked {
    pub sun: Vec3,
    pub ground: f32,
    pub weather: Weather,
    pub data: Vec<u8>,
}

fn repeating() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    })
}

fn volume_image(volume: &Volume) -> Image {
    let n = volume.size as u32;
    let mut image = Image::new(
        Extent3d { width: n, height: n, depth_or_array_layers: n },
        TextureDimension::D3,
        volume.data.clone(),
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = repeating();
    image
}

fn sheet_image(sheet: &Sheet) -> Image {
    let n = sheet.size as u32;
    let mut image = Image::new(
        Extent3d { width: n, height: n, depth_or_array_layers: 1 },
        TextureDimension::D2,
        sheet.data.clone(),
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = repeating();
    image
}

/// A shadow map of the given transmittance everywhere.
pub fn flat_shadow(value: u8) -> Image {
    let mut image = Image::new(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        vec![value],
        TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = repeating();
    image
}

pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<CloudVolumeMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let started = std::time::Instant::now();
    let field = Field {
        base: noise::base_noise(BASE_RESOLUTION),
        detail: noise::detail_noise(DETAIL_RESOLUTION),
    };
    info!("cloud noise generated in {:.0} ms", started.elapsed().as_secs_f32() * 1000.0);
    let weather = noise::weather_sheet(WEATHER_RESOLUTION, Cover::default());

    let params = CloudParams { tuning: Tuning::from_env(), ..default() };
    let material = materials.add(CloudVolumeMaterial {
        params,
        base: images.add(volume_image(&field.base)),
        detail: images.add(volume_image(&field.detail)),
        weather: images.add(sheet_image(&weather)),
    });

    // Tall enough to reach the cirrus, wide enough that its walls stay past
    // the far end of the march.
    let height = CIRRUS_HEIGHT + 10.0 - LAYER_BOTTOM;
    let width = MAX_DISTANCE * 2.2;
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(width, height, width))),
        MeshMaterial3d(material.clone()),
        Transform::from_xyz(0.0, LAYER_BOTTOM + height * 0.5, 0.0),
        NotShadowCaster,
        NotShadowReceiver,
        CloudSlab,
    ));

    commands.insert_resource(CloudState {
        field: Arc::new(field),
        weather: Arc::new(weather),
        params,
        material,
        built_for: None,
    });
}

/// Keeps the slab around the camera, drifts the wind, and rebuilds the weather
/// map when Dwarf Fortress changes the sky.
pub fn drive(
    time: Res<Time>,
    weather: Res<Weather>,
    ground: Res<GroundLevel>,
    mut state: ResMut<CloudState>,
    mut materials: ResMut<Assets<CloudVolumeMaterial>>,
    mut images: ResMut<Assets<Image>>,
    camera: Query<&Transform, (With<Camera3d>, Without<CloudSlab>)>,
    mut slab: Query<&mut Transform, With<CloudSlab>>,
) {
    let Ok(mut transform) = slab.single_mut() else { return };
    let Ok(camera) = camera.single() else { return };

    let height = CIRRUS_HEIGHT + 10.0 - LAYER_BOTTOM;
    transform.translation = Vec3::new(
        camera.translation.x,
        ground.0 + LAYER_BOTTOM + height * 0.5,
        camera.translation.z,
    );

    state.params.wind += wind() * time.delta_secs();
    state.params.bottom = ground.0 + LAYER_BOTTOM;
    state.params.top = ground.0 + LAYER_TOP;
    state.params.cirrus_height = ground.0 + CIRRUS_HEIGHT;
    state.params.enabled = if weather.is_clear() { 0.0 } else { 1.0 };

    let Some(mut material) = materials.get_mut(&state.material) else { return };
    if state.built_for != Some(*weather) {
        state.built_for = Some(*weather);
        let sheet = noise::weather_sheet(WEATHER_RESOLUTION, weather.cover());
        material.weather = images.add(sheet_image(&sheet));
        state.weather = Arc::new(sheet);
    }
    material.params = state.params;
}

/// Bakes the sun's transmittance through the layer onto the ground plane, in
/// field space, on a worker thread. Rebakes when the weather changes or the
/// sun has moved enough to matter.
pub fn bake_shadow(
    time: Res<Time>,
    clock: Res<crate::sky::Clock>,
    weather: Res<Weather>,
    ground: Res<GroundLevel>,
    state: Res<CloudState>,
    mut bake: ResMut<ShadowBake>,
    mut images: ResMut<Assets<Image>>,
    mut terrain: ResMut<Assets<crate::shadow::TerrainMaterial>>,
) {
    bake.since += time.delta_secs();

    // Collect a finished bake.
    let finished = bake
        .pending
        .as_ref()
        .and_then(|rx| rx.lock().expect("bake receiver").try_recv().ok());
    if let Some(baked) = finished {
        bake.pending = None;
        bake.baked_sun = baked.sun;
        bake.baked_for = Some(baked.weather);
        bake.baked_ground = baked.ground;
        let n = SHADOW_RESOLUTION as u32;
        let mut image = Image::new(
            Extent3d { width: n, height: n, depth_or_array_layers: 1 },
            TextureDimension::D2,
            baked.data,
            TextureFormat::R8Unorm,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.sampler = repeating();
        let map = images.add(image);
        // Both terrain materials, fine and horizon, read the same map.
        for (_, terrain) in terrain.iter_mut() {
            terrain.extension.map = map.clone();
            terrain.extension.uniform.sun = baked.sun;
            terrain.extension.uniform.ground = baked.ground;
        }
    }

    // Keep the live parts of the terrain uniforms current every frame.
    for (_, terrain) in terrain.iter_mut() {
        let u = &mut terrain.extension.uniform;
        u.wind = state.params.wind;
        u.period = WEATHER_PERIOD;
        u.strength = state.params.tuning.shadow_strength;
        u.enabled = if weather.is_clear() || bake.baked_for.is_none() { 0.0 } else { 1.0 };
    }

    if bake.pending.is_some() {
        return;
    }
    let sun = clock.sun_direction();
    if sun.y <= 0.02 || weather.is_clear() {
        return;
    }
    let moved = bake.baked_sun.dot(sun).clamp(-1.0, 1.0).acos() > 1.5f32.to_radians();
    let stale = bake.baked_for != Some(*weather)
        || (bake.baked_ground - ground.0).abs() > 0.5
        || (moved && bake.since > 1.0);
    if !stale {
        return;
    }

    bake.since = 0.0;
    let (tx, rx) = channel();
    bake.pending = Some(Mutex::new(rx));
    let field = state.field.clone();
    let sheet = state.weather.clone();
    let params = state.params;
    let weather = *weather;
    let ground = ground.0;
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let data = bake_transmittance(&field, &sheet, &params, sun, ground);
        let covered = data.iter().filter(|&&t| t < 200).count() as f32 / data.len() as f32;
        info!(
            "cloud shadow baked in {:.0} ms, {:.0}% of ground shaded",
            started.elapsed().as_secs_f32() * 1000.0,
            covered * 100.0
        );
        let _ = tx.send(Baked { sun, ground, weather, data });
    });
}

/// Transmittance toward the sun from every point of one period of ground.
fn bake_transmittance(field: &Field, sheet: &Sheet, params: &CloudParams, sun: Vec3, ground: f32) -> Vec<u8> {
    const SAMPLES: usize = 24;
    let n = SHADOW_RESOLUTION;
    let start = (params.bottom - ground) / sun.y;
    let end = (params.top - ground) / sun.y;
    let step = (end - start) / SAMPLES as f32;
    let mut data = vec![255u8; n * n];

    let threads = std::thread::available_parallelism().map(|t| t.get()).unwrap_or(4);
    let rows_per = n.div_ceil(threads);
    std::thread::scope(|scope| {
        for (t, chunk) in data.chunks_mut(rows_per * n).enumerate() {
            scope.spawn(move || {
                for (i, texel) in chunk.iter_mut().enumerate() {
                    let index = t * rows_per * n + i;
                    let x = (index % n) as f32 + 0.5;
                    let z = (index / n) as f32 + 0.5;
                    let origin = Vec3::new(
                        x / n as f32 * params.weather_period,
                        ground,
                        z / n as f32 * params.weather_period,
                    );
                    let mut optical_depth = 0.0;
                    for s in 0..SAMPLES {
                        let p = origin + sun * (start + step * (s as f32 + 0.5));
                        optical_depth += field.density(sheet, params, p, false);
                    }
                    let transmittance = (-optical_depth * step * params.tuning.sigma).exp();
                    *texel = (transmittance * 255.0 + 0.5) as u8;
                }
            });
        }
    });
    data
}

pub struct CloudPlugin;

impl Plugin for CloudPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "cloud_volume.wgsl");
        app.add_plugins(MaterialPlugin::<CloudVolumeMaterial>::default())
            .init_resource::<GroundLevel>()
            .init_resource::<ShadowBake>()
            .add_systems(Startup, setup)
            .add_systems(Update, (drive, bake_shadow).chain());
    }
}
