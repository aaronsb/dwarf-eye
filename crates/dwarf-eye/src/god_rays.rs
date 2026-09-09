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
//! The weather sets how thick the medium is: a clear day is a thin haze, fog
//! thickens it and pulls it down toward the ground. `DWARF_EYE_GODRAYS`
//! overrides any of it, and `G` toggles the pass.

use crate::clouds::Weather;
use crate::shadow::TerrainMaterial;
use crate::sky::Clock;
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

/// Everything the pass needs, gathered in the main world and handed straight
/// to the render world.
#[derive(Resource, Clone, ExtractResource)]
pub struct GodRays {
    /// Toggled with `G`.
    pub enabled: bool,
    /// Extinction per tile of the medium at ground level.
    pub density: f32,
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
            density: 0.002,
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

/// Follows the weather and the cloud bake.
///
/// The cloud terms come out of the terrain material rather than the bake
/// directly, so the shafts read exactly the map the ground is shaded with,
/// wind offset and all.
fn drive(
    weather: Res<Weather>,
    precip: Res<crate::precipitation::Precipitation>,
    clock: Res<Clock>,
    over: Res<Overrides>,
    terrain: Res<crate::TerrainMaterial>,
    materials: Res<Assets<TerrainMaterial>>,
    mut rays: ResMut<GodRays>,
) {
    let Weather { cumulus, stratus, cirrus: _, fog, countdown } = *weather;

    // A clear day is a thin haze, thicker in the first and last hours of
    // light when the air holds the night's moisture; fog thickens it and
    // pulls it to the ground. Rain arrives as stratus, which adds a little.
    //
    // `fog` is DF's own kind rather than a guess: 0.25 mist, 0.55 fog, 0.85
    // thick, straight off the region tile the Lua probe reads. The stratus
    // countdown rides with it, so the sheet's own build-up thickens the air
    // ahead of the change instead of the haze stepping when the kind flips, and
    // falling rain wets it on top of all of that (issue #18).
    let elevation = clock.sun_direction().y.clamp(0.0, 1.0);
    let low_sun = 1.0 - (elevation * 4.0).min(1.0);
    let (_, falling) = precip.drawn();
    rays.density = over.density.unwrap_or(
        0.006 + low_sun * 0.014 + fog * 0.033 + stratus * 0.005 + cumulus * 0.002
            + countdown * 0.004 + falling * 0.010,
    );
    let height = 60.0 - 44.0 * fog.clamp(0.0, 1.0);
    rays.falloff = over.falloff.unwrap_or(1.0 / height);
    rays.g = over.g.unwrap_or(0.6);
    rays.max_distance = over.distance.unwrap_or(600.0);
    rays.steps = over.steps.unwrap_or(32).clamp(4, 128);
    // Fog wants to be felt as fog, so it dims the scene behind the shafts more.
    rays.dim = over.dim.unwrap_or(0.15 + 0.55 * fog.clamp(0.0, 1.0));

    // No sun, no shafts.
    let sun = clock.sun_direction();
    let daylight = (sun.y * 6.0).clamp(0.0, 1.0);
    // Shafts read strongest when the sun is low and the light comes in sideways.
    rays.strength = over.strength.unwrap_or(0.45 + 0.35 * low_sun) * daylight;

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
            .add_plugins(ExtractResourcePlugin::<GodRays>::default())
            .add_systems(Update, (drive, toggle));

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
