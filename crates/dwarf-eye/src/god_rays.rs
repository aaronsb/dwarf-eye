//! Volumetric light shafts, as a screen-space pass of our own.
//!
//! Bevy ships `VolumetricFog`, but its shader knows nothing about our cloud
//! transmittance map and its ambient term greys the whole frame. This is the
//! same idea rebuilt around what this renderer already has: the sun's shadow
//! cascades, which tree canopies and terrain write into, and the CPU-baked
//! cloud map, read along the same slant `cloud_shadow.wgsl` reads it.
//!
//! The pass runs in the `Core3d` schedule after the main pass — so the clouds
//! and the terrain are already in the view target — and before the early post
//! process, so bloom picks the shafts up and tonemapping brings them back into
//! range. It binds Bevy's own mesh view layout as group 0, exactly as
//! `bevy_pbr::volumetric_fog` does, which is what gives a fullscreen pass
//! access to the lights, the shadow cascade array and the atmosphere.
//!
//! The weather sets how thick the medium is: `Humidity::density` is the whole
//! model, and it reads what the air is actually carrying rather than the cloud
//! kind alone. `DWARF_EYE_GODRAYS` overrides any of it, `G` toggles the pass
//! and `F` cycles a forced haze level over the derived one.

use crate::clouds::Weather;
use crate::shadow::TerrainMaterial;
use crate::sky::Clock;
use dwarf_eye_world::weather::RainHistory;
use bevy::asset::{embedded_asset, load_embedded_asset};
use bevy::camera::Camera3d;
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::core_3d::prepare_core_3d_depth_textures;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::pbr::{
    MeshPipelineSystems, MeshPipelineViewLayoutKey, MeshPipelineViewLayouts, MeshViewBindGroup,
    ViewKeyCache,
};
use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    BindGroupLayoutDescriptor, BindGroupLayoutEntries, BindingResource, BlendComponent, BlendFactor,
    BlendOperation, BlendState, CachedRenderPipelineId, ColorTargetState, ColorWrites,
    DynamicBindGroupEntries, FragmentState, LoadOp, Operations, PipelineCache, PrimitiveState,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, SamplerBindingType,
    ShaderStages, ShaderType, SpecializedRenderPipeline, SpecializedRenderPipelines, StoreOp,
    TextureFormat, TextureSampleType, TextureUsages, UniformBuffer,
    binding_types::{sampler, texture_2d, texture_depth_2d, texture_depth_2d_multisampled, uniform_buffer},
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::GpuImage;
use bevy::render::view::{ExtractedView, Msaa, ViewDepthTexture, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;

/// The clear, dry, high-sun floor: the haze a desert noon still has. Near
/// enough to nothing that the shafts read as absent rather than as a wash.
pub const HAZE_FLOOR: f32 = 0.002;
/// Extinction the low-sun term adds at its fullest. Kept at the value the
/// approved shots were taken with (27f5af3).
const LOW_SUN: f32 = 0.014;
/// DF's own fog kind, the strongest single input: 0.25 mist, 0.55 fog, 0.85
/// thick. Also from the approved shots.
const FOG: f32 = 0.033;
const STRATUS: f32 = 0.005;
const CUMULUS: f32 = 0.002;
/// The stratus countdown, so the sheet's build-up thickens the air ahead of the
/// change instead of the haze stepping when the kind flips.
const COUNTDOWN: f32 = 0.004;
/// Rain in the air right now.
const FALLING: f32 = 0.010;
/// What a saturated, cold, still dawn over standing water would add on its own.
/// This is the humidity term proper; everything above it is the sky.
const DAMP: f32 = 0.019;
/// How the damp splits between the region's climate, the last shower and the
/// water underfoot. Sums to one, so `DAMP` is the whole of it.
const RAINFALL_SHARE: f32 = 0.45;
const RECENT_SHARE: f32 = 0.30;
const WATER_SHARE: f32 = 0.25;
/// Minutes for the memory of a shower to fall by 1/e. Two hours after the rain
/// stops the air is most of the way back to the region's own damp.
const DRY_MINUTES: f32 = 90.0;
/// How much of the damp a hot day burns off. At the top of the temperature
/// scale a third of it survives.
const WARM_BURN: f32 = 0.65;
/// How much the dawn and dusk peaks lift the damp over the middle of the day.
const TWILIGHT_PEAK: f32 = 0.6;
/// Where the peaks sit and how wide they are, in hours. Dusk is the broader of
/// the two: the ground gives its heat back more slowly than the sun takes it.
const DAWN_HOUR: f32 = 6.0;
const DAWN_WIDTH: f32 = 2.0;
const DUSK_HOUR: f32 = 18.0;
const DUSK_WIDTH: f32 = 2.5;
/// The density a forced haze of 1.0 stands for: a thick fog at a low sun, the
/// top of what the derived model reaches. `F` cycles fractions of it.
pub const HAZE_FULL: f32 = 0.05;
/// The moon's shafts as a fraction of the sun's. Moonlight is already a
/// three-hundredth of daylight and the night exposure only gives forty of that
/// back, so this is the last of the three factors rather than the whole story.
const MOON_RAYS: f32 = 0.5;

/// The dawn and dusk peaks, 0 in the middle of the day and at midnight, 1 on
/// the hour each sits at.
///
/// Overnight cooling brings the air to saturation and the first sun burns it
/// off through the morning; the evening damp comes back as the ground gives up
/// its heat. Both are gaussians on the hour, wrapped round the clock.
fn twilight(hour: f32) -> f32 {
    let bump = |centre: f32, width: f32| {
        let d = ((hour - centre + 12.0).rem_euclid(24.0) - 12.0) / width;
        (-d * d).exp()
    };
    bump(DAWN_HOUR, DAWN_WIDTH).max(bump(DUSK_HOUR, DUSK_WIDTH))
}

/// Everything the haze reads, in one place: the sky Dwarf Fortress reports, the
/// water the region and the map hold, and where the sun is.
///
/// All the 0..1 fields are fractions of their own scale — `fog` is DF's own
/// four-step kind through `FOG_COVER`, `rainfall` is `RegionTile.rainfall` over
/// 100, `temperature` the region tile's own 0-to-100 field, `water` the share
/// of the loaded window that is water.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Humidity {
    pub fog: f32,
    pub stratus: f32,
    pub cumulus: f32,
    /// DF's four-bit stratus countdown as a fraction.
    pub countdown: f32,
    /// Rain or snow actually falling, as `Precipitation::drawn` reports it.
    pub falling: f32,
    /// The region's own rainfall at the window centre, 0..1.
    pub rainfall: f32,
    /// Minutes since it last rained, or `None` where it has not rained while
    /// the viewer has been watching.
    pub since_rain: Option<f32>,
    /// The region tile's temperature, 0 cold to 1 hot.
    pub temperature: f32,
    /// How much water is near: the share of the loaded window that carries it.
    pub water: f32,
    /// Hour of the game's day, 0..24.
    pub hour: f32,
    /// The sun's elevation, `sun_direction().y`.
    pub elevation: f32,
}

impl Default for Humidity {
    /// A clear, dry, temperate noon: the state the floor is defined at.
    fn default() -> Self {
        Self {
            fog: 0.0,
            stratus: 0.0,
            cumulus: 0.0,
            countdown: 0.0,
            falling: 0.0,
            rainfall: 0.0,
            since_rain: None,
            temperature: 0.5,
            water: 0.0,
            hour: 12.0,
            elevation: 1.0,
        }
    }
}

impl Humidity {
    /// Extinction per tile of the medium at ground level.
    ///
    /// A sum of small densities, so every input is separately monotone and the
    /// constants can be read off one at a time. Two groups: the sky, which is
    /// what DF's cloud bitfield says, and the damp, which is the water the
    /// region, the last shower and the map underfoot are holding. The damp is
    /// the part that a hot afternoon burns away and a dawn brings back, so the
    /// temperature and the hour scale it rather than adding to it — that is
    /// what keeps a dry noon faint whatever the hour term is doing.
    ///
    /// ```text
    /// low_sun = 1 - min(elevation * 4, 1)
    /// recent  = exp(-minutes_since_rain / 90)
    /// damp    = 0.45*rainfall + 0.30*recent + 0.25*water
    /// warmth  = 1 - 0.65*temperature
    /// peak    = 1 + 0.6*twilight(hour)
    /// density = 0.002 + 0.014*low_sun
    ///         + 0.033*fog + 0.005*stratus + 0.002*cumulus
    ///         + 0.004*countdown + 0.010*falling
    ///         + 0.019*damp*warmth*peak
    /// ```
    pub fn density(&self) -> f32 {
        let unit = |v: f32| v.clamp(0.0, 1.0);
        let low_sun = 1.0 - (unit(self.elevation) * 4.0).min(1.0);
        let recent = match self.since_rain {
            Some(minutes) => (-minutes.max(0.0) / DRY_MINUTES).exp(),
            None => 0.0,
        };
        let damp = RAINFALL_SHARE * unit(self.rainfall)
            + RECENT_SHARE * recent
            + WATER_SHARE * unit(self.water);
        let warmth = 1.0 - WARM_BURN * unit(self.temperature);
        let peak = 1.0 + TWILIGHT_PEAK * twilight(self.hour);

        HAZE_FLOOR
            + LOW_SUN * low_sun
            + FOG * unit(self.fog)
            + STRATUS * unit(self.stratus)
            + CUMULUS * unit(self.cumulus)
            + COUNTDOWN * unit(self.countdown)
            + FALLING * unit(self.falling)
            + DAMP * damp * warmth * peak
    }
}

/// What `F` is showing: the model's own answer, or a level forced over it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Haze {
    #[default]
    Derived,
    /// A fraction of `HAZE_FULL`, which is a thick fog at a low sun.
    Forced(f32),
}

/// The levels `F` walks through, starting from and returning to the derived
/// density.
pub const HAZE_CYCLE: [Haze; 5] = [
    Haze::Derived,
    Haze::Forced(0.0),
    Haze::Forced(0.2),
    Haze::Forced(0.5),
    Haze::Forced(0.8),
];

impl Haze {
    pub fn next(self) -> Self {
        let at = HAZE_CYCLE.iter().position(|&h| h == self).unwrap_or(0);
        HAZE_CYCLE[(at + 1) % HAZE_CYCLE.len()]
    }

    /// The density this state asks for, given what the model derived.
    pub fn density(self, derived: f32) -> f32 {
        match self {
            Haze::Derived => derived,
            Haze::Forced(level) => level.clamp(0.0, 1.0) * HAZE_FULL,
        }
    }

    /// The HUD's reading: `derived 0.021` or `forced 0.5`.
    pub fn describe(self, derived: f32) -> String {
        match self {
            Haze::Derived => format!("derived {derived:.3}"),
            Haze::Forced(level) => format!("forced {level:.1}"),
        }
    }
}

/// Everything the pass needs, gathered in the main world and handed straight
/// to the render world.
#[derive(Resource, Clone, ExtractResource)]
pub struct GodRays {
    /// Toggled with `G`.
    pub enabled: bool,
    /// Extinction per tile of the medium at ground level.
    pub density: f32,
    /// What `F` last asked for, and what the model would have said. The HUD
    /// reads both; only `density` reaches the shader.
    pub haze: Haze,
    pub derived: f32,
    /// Henyey-Greenstein asymmetry: how tightly the shafts hug the sun.
    pub g: f32,
    /// Multiplier on the in-scattered sunlight.
    pub strength: f32,
    /// Reciprocal scale height of the medium, in 1/tiles.
    pub falloff: f32,
    /// How much of the medium's own extinction dims the scene behind it.
    pub dim: f32,
    /// How far the march follows the ray, in tiles.
    pub max_distance: f32,
    pub steps: u32,
    /// The cloud transmittance map and how the terrain reads it. Copied from
    /// the terrain material, so the shafts and the ground shade agree.
    pub map: Handle<Image>,
    pub sun: Vec3,
    pub wind: Vec2,
    pub period: f32,
    pub ground: f32,
    pub cloud_strength: f32,
    pub cloud_enabled: f32,
}

impl Default for GodRays {
    fn default() -> Self {
        Self {
            enabled: true,
            density: HAZE_FLOOR,
            haze: Haze::Derived,
            derived: HAZE_FLOOR,
            g: 0.6,
            strength: 0.35,
            falloff: 1.0 / 60.0,
            dim: 0.2,
            max_distance: 600.0,
            steps: 32,
            map: Handle::default(),
            sun: Vec3::Y,
            wind: Vec2::ZERO,
            period: 2048.0,
            ground: 0.0,
            cloud_strength: 0.0,
            cloud_enabled: 0.0,
        }
    }
}

/// Overrides from the environment, for tuning without a rebuild:
/// `DWARF_EYE_GODRAYS=density=0.01,g=0.7,strength=0.5,dim=0.4,steps=48,off`.
#[derive(Resource, Clone, Copy, Default)]
pub struct Overrides {
    pub density: Option<f32>,
    pub g: Option<f32>,
    pub strength: Option<f32>,
    pub falloff: Option<f32>,
    pub dim: Option<f32>,
    pub distance: Option<f32>,
    pub steps: Option<u32>,
    pub off: bool,
}

impl Overrides {
    pub fn from_env() -> Self {
        let mut over = Self::default();
        let Ok(spec) = std::env::var("DWARF_EYE_GODRAYS") else { return over };
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((name, value)) = part.split_once('=') else {
                if part == "off" {
                    over.off = true;
                }
                continue;
            };
            let Ok(value) = value.trim().parse::<f32>() else { continue };
            match name.trim() {
                "density" => over.density = Some(value),
                "g" => over.g = Some(value),
                "strength" => over.strength = Some(value),
                "falloff" => over.falloff = Some(value),
                "height" => over.falloff = Some(1.0 / value.max(1.0)),
                "dim" => over.dim = Some(value),
                "distance" | "dist" => over.distance = Some(value),
                "steps" => over.steps = Some(value as u32),
                _ => {}
            }
        }
        over
    }
}

/// How long since it last rained, kept frame to frame. The type is
/// `weather.rs`'s; the world crate knows nothing of Bevy, so the resource is
/// this wrapper.
#[derive(Resource, Default, Deref, DerefMut)]
pub struct Rain(pub RainHistory);

/// The two humidity inputs the protocol never sends and the worker does not
/// forward, plus the one the viewer measures for itself.
///
/// `RegionTile.rainfall` and the region tile's temperature both sit in
/// `weather.rs:Reading` territory but stop at `worker.rs:WeatherReport`, which
/// this pass does not own; until a line each carries them, they stand at a
/// temperate embark's own values and `DWARF_EYE_HAZE=rainfall=0.9,temp=0.2`
/// sets them by hand. `water` is live: `measure_water` counts it off the
/// chunks that are loaded.
#[derive(Resource, Clone, Copy)]
pub struct Climate {
    pub rainfall: f32,
    pub temperature: f32,
    pub water: f32,
}

impl Default for Climate {
    fn default() -> Self {
        Self { rainfall: 0.5, temperature: 0.5, water: 0.0 }
    }
}

impl Climate {
    pub fn from_env() -> Self {
        let mut climate = Self::default();
        let Ok(spec) = std::env::var("DWARF_EYE_HAZE") else { return climate };
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((name, value)) = part.split_once('=') else { continue };
            let Ok(value) = value.trim().parse::<f32>() else { continue };
            match name.trim() {
                "rainfall" | "rain" => climate.rainfall = value,
                "temp" | "temperature" => climate.temperature = value,
                "water" => climate.water = value,
                _ => {}
            }
        }
        climate
    }
}

/// How much of the loaded window carries water, as the share of chunks that
/// spawned a water surface. A chunk is sixteen tiles square, so this is
/// proximity rather than a tile count: a river through the middle of the view
/// reads a few per cent, a coast reads half.
///
/// Held apart from `drive` so the query runs a couple of times a second rather
/// than every frame; the number moves at the speed the map is fetched.
fn measure_water(
    time: Res<Time>,
    terrain: Res<crate::TerrainMaterial>,
    water: Res<crate::WaterMaterial>,
    surfaces: Query<&MeshMaterial3d<TerrainMaterial>>,
    mut climate: ResMut<Climate>,
    mut due: Local<f32>,
) {
    *due -= time.delta_secs();
    if *due > 0.0 {
        return;
    }
    *due = 0.5;
    let (mut ground, mut wet) = (0u32, 0u32);
    for material in surfaces.iter() {
        ground += (material.0 == terrain.0) as u32;
        wet += (material.0 == water.0) as u32;
    }
    if ground > 0 {
        climate.water = (wet as f32 / ground as f32).clamp(0.0, 1.0);
    }
}

/// Follows the weather and the cloud bake.
///
/// The cloud terms come out of the terrain material rather than the bake
/// directly, so the shafts read exactly the map the ground is shaded with,
/// wind offset and all.
fn drive(
    time: Res<Time>,
    weather: Res<Weather>,
    precip: Res<crate::precipitation::Precipitation>,
    clock: Res<Clock>,
    over: Res<Overrides>,
    climate: Res<Climate>,
    terrain: Res<crate::TerrainMaterial>,
    materials: Res<Assets<TerrainMaterial>>,
    mut history: ResMut<Rain>,
    mut rays: ResMut<GodRays>,
) {
    let Weather { cumulus, stratus, cirrus: _, fog, countdown } = *weather;
    let (kind, falling) = precip.drawn();
    history.observe(kind == dwarf_eye_world::weather::Precip::Rain && falling > 0.0,
        time.delta_secs());

    // Everything the air is carrying, in one call. `Humidity::density` carries
    // the model and the constants; the shots it was calibrated against are in
    // `docs/architecture/sky/weather.md` (issue #18).
    let sun = clock.sun_direction();
    let humidity = Humidity {
        fog,
        stratus,
        cumulus,
        countdown,
        falling,
        rainfall: climate.rainfall,
        since_rain: history.minutes(),
        temperature: climate.temperature,
        water: climate.water,
        hour: clock.day_fraction() * 24.0,
        elevation: sun.y,
    };
    rays.derived = humidity.density();
    // `F` beats the environment, which beats the model: the key is a hand on
    // the dial, and a shot that pins the density still gets what it asked for.
    rays.density = match rays.haze {
        Haze::Derived => over.density.unwrap_or(rays.derived),
        forced => forced.density(rays.derived),
    };

    let low_sun = 1.0 - (sun.y.clamp(0.0, 1.0) * 4.0).min(1.0);
    let height = 60.0 - 44.0 * fog.clamp(0.0, 1.0);
    rays.falloff = over.falloff.unwrap_or(1.0 / height);
    rays.g = over.g.unwrap_or(0.6);
    rays.max_distance = over.distance.unwrap_or(600.0);
    rays.steps = over.steps.unwrap_or(32).clamp(4, 128);
    // Fog wants to be felt as fog, so it dims the scene behind the shafts more.
    rays.dim = over.dim.unwrap_or(0.15 + 0.55 * fog.clamp(0.0, 1.0));

    // No light, no shafts. The moon carries them at a fraction of the sun's
    // strength once it is up, which is what gives a misty night its glow; the
    // shader already marches whichever directional light is brightest, so the
    // only thing the moon needs from here is a strength that is not zero.
    let daylight = (sun.y * 6.0).clamp(0.0, 1.0);
    let moonlight = MOON_RAYS * clock.moon_light();
    // Shafts read strongest when the sun is low and the light comes in sideways.
    rays.strength = over.strength.unwrap_or(0.45 + 0.35 * low_sun) * daylight.max(moonlight);

    if let Some(material) = materials.get(&terrain.0) {
        let u = &material.extension.uniform;
        rays.map = material.extension.map.clone();
        rays.sun = u.sun;
        rays.wind = u.wind;
        rays.period = if u.period > 0.0 { u.period } else { 2048.0 };
        rays.ground = u.ground;
        rays.cloud_strength = u.strength;
        rays.cloud_enabled = u.enabled;
    }
}

fn toggle(keys: Res<ButtonInput<KeyCode>>, mut rays: ResMut<GodRays>) {
    if keys.just_pressed(KeyCode::KeyG) {
        rays.enabled = !rays.enabled;
    }
    if keys.just_pressed(KeyCode::KeyF) {
        rays.haze = rays.haze.next();
    }
}

/// The pass's own uniform, group 1 binding 0. Mirrors `GodRays` in
/// `god_rays.wgsl`.
#[derive(ShaderType, Clone, Copy, Default)]
struct GodRaysUniform {
    sun: Vec3,
    density: f32,
    wind: Vec2,
    period: f32,
    ground: f32,
    cloud_strength: f32,
    cloud_enabled: f32,
    g: f32,
    strength: f32,
    falloff: f32,
    max_distance: f32,
    dim: f32,
    steps: f32,
}

#[derive(Resource, Default, Deref, DerefMut)]
struct GodRaysBuffer(UniformBuffer<GodRaysUniform>);

/// Two layouts: one for a multisampled depth attachment, one for a plain one.
#[derive(Resource)]
struct GodRaysPipeline {
    mesh_view_layouts: MeshPipelineViewLayouts,
    layouts: [BindGroupLayoutDescriptor; 2],
    shader: Handle<Shader>,
    fullscreen: FullscreenShader,
}

fn layout(multisampled: bool) -> BindGroupLayoutDescriptor {
    let entries = BindGroupLayoutEntries::with_indices(
        ShaderStages::FRAGMENT,
        (
            (0, uniform_buffer::<GodRaysUniform>(false)),
            (
                1,
                if multisampled { texture_depth_2d_multisampled() } else { texture_depth_2d() },
            ),
            (2, texture_2d(TextureSampleType::Float { filterable: true })),
            (3, sampler(SamplerBindingType::Filtering)),
        ),
    );
    let label = if multisampled { "god rays layout (multisampled)" } else { "god rays layout" };
    BindGroupLayoutDescriptor::new(label, &entries)
}

fn init_pipeline(
    mut commands: Commands,
    mesh_view_layouts: Res<MeshPipelineViewLayouts>,
    fullscreen: Res<FullscreenShader>,
    asset_server: Res<AssetServer>,
) {
    commands.insert_resource(GodRaysPipeline {
        mesh_view_layouts: mesh_view_layouts.clone(),
        layouts: [layout(false), layout(true)],
        shader: load_embedded_asset!(asset_server.as_ref(), "god_rays.wgsl"),
        fullscreen: fullscreen.clone(),
    });
}

#[derive(PartialEq, Eq, Hash, Clone)]
struct GodRaysKey {
    view_key: MeshPipelineViewLayoutKey,
    target_format: TextureFormat,
}

impl SpecializedRenderPipeline for GodRaysPipeline {
    type Key = GodRaysKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        // Hardware 2x2 is all a volume march needs; the softer filters cost
        // more than the grain they remove is worth here.
        let mut shader_defs = vec!["SHADOW_FILTER_METHOD_HARDWARE_2X2".into()];
        let multisampled = key.view_key.contains(MeshPipelineViewLayoutKey::MULTISAMPLED);
        if multisampled {
            shader_defs.push("MULTISAMPLED".into());
        }
        if key.view_key.contains(MeshPipelineViewLayoutKey::ATMOSPHERE) {
            shader_defs.push("ATMOSPHERE".into());
        }

        let view_layout = self.mesh_view_layouts.get_view_layout(key.view_key);
        RenderPipelineDescriptor {
            label: Some("god rays pipeline".into()),
            layout: vec![
                view_layout.main_layout.clone(),
                self.layouts[multisampled as usize].clone(),
            ],
            vertex: self.fullscreen.to_vertex_state(),
            primitive: PrimitiveState::default(),
            fragment: Some(FragmentState {
                shader: self.shader.clone(),
                shader_defs,
                targets: vec![Some(ColorTargetState {
                    format: key.target_format,
                    // Add the shafts on, and let the medium's own extinction
                    // take a bite out of what is behind them.
                    blend: Some(BlendState {
                        color: BlendComponent {
                            src_factor: BlendFactor::One,
                            dst_factor: BlendFactor::OneMinusSrcAlpha,
                            operation: BlendOperation::Add,
                        },
                        alpha: BlendComponent {
                            src_factor: BlendFactor::Zero,
                            dst_factor: BlendFactor::One,
                            operation: BlendOperation::Add,
                        },
                    }),
                    write_mask: ColorWrites::ALL,
                })],
                ..default()
            }),
            ..default()
        }
    }
}

#[derive(Component)]
struct ViewGodRays(CachedRenderPipelineId);

fn prepare_pipelines(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<GodRaysPipeline>>,
    god_rays_pipeline: Res<GodRaysPipeline>,
    rays: Res<GodRays>,
    views: Query<(Entity, &ExtractedView), With<Camera3d>>,
    view_keys: Res<ViewKeyCache>,
) {
    for (entity, view) in views.iter() {
        if !rays.enabled {
            commands.entity(entity).remove::<ViewGodRays>();
            continue;
        }
        let Some(mesh_key) = view_keys.get(&view.retained_view_entity) else { continue };
        let id = pipelines.specialize(
            &pipeline_cache,
            &god_rays_pipeline,
            GodRaysKey {
                view_key: (*mesh_key).into(),
                target_format: view.target_format,
            },
        );
        commands.entity(entity).insert(ViewGodRays(id));
    }
}

fn prepare_uniform(
    rays: Res<GodRays>,
    mut buffer: ResMut<GodRaysBuffer>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    buffer.set(GodRaysUniform {
        sun: rays.sun,
        density: rays.density,
        wind: rays.wind,
        period: rays.period,
        ground: rays.ground,
        cloud_strength: rays.cloud_strength,
        cloud_enabled: rays.cloud_enabled,
        g: rays.g,
        strength: rays.strength,
        falloff: rays.falloff,
        max_distance: rays.max_distance,
        dim: rays.dim,
        steps: rays.steps as f32,
    });
    buffer.write_buffer(&device, &queue);
}

/// The depth attachment is not readable in a shader by default, and the march
/// has to stop at the scene.
fn prepare_depth_usage(mut cameras: Query<&mut Camera3d>, rays: Res<GodRays>) {
    if !rays.enabled {
        return;
    }
    for mut camera in cameras.iter_mut() {
        camera.depth_texture_usages.0 |= TextureUsages::TEXTURE_BINDING.bits();
    }
}

fn god_rays(
    view: ViewQuery<(&ViewTarget, &ViewDepthTexture, &ViewGodRays, &MeshViewBindGroup, &Msaa)>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<GodRaysPipeline>,
    buffer: Res<GodRaysBuffer>,
    rays: Res<GodRays>,
    images: Res<RenderAssets<GpuImage>>,
    mut ctx: RenderContext,
) {
    let (target, depth, view_pipeline, view_bind_group, msaa) = view.into_inner();
    if !rays.enabled || rays.strength <= 0.0 {
        return;
    }
    let (Some(render_pipeline), Some(uniform)) =
        (pipeline_cache.get_render_pipeline(view_pipeline.0), buffer.binding())
    else {
        return;
    };
    let Some(cloud) = images.get(&rays.map) else { return };

    let multisampled = !matches!(*msaa, Msaa::Off);
    let bind_group_layout =
        pipeline_cache.get_bind_group_layout(&pipeline.layouts[multisampled as usize]);
    let entries = DynamicBindGroupEntries::sequential((
        uniform,
        BindingResource::TextureView(depth.view()),
        BindingResource::TextureView(&cloud.texture_view),
        BindingResource::Sampler(&cloud.sampler),
    ));
    let bind_group = ctx.render_device().create_bind_group(None, &bind_group_layout, &entries);

    let descriptor = RenderPassDescriptor {
        label: Some("god rays"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: target.main_texture_view(),
            depth_slice: None,
            resolve_target: None,
            ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    };

    let encoder = ctx.command_encoder();
    let mut pass = encoder.begin_render_pass(&descriptor);
    pass.set_pipeline(render_pipeline);
    pass.set_bind_group(0, &view_bind_group.main, &view_bind_group.main_offsets);
    pass.set_bind_group(1, &bind_group, &[]);
    pass.draw(0..3, 0..1);
}

pub struct GodRaysPlugin;

impl Plugin for GodRaysPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "god_rays.wgsl");
        let over = Overrides::from_env();
        app.insert_resource(over)
            .insert_resource(GodRays { enabled: !over.off, ..default() })
            .insert_resource(Climate::from_env())
            .init_resource::<Rain>()
            .add_plugins(ExtractResourcePlugin::<GodRays>::default())
            .add_systems(Update, (measure_water, drive, toggle));

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<GodRaysPipeline>>()
            .init_gpu_resource::<GodRaysBuffer>()
            .add_systems(RenderStartup, init_pipeline.after(MeshPipelineSystems))
            .add_systems(
                Render,
                (
                    prepare_pipelines.in_set(RenderSystems::Prepare),
                    prepare_uniform.in_set(RenderSystems::Prepare),
                    prepare_depth_usage
                        .in_set(RenderSystems::Prepare)
                        .before(prepare_core_3d_depth_textures),
                ),
            )
            .add_systems(
                Core3d,
                god_rays.after(Core3dSystems::MainPass).before(Core3dSystems::EarlyPostProcess),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sky::TICKS_PER_DAY;

    /// 15 Granite of year 100 at 07:00, which is the clock the approved shots
    /// were lit by: `DWARF_EYE_HOUR=7` pins the tick of day and keeps the date.
    fn dawn() -> Clock {
        Clock { year: 100, tick: 14 * TICKS_PER_DAY + 7 * TICKS_PER_DAY / 24, moon: None }
    }

    /// The scene the approved shots were framed at: that clock,
    /// `DWARF_EYE_CLOUDS=cumulus=0.5,fog=0.4`, over a temperate embark with a
    /// stream in the window and no rain for days.
    fn approved() -> Humidity {
        Humidity {
            fog: 0.4,
            cumulus: 0.5,
            rainfall: 0.5,
            water: 0.05,
            temperature: 0.5,
            hour: 7.0,
            elevation: dawn().sun_direction().y,
            ..default()
        }
    }

    /// The density the approved shots were rendered with, straight out of
    /// 27f5af3, which is what the model replaces.
    fn legacy(h: &Humidity) -> f32 {
        let low_sun = 1.0 - (h.elevation.clamp(0.0, 1.0) * 4.0).min(1.0);
        0.006 + 0.014 * low_sun + 0.033 * h.fog + 0.005 * h.stratus + 0.002 * h.cumulus
    }

    #[test]
    fn a_foggy_dawn_lands_where_the_approved_shots_did() {
        // 0.0211 against the shots' own 0.0207: two and a third per cent over.
        let h = approved();
        let (was, is) = (legacy(&h), h.density());
        assert!((is - was).abs() / was < 0.10, "{is} is not within a tenth of the approved {was}");
    }

    #[test]
    fn a_dry_noon_is_faint_and_a_desert_one_is_nearly_nothing() {
        let temperate = Humidity {
            rainfall: 0.5,
            water: 0.0,
            temperature: 0.6,
            hour: 12.0,
            elevation: 0.9,
            ..default()
        };
        let desert = Humidity { rainfall: 0.1, temperature: 0.85, ..temperate };
        assert!(temperate.density() < approved().density() / 3.0, "{}", temperate.density());
        assert!(desert.density() < temperate.density());
        assert!(desert.density() < 2.0 * HAZE_FLOOR, "{}", desert.density());
    }

    #[test]
    fn a_clear_dry_high_sun_is_the_floor() {
        let clear = Humidity { rainfall: 0.0, water: 0.0, temperature: 1.0, ..default() };
        assert_eq!(clear.density(), HAZE_FLOOR);
        // And no hour of the day can drive a dry sky below it.
        for hour in 0..24 {
            let h = Humidity { hour: hour as f32, ..clear };
            assert!(h.density() >= HAZE_FLOOR, "hour {hour}");
        }
    }

    #[test]
    fn every_input_moves_the_haze_the_way_it_should() {
        let base = approved();
        // Wetter, cloudier or darker air holds more.
        for (name, more) in [
            ("fog", Humidity { fog: 0.85, ..base }),
            ("stratus", Humidity { stratus: 0.75, ..base }),
            ("cumulus", Humidity { cumulus: 0.88, ..base }),
            ("countdown", Humidity { countdown: 1.0, ..base }),
            ("falling", Humidity { falling: 1.0, ..base }),
            ("rainfall", Humidity { rainfall: 1.0, ..base }),
            ("water", Humidity { water: 1.0, ..base }),
            ("a lower sun", Humidity { elevation: 0.0, ..base }),
        ] {
            assert!(more.density() > base.density(), "more {name} thinned the haze");
        }
        // A warm day burns it off; a cold one keeps it.
        assert!(Humidity { temperature: 1.0, ..base }.density() < base.density());
        assert!(Humidity { temperature: 0.0, ..base }.density() > base.density());

        // And the memory of the last shower fades the longer ago it was.
        let mut previous = f32::INFINITY;
        for minutes in [0.0, 10.0, 30.0, 90.0, 240.0, 1440.0] {
            let h = Humidity { since_rain: Some(minutes), ..base };
            assert!(h.density() < previous, "{minutes} minutes did not dry the air");
            previous = h.density();
        }
        // Never having rained is where a long dry spell is heading: a day out,
        // the memory of the shower has all but gone.
        let never = Humidity { since_rain: None, ..base }.density();
        assert!(never <= previous && previous - never < 1e-6, "{never} against {previous}");
        assert!(never < Humidity { since_rain: Some(240.0), ..base }.density());
    }

    #[test]
    fn the_haze_peaks_at_dawn_and_dusk() {
        let damp = Humidity { rainfall: 0.8, water: 0.1, elevation: 0.5, ..default() };
        let at = |hour| Humidity { hour, ..damp }.density();
        assert!(at(DAWN_HOUR) > at(12.0), "dawn is no thicker than noon");
        assert!(at(DUSK_HOUR) > at(12.0), "dusk is no thicker than noon");
        assert!(at(DAWN_HOUR) > at(3.0) && at(DAWN_HOUR) > at(9.0), "dawn is not a peak");
        assert!(at(DUSK_HOUR) > at(15.0) && at(DUSK_HOUR) > at(21.0), "dusk is not a peak");
        // Midnight is quiet: the peaks are the crossings, not the whole night.
        assert!(at(0.0) < at(DAWN_HOUR) && at(0.0) < at(DUSK_HOUR));
        // The peaks wrap the clock rather than running off either end.
        assert!((twilight(DAWN_HOUR) - 1.0).abs() < 1e-6);
        assert!((twilight(24.0 + DAWN_HOUR) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn f_cycles_the_forced_levels_and_comes_back_to_the_model() {
        let mut haze = Haze::default();
        assert_eq!(haze, Haze::Derived);
        for expected in [
            Haze::Forced(0.0),
            Haze::Forced(0.2),
            Haze::Forced(0.5),
            Haze::Forced(0.8),
            Haze::Derived,
        ] {
            haze = haze.next();
            assert_eq!(haze, expected);
        }
    }

    #[test]
    fn a_forced_level_stands_in_for_the_model_and_reads_as_forced() {
        let derived = approved().density();
        assert_eq!(Haze::Derived.density(derived), derived);
        assert_eq!(Haze::Forced(0.0).density(derived), 0.0);
        assert_eq!(Haze::Forced(0.5).density(derived), 0.5 * HAZE_FULL);
        // The approved dawn sits between the two levels either side of it.
        assert!(derived > Haze::Forced(0.2).density(derived));
        assert!(derived < Haze::Forced(0.5).density(derived));
        assert_eq!(Haze::Forced(0.5).describe(derived), "forced 0.5");
        assert!(Haze::Derived.describe(derived).starts_with("derived 0.0"));
    }

    #[test]
    fn the_moon_carries_the_shafts_at_a_fraction_of_the_suns() {
        // Midnight of a full moon: the sun is down, so the moon term is all
        // that is left, and it is a fraction rather than nothing.
        let midnight = Clock { year: 100, tick: 14 * TICKS_PER_DAY, moon: None };
        assert_eq!((midnight.sun_direction().y * 6.0).clamp(0.0, 1.0), 0.0);
        let moonlit = MOON_RAYS * midnight.moon_light();
        assert!(moonlit > 0.4 && moonlit < 1.0, "{moonlit}");

        // At noon the moon is down and the sun has it all.
        let noon = Clock { tick: midnight.tick + TICKS_PER_DAY / 2, ..midnight };
        assert_eq!(noon.moon_light(), 0.0);
        assert_eq!((noon.sun_direction().y * 6.0).clamp(0.0, 1.0), 1.0);
    }
}
