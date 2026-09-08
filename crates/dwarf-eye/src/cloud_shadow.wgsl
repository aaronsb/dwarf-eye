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
    pbr_bindings::{base_color_texture, base_color_sampler},
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
    // Non-zero on a horizon material, which yields to loaded blocks: 1 on the
    // coarse ground, whose UV names an atlas cell, 2 on the far crowns, whose
    // UVs are world-space on a leaf texture of their own.
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

// The ground atlas's own layout, from `dwarf_eye_art::atlas`. A cell is 32 px
// of sprite with 16 px of its own edge bled around it, 32 cells to a side.
// `horizon::skin` pins these with a test.
const ATLAS_GRID: f32 = 32.0;
const ATLAS_CELL: f32 = 32.0;
const ATLAS_PAD: f32 = 16.0;
const ATLAS_STRIDE: f32 = ATLAS_CELL + ATLAS_PAD * 2.0;
const ATLAS_SIDE: f32 = ATLAS_GRID * ATLAS_STRIDE;

// Where a coarse surface reads the sprite from, in world tiles.
//
// A terrace slab is one quad six to forty-eight tiles across, so the repeat
// cannot live in its UVs: the atlas has no room around a cell to tile into.
// The vertex carries the cell's centre instead and the surface's own world
// position supplies the phase, which puts one sprite on every world tile —
// Dwarf Fortress's own density, and the fine map's, so the two meet without a
// change of scale.
fn surface_coords(world: vec3<f32>, normal: vec3<f32>) -> vec2<f32> {
    if abs(normal.y) > 0.5 {
        return world.xz;
    }
    if abs(normal.x) > 0.5 {
        return vec2(world.z, -world.y);
    }
    return vec2(world.x, -world.y);
}

// The sprite the coarse ground's own UV names, wrapped across the surface.
//
// Derivatives come from the unwrapped coordinate, so the mip level is
// continuous across a tile boundary; taking them from the wrapped one would
// spike at every seam and rule a sharp grid over the whole band.
fn horizon_texel(uv: vec2<f32>, tile: vec2<f32>, ddx: vec2<f32>, ddy: vec2<f32>) -> vec4<f32> {
    let slot = floor(uv * ATLAS_GRID);
    let origin = slot * ATLAS_STRIDE + ATLAS_PAD;
    let inside = origin + fract(tile) * ATLAS_CELL + 0.5;
    let grad = ATLAS_CELL / ATLAS_SIDE;
    return textureSampleGrad(
        base_color_texture,
        base_color_sampler,
        inside / ATLAS_SIDE,
        ddx * grad,
        ddy * grad,
    );
}

// The canopy's leaf surface on a far crown.
//
// A crown out here is one shared mesh at one transform per tree, so its UVs
// cannot be world-space in the vertex buffer the way a chunk's are: the
// instance's own scale would take them with it and every tree would wear a
// different texel size. Deriving them from the world position gives every
// crown, grown or boxed, exactly the near canopy's density — one repeat to a
// world tile — however large the instance is.
fn horizon_leaf(tile: vec2<f32>, ddx: vec2<f32>, ddy: vec2<f32>) -> vec4<f32> {
    return textureSampleGrad(base_color_texture, base_color_sampler, tile, ddx, ddy);
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
    // The surface's own world coordinate in tiles, and its derivatives, taken
    // once and outside the branch: a gradient asked for inside one is not
    // guaranteed to be uniform across the quad.
    let tile = surface_coords(in.world_position.xyz, in.world_normal);
    let ddx = dpdx(tile);
    let ddy = dpdy(tile);
    var pbr_input = pbr_input_from_standard_material(in, is_front);
#ifdef VERTEX_COLORS
    // A horizon surface samples its own texture: the coarse ground's UV names
    // an atlas cell rather than a point, and a far crown has no usable UV at
    // all. Both are wrapped across the surface by its world position, so the
    // texel density is the fine map's whatever the geometry's scale.
    // Untextured horizon geometry — the world grid, rivers, buildings, the
    // blob shadows — points at the white cell and comes through as its own
    // vertex colour.
    if cloud.horizon > 0.5 {
        var texel = horizon_leaf(tile, ddx, ddy);
        if cloud.horizon < 1.5 {
            texel = horizon_texel(in.uv, tile, ddx, ddy);
        }
        pbr_input.material.base_color = vec4(texel.rgb, 1.0) * in.color;
    }
#endif
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
