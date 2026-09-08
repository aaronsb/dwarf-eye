// Cloud shading: a lit solid, not a participating medium.
//
// Bevy's volumetric fog attenuates its own ambient by Beer's law, so a thick
// cloud gets less fill the deeper it is and renders black. Clouds here are
// ordinary geometry, shaded with the two things that make a cloud read: light
// wrapping far past the terminator, and light bleeding through where the cloud
// is thin.
//
// This replaces the lighting rather than adding to it, but stays a fragment-only
// extension so the standard vertex path — and the depth prepass that depends on
// it — keeps working.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::view,
}

struct CloudUniform {
    sun: vec3<f32>,
    // How far past the terminator light wraps. 1.0 lights the whole sphere.
    wrap: f32,
    sun_color: vec3<f32>,
    // Strength of light bleeding through thin parts.
    translucency: f32,
    // Sky light from above.
    sky_color: vec3<f32>,
    // Tightness of the forward-scattering lobe.
    scatter_power: f32,
    // Light bouncing up off the ground.
    ground_color: vec3<f32>,
    brightness: f32,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> cloud: CloudUniform;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    let pbr_input = pbr_input_from_standard_material(in, is_front);
    let normal = normalize(pbr_input.world_normal);
    let sun = normalize(cloud.sun);

    // Wrapped diffuse. A cloud has no hard terminator, so the falloff stretches
    // around the far side rather than clipping at zero.
    let lambert = dot(normal, sun);
    let wrapped = clamp((lambert + cloud.wrap) / (1.0 + cloud.wrap), 0.0, 1.0);

    // Vertex red is how deep in the body this point sits, green how far up the
    // cloud it is.
#ifdef VERTEX_COLORS
    let depth = in.color.r;
    let height = in.color.g;
#else
    let depth = 0.5;
    let height = 0.5;
#endif

    // Subsurface: light entering the far side and leaving toward the eye. Thin
    // parts glow, the core stays dense.
    let view_dir = normalize(in.world_position.xyz - view.world_position);
    let forward = pow(clamp(dot(view_dir, sun), 0.0, 1.0), cloud.scatter_power);
    let through = forward * (1.0 - clamp(depth, 0.0, 1.0)) * cloud.translucency;

    // Sky above, ground bounce below.
    let up = normal.y * 0.5 + 0.5;
    var ambient = mix(cloud.ground_color, cloud.sky_color, up);

    // A cumulus is bright at the crown and flat grey underneath. Without this
    // the whole cloud reads as one even blob.
    let base_shade = mix(0.42, 1.0, height);
    ambient = ambient * base_shade;

    var out: FragmentOutput;
    out.color = vec4<f32>(
        (ambient + cloud.sun_color * (wrapped * base_shade + through)) * cloud.brightness,
        1.0,
    );
    return out;
}
