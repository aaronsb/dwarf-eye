// Shades the ground by the clouds overhead.
//
// The clouds are a transparent volume and never enter the shadow map. The CPU
// bakes the sun's transmittance through the cloud layer onto the ground plane;
// this looks that map up where the sun ray from each fragment crosses the bake
// plane.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}

struct CloudShadow {
    // Direction toward the sun the map was baked for.
    sun: vec3<f32>,
    // Fraction of light a fully opaque cloud takes away, 0..1.
    strength: f32,
    // Wind offset in tiles: the field is sampled at world + wind.
    wind: vec2<f32>,
    // The map covers one period of the field.
    period: f32,
    // Height the map was baked at.
    ground: f32,
    enabled: f32,
    padding: vec3<f32>,
}

// Bevy substitutes the material group index; it is 3 in this version, and
// hardcoding 2 (the mesh group) leaves the bindings out of the pipeline layout.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> cloud: CloudShadow;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var shadow_map: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var shadow_sampler: sampler;

// Fraction of sunlight reaching a point on the ground.
fn transmittance(world: vec3<f32>) -> f32 {
    if cloud.enabled < 0.5 || cloud.sun.y <= 0.02 {
        return 1.0;
    }
    // Follow the sun ray down to the bake plane: a higher fragment sees the
    // clouds a lower one sees from further along the sun's slant.
    let on_plane = world.xz - cloud.sun.xz / cloud.sun.y * (world.y - cloud.ground);
    let uv = (on_plane + cloud.wind) / cloud.period;
    let t = textureSampleLevel(shadow_map, shadow_sampler, uv, 0.0).r;
    return 1.0 - cloud.strength * (1.0 - t);
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color =
        alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

    let shade = transmittance(in.world_position.xyz);
    out.color = vec4(out.color.rgb * shade, out.color.a);
    return out;
}
