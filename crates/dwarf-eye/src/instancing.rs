//! True GPU instancing for the far band's trees.
//!
//! A tree used to be an entity per stage, and 88k of them cost 5 fps at 4.5 M
//! triangles: the bound was per-entity CPU work — visibility ranges, transform
//! propagation, extraction — and not the triangles at all. Here a whole forest
//! is one storage buffer, one compute dispatch per view and one indirect draw
//! per (mesh, material) group, and the ECS holds nothing per tree.
//!
//! The three passes that matter all read the same instance buffer through the
//! same vertex shader: the main pass, the depth prepass (which drives occlusion
//! culling and the god rays' march) and the sun's shadow cascades. A custom
//! vertex shader that only the main pass runs leaves the depth buffer holding
//! untransformed geometry, which is the trap behind the old note that "a custom
//! vertex shader breaks the prepass".
//!
//! See `docs/architecture/lod/instancing.md`.

use std::num::NonZeroU64;

use bevy::asset::load_embedded_asset;
use bevy::core_pipeline::core_3d::{Opaque3d, Opaque3dBatchSetKey, Opaque3dBinKey};
use bevy::core_pipeline::prepass::{
    Opaque3dPrepass, OpaqueNoLightmap3dBatchSetKey, OpaqueNoLightmap3dBinKey,
};
use bevy::ecs::system::SystemParamItem;
use bevy::ecs::system::lifetimeless::{Read, SRes};
use bevy::math::{Mat4, Vec3, Vec4};
use bevy::mesh::{
    MeshVertexBufferLayoutRef, MeshVertexBufferLayouts, PrimitiveTopology, VertexBufferLayout,
};
use bevy::pbr::{
    LightEntity, MeshPipeline, MeshPipelineKey, MeshPipelineSystems, SetMeshViewBindGroup,
    SetMeshViewBindingArrayBindGroup, Shadow, ShadowBatchSetKey, ShadowBinKey, ViewKeyCache,
};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::mesh::allocator::MeshSlabs;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_phase::{
    BinnedRenderPhaseType, DrawFunctions, InputUniformIndex, PhaseItem, RenderCommand,
    RenderCommandResult, SetItemPipeline, TrackedRenderPass, ViewBinnedRenderPhases,
};
use bevy::render::render_resource::binding_types::{
    sampler, storage_buffer_read_only_sized, storage_buffer_sized, texture_2d,
    uniform_buffer_sized,
};
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayoutEntries, BindingResource, Buffer,
    BufferDescriptor, BufferInitDescriptor, BufferUsages, CachedComputePipelineId,
    CachedRenderPipelineId, ComputePassDescriptor, ComputePipelineDescriptor, IndexFormat,
    BindGroupLayoutDescriptor, MultisampleState, PipelineCache, RenderPipelineDescriptor,
    SamplerBindingType, ShaderStages, SpecializedMeshPipeline, SpecializedRenderPipeline,
    SpecializedRenderPipelines, TextureSampleType, VertexAttribute, VertexFormat,
    VertexStepMode,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderGraph, RenderQueue};
use bevy::render::sync_world::MainEntity;
use bevy::render::texture::GpuImage;
use bevy::render::view::{ExtractedView, RetainedViewEntity};
use bevy::render::{
    Extract, ExtractSchedule, GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems,
};
use bevy::shader::{Shader, ShaderDefVal};
use bytemuck::{Pod, Zeroable};

use crate::shadow::ShadowUniform;

/// Bevy's mesh pipeline fixes the vertex attribute locations, and the shader
/// defs it hands out are keyed to them.
const POSITION: u32 = 0;
const NORMAL: u32 = 1;
const UV: u32 = 2;
const COLOR: u32 = 5;

/// Uniform buffers with dynamic offsets are aligned to the device's own
/// minimum, which is 256 bytes everywhere that matters.
const BATCH_STRIDE: u64 = 256;

/// Five `u32`s per `DrawIndexedIndirectArgs`.
const ARGS_STRIDE: u64 = 20;

/// One thread per instance, in workgroups of this size (`cull.wgsl`).
const WORKGROUP: u32 = 64;

/// Where the crossfade rule puts a stage that never fades out.
pub const FAR: f32 = 40000.0;

// ---------------------------------------------------------------------------
// What the main world hands over
// ---------------------------------------------------------------------------

/// One placed instance, as the CPU builds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Instance {
    pub pos: Vec3,
    pub yaw: f32,
    pub scale: f32,
    /// Bounding sphere of the scaled mesh: centre `centre_y` above `pos`.
    pub radius: f32,
    pub centre_y: f32,
    /// A stable threshold in 0..1. Every stage of one tree shares it, which is
    /// what makes exactly one stage draw that tree.
    pub dither: f32,
    /// Fade-in start and end, then fade-out start and end, in tiles.
    pub band: [f32; 4],
    pub tint: [f32; 4],
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            pos: Vec3::ZERO,
            yaw: 0.0,
            scale: 1.0,
            radius: 1.0,
            centre_y: 0.0,
            dither: 0.0,
            band: [0.0, 0.0, FAR, FAR],
            tint: [1.0; 4],
        }
    }
}

/// One (mesh, material) group and everywhere it stands.
#[derive(Clone, Default)]
pub struct Batch {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub instances: Vec<Instance>,
    /// How much of the sky's fill this stage's foliage keeps. Zero where the
    /// vertex colours already carry a lit top and darker sides.
    pub canopy: f32,
    pub roughness: f32,
    /// Whether this stage goes through the shadow cascades. A stage wholly past
    /// them casts a blob instead and would only cost four cascades' work.
    pub casts: bool,
}

impl Batch {
    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Everything the instanced path draws, replaced whole whenever the horizon is
/// rebuilt.
#[derive(Resource, Default)]
pub struct Instanced {
    pub batches: Vec<Batch>,
    /// Bumped on every replacement, so the render world knows to re-upload.
    pub generation: u64,
    /// The leaf surface every stage wears, plus the cloud shadow bake and the
    /// block mask the horizon material carries.
    pub leaf: Option<Handle<Image>>,
    pub cloud: Option<Handle<Image>>,
    pub mask: Option<Handle<Image>>,
    /// The weather and mask terms, shared with `shadow.rs:ShadowUniform`.
    pub scene: ShadowUniform,
}

impl Instanced {
    /// Replaces the whole set. Nothing is diffed: a window move rebuilds the
    /// scatter anyway, and one upload beats tracking which tree moved.
    pub fn replace(&mut self, batches: Vec<Batch>) {
        self.batches = batches;
        self.generation += 1;
    }

    pub fn instances(&self) -> usize {
        self.batches.iter().map(|b| b.instances.len()).sum()
    }

    fn ready(&self) -> bool {
        self.leaf.is_some() && self.cloud.is_some() && self.mask.is_some()
    }
}

/// What the HUD reports: how many instances exist and how many the camera's own
/// cull keeps.
///
/// Mirrored on the CPU from the same rule the compute shader runs
/// ([`survives`]) rather than read back off the GPU: a readback either stalls
/// the frame or arrives late, and the number is only ever read by a human. The
/// tests pin the two rules against each other.
#[derive(Resource, Default, Clone, Copy)]
pub struct InstanceCounts {
    pub instances: usize,
    pub drawn: usize,
    pub triangles: usize,
}

impl InstanceCounts {
    pub fn culled(&self) -> usize {
        self.instances.saturating_sub(self.drawn)
    }
}

// ---------------------------------------------------------------------------
// The cull rule, in one place
// ---------------------------------------------------------------------------

/// How far into a crossfade a distance falls, 0 before it and 1 after.
///
/// `cull.wgsl` runs the same function. The margins are the log-spaced ones
/// `main.rs:band_fades` hands Bevy's `VisibilityRange`, so the ramp inside one
/// is linear for the same reason Bevy's is.
pub fn ramp(d: f32, from: f32, to: f32) -> f32 {
    if to <= from {
        return if d >= to { 1.0 } else { 0.0 };
    }
    ((d - from) / (to - from)).clamp(0.0, 1.0)
}

/// Whether an instance draws at its own stage, at this distance from the eye.
///
/// One tree draws at exactly one stage. The threshold is the tree's own and is
/// shared by all of its stages; the fade ramps are monotonic and ordered
/// outward, so exactly one stage has faded in past the threshold without also
/// having faded out past it.
pub fn in_band(inst: &Instance, d: f32) -> bool {
    let fade_in = ramp(d, inst.band[0], inst.band[1]);
    let fade_out = ramp(d, inst.band[2], inst.band[3]);
    fade_in > inst.dither && inst.dither >= fade_out
}

/// Whether an instance's bounding sphere is anywhere inside the frustum.
///
/// A degenerate plane — an infinite reversed-Z projection has no far plane —
/// is skipped rather than failing everything.
pub fn in_frustum(inst: &Instance, planes: &[Vec4; 6]) -> bool {
    let centre = inst.pos + Vec3::new(0.0, inst.centre_y, 0.0);
    planes.iter().all(|p| {
        p.truncate().length_squared() < 0.5 || p.truncate().dot(centre) + p.w >= -inst.radius
    })
}

/// The whole test, frustum then band: what one thread of `cull.wgsl` decides.
pub fn survives(inst: &Instance, planes: &[Vec4; 6], eye: Vec3) -> bool {
    let centre = inst.pos + Vec3::new(0.0, inst.centre_y, 0.0);
    in_frustum(inst, planes) && in_band(inst, centre.distance(eye))
}

/// The six world-space frustum planes of a view, inward-facing and normalised.
///
/// Gribb–Hartmann off the clip-from-world matrix, which works whatever the
/// depth convention is: the near and far rows come out degenerate under an
/// infinite reversed-Z projection, and [`in_frustum`] skips those.
pub fn frustum_planes(clip_from_world: Mat4) -> [Vec4; 6] {
    // glam is column-major and `clip = M * world`, so the row that produces
    // clip component i is `M.row(i)`: no transpose.
    let m = clip_from_world;
    let mut planes = [
        m.row(3) + m.row(0),
        m.row(3) - m.row(0),
        m.row(3) + m.row(1),
        m.row(3) - m.row(1),
        m.row(2),
        m.row(3) - m.row(2),
    ];
    for plane in &mut planes {
        let n = plane.truncate().length();
        *plane = if n > 1e-6 { *plane / n } else { Vec4::ZERO };
    }
    planes
}

/// A stable threshold in 0..1 for a tree at this position.
///
/// Hashed from the position rather than from an index, so every stage of one
/// tree agrees without the stages having to be built in the same order.
pub fn dither_of(pos: Vec3) -> f32 {
    let mut h = 0x9e37_79b9u32;
    for v in [pos.x, pos.y, pos.z] {
        h ^= (v * 64.0).round() as i32 as u32;
        h = h.wrapping_mul(0x85eb_ca6b);
        h ^= h >> 13;
    }
    (h >> 8) as f32 / (1u32 << 24) as f32
}

// ---------------------------------------------------------------------------
// GPU records
// ---------------------------------------------------------------------------

/// 80 bytes, matching `instance_common.wgsl:Instance`.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
pub struct GpuInstance {
    pos: [f32; 3],
    yaw: f32,
    scale: f32,
    radius: f32,
    centre_y: f32,
    dither: f32,
    band: [f32; 4],
    tint: [f32; 4],
    batch: u32,
    _pad: [u32; 3],
}

impl GpuInstance {
    fn of(inst: &Instance, batch: u32) -> Self {
        Self {
            pos: inst.pos.to_array(),
            yaw: inst.yaw,
            scale: inst.scale,
            radius: inst.radius,
            centre_y: inst.centre_y,
            dither: inst.dither,
            band: inst.band,
            tint: inst.tint,
            batch,
            _pad: [0; 3],
        }
    }
}

/// 16 bytes, matching `cull.wgsl:BatchSpan`.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
pub struct GpuSpan {
    first: u32,
    count: u32,
    visible_at: u32,
    _pad: u32,
}

/// Matching `instance_common.wgsl:Batch`. Written at [`BATCH_STRIDE`].
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
pub struct GpuBatch {
    first: u32,
    count: u32,
    visible_at: u32,
    flags: u32,
    canopy: f32,
    roughness: f32,
    _pad: [f32; 2],
}

/// Matching `instance_common.wgsl:ViewParams`.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
pub struct GpuView {
    clip_from_world: [f32; 16],
    planes: [[f32; 4]; 6],
    eye: [f32; 3],
    count: u32,
    frustum: u32,
    _pad: [u32; 3],
}

/// Matching `instance_common.wgsl:Scene`.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
pub struct GpuScene {
    sun: [f32; 3],
    strength: f32,
    wind: [f32; 2],
    period: f32,
    ground: f32,
    enabled: f32,
    wet: f32,
    polish: f32,
    snow: f32,
    mask_origin: [f32; 2],
    mask_on: f32,
    _pad: f32,
}

impl GpuScene {
    fn of(u: &ShadowUniform) -> Self {
        Self {
            sun: u.sun.to_array(),
            strength: u.strength,
            wind: u.wind.to_array(),
            period: if u.period > 0.0 { u.period } else { 1.0 },
            ground: u.ground,
            enabled: u.enabled,
            wet: u.wet,
            polish: u.polish,
            snow: u.snow,
            mask_origin: u.mask_origin.to_array(),
            mask_on: if u.horizon > 0.5 { 1.0 } else { 0.0 },
            _pad: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Render world state
// ---------------------------------------------------------------------------

/// The scatter as it crosses into the render world. Cloned only when the
/// generation changes: the buffers are megabytes and the scatter only moves
/// when the window does.
#[derive(Resource, Default)]
struct Staged {
    generation: u64,
    batches: Vec<Batch>,
    fresh: bool,
    scene: GpuScene,
    leaf: Option<Handle<Image>>,
    cloud: Option<Handle<Image>>,
    mask: Option<Handle<Image>>,
}

/// What the render world holds for the whole scatter: one vertex and one index
/// buffer for every mesh, one instance record for every tree.
#[derive(Resource, Default)]
struct Store {
    generation: u64,
    buffers: Option<StoreBuffers>,
    draws: Vec<DrawSpan>,
    instances: u32,
    /// Zeroed draw arguments, re-uploaded each frame before the cull fills the
    /// instance counts in.
    args_template: Vec<u8>,
}

struct StoreBuffers {
    positions: Buffer,
    normals: Buffer,
    uvs: Buffer,
    colors: Buffer,
    indices: Buffer,
    instances: Buffer,
    spans: Buffer,
    batches: Buffer,
    scene: Buffer,
}

#[derive(Clone, Copy)]
struct DrawSpan {
    index_count: u32,
    first_index: u32,
    base_vertex: i32,
    casts: bool,
}

/// Per-view buffers, kept across frames so a view's allocation is not churned.
#[derive(Resource, Default)]
struct ViewStore(HashMap<RetainedViewEntity, ViewBuffers>);

struct ViewBuffers {
    generation: u64,
    args: Buffer,
    visible: Buffer,
    params: Buffer,
}

/// What a draw needs from the view it is drawing for.
#[derive(Component, Clone)]
struct ViewInstances {
    args: Buffer,
    draw: BindGroup,
}

/// What the cull dispatch needs.
#[derive(Component, Clone)]
struct ViewCull {
    bind_group: BindGroup,
    workgroups: u32,
}

/// Which pipelines a view draws with.
#[derive(Component, Clone, Copy)]
struct ViewInstancePipelines {
    main: Option<CachedRenderPipelineId>,
    prepass: Option<CachedRenderPipelineId>,
    shadow: Option<CachedRenderPipelineId>,
}

// ---------------------------------------------------------------------------
// Pipelines
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct InstancePipelines {
    mesh: MeshPipeline,
    layout: BindGroupLayoutDescriptor,
    shader: Handle<Shader>,
    /// A mesh layout with exactly our four attributes, so Bevy's own
    /// specialisation hands out the shader defs its view layout was built with.
    vertex_layout: MeshVertexBufferLayoutRef,
    cull: CachedComputePipelineId,
    cull_layout: BindGroupLayoutDescriptor,
}

/// Which pass a pipeline is for.
///
/// The main pass takes Bevy's view bindings at groups 0 and 1 — the environment
/// map and the clustered decals live in group 1, and the canopy's sky term is
/// the environment map's own fill scaled down, so dropping it would light an
/// instanced tree differently from a window one. The instance group therefore
/// goes at 2, which is where the mesh bind group a custom pipeline does not use
/// would have been. The depth passes need no view bindings at all: the vertex
/// shader takes its clip matrix from the instance group's own per-view uniform.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pass {
    Main,
    Depth,
}

impl Pass {
    fn group(self) -> u32 {
        match self {
            Pass::Main => 2,
            Pass::Depth => 0,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct InstanceKey {
    pass: Pass,
    mesh_key: MeshPipelineKey,
}

/// A view's own key, plus the one thing a view key never carries: what topology
/// the geometry is. `MeshPipelineKey::primitive_topology` reads zero as
/// `PointList`, which draws a crown as sixty scattered dots.
fn with_topology(key: MeshPipelineKey) -> MeshPipelineKey {
    key | MeshPipelineKey::from_primitive_topology_and_strip_index(
        PrimitiveTopology::TriangleList,
        None,
    )
}

/// The four vertex buffers, one per attribute, in `MeshData`'s own shape.
fn vertex_buffers() -> Vec<VertexBufferLayout> {
    let one = |location: u32, format: VertexFormat| VertexBufferLayout {
        array_stride: format.size(),
        step_mode: VertexStepMode::Vertex,
        attributes: vec![VertexAttribute { format, offset: 0, shader_location: location }],
    };
    vec![
        one(POSITION, VertexFormat::Float32x3),
        one(NORMAL, VertexFormat::Float32x3),
        one(UV, VertexFormat::Float32x2),
        one(COLOR, VertexFormat::Float32x4),
    ]
}

impl SpecializedRenderPipeline for InstancePipelines {
    type Key = InstanceKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        // Start from Bevy's own mesh pipeline, so every shader def its view
        // layout was built with is present and the two cannot drift. Only the
        // shaders, the vertex buffers and the third bind group are ours.
        let mut descriptor = self
            .mesh
            .specialize(key.mesh_key, &self.vertex_layout)
            .expect("the mesh pipeline accepts our four attributes");
        let group = ShaderDefVal::UInt("INSTANCE_BIND_GROUP".into(), key.pass.group());
        descriptor.vertex.buffers = vertex_buffers();
        descriptor.vertex.shader = self.shader.clone();
        descriptor.vertex.entry_point = Some("vertex".into());
        descriptor.vertex.shader_defs.push(group.clone());
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader = self.shader.clone();
            fragment.entry_point = Some("fragment".into());
            fragment.shader_defs.push(group.clone());
        }
        match key.pass {
            Pass::Main => {
                descriptor.label = Some("instanced crowns".into());
                descriptor.vertex.shader_defs.push("MAIN_PASS".into());
                if let Some(fragment) = descriptor.fragment.as_mut() {
                    fragment.shader_defs.push("MAIN_PASS".into());
                }
                // Groups 0 and 1 stay Bevy's; the mesh group at 2 becomes ours.
                descriptor.layout.truncate(2);
                descriptor.layout.push(self.layout.clone());
            }
            Pass::Depth => {
                descriptor.label = Some("instanced crowns (depth)".into());
                descriptor.layout = vec![self.layout.clone()];
                // Depth only: the fragment stage exists so the block mask can
                // discard, and writes no colour.
                if let Some(fragment) = descriptor.fragment.as_mut() {
                    fragment.targets = vec![];
                    fragment.shader_defs = vec![group];
                }
                descriptor.vertex.shader_defs = vec![ShaderDefVal::UInt(
                    "INSTANCE_BIND_GROUP".into(),
                    key.pass.group(),
                )];
                if key.mesh_key.msaa_samples() == 1 {
                    descriptor.multisample = MultisampleState::default();
                }
            }
        }
        descriptor
    }
}

fn instance_layout() -> BindGroupLayoutDescriptor {
    let texture = || texture_2d(TextureSampleType::Float { filterable: true });
    let entries = BindGroupLayoutEntries::with_indices(
            ShaderStages::VERTEX_FRAGMENT,
            (
                (0, storage_buffer_read_only_sized(false, None)),
                (1, storage_buffer_read_only_sized(false, None)),
                (
                    2,
                    uniform_buffer_sized(
                        true,
                        NonZeroU64::new(std::mem::size_of::<GpuBatch>() as u64),
                    ),
                ),
                (3, uniform_buffer_sized(false, None)),
                (4, uniform_buffer_sized(false, None)),
                (5, texture().visibility(ShaderStages::FRAGMENT)),
                (
                    6,
                    sampler(SamplerBindingType::Filtering).visibility(ShaderStages::FRAGMENT),
                ),
                (7, texture().visibility(ShaderStages::FRAGMENT)),
                (
                    8,
                    sampler(SamplerBindingType::Filtering).visibility(ShaderStages::FRAGMENT),
                ),
                (9, texture().visibility(ShaderStages::FRAGMENT)),
            ),
    );
    BindGroupLayoutDescriptor::new("instanced crowns", &entries)
}

fn cull_layout() -> BindGroupLayoutDescriptor {
    let entries = BindGroupLayoutEntries::with_indices(
            ShaderStages::COMPUTE,
            (
                (0, storage_buffer_read_only_sized(false, None)),
                (1, storage_buffer_sized(false, None)),
                (2, storage_buffer_read_only_sized(false, None)),
                (3, uniform_buffer_sized(false, None)),
                (4, storage_buffer_sized(false, None)),
            ),
    );
    BindGroupLayoutDescriptor::new("instance cull", &entries)
}

fn init_pipelines(
    mut commands: Commands,
    mesh_pipeline: Res<MeshPipeline>,
    pipeline_cache: Res<PipelineCache>,
    mut layouts: ResMut<MeshVertexBufferLayouts>,
    asset_server: Res<AssetServer>,
) {
    // A mesh carrying exactly our four attributes, only so Bevy's own
    // specialisation can be asked what shader defs they call for.
    let mut probe = Mesh::new(
        bevy::mesh::PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    probe.insert_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new());
    probe.insert_attribute(Mesh::ATTRIBUTE_NORMAL, Vec::<[f32; 3]>::new());
    probe.insert_attribute(Mesh::ATTRIBUTE_UV_0, Vec::<[f32; 2]>::new());
    probe.insert_attribute(Mesh::ATTRIBUTE_COLOR, Vec::<[f32; 4]>::new());
    let vertex_layout = probe.get_mesh_vertex_buffer_layout(&mut layouts);

    let cull_layout = cull_layout();
    let shader: Handle<Shader> = load_embedded_asset!(asset_server.as_ref(), "instancing.wgsl");
    let cull = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some("instance cull".into()),
        layout: vec![cull_layout.clone()],
        shader: shader.clone(),
        shader_defs: vec![
            ShaderDefVal::UInt("INSTANCE_BIND_GROUP".into(), 0),
            "CULL_PASS".into(),
        ],
        entry_point: Some("cull".into()),
        ..default()
    });

    commands.insert_resource(InstancePipelines {
        mesh: mesh_pipeline.clone(),
        layout: instance_layout(),
        shader,
        vertex_layout,
        cull,
        cull_layout,
    });
}

// ---------------------------------------------------------------------------
// Extract and prepare
// ---------------------------------------------------------------------------

fn extract(mut staged: ResMut<Staged>, source: Extract<Res<Instanced>>) {
    staged.scene = GpuScene::of(&source.scene);
    staged.leaf = source.leaf.clone();
    staged.cloud = source.cloud.clone();
    staged.mask = source.mask.clone();
    staged.fresh = source.generation != staged.generation && source.ready();
    if staged.fresh {
        staged.generation = source.generation;
        staged.batches = source.batches.clone();
    }
}

fn prepare_store(
    mut store: ResMut<Store>,
    staged: Res<Staged>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    if staged.fresh {
        rebuild(&mut store, &staged, &device);
    }
    let Some(buffers) = store.buffers.as_ref() else { return };
    queue.write_buffer(&buffers.scene, 0, bytemuck::bytes_of(&staged.scene));
}

/// One vertex buffer per attribute and one index buffer for the whole scatter,
/// with a batch's mesh addressed by `first_index` and `base_vertex`: the draw
/// binds geometry once and then issues one indirect call per batch.
fn rebuild(store: &mut Store, staged: &Staged, device: &RenderDevice) {
    let (mut positions, mut normals, mut uvs, mut colors, mut indices) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut instances: Vec<GpuInstance> = Vec::new();
    let mut spans: Vec<GpuSpan> = Vec::new();
    let mut batches: Vec<u8> = Vec::new();
    let mut draws = Vec::new();
    let mut args: Vec<u8> = Vec::new();

    for (index, batch) in staged.batches.iter().enumerate() {
        let base_vertex = positions.len() as i32;
        let first_index = indices.len() as u32;
        positions.extend_from_slice(&batch.positions);
        normals.extend_from_slice(&batch.normals);
        uvs.extend_from_slice(&batch.uvs);
        colors.extend_from_slice(&batch.colors);
        indices.extend_from_slice(&batch.indices);

        let first = instances.len() as u32;
        instances.extend(batch.instances.iter().map(|i| GpuInstance::of(i, index as u32)));
        let count = batch.instances.len() as u32;
        spans.push(GpuSpan { first, count, visible_at: first, _pad: 0 });

        let record = GpuBatch {
            first,
            count,
            visible_at: first,
            flags: 0,
            canopy: batch.canopy,
            roughness: batch.roughness,
            _pad: [0.0; 2],
        };
        let mut padded = [0u8; BATCH_STRIDE as usize];
        padded[..std::mem::size_of::<GpuBatch>()].copy_from_slice(bytemuck::bytes_of(&record));
        batches.extend_from_slice(&padded);

        draws.push(DrawSpan {
            index_count: batch.indices.len() as u32,
            first_index,
            base_vertex,
            casts: batch.casts,
        });
        // index_count, instance_count, first_index, base_vertex, first_instance.
        // The instance count is what the cull fills in.
        for word in [batch.indices.len() as u32, 0, first_index, base_vertex as u32, 0] {
            args.extend_from_slice(&word.to_le_bytes());
        }
    }

    if positions.is_empty() || instances.is_empty() {
        store.buffers = None;
        store.draws.clear();
        store.instances = 0;
        store.generation = staged.generation;
        return;
    }

    let vertex = |label: &str, data: &[u8]| {
        device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some(label),
            contents: data,
            usage: BufferUsages::VERTEX,
        })
    };
    let storage = |label: &str, data: &[u8]| {
        device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some(label),
            contents: data,
            usage: BufferUsages::STORAGE,
        })
    };

    store.buffers = Some(StoreBuffers {
        positions: vertex("crown positions", bytemuck::cast_slice(&positions)),
        normals: vertex("crown normals", bytemuck::cast_slice(&normals)),
        uvs: vertex("crown uvs", bytemuck::cast_slice(&uvs)),
        colors: vertex("crown colors", bytemuck::cast_slice(&colors)),
        indices: device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("crown indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: BufferUsages::INDEX,
        }),
        instances: storage("crown instances", bytemuck::cast_slice(&instances)),
        spans: storage("crown spans", bytemuck::cast_slice(&spans)),
        batches: device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("crown batches"),
            contents: &batches,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        }),
        scene: device.create_buffer(&BufferDescriptor {
            label: Some("crown scene"),
            size: std::mem::size_of::<GpuScene>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
    });
    store.instances = instances.len() as u32;
    store.draws = draws;
    store.args_template = args;
    store.generation = staged.generation;
}

fn prepare_view_pipelines(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<InstancePipelines>>,
    instance_pipelines: Res<InstancePipelines>,
    view_keys: Res<ViewKeyCache>,
    views: Query<(Entity, &ExtractedView, Option<&LightEntity>)>,
) {
    for (entity, view, light) in views.iter() {
        let mut set =
            ViewInstancePipelines { main: None, prepass: None, shadow: None };
        if light.is_some() {
            // A cascade draws depth only, into a single-sampled shadow map.
            set.shadow = Some(pipelines.specialize(
                &pipeline_cache,
                &instance_pipelines,
                InstanceKey {
                    pass: Pass::Depth,
                    mesh_key: with_topology(MeshPipelineKey::empty()),
                },
            ));
        } else if let Some(key) = view_keys.get(&view.retained_view_entity) {
            set.main = Some(pipelines.specialize(
                &pipeline_cache,
                &instance_pipelines,
                InstanceKey { pass: Pass::Main, mesh_key: with_topology(*key) },
            ));
            set.prepass = Some(pipelines.specialize(
                &pipeline_cache,
                &instance_pipelines,
                InstanceKey {
                    pass: Pass::Depth,
                    mesh_key: with_topology(MeshPipelineKey::from_msaa_samples(
                        key.msaa_samples(),
                    )),
                },
            ));
        }
        commands.entity(entity).insert(set);
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_view_instances(
    mut commands: Commands,
    mut view_store: ResMut<ViewStore>,
    store: Res<Store>,
    staged: Res<Staged>,
    pipelines: Res<InstancePipelines>,
    pipeline_cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    images: Res<RenderAssets<GpuImage>>,
    views: Query<(Entity, &ExtractedView)>,
) {
    let draw_layout = pipeline_cache.get_bind_group_layout(&pipelines.layout);
    let cull_layout = pipeline_cache.get_bind_group_layout(&pipelines.cull_layout);
    let Some(buffers) = store.buffers.as_ref() else { return };
    let (Some(leaf), Some(cloud), Some(mask)) = (
        staged.leaf.as_ref().and_then(|h| images.get(h)),
        staged.cloud.as_ref().and_then(|h| images.get(h)),
        staged.mask.as_ref().and_then(|h| images.get(h)),
    ) else {
        return;
    };

    for (entity, view) in views.iter() {
        let held = view_store.0.entry(view.retained_view_entity).or_insert_with(|| ViewBuffers {
            generation: u64::MAX,
            args: empty_buffer(&device),
            visible: empty_buffer(&device),
            params: empty_buffer(&device),
        });
        if held.generation != store.generation {
            held.args = device.create_buffer(&BufferDescriptor {
                label: Some("crown draw args"),
                size: (store.draws.len() as u64 * ARGS_STRIDE).max(ARGS_STRIDE),
                usage: BufferUsages::STORAGE | BufferUsages::INDIRECT | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            held.visible = device.create_buffer(&BufferDescriptor {
                label: Some("crown visible list"),
                size: (store.instances as u64 * 4).max(4),
                usage: BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            held.params = device.create_buffer(&BufferDescriptor {
                label: Some("crown view params"),
                size: std::mem::size_of::<GpuView>() as u64,
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            held.generation = store.generation;
        }

        // The instance counts start at zero every frame; the cull's atomic adds
        // are what fill them in.
        queue.write_buffer(&held.args, 0, &store.args_template);
        queue.write_buffer(&held.params, 0, bytemuck::bytes_of(&view_params(view, &store)));

        let draw = device.create_bind_group(
            "crown draw",
            &draw_layout,
            &BindGroupEntries::with_indices((
                (0, buffers.instances.as_entire_binding()),
                (1, held.visible.as_entire_binding()),
                (
                    2,
                    BindingResource::Buffer(bevy::render::render_resource::BufferBinding {
                        buffer: &buffers.batches,
                        offset: 0,
                        size: NonZeroU64::new(std::mem::size_of::<GpuBatch>() as u64),
                    }),
                ),
                (3, held.params.as_entire_binding()),
                (4, buffers.scene.as_entire_binding()),
                (5, &leaf.texture_view),
                (6, &leaf.sampler),
                (7, &cloud.texture_view),
                (8, &cloud.sampler),
                (9, &mask.texture_view),
            )),
        );
        let cull = device.create_bind_group(
            "crown cull",
            &cull_layout,
            &BindGroupEntries::with_indices((
                (0, buffers.instances.as_entire_binding()),
                (1, held.visible.as_entire_binding()),
                (2, buffers.spans.as_entire_binding()),
                (3, held.params.as_entire_binding()),
                (4, held.args.as_entire_binding()),
            )),
        );

        commands.entity(entity).insert((
            ViewInstances { args: held.args.clone(), draw },
            ViewCull {
                bind_group: cull,
                workgroups: store.instances.div_ceil(WORKGROUP).max(1),
            },
        ));
    }
}

fn empty_buffer(device: &RenderDevice) -> Buffer {
    device.create_buffer(&BufferDescriptor {
        label: Some("crown placeholder"),
        size: 4,
        usage: BufferUsages::STORAGE | BufferUsages::INDIRECT | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// The frustum and the eye the cull tests against, for one view.
///
/// A shadow cascade's own clip matrix is what its cull uses, so a tree behind
/// the camera is dropped from the main pass and still casts into the map.
fn view_params(view: &ExtractedView, store: &Store) -> GpuView {
    let clip_from_world = view
        .clip_from_world
        .unwrap_or_else(|| view.clip_from_view * view.world_from_view.to_matrix().inverse());
    GpuView {
        clip_from_world: clip_from_world.to_cols_array(),
        planes: frustum_planes(clip_from_world).map(|p| p.to_array()),
        eye: view.world_from_view.translation().to_array(),
        count: store.instances,
        frustum: 1,
        _pad: [0; 3],
    }
}

// ---------------------------------------------------------------------------
// Cull
// ---------------------------------------------------------------------------

/// One dispatch per view, before any pass runs: the camera and every shadow
/// cascade get their own compacted list and their own draw arguments.
fn cull(
    pipeline_cache: Res<PipelineCache>,
    pipelines: Res<InstancePipelines>,
    views: Query<&ViewCull>,
    mut ctx: RenderContext,
) {
    let Some(pipeline) = pipeline_cache.get_compute_pipeline(pipelines.cull) else { return };
    let mut pass = ctx
        .command_encoder()
        .begin_compute_pass(&ComputePassDescriptor { label: Some("instance cull"), ..default() });
    pass.set_pipeline(pipeline);
    for view in views.iter() {
        pass.set_bind_group(0, &view.bind_group, &[]);
        pass.dispatch_workgroups(view.workgroups, 1, 1);
    }
}

// ---------------------------------------------------------------------------
// Queue and draw
// ---------------------------------------------------------------------------

/// One phase item per view per pass. The item stands for the whole scatter: the
/// draw command walks the batches itself, so a forest costs three phase items
/// rather than three per species.
fn queue_instances(
    store: Res<Store>,
    opaque_functions: Res<DrawFunctions<Opaque3d>>,
    prepass_functions: Res<DrawFunctions<Opaque3dPrepass>>,
    shadow_functions: Res<DrawFunctions<Shadow>>,
    mut opaque: ResMut<ViewBinnedRenderPhases<Opaque3d>>,
    mut prepass: ResMut<ViewBinnedRenderPhases<Opaque3dPrepass>>,
    mut shadows: ResMut<ViewBinnedRenderPhases<Shadow>>,
    views: Query<(Entity, &ExtractedView, &ViewInstancePipelines)>,
) {
    if store.draws.is_empty() {
        return;
    }
    let opaque_draw = opaque_functions.read().id::<DrawInstancedMain>();
    let prepass_draw = prepass_functions.read().id::<DrawInstancedDepth>();
    let shadow_draw = shadow_functions.read().id::<DrawInstancedShadow>();
    // A synthetic asset id, distinct from any mesh, so the bin holds our item
    // apart from Bevy's own.
    let asset_id = AssetId::<Mesh>::invalid().untyped();

    for (entity, view, set) in views.iter() {
        let key = view.retained_view_entity;
        // The item stands for the whole scatter rather than for one tree, so it
        // needs no main-world entity of its own; the draw command walks the
        // batches itself.
        let holder = (entity, MainEntity::from(Entity::PLACEHOLDER));
        if let (Some(pipeline), Some(phase)) = (set.main, opaque.get_mut(&key)) {
            phase.add(
                Opaque3dBatchSetKey {
                    pipeline,
                    draw_function: opaque_draw,
                    material_bind_group_index: None,
                    slabs: MeshSlabs::default(),
                    lightmap_slab: None,
                },
                Opaque3dBinKey { asset_id },
                holder,
                InputUniformIndex(0),
                BinnedRenderPhaseType::NonMesh,
            );
        }
        if let (Some(pipeline), Some(phase)) = (set.prepass, prepass.get_mut(&key)) {
            phase.add(
                OpaqueNoLightmap3dBatchSetKey {
                    pipeline,
                    draw_function: prepass_draw,
                    material_bind_group_index: None,
                    slabs: MeshSlabs::default(),
                },
                OpaqueNoLightmap3dBinKey { asset_id },
                holder,
                InputUniformIndex(0),
                BinnedRenderPhaseType::NonMesh,
            );
        }
        if let (Some(pipeline), Some(phase)) = (set.shadow, shadows.get_mut(&key)) {
            phase.add(
                ShadowBatchSetKey {
                    pipeline,
                    draw_function: shadow_draw,
                    material_bind_group_index: None,
                    slabs: MeshSlabs::default(),
                },
                ShadowBinKey { asset_id },
                holder,
                InputUniformIndex(0),
                BinnedRenderPhaseType::NonMesh,
            );
        }
    }
}

/// The whole scatter in one command: bind the geometry once, then one indirect
/// call per batch with the batch's own uniform selected by a dynamic offset.
///
/// `SHADOWS` drops the batches that do not go through the cascades, which is
/// where the old `NotShadowCaster` on the far stages lives now.
struct DrawInstanced<const GROUP: u32, const SHADOWS: bool>;

impl<P: PhaseItem, const GROUP: u32, const SHADOWS: bool> RenderCommand<P>
    for DrawInstanced<GROUP, SHADOWS>
{
    type Param = SRes<Store>;
    type ViewQuery = Read<ViewInstances>;
    type ItemQuery = ();

    fn render<'w>(
        _item: &P,
        view: bevy::ecs::query::ROQueryItem<'w, '_, Self::ViewQuery>,
        _entity: Option<()>,
        store: SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let store = store.into_inner();
        let Some(buffers) = store.buffers.as_ref() else {
            return RenderCommandResult::Skip;
        };
        pass.set_vertex_buffer(0, buffers.positions.slice(..));
        pass.set_vertex_buffer(1, buffers.normals.slice(..));
        pass.set_vertex_buffer(2, buffers.uvs.slice(..));
        pass.set_vertex_buffer(3, buffers.colors.slice(..));
        pass.set_index_buffer(buffers.indices.slice(..), IndexFormat::Uint32);
        for (index, draw) in store.draws.iter().enumerate() {
            if SHADOWS && !draw.casts {
                continue;
            }
            pass.set_bind_group(
                GROUP as usize,
                &view.draw,
                &[index as u32 * BATCH_STRIDE as u32],
            );
            pass.draw_indexed_indirect(&view.args, index as u64 * ARGS_STRIDE);
        }
        RenderCommandResult::Success
    }
}

type DrawInstancedMain = (
    SetItemPipeline,
    SetMeshViewBindGroup<0>,
    SetMeshViewBindingArrayBindGroup<1>,
    DrawInstanced<2, false>,
);

type DrawInstancedDepth = (SetItemPipeline, DrawInstanced<0, false>);
type DrawInstancedShadow = (SetItemPipeline, DrawInstanced<0, true>);

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct InstancingPlugin;

impl Plugin for InstancingPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "instancing.wgsl");
        app.init_resource::<Instanced>()
            .init_resource::<InstanceCounts>()
            .add_systems(Update, count_instances);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_resource::<Store>()
            .init_resource::<Staged>()
            .init_resource::<ViewStore>()
            .init_gpu_resource::<SpecializedRenderPipelines<InstancePipelines>>()
            .add_systems(RenderStartup, init_pipelines.after(MeshPipelineSystems))
            .add_systems(ExtractSchedule, extract)
            .add_systems(
                Render,
                (
                    // The store and the pipelines have to be ready before the
                    // phase items are queued, and the buffers only before the
                    // passes run: Bevy's own order is Queue, QueueMeshes,
                    // PhaseSort, Prepare, PrepareBindGroups, Render.
                    (prepare_store, prepare_view_pipelines).chain().in_set(RenderSystems::Queue),
                    queue_instances.in_set(RenderSystems::QueueMeshes),
                    prepare_view_instances.in_set(RenderSystems::PrepareBindGroups),
                ),
            )
            .add_systems(
                RenderGraph,
                cull.in_set(bevy::render::renderer::RenderGraphSystems::Begin),
            );

        use bevy::render::render_phase::AddRenderCommand;
        render_app
            .add_render_command::<Opaque3d, DrawInstancedMain>()
            .add_render_command::<Opaque3dPrepass, DrawInstancedDepth>()
            .add_render_command::<Shadow, DrawInstancedShadow>();
    }
}

/// The HUD's numbers, off the same rule the GPU runs.
fn count_instances(
    instanced: Res<Instanced>,
    mut counts: ResMut<InstanceCounts>,
    camera: Query<(&GlobalTransform, &Projection), With<Camera3d>>,
) {
    let Ok((transform, projection)) = camera.single() else { return };
    let eye = transform.translation();
    let planes = frustum_planes(projection.get_clip_from_view() * transform.to_matrix().inverse());
    let mut drawn = 0;
    let mut triangles = 0;
    for batch in &instanced.batches {
        let hits = batch.instances.iter().filter(|i| survives(i, &planes, eye)).count();
        drawn += hits;
        triangles += hits * batch.triangles();
    }
    counts.instances = instanced.instances();
    counts.drawn = drawn;
    counts.triangles = triangles;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(pos: Vec3, band: [f32; 4], dither: f32) -> Instance {
        Instance { pos, dither, band, radius: 1.0, ..default() }
    }

    /// A perspective view looking down -Z from the origin, the way Bevy's
    /// camera does.
    fn looking_down_z() -> Mat4 {
        Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_4, 16.0 / 9.0, 0.1)
    }

    #[test]
    fn the_four_side_planes_keep_what_is_in_front_and_drop_what_is_behind() {
        let planes = frustum_planes(looking_down_z());
        let ahead = at(Vec3::new(0.0, 0.0, -50.0), [0.0, 0.0, FAR, FAR], 0.0);
        let behind = at(Vec3::new(0.0, 0.0, 50.0), [0.0, 0.0, FAR, FAR], 0.0);
        let aside = at(Vec3::new(500.0, 0.0, -50.0), [0.0, 0.0, FAR, FAR], 0.0);
        assert!(in_frustum(&ahead, &planes));
        assert!(!in_frustum(&behind, &planes));
        assert!(!in_frustum(&aside, &planes));
    }

    /// The far plane of an infinite reversed-Z projection is degenerate, and a
    /// tree a mile off must not be culled by it.
    #[test]
    fn a_degenerate_plane_culls_nothing() {
        let planes = frustum_planes(looking_down_z());
        let far = at(Vec3::new(0.0, 0.0, -30000.0), [0.0, 0.0, FAR, FAR], 0.0);
        assert!(in_frustum(&far, &planes));
    }

    #[test]
    fn a_bounding_sphere_that_only_clips_the_edge_is_kept() {
        let planes = frustum_planes(looking_down_z());
        let mut edge = at(Vec3::new(0.0, 0.0, -50.0), [0.0, 0.0, FAR, FAR], 0.0);
        edge.pos.x = 60.0;
        edge.radius = 0.5;
        assert!(!in_frustum(&edge, &planes));
        edge.radius = 40.0;
        assert!(in_frustum(&edge, &planes));
    }

    #[test]
    fn the_ramp_is_flat_outside_its_margin_and_linear_inside_it() {
        assert_eq!(ramp(50.0, 100.0, 200.0), 0.0);
        assert_eq!(ramp(150.0, 100.0, 200.0), 0.5);
        assert_eq!(ramp(250.0, 100.0, 200.0), 1.0);
        // A margin of no width is a step, which is what the first and last
        // stages of a chain carry.
        assert_eq!(ramp(0.0, 0.0, 0.0), 1.0);
    }

    /// The point of the per-instance threshold: over a chain of stages sharing
    /// one tree's threshold, exactly one stage draws it at any distance.
    #[test]
    fn exactly_one_stage_draws_a_tree_at_any_distance() {
        let edges = [100.0f32, 200.0, 400.0];
        let fade = |at: f32| (at * 0.8, at * 1.25);
        let bands: Vec<[f32; 4]> = (0..=edges.len())
            .map(|stage| {
                let (in_lo, in_hi) =
                    if stage == 0 { (0.0, 0.0) } else { fade(edges[stage - 1]) };
                let (out_lo, out_hi) =
                    edges.get(stage).map(|&e| fade(e)).unwrap_or((FAR, FAR));
                [in_lo, in_hi, out_lo, out_hi]
            })
            .collect();
        for tenth in 0..10 {
            let dither = tenth as f32 / 10.0;
            for step in 0..600 {
                let d = step as f32;
                let drawing = bands
                    .iter()
                    .filter(|band| in_band(&at(Vec3::ZERO, **band, dither), d))
                    .count();
                assert_eq!(drawing, 1, "distance {d}, threshold {dither}");
            }
        }
    }

    /// Two stages of one tree must agree about the threshold, whatever order
    /// they were built in.
    #[test]
    fn the_threshold_is_the_trees_own() {
        let pos = Vec3::new(123.5, 40.25, -900.75);
        assert_eq!(dither_of(pos), dither_of(pos));
        assert!((0.0..1.0).contains(&dither_of(pos)));
        assert_ne!(dither_of(pos), dither_of(pos + Vec3::X));
    }

    /// The buffer layouts the shaders declare. A record that grows without the
    /// WGSL growing with it reads garbage rather than failing.
    #[test]
    fn the_gpu_records_are_the_size_the_shaders_declare() {
        assert_eq!(std::mem::size_of::<GpuInstance>(), 80);
        assert_eq!(std::mem::align_of::<GpuInstance>(), 4);
        assert_eq!(std::mem::size_of::<GpuSpan>(), 16);
        assert_eq!(std::mem::size_of::<GpuBatch>(), 32);
        assert!(std::mem::size_of::<GpuBatch>() as u64 <= BATCH_STRIDE);
        // mat4x4 + 6 vec4 + vec3 + 3 u32 padded to 16.
        assert_eq!(std::mem::size_of::<GpuView>(), 64 + 96 + 16 + 16);
        assert_eq!(std::mem::size_of::<GpuScene>() % 16, 0);
        assert_eq!(ARGS_STRIDE, std::mem::size_of::<[u32; 5]>() as u64);
    }
}
