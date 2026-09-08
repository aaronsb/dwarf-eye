//! Tileable noise volumes for the cloud field.
//!
//! The same arrays are uploaded as GPU textures and sampled on the CPU by the
//! shadow bake, so the clouds in the sky and the shadows on the ground come
//! from one function. Everything tiles at an integer period, so the field
//! repeats seamlessly across the map.

use bevy::prelude::*;

/// A cubic volume of 8-bit RGBA samples.
pub struct Volume {
    pub size: usize,
    pub data: Vec<u8>,
}

impl Volume {
    /// Trilinear sample at a wrapping texture coordinate, each channel 0..1.
    pub fn sample(&self, uvw: Vec3) -> Vec4 {
        let n = self.size as f32;
        // Texel centres sit at half offsets, as in the GPU sampler.
        let p = uvw * n - Vec3::splat(0.5);
        let base = p.floor();
        let f = p - base;
        let (x0, y0, z0) = (base.x as i64, base.y as i64, base.z as i64);

        let at = |x: i64, y: i64, z: i64| -> Vec4 {
            let m = self.size as i64;
            let i = (((z.rem_euclid(m) * m + y.rem_euclid(m)) * m + x.rem_euclid(m)) * 4) as usize;
            let d = &self.data[i..i + 4];
            Vec4::new(d[0] as f32, d[1] as f32, d[2] as f32, d[3] as f32) / 255.0
        };

        let c00 = at(x0, y0, z0).lerp(at(x0 + 1, y0, z0), f.x);
        let c10 = at(x0, y0 + 1, z0).lerp(at(x0 + 1, y0 + 1, z0), f.x);
        let c01 = at(x0, y0, z0 + 1).lerp(at(x0 + 1, y0, z0 + 1), f.x);
        let c11 = at(x0, y0 + 1, z0 + 1).lerp(at(x0 + 1, y0 + 1, z0 + 1), f.x);
        c00.lerp(c10, f.y).lerp(c01.lerp(c11, f.y), f.z)
    }
}

/// A square sheet of 8-bit RGBA samples.
pub struct Sheet {
    pub size: usize,
    pub data: Vec<u8>,
}

impl Sheet {
    /// Bilinear sample at a wrapping texture coordinate, each channel 0..1.
    pub fn sample(&self, uv: Vec2) -> Vec4 {
        let n = self.size as f32;
        let p = uv * n - Vec2::splat(0.5);
        let base = p.floor();
        let f = p - base;
        let (x0, y0) = (base.x as i64, base.y as i64);

        let at = |x: i64, y: i64| -> Vec4 {
            let m = self.size as i64;
            let i = ((y.rem_euclid(m) * m + x.rem_euclid(m)) * 4) as usize;
            let d = &self.data[i..i + 4];
            Vec4::new(d[0] as f32, d[1] as f32, d[2] as f32, d[3] as f32) / 255.0
        };

        at(x0, y0)
            .lerp(at(x0 + 1, y0), f.x)
            .lerp(at(x0, y0 + 1).lerp(at(x0 + 1, y0 + 1), f.x), f.y)
    }
}

fn hash(x: i32, y: i32, z: i32, salt: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD816_3841)
        ^ (z as u32).wrapping_mul(0xCB1A_B31F)
        ^ salt.wrapping_mul(0x9E37_79B9);
    h ^= h >> 13;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 16;
    h
}

fn hash01(x: i32, y: i32, z: i32, salt: u32) -> f32 {
    (hash(x, y, z, salt) & 0xFFFF) as f32 / 65535.0
}

const GRADIENTS: [[f32; 3]; 12] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
];

fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Perlin noise that repeats every `period` lattice cells on each axis. `p` is
/// in lattice units. Returns roughly -1..1.
fn perlin(p: Vec3, period: IVec3, salt: u32) -> f32 {
    let cell = p.floor();
    let f = p - cell;
    let c = cell.as_ivec3();
    let grad = |dx: i32, dy: i32, dz: i32| -> f32 {
        let g = GRADIENTS[(hash(
            (c.x + dx).rem_euclid(period.x),
            (c.y + dy).rem_euclid(period.y),
            (c.z + dz).rem_euclid(period.z),
            salt,
        ) % 12) as usize];
        let d = f - Vec3::new(dx as f32, dy as f32, dz as f32);
        g[0] * d.x + g[1] * d.y + g[2] * d.z
    };
    let u = fade(f.x);
    let v = fade(f.y);
    let w = fade(f.z);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(grad(0, 0, 0), grad(1, 0, 0), u);
    let x10 = lerp(grad(0, 1, 0), grad(1, 1, 0), u);
    let x01 = lerp(grad(0, 0, 1), grad(1, 0, 1), u);
    let x11 = lerp(grad(0, 1, 1), grad(1, 1, 1), u);
    lerp(lerp(x00, x10, v), lerp(x01, x11, v), w)
}

/// Three octaves of tiling Perlin, mapped to 0..1.
fn perlin_fbm(uvw: Vec3, freq: i32, salt: u32) -> f32 {
    let mut total = 0.0;
    let mut amp = 0.5;
    let mut f = freq;
    for octave in 0..3 {
        total += perlin(uvw * f as f32, IVec3::splat(f), salt + octave) * amp;
        amp *= 0.5;
        f *= 2;
    }
    // Perlin's range is well inside -1..1; stretch so the fbm fills 0..1.
    (total * 1.6 + 0.5).clamp(0.0, 1.0)
}

/// Two-dimensional tiling fbm, for the weather map.
fn perlin_fbm_2d(uv: Vec2, freq: i32, salt: u32) -> f32 {
    let mut total = 0.0;
    let mut amp = 0.5;
    let mut f = freq;
    for octave in 0..4 {
        let p = Vec3::new(uv.x * f as f32, uv.y * f as f32, 0.37);
        total += perlin(p, IVec3::new(f, f, 1), salt + octave) * amp;
        amp *= 0.5;
        f *= 2;
    }
    (total * 1.6 + 0.5).clamp(0.0, 1.0)
}

/// Inverted Worley noise: 1 at a feature point, falling to 0 between them.
/// `cells` feature cells per axis, tiling.
fn worley(uvw: Vec3, cells: i32, salt: u32) -> f32 {
    let p = uvw * cells as f32;
    let cell = p.floor();
    let c = cell.as_ivec3();
    let mut best = f32::MAX;
    for dz in -1..=1 {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (x, y, z) = (c.x + dx, c.y + dy, c.z + dz);
                let (wx, wy, wz) = (x.rem_euclid(cells), y.rem_euclid(cells), z.rem_euclid(cells));
                let feature = Vec3::new(
                    x as f32 + hash01(wx, wy, wz, salt),
                    y as f32 + hash01(wx, wy, wz, salt + 1),
                    z as f32 + hash01(wx, wy, wz, salt + 2),
                );
                best = best.min(p.distance_squared(feature));
            }
        }
    }
    (1.0 - best.sqrt()).clamp(0.0, 1.0)
}

/// Three octaves of Worley at doubling frequency.
fn worley_fbm(uvw: Vec3, cells: i32, salt: u32) -> f32 {
    worley(uvw, cells, salt) * 0.625
        + worley(uvw, cells * 2, salt + 10) * 0.25
        + worley(uvw, cells * 4, salt + 20) * 0.125
}

fn remap(v: f32, lo: f32, hi: f32, new_lo: f32, new_hi: f32) -> f32 {
    new_lo + (v - lo) / (hi - lo) * (new_hi - new_lo)
}

/// Fills a volume in parallel, one closure call per voxel.
fn fill_volume(size: usize, f: impl Fn(Vec3) -> [f32; 4] + Sync) -> Volume {
    let mut data = vec![0u8; size * size * size * 4];
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let slab = size * size * 4;
    let rows_per = size.div_ceil(threads);
    std::thread::scope(|scope| {
        for (t, chunk) in data.chunks_mut(slab * rows_per).enumerate() {
            let f = &f;
            scope.spawn(move || {
                for (i, texel) in chunk.chunks_exact_mut(4).enumerate() {
                    let index = t * rows_per * size * size + i;
                    let x = index % size;
                    let y = (index / size) % size;
                    let z = index / (size * size);
                    let uvw = (Vec3::new(x as f32, y as f32, z as f32) + Vec3::splat(0.5))
                        / size as f32;
                    let v = f(uvw);
                    for c in 0..4 {
                        texel[c] = (v[c].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    }
                }
            });
        }
    });
    Volume { size, data }
}

/// The low-frequency shape noise: Perlin-Worley in red, Worley octaves in the
/// rest, after Schneider's Nubis layout.
pub fn base_noise(size: usize) -> Volume {
    fill_volume(size, |uvw| {
        let perlin = perlin_fbm(uvw, 4, 1);
        let worley_low = worley_fbm(uvw, 4, 100);
        // Perlin billows, with Worley carving the cauliflower into them.
        let perlin_worley = remap(perlin, 0.0, 1.0, worley_low, 1.0);
        [
            perlin_worley,
            worley_fbm(uvw, 8, 200),
            worley_fbm(uvw, 16, 300),
            worley_fbm(uvw, 32, 400),
        ]
    })
}

/// High-frequency Worley, for eroding cloud edges.
pub fn detail_noise(size: usize) -> Volume {
    fill_volume(size, |uvw| {
        [
            worley_fbm(uvw, 2, 500),
            worley_fbm(uvw, 4, 600),
            worley_fbm(uvw, 8, 700),
            1.0,
        ]
    })
}

/// How much sky each cloud kind should take, 0..1.
#[derive(Clone, Copy, PartialEq, Default)]
pub struct Cover {
    pub cumulus: f32,
    pub stratus: f32,
    pub cirrus: f32,
}

/// The weather map: coverage in red, cloud type in green (0 stratus, 1
/// cumulus), cirrus coverage in blue.
pub fn weather_sheet(size: usize, cover: Cover) -> Sheet {
    let mut data = vec![0u8; size * size * 4];
    let threshold = |amount: f32, high: f32, span: f32| high - amount * span;
    let coverage = |noise: f32, threshold: f32, softness: f32| {
        ((noise - threshold) / softness).clamp(0.0, 1.0)
    };

    for (i, texel) in data.chunks_exact_mut(4).enumerate() {
        let x = i % size;
        let y = i / size;
        let uv = (Vec2::new(x as f32, y as f32) + Vec2::splat(0.5)) / size as f32;

        // Cumulus gather in groups; stratus spread as broad sheets.
        let cumulus = if cover.cumulus > 0.005 {
            coverage(perlin_fbm_2d(uv, 4, 1000), threshold(cover.cumulus, 0.80, 0.55), 0.28)
        } else {
            0.0
        };
        let stratus = if cover.stratus > 0.005 {
            coverage(perlin_fbm_2d(uv, 2, 2000), threshold(cover.stratus, 0.75, 0.70), 0.35)
        } else {
            0.0
        };
        let cirrus = if cover.cirrus > 0.005 {
            coverage(perlin_fbm_2d(uv, 3, 3000), threshold(cover.cirrus, 0.85, 0.60), 0.30)
        } else {
            0.0
        };

        let total = cumulus.max(stratus);
        let kind = if total > 0.0 { cumulus / (cumulus + stratus).max(1e-3) } else { 1.0 };
        texel[0] = (total * 255.0 + 0.5) as u8;
        texel[1] = (kind * 255.0 + 0.5) as u8;
        texel[2] = (cirrus * 255.0 + 0.5) as u8;
        texel[3] = 255;
    }
    Sheet { size, data }
}
