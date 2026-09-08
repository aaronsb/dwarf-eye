// Casts the cloud deck's shadow onto the ground.
//
// Bevy's volumetric fog lights the fog but never shadows scene geometry, so the
// clouds would otherwise float over a fully lit landscape. This marches the same
// 3D density volume from each fragment toward the sun and dims the surface by
// what it passes through.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}

struct CloudShadow {
    // Direction from the world toward the sun.
    sun: vec3<f32>,
    // How much of the light a fully opaque cloud takes away, 0..1.
    strength: f32,
    // Centre of the cloud deck in world space.
    centre: vec3<f32>,
    density_scale: f32,
    // Full extent of the deck in world space.
    size: vec3<f32>,
    enabled: f32,
    // Wind offset applied to the density texture.
    offset: vec3<f32>,
    padding: f32,
}

// Bevy substitutes the material group index; it is 3 in this version, and
// hardcoding 2 (the mesh group) leaves the bindings out of the pipeline layout.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> cloud: CloudShadow;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var density_texture: texture_3d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var density_sampler: sampler;

const SAMPLES: i32 = 12;

// Density at a world position.
//
// Only altitude bounds the deck. Clipping horizontally would put a straight
// box edge across the ground where the sun ray leaves the volume, so the
// texture wraps instead and the cloud field carries on past the horizon.
fn sample_density(world: vec3<f32>) -> f32 {
    let local = (world - cloud.centre) / cloud.size;
    if abs(local.y) > 0.5 {
        return 0.0;
    }
    let uvw = local + vec3(0.5) + cloud.offset;
    return textureSampleLevel(density_texture, density_sampler, uvw, 0.0).r;
}

// Fraction of sunlight that survives the trip up through the deck.
fn transmittance(world: vec3<f32>) -> f32 {
    if cloud.enabled < 0.5 || cloud.sun.y <= 0.02 {
        return 1.0;
    }

    // Where the ray toward the sun enters and leaves the deck's slab.
    let bottom = cloud.centre.y - cloud.size.y * 0.5;
    let top = cloud.centre.y + cloud.size.y * 0.5;
    if world.y > top {
        return 1.0;
    }
    let start = max((bottom - world.y) / cloud.sun.y, 0.0);
    let end = (top - world.y) / cloud.sun.y;
    if end <= start {
        return 1.0;
    }

    let step = (end - start) / f32(SAMPLES);
    var total = 0.0;
    for (var i = 0; i < SAMPLES; i = i + 1) {
        let t = start + step * (f32(i) + 0.5);
        total = total + sample_density(world + cloud.sun * t);
    }

    // Beer's law over the sampled column.
    let optical_depth = total * step * cloud.density_scale;
    return clamp(exp(-optical_depth), 1.0 - cloud.strength, 1.0);
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color =
        alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

    // Shade the surface by what the sun had to pass through to reach it.
    let shade = transmittance(in.world_position.xyz);
    out.color = vec4(out.color.rgb * shade, out.color.a);
    return out;
}
