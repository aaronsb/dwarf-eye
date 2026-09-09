// The instanced far band: a compute pass that culls, and one draw per
// (mesh, material) whose transform is read out of a storage buffer by
// `instance_index`.
//
// All four programs live in one file because they have to agree, to the byte,
// about one record. Splitting the record into an imported library makes
// naga_oil emit a copy of it per importing module, and a function then rejects
// a value of what looks like its own parameter type. So: three `#ifdef`s rather
// than three files.
//
// - `CULL_PASS`  the compute pass, writing a compacted list and draw arguments;
// - `MAIN_PASS`  the shaded pass;
// - neither      the depth prepass and the shadow cascades, which must apply
//   the same instance transform or shadows and occlusion culling both break.

#ifdef MAIN_PASS
#import bevy_pbr::{
    mesh_view_bindings::view,
    pbr_types,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing, calculate_view},
    mesh_types::MESH_FLAGS_SHADOW_RECEIVER_BIT,
}
#endif

// One tree, as the GPU sees it. 80 bytes, std430; `instancing.rs:GpuInstance`
// is the same record and a test pins its size.
struct Instance {
    // Where the instance stands, in world space.
    pos: vec3<f32>,
    yaw: f32,
    scale: f32,
    // Bounding sphere, already scaled: the centre sits `centre_y` above `pos`.
    radius: f32,
    centre_y: f32,
    // A stable threshold in 0..1, hashed from the tree's own position, so every
    // stage of one tree gets the same number and exactly one of them draws it.
    dither: f32,
    // The stage's own crossfade: fade-in start and end, then fade-out start and
    // end, in tiles from the eye. The nearest stage fades in over 0..0, the
    // last one never fades out.
    band: vec4<f32>,
    tint: vec4<f32>,
    // Which batch this instance belongs to, so one dispatch culls them all.
    batch: u32,
    // Three scalars rather than a `vec3<u32>`, which would align the tail to 16
    // and round the record up to 96 bytes.
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

// One view: the camera, or one shadow cascade.
struct ViewParams {
    clip_from_world: mat4x4<f32>,
    planes: array<vec4<f32>, 6>,
    // World position the distance band is measured from.
    eye: vec3<f32>,
    // How many instances the cull dispatch covers.
    count: u32,
    // Whether the frustum planes are worth testing.
    frustum: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(#{INSTANCE_BIND_GROUP}) @binding(0) var<storage, read> instances: array<Instance>;

#ifdef CULL_PASS

// Where a batch's instances and its slice of the compacted list start.
struct BatchSpan {
    first: u32,
    count: u32,
    visible_at: u32,
    _pad: u32,
}

@group(#{INSTANCE_BIND_GROUP}) @binding(1) var<storage, read_write> visible: array<u32>;
@group(#{INSTANCE_BIND_GROUP}) @binding(2) var<storage, read> spans: array<BatchSpan>;
@group(#{INSTANCE_BIND_GROUP}) @binding(3) var<uniform> viewp: ViewParams;
// `DrawIndexedIndirectArgs` per batch, five words each; word 1 is the instance
// count the compaction fills in.
@group(#{INSTANCE_BIND_GROUP}) @binding(4) var<storage, read_write> args: array<atomic<u32>>;

#else

// One (mesh, material) group, selected by a dynamic uniform offset at draw
// time, so the whole forest rides one bind group.
struct Batch {
    first: u32,
    count: u32,
    visible_at: u32,
    flags: u32,
    // How much of the sky's fill this stage's foliage keeps: the canopy term of
    // `shadow.rs:ShadowUniform`. Zero where the vertex colours already carry a
    // lit top and darker sides.
    canopy: f32,
    roughness: f32,
    _pad: vec2<f32>,
}

// Everything the whole scene shares: the cloud shadow bake, the weather and the
// block mask. The numbers `shadow.rs:ShadowUniform` carries.
struct Scene {
    sun: vec3<f32>,
    strength: f32,
    wind: vec2<f32>,
    period: f32,
    ground: f32,
    enabled: f32,
    wet: f32,
    polish: f32,
    snow: f32,
    mask_origin: vec2<f32>,
    // Zero where there is no fine map to yield to.
    mask_on: f32,
    _pad: f32,
}

@group(#{INSTANCE_BIND_GROUP}) @binding(1) var<storage, read> visible: array<u32>;
@group(#{INSTANCE_BIND_GROUP}) @binding(2) var<uniform> batch: Batch;
@group(#{INSTANCE_BIND_GROUP}) @binding(3) var<uniform> viewp: ViewParams;
@group(#{INSTANCE_BIND_GROUP}) @binding(4) var<uniform> scene: Scene;
@group(#{INSTANCE_BIND_GROUP}) @binding(5) var leaf_texture: texture_2d<f32>;
@group(#{INSTANCE_BIND_GROUP}) @binding(6) var leaf_sampler: sampler;
@group(#{INSTANCE_BIND_GROUP}) @binding(7) var cloud_map: texture_2d<f32>;
@group(#{INSTANCE_BIND_GROUP}) @binding(8) var cloud_sampler: sampler;
@group(#{INSTANCE_BIND_GROUP}) @binding(9) var block_mask: texture_2d<f32>;

#endif

#ifdef CULL_PASS

// ---------------------------------------------------------------------------
// The cull pass
// ---------------------------------------------------------------------------

const ARGS_STRIDE: u32 = 5u;
const INSTANCE_COUNT_WORD: u32 = 1u;

// How far into a crossfade a distance falls, 0 before it and 1 after. The same
// straddling margins `main.rs:band_fades` hands Bevy's `VisibilityRange`, and
// linear across them for the same reason Bevy's is: the margin is already
// log-spaced, so the ramp inside it need not be.
fn ramp(d: f32, lo: f32, hi: f32) -> f32 {
    if hi <= lo {
        return select(0.0, 1.0, d >= hi);
    }
    return clamp((d - lo) / (hi - lo), 0.0, 1.0);
}

// Whether this instance draws at this stage, at this distance from the eye.
//
// One tree draws at exactly one stage: the threshold is the tree's own, shared
// by every stage of it, and the ramps are monotonic and ordered, so the stage
// whose fade-in has passed the threshold and whose fade-out has not is unique.
// That is the crossfade Bevy dithers per pixel, done per tree instead — at this
// range a tree is small enough that swapping the whole of it reads as the same
// soft hand-off, and it costs no `VISIBILITY_RANGE_DITHER` discard.
fn in_band(inst: Instance, d: f32) -> bool {
    let fade_in = ramp(d, inst.band.x, inst.band.y);
    let fade_out = ramp(d, inst.band.z, inst.band.w);
    return fade_in > inst.dither && inst.dither >= fade_out;
}

// Whether the instance's bounding sphere is anywhere inside the view's frustum.
//
// A sphere and not a box: an instance is a tree at a yaw, and a bound that
// turns with it costs more than the few extra trees a sphere lets through. A
// degenerate plane — an infinite reversed-Z projection has no far plane — is
// skipped rather than failing everything.
fn in_frustum(centre: vec3<f32>, radius: f32) -> bool {
    if viewp.frustum == 0u {
        return true;
    }
    for (var i = 0u; i < 6u; i = i + 1u) {
        let plane = viewp.planes[i];
        if dot(plane.xyz, plane.xyz) < 0.5 {
            continue;
        }
        if dot(plane.xyz, centre) + plane.w < -radius {
            return false;
        }
    }
    return true;
}

// One thread per instance, once per view per frame — the camera and every
// shadow cascade — so a tree behind the eye is dropped from the main pass and
// still casts, and the stage a tree draws at is decided against the eye rather
// than against the window's centre. Nothing is read back.
@compute @workgroup_size(64)
fn cull(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= viewp.count {
        return;
    }
    let inst = instances[i];
    let centre = inst.pos + vec3(0.0, inst.centre_y, 0.0);
    if !in_frustum(centre, inst.radius) {
        return;
    }
    if !in_band(inst, distance(centre, viewp.eye)) {
        return;
    }
    let span = spans[inst.batch];
    let slot = atomicAdd(&args[inst.batch * ARGS_STRIDE + INSTANCE_COUNT_WORD], 1u);
    // The draw's `instance_index` counts from zero within its own batch, so the
    // list holds an offset from the batch's first instance rather than an
    // absolute one: `first_instance` has to stay zero without the indirect
    // first-instance feature.
    visible[span.visible_at + slot] = i - span.first;
}

#else

// ---------------------------------------------------------------------------
// The draw passes
// ---------------------------------------------------------------------------

struct Vertex {
    @builtin(instance_index) index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(5) color: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
}

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    // The cull pass wrote a compacted list, so `instance_index` counts through
    // what is drawn rather than through what exists: this indirection is what
    // turns it back into an instance.
    let inst = instances[batch.first + visible[batch.visible_at + v.index]];
    // Yaw, then uniform scale, then translation: the transform the per-entity
    // spawn built with `from_translation .with_rotation .with_scale`.
    let s = sin(inst.yaw);
    let c = cos(inst.yaw);
    let spun = vec3(
        c * v.position.x + s * v.position.z,
        v.position.y,
        -s * v.position.x + c * v.position.z,
    );
    let world = inst.pos + spun * inst.scale;

    var out: VertexOutput;
    out.clip = viewp.clip_from_world * vec4(world, 1.0);
    out.world = world;
    // A normal takes the rotation and not the scale, which is uniform anyway.
    out.normal = normalize(vec3(
        c * v.normal.x + s * v.normal.z,
        v.normal.y,
        -s * v.normal.x + c * v.normal.z,
    ));
    out.color = v.color * inst.tint;
    return out;
}

// Whether this fragment stands where a fine chunk is loaded, in which case the
// far band yields to it. The horizon's half of the block mask.
fn masked(world: vec3<f32>) -> bool {
    if scene.mask_on < 0.5 {
        return false;
    }
    let block = vec2<i32>(floor(world.xz / 16.0)) - vec2<i32>(scene.mask_origin);
    let size = vec2<i32>(textureDimensions(block_mask));
    if any(block < vec2(0)) || any(block >= size) {
        return false;
    }
    return textureLoad(block_mask, block, 0).r > 0.5;
}

#ifdef MAIN_PASS

// Lying snow, in the same linear space the base colour is in: the constant
// `cloud_shadow.wgsl` shades the ground toward.
const SNOW: vec3<f32> = vec3<f32>(0.84, 0.87, 0.93);

// The surface's own world coordinate in tiles, one repeat to a tile.
//
// An instanced crown cannot carry world-space UVs in its vertex buffer the way
// a chunk's mesh does — the instance's scale would take them with it and every
// tree would wear a different texel size — so they come off the world position,
// which is what `cloud_shadow.wgsl:horizon_leaf` does for the same reason.
fn surface_coords(world: vec3<f32>, normal: vec3<f32>) -> vec2<f32> {
    if abs(normal.y) > 0.5 {
        return world.xz;
    }
    if abs(normal.x) > 0.5 {
        return vec2(world.z, -world.y);
    }
    return vec2(world.x, -world.y);
}

// Fraction of sunlight reaching a point, through the baked cloud shadow.
fn transmittance(world: vec3<f32>) -> f32 {
    if scene.enabled < 0.5 || scene.sun.y <= 0.02 {
        return 1.0;
    }
    let on_plane = world.xz - scene.sun.xz / scene.sun.y * (world.y - scene.ground);
    let uv = (on_plane + scene.wind) / scene.period;
    let t = textureSampleLevel(cloud_map, cloud_sampler, uv, 0.0).r;
    return 1.0 - scene.strength * (1.0 - t);
}

// How much of the sky one canopy face sees, from its own normal: a leaf looking
// up is open to the whole sky, one looking sideways to half of it.
fn sky_reach(normal: vec3<f32>) -> f32 {
    let up = clamp(normal.y * 0.5 + 0.5, 0.0, 1.0);
    return mix(0.1, 1.0, up * up);
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> @location(0) vec4<f32> {
    if masked(in.world) {
        discard;
    }
    let tile = surface_coords(in.world, in.normal);
    let texel = textureSampleGrad(leaf_texture, leaf_sampler, tile, dpdx(tile), dpdy(tile));

    let normal = select(-in.normal, in.normal, is_front);
    var pbr = pbr_types::pbr_input_new();
    pbr.material.base_color = vec4(texel.rgb, 1.0) * in.color;
    pbr.material.perceptual_roughness = batch.roughness;
    pbr.material.reflectance = vec3(0.02);
    pbr.material.flags = pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_OPAQUE
        | pbr_types::STANDARD_MATERIAL_FLAGS_FOG_ENABLED_BIT;
    pbr.frag_coord = in.clip;
    pbr.world_position = vec4(in.world, 1.0);
    pbr.world_normal = normal;
    pbr.N = normal;
    pbr.V = calculate_view(vec4(in.world, 1.0), false);
    pbr.is_orthographic = false;
    // A crown that receives no shadow reads as lit from inside.
    pbr.flags = MESH_FLAGS_SHADOW_RECEIVER_BIT;

    // Weather on the surface: rain wets everything but pools on what looks up,
    // snow lies only on what looks up at all.
    let up = clamp(normal.y, 0.0, 1.0);
    if scene.wet > 0.0 {
        pbr.material.base_color = vec4(
            pbr.material.base_color.rgb * (1.0 - scene.wet * mix(0.45, 1.0, up)),
            pbr.material.base_color.a,
        );
        pbr.material.perceptual_roughness =
            max(0.08, pbr.material.perceptual_roughness * (1.0 - scene.polish * up));
    }
    if scene.snow > 0.0 {
        let lying = scene.snow * smoothstep(0.35, 0.85, up);
        pbr.material.base_color =
            vec4(mix(pbr.material.base_color.rgb, SNOW, lying), pbr.material.base_color.a);
        pbr.material.perceptual_roughness = mix(pbr.material.perceptual_roughness, 0.86, lying);
    }

    // Occlusion only touches the indirect terms, so this dims the sky's fill on
    // a crown without touching the sun: the shaded side of a tree goes dark
    // because nothing but the sky was ever lighting it.
    if batch.canopy > 0.0 {
        let reach = batch.canopy * sky_reach(in.normal);
        pbr.diffuse_occlusion *= vec3(reach);
        pbr.specular_occlusion *= reach;
    }

    var color = apply_pbr_lighting(pbr);
    color = main_pass_post_lighting_processing(pbr, color);
    return vec4(color.rgb * transmittance(in.world), color.a);
}

#else

// The depth passes write nothing but depth: the prepass feeds occlusion culling
// and the god rays' march, and the cascades feed the shadow map.
@fragment
fn fragment(in: VertexOutput) {
    if masked(in.world) {
        discard;
    }
}

#endif
#endif
