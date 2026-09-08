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
    // One on the horizon material, which yields to loaded blocks.
    horizon: f32,
    // Block coordinate of the mask's first texel.
    mask_origin: vec2<f32>,
    // How much of the sky's indirect light foliage keeps, 0 on the ground.
    canopy: f32,
}

// Bevy substitutes the material group index; it is 3 in this version, and
// hardcoding 2 (the mesh group) leaves the bindings out of the pipeline layout.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> cloud: CloudShadow;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var shadow_map: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var shadow_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var block_mask: texture_2d<f32>;

// Whether a horizon fragment stands where a fine chunk is loaded.
fn masked(world: vec3<f32>) -> bool {
    if cloud.horizon < 0.5 {
        return false;
    }
    let block = vec2<i32>(floor(world.xz / 16.0)) - vec2<i32>(cloud.mask_origin);
    let size = vec2<i32>(textureDimensions(block_mask));
    if any(block < vec2(0)) || any(block >= size) {
        return false;
    }
    return textureLoad(block_mask, block, 0).r > 0.5;
}

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

// How much of the sky one canopy face sees, from the face's own normal rather
// than the one flipped toward the camera: a leaf looking up is open to the
// whole sky, one looking sideways to half of it, and one looking down to the
// ground under the tree.
fn sky_reach(normal: vec3<f32>) -> f32 {
    let up = clamp(normal.y * 0.5 + 0.5, 0.0, 1.0);
    return mix(0.1, 1.0, up * up);
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    if masked(in.world_position.xyz) {
        discard;
    }
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color =
        alpha_discard(pbr_input.material, pbr_input.material.base_color);

    // Occlusion only touches the indirect terms, so this dims the sky's fill
    // on a crown without touching the sun: the shaded side of a tree goes dark
    // because nothing but the sky was ever lighting it.
    if cloud.canopy > 0.0 {
        let reach = cloud.canopy * sky_reach(in.world_normal);
        pbr_input.diffuse_occlusion *= vec3(reach);
        pbr_input.specular_occlusion *= reach;
    }

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

    let shade = transmittance(in.world_position.xyz);
    out.color = vec4(out.color.rgb * shade, out.color.a);
    return out;
}
