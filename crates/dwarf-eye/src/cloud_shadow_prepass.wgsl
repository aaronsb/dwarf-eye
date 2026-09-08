// Depth prepass for the terrain material.
//
// Mirrors Bevy's own prepass fragment, with one addition: horizon fragments
// over a loaded block are discarded, so their depth never hides the fine
// ground behind them. The same shader serves the shadow pass, so the horizon
// casts no shadow there either.

#import bevy_pbr::{
    pbr_prepass_functions,
    prepass_io,
}

struct CloudShadow {
    sun: vec3<f32>,
    strength: f32,
    wind: vec2<f32>,
    period: f32,
    ground: f32,
    enabled: f32,
    horizon: f32,
    mask_origin: vec2<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> cloud: CloudShadow;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var block_mask: texture_2d<f32>;

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

#ifdef PREPASS_FRAGMENT
@fragment
fn fragment(in: prepass_io::VertexOutput) -> prepass_io::FragmentOutput {
    if masked(in.world_position.xyz) {
        discard;
    }
    pbr_prepass_functions::prepass_alpha_discard(in);
    var out: prepass_io::FragmentOutput;
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.frag_depth = in.unclipped_depth;
#endif
#ifdef NORMAL_PREPASS
    out.normal = vec4(in.world_normal * 0.5 + vec3(0.5), 1.0);
#endif
    return out;
}
#else
@fragment
fn fragment(in: prepass_io::VertexOutput) {
    if masked(in.world_position.xyz) {
        discard;
    }
    pbr_prepass_functions::prepass_alpha_discard(in);
}
#endif
