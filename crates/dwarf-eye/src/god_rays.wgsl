// Volumetric light shafts, as a screen-space post-process.
//
// A fullscreen triangle marches the view ray from the camera to whatever the
// depth buffer says it hit, and at every step asks two questions: does the
// sun's shadow map see this point, and does the cloud transmittance map? The
// product is how much sunlight reaches it, and a Henyey-Greenstein phase
// function decides how much of that turns toward the eye.
//
// The cloud lookup is the same slant lookup `cloud_shadow.wgsl` uses on the
// ground, so a shaft dims under the same cloud that shades the terrain below
// it. The shadow lookup is the same cascade fetch the terrain uses, so tree
// canopies, walls and hillsides all cut the beam.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput
#import bevy_pbr::mesh_view_bindings::{view, lights, globals}
#import bevy_pbr::mesh_view_bindings as bindings
#import bevy_pbr::shadows::{get_cascade_index, world_to_directional_light_local}
#import bevy_pbr::shadow_sampling::sample_shadow_map_hardware
#import bevy_pbr::view_transformations::{uv_to_ndc, position_ndc_to_world, depth_ndc_to_view_z}
#import bevy_pbr::utils::interleaved_gradient_noise
#ifdef ATMOSPHERE
#import bevy_pbr::atmosphere::bruneton_functions::transmittance_lut_r_mu_to_uv
#endif

struct GodRays {
    // Direction toward the sun the cloud map was baked for.
    sun: vec3<f32>,
    // Extinction per tile of the medium at ground level.
    density: f32,
    // Wind offset in tiles: the cloud field is sampled at world + wind.
    wind: vec2<f32>,
    // The cloud map covers one period of the field.
    period: f32,
    // Height the cloud map was baked at, and where the medium is densest.
    ground: f32,
    // Fraction of light a fully opaque cloud takes away, 0..1.
    cloud_strength: f32,
    // Zero when the sky is clear or no bake has landed.
    cloud_enabled: f32,
    // Henyey-Greenstein asymmetry.
    g: f32,
    // Multiplier on the in-scattered sunlight.
    strength: f32,
    // Reciprocal scale height of the medium, in 1/tiles.
    falloff: f32,
    // How far the march follows the ray, in tiles.
    max_distance: f32,
    // How much of the medium's own extinction dims the scene behind it, 0..1.
    dim: f32,
    steps: f32,
}

@group(1) @binding(0) var<uniform> rays: GodRays;
#ifdef MULTISAMPLED
@group(1) @binding(1) var depth_texture: texture_depth_multisampled_2d;
#else
@group(1) @binding(1) var depth_texture: texture_depth_2d;
#endif
@group(1) @binding(2) var cloud_map: texture_2d<f32>;
@group(1) @binding(3) var cloud_sampler: sampler;

const FRAC_4_PI: f32 = 0.07957747154594767;

fn henyey_greenstein(cos_theta: f32) -> f32 {
    let g = rays.g;
    let denom = 1.0 + g * g - 2.0 * g * cos_theta;
    return FRAC_4_PI * (1.0 - g * g) / (denom * sqrt(max(denom, 1.0e-4)));
}

// Fraction of sunlight the clouds leave at a point, read the same way the
// terrain reads it: follow the sun ray back to the plane the map was baked on.
fn cloud_transmittance(p: vec3<f32>) -> f32 {
    if rays.cloud_enabled < 0.5 || rays.sun.y <= 0.02 {
        return 1.0;
    }
    let on_plane = p.xz - rays.sun.xz / rays.sun.y * (p.y - rays.ground);
    let uv = (on_plane + rays.wind) / rays.period;
    let t = textureSampleLevel(cloud_map, cloud_sampler, uv, 0.0).r;
    return 1.0 - rays.cloud_strength * (1.0 - t);
}

// The medium thins with height, so shafts read against the ground rather than
// filling the sky.
fn medium(y: f32) -> f32 {
    return rays.density * exp(-max(y - rays.ground, 0.0) * rays.falloff);
}

// The sky's brightest directional light. Bevy sorts the lights by their shadow
// flags rather than by entity, so with a moon in the scene index 0 is as likely
// to be the moon as the sun; the shafts belong to whichever is brighter.
fn brightest_light() -> u32 {
    var best = 0u;
    var best_lum = -1.0;
    for (var i = 0u; i < lights.n_directional_lights; i = i + 1u) {
        let c = lights.directional_lights[i].color.rgb;
        let lum = c.r + c.g + c.b;
        if lum > best_lum {
            best_lum = lum;
            best = i;
        }
    }
    return best;
}

// Whether the light's shadow map sees a point. Outside the cascades the map has
// nothing to say, so the point counts as lit rather than as a black disc.
//
// The moon carries no cascades at all (`main.rs`: a second shadow pass for
// light the scene barely resolves is not worth it), so `num_cascades` is zero
// and a moonlit night gets the medium's own glow toward the moon rather than
// shafts cut by the trees. The phase function still shapes it.
fn sun_visibility(light_id: u32, p: vec3<f32>, view_z: f32) -> f32 {
    let light = &lights.directional_lights[light_id];
    let cascade_index = get_cascade_index(light_id, view_z);
    if cascade_index >= (*light).num_cascades {
        return 1.0;
    }
    let offset = (*light).shadow_depth_bias * (*light).direction_to_light.xyz;
    let light_local = world_to_directional_light_local(light_id, cascade_index, vec4(p + offset, 1.0));
    if light_local.w == 0.0 {
        return 1.0;
    }
    let array_index = i32((*light).depth_texture_base_index + cascade_index);
    return sample_shadow_map_hardware(light_local.xy, light_local.z, array_index);
}

#ifdef ATMOSPHERE
// Sunlight left after the atmosphere, at radius `r` and sun cosine `mu`.
fn sun_transmittance(r: f32, mu: f32) -> vec3<f32> {
    let uv = transmittance_lut_r_mu_to_uv(bindings::atmosphere, r, mu);
    return textureSampleLevel(
        bindings::atmosphere_transmittance_texture,
        bindings::atmosphere_transmittance_sampler, uv, 0.0).rgb;
}
#endif

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let light_id = brightest_light();
    let light = &lights.directional_lights[light_id];
    let sun = (*light).direction_to_light.xyz;
    if rays.strength <= 0.0 || rays.density <= 0.0 || sun.y <= 0.0 {
        return vec4(0.0);
    }

    let ro = view.world_position;
    let rd = normalize(position_ndc_to_world(vec3(uv_to_ndc(in.uv), 0.5)) - ro);
    // The camera's forward axis, for turning view depth into ray distance.
    let forward = -view.world_from_view[2].xyz;
    let along = max(dot(rd, forward), 1.0e-4);

    // Stop at the scene. With MSAA one sample is close enough for a volume.
    var t_end = rays.max_distance;
    let ndc_depth = textureLoad(depth_texture, vec2<i32>(in.position.xy), 0);
    if ndc_depth > 0.0 {
        t_end = min(-depth_ndc_to_view_z(ndc_depth) / along, rays.max_distance);
    }
    if t_end <= 0.0 {
        return vec4(0.0);
    }

    var sun_radiance = (*light).color.rgb;
#ifdef ATMOSPHERE
    {
        let atmosphere = bindings::atmosphere;
        let p_as = (atmosphere.world_to_atmosphere * vec4(ro, 1.0)).xyz;
        let r = max(length(p_as), atmosphere.inner_radius + 1.0e-3);
        let mu = dot(sun, normalize(p_as));
        sun_radiance = sun_radiance * sun_transmittance(r, mu);
    }
#endif

    let phase = henyey_greenstein(dot(rd, sun));
    let steps = i32(rays.steps);
    let dt = t_end / f32(steps);
    // Dither the first step so the march grains rather than bands, rotated per
    // frame so the grain moves instead of standing still.
    var t = dt * interleaved_gradient_noise(in.position.xy, globals.frame_count);

    var scatter = 0.0;
    var transmittance = 1.0;
    for (var i = 0; i < steps; i = i + 1) {
        let p = ro + rd * t;
        let sigma = medium(p.y);
        if sigma > 1.0e-7 {
            let step_transmittance = exp(-sigma * dt);
            let visibility =
                sun_visibility(light_id, p, -dot(p - ro, forward)) * cloud_transmittance(p);
            // Energy-conserving integration of in-scatter over the step.
            scatter = scatter + transmittance * visibility * (1.0 - step_transmittance);
            transmittance = transmittance * step_transmittance;
        }
        t = t + dt;
    }

    let color = sun_radiance * scatter * phase * rays.strength * view.exposure;
    return vec4(color, (1.0 - transmittance) * rays.dim);
}
