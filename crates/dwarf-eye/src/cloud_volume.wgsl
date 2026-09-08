// Raymarched clouds.
//
// The mesh is a slab around the camera; each fragment on its far faces walks
// the ray from the camera through the cloud layer, accumulating scattered
// sunlight and sky light and attenuating by Beer's law. The density function
// here and `Field::density` in `clouds.rs` are the same function, so the
// shadow map the terrain reads matches the clouds overhead.
//
// After Schneider's Nubis (Horizon Zero Dawn) for the shape, and Wrenninge's
// octave sum for the multiple scattering.

#import bevy_pbr::{
    mesh_view_bindings::{view, lights, light_probes},
    mesh_view_bindings as bindings,
    forward_io::VertexOutput,
}
#ifdef DEPTH_PREPASS
#import bevy_pbr::prepass_utils::prepass_depth
#endif
#ifdef ATMOSPHERE
#import bevy_pbr::atmosphere::bruneton_functions::transmittance_lut_r_mu_to_uv
#endif

struct Tuning {
    sigma: f32,
    detail: f32,
    sun_gain: f32,
    ambient_gain: f32,
    haze: f32,
    cirrus_opacity: f32,
    shadow_strength: f32,
    steps: f32,
}

struct Params {
    bottom: f32,
    top: f32,
    cirrus_height: f32,
    max_distance: f32,
    weather_period: f32,
    base_period: f32,
    detail_period: f32,
    enabled: f32,
    wind: vec2<f32>,
    padding: vec2<f32>,
    tuning: Tuning,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: Params;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var base_texture: texture_3d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var base_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var detail_texture: texture_3d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var detail_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var weather_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var weather_sampler: sampler;

const PI: f32 = 3.14159265;
const SUN_SAMPLES: i32 = 5;
const SUN_STEP: f32 = 12.0;
const UP_SAMPLES: i32 = 3;
const UP_STEP: f32 = 14.0;

fn remap(v: f32, lo: f32, hi: f32, new_lo: f32, new_hi: f32) -> f32 {
    return new_lo + (v - lo) / (hi - lo) * (new_hi - new_lo);
}

fn height_fraction(y: f32) -> f32 {
    return (y - params.bottom) / (params.top - params.bottom);
}

fn weather_at(xz: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(weather_texture, weather_sampler, xz / params.weather_period, 0.0);
}

// Cloud density at a point in the field, 0..1. `p.xz` already carries the
// wind offset. `cheap` skips the detail erosion.
fn density(p: vec3<f32>, cheap: bool) -> f32 {
    let h = height_fraction(p.y);
    if h <= 0.0 || h >= 1.0 {
        return 0.0;
    }
    let w = weather_at(p.xz);
    let coverage = w.r;
    if coverage <= 0.005 {
        return 0.0;
    }
    let kind = w.g;

    // Stratus: a thin sheet near the base. Cumulus: a tall column with a flat
    // bottom and a tapering crown.
    let stratus = saturate(remap(h, 0.0, 0.07, 0.0, 1.0)) * saturate(remap(h, 0.18, 0.30, 1.0, 0.0));
    let cumulus = saturate(remap(h, 0.0, 0.10, 0.0, 1.0)) * saturate(remap(h, 0.55, 0.95, 1.0, 0.0));
    let gradient = mix(stratus, cumulus, kind);

    let n = textureSampleLevel(base_texture, base_sampler, p / params.base_period, 0.0);
    let low = n.g * 0.625 + n.b * 0.25 + n.a * 0.125;
    let base = saturate(remap(n.r, low - 1.0, 1.0, 0.0, 1.0)) * gradient;
    var shape = saturate(remap(base, 1.0 - coverage, 1.0, 0.0, 1.0)) * coverage;

    if !cheap && shape > 0.0 {
        let d = textureSampleLevel(detail_texture, detail_sampler, p / params.detail_period, 0.0);
        let fbm = d.r * 0.625 + d.g * 0.25 + d.b * 0.125;
        // Wispy at the base, billowy at the crown.
        let erode = mix(fbm, 1.0 - fbm, saturate(h * 8.0));
        shape = saturate(remap(shape, erode * params.tuning.detail, 1.0, 0.0, 1.0));
    }
    return shape;
}

// Henyey-Greenstein phase function.
fn hg(cos_theta: f32, g: f32) -> f32 {
    let g2 = g * g;
    return (1.0 - g2) / (4.0 * PI * pow(1.0 + g2 - 2.0 * g * cos_theta, 1.5));
}

// A forward lobe for silver linings, a back lobe so sunlit faces read.
fn phase(cos_theta: f32, sharpness: f32) -> f32 {
    return mix(hg(cos_theta, -0.3 * sharpness), hg(cos_theta, 0.65 * sharpness), 0.6);
}

// Light reaching the eye from a point with `optical_depth` toward the sun.
// Each octave stands in for one more bounce: dimmer, less attenuated, less
// directional.
fn scattered_sunlight(optical_depth: f32, cos_theta: f32) -> f32 {
    var total = 0.0;
    var a = 1.0;
    var b = 1.0;
    var c = 1.0;
    for (var n = 0; n < 3; n = n + 1) {
        total = total + a * exp(-optical_depth * b) * phase(cos_theta, c);
        a = a * 0.5;
        b = b * 0.5;
        c = c * 0.5;
    }
    return total;
}

// Optical depth from `p` toward the sun, over a short cone.
fn sun_optical_depth(p: vec3<f32>, sun: vec3<f32>) -> f32 {
    var total = 0.0;
    for (var i = 0; i < SUN_SAMPLES; i = i + 1) {
        let q = p + sun * SUN_STEP * (f32(i) + 0.5);
        total = total + density(q, i >= 2);
    }
    return total * SUN_STEP;
}

// Optical depth from `p` straight up, for how much sky the point can see.
fn up_optical_depth(p: vec3<f32>) -> f32 {
    var total = 0.0;
    for (var i = 0; i < UP_SAMPLES; i = i + 1) {
        let q = p + vec3(0.0, UP_STEP * (f32(i) + 0.5), 0.0);
        total = total + density(q, true);
    }
    return total * UP_STEP;
}

#ifdef ATMOSPHERE
// Sunlight left after the atmosphere, at radius `r` and sun cosine `mu`. The
// atmosphere pass's own module carries its bindings, so this repeats the
// lookup rather than importing it.
fn sun_transmittance(r: f32, mu: f32) -> vec3<f32> {
    let uv = transmittance_lut_r_mu_to_uv(bindings::atmosphere, r, mu);
    return textureSampleLevel(
        bindings::atmosphere_transmittance_texture,
        bindings::atmosphere_transmittance_sampler, uv, 0.0).rgb;
}
#endif

// The sky's brightest directional light. Bevy sorts the lights by their shadow
// flags rather than by entity, so with a moon in the scene index 0 is as likely
// to be the moon as the sun; the clouds take their light from whichever is
// brighter, which at night is the moon.
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

fn rotate(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    let t = 2.0 * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}

// Sky radiance from the view's environment map, which the atmosphere fills.
fn sky_radiance(direction: vec3<f32>, diffuse: bool) -> vec3<f32> {
#ifdef ENVIRONMENT_MAP
    if light_probes.view_cubemap_index >= 0 {
        var dir = rotate(light_probes.view_rotation, direction);
        dir.z = -dir.z;
#ifdef MULTIPLE_LIGHT_PROBES_IN_ARRAY
        let index = light_probes.view_cubemap_index;
        if diffuse {
            return textureSampleLevel(bindings::diffuse_environment_maps[index], bindings::environment_map_sampler, dir, 0.0).rgb;
        }
        return textureSampleLevel(bindings::specular_environment_maps[index], bindings::environment_map_sampler, dir, 1.0).rgb;
#else
        if diffuse {
            return textureSampleLevel(bindings::diffuse_environment_map, bindings::environment_map_sampler, dir, 0.0).rgb;
        }
        return textureSampleLevel(bindings::specular_environment_map, bindings::environment_map_sampler, dir, 1.0).rgb;
#endif
    }
#endif
    // No sky map: a pale blue fraction of the sun.
    return lights.directional_lights[brightest_light()].color.rgb * vec3(0.05, 0.07, 0.10);
}

// Interleaved gradient noise, to break the march into grain rather than bands.
fn gradient_noise(pixel: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(0.06711056 * pixel.x + 0.00583715 * pixel.y));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    if params.enabled < 0.5 {
        return vec4(0.0);
    }

    let ro = view.world_position;
    let rd = normalize(in.world_position.xyz - ro);
    let wind = vec3(params.wind.x, 0.0, params.wind.y);

    // How far the ray gets before it hits terrain.
    var t_scene = 1.0e9;
#ifdef DEPTH_PREPASS
    let depth = prepass_depth(in.position, 0u);
    if depth > 0.0 {
        // Infinite reverse-z: view depth is near / depth.
        let near = view.clip_from_view[3][2];
        let forward = -view.world_from_view[2].xyz;
        t_scene = near / depth / max(dot(rd, forward), 1.0e-4);
    }
#endif

    // Where the ray crosses the cloud layer.
    var t0 = 1.0;
    var t1 = 0.0;
    if abs(rd.y) < 1.0e-4 {
        if ro.y > params.bottom && ro.y < params.top {
            t0 = 0.0;
            t1 = params.max_distance;
        }
    } else {
        let ta = (params.bottom - ro.y) / rd.y;
        let tb = (params.top - ro.y) / rd.y;
        t0 = max(min(ta, tb), 0.0);
        t1 = min(max(ta, tb), params.max_distance);
    }
    t1 = min(t1, t_scene);

    let light = lights.directional_lights[brightest_light()];
    let sun = light.direction_to_light;
    var sun_radiance = light.color.rgb;
#ifdef ATMOSPHERE
    {
        // Sunlight reddens through the atmosphere before it reaches the layer.
        let atmosphere = bindings::atmosphere;
        let p_as = (atmosphere.world_to_atmosphere * vec4(ro.x, params.top, ro.z, 1.0)).xyz;
        let r = max(length(p_as), atmosphere.inner_radius + 1.0e-3);
        let mu = dot(sun, normalize(p_as));
        sun_radiance = sun_radiance * sun_transmittance(r, mu);
    }
#endif
    let cos_theta = dot(rd, sun);
    let sky_up = sky_radiance(vec3(0.0, 1.0, 0.0), true)
        * light_probes.intensity_for_view * params.tuning.ambient_gain;
    // Ground bounce: a dim, warm fraction of the sun.
    let ground_up = sun_radiance * 0.02;

    var color = vec3(0.0);
    var transmittance = 1.0;
    let tuning = params.tuning;

    if t1 > t0 {
        let steps = i32(tuning.steps);
        let dt = (t1 - t0) / f32(steps);
        var t = t0 + dt * gradient_noise(in.position.xy);
        for (var i = 0; i < steps; i = i + 1) {
            if t >= t1 {
                break;
            }
            let p = ro + rd * t + wind;
            if density(p, true) > 0.0 {
                let d = density(p, false);
                if d > 0.001 {
                    let extinction = d * tuning.sigma;
                    let od_sun = sun_optical_depth(p, sun) * tuning.sigma;
                    let od_up = up_optical_depth(p) * tuning.sigma;
                    let h = saturate(height_fraction(p.y));

                    let sunlight = sun_radiance * scattered_sunlight(od_sun, cos_theta) * tuning.sun_gain;
                    // Sky from above, less the cloud in the way; a little ground
                    // bounce from below.
                    let ambient = sky_up * (exp(-od_up) * 0.85 + 0.15) * mix(0.5, 1.0, h)
                        + ground_up * (1.0 - h);

                    // Energy-conserving integration over the step.
                    let source = (sunlight + ambient) * extinction;
                    let step_transmittance = exp(-extinction * dt);
                    color = color + transmittance * (source - source * step_transmittance) / extinction;
                    transmittance = transmittance * step_transmittance;
                    if transmittance < 0.01 {
                        break;
                    }
                }
            }
            t = t + dt;
        }
    }

    // Distant clouds sink into the sky.
    let sky_along = sky_radiance(rd, false);
    let haze = 1.0 - exp(-t0 * tuning.haze);
    color = mix(color, sky_along * (1.0 - transmittance), haze);

    // Cirrus: a single thin sheet, drawn out along the wind.
    if tuning.cirrus_opacity > 0.0 && abs(rd.y) > 1.0e-3 {
        let tc = (params.cirrus_height - ro.y) / rd.y;
        if tc > 0.0 && tc < t_scene && tc < params.max_distance * 1.6 {
            let p = ro + rd * tc + wind;
            let cover = weather_at(p.xz).b;
            if cover > 0.0 {
                let streak_a = textureSampleLevel(base_texture, base_sampler,
                    vec3(p.x / (params.base_period * 3.0), 0.31, p.z / (params.base_period * 0.8)), 0.0).g;
                let streak_b = textureSampleLevel(base_texture, base_sampler,
                    vec3(p.x / (params.base_period * 1.2), 0.62, p.z / (params.base_period * 0.3)), 0.0).b;
                let streak = saturate(remap(streak_a * 0.6 + streak_b * 0.4, 0.45, 0.85, 0.0, 1.0)) * cover;
                // Thin sheets look denser edge-on.
                let slant = min(1.0 / max(abs(rd.y), 0.15), 3.0);
                let alpha = min(streak * tuning.cirrus_opacity * slant * 0.4, 0.85);
                let lit = sun_radiance * tuning.sun_gain * (0.7 * hg(cos_theta, 0.7) + 0.3 * 0.08) + sky_up * 0.6;
                let haze_c = 1.0 - exp(-tc * tuning.haze);
                let cirrus_color = mix(lit, sky_along, haze_c) * alpha;
                if tc >= t1 {
                    color = color + transmittance * cirrus_color;
                } else {
                    color = cirrus_color + color * (1.0 - alpha);
                }
                transmittance = transmittance * (1.0 - alpha);
            }
        }
    }

    return vec4(color * view.exposure, 1.0 - transmittance);
}
