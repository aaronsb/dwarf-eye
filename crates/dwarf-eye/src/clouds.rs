//! Volumetric clouds, shaped by Dwarf Fortress's own weather.
//!
//! DF reports a cloud type per world tile — cumulus, stratus, cirrus and fog —
//! rather than a coverage number, and the three cloud kinds differ mostly in how
//! they occupy height. That maps onto a 3D density texture: a fog volume
//! raymarches it against the sun, so the clouds cast light shafts instead of
//! being a picture pasted on the sky.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::FogVolume;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Density volume resolution. Height gets fewer samples than the ground plane
/// because the layers are broad and flat relative to their thickness.
const NX: usize = 96;
const NY: usize = 48;
const NZ: usize = 96;

/// How wide the cloud deck is, in tiles.
const DECK_WIDTH: f32 = 900.0;
/// How tall, from the base of the lowest layer to the top of the highest.
const DECK_HEIGHT: f32 = 260.0;
/// Height of the deck's base above the terrain.
const DECK_BASE: f32 = 40.0;

/// The z-level the terrain sits at, so the deck can hold a fixed altitude.
///
/// Tying the deck to the camera instead would put the viewer inside it, and a
/// camera inside the cloud volume sees nothing but fog.
#[derive(Resource, Default)]
pub struct GroundLevel(pub f32);

/// Cloud cover as Dwarf Fortress reports it, reduced to what a sky needs.
#[derive(Resource, Clone, Copy, PartialEq, Debug)]
pub struct Weather {
    /// Puffy and tall: little ground cover, a lot of vertical extent.
    pub cumulus: f32,
    /// A flat sheet: nearly total cover, very little depth.
    pub stratus: f32,
    /// High and thin.
    pub cirrus: f32,
    /// Sits on the ground rather than above it.
    pub fog: f32,
}

impl Default for Weather {
    fn default() -> Self {
        Self { cumulus: 0.0, stratus: 0.0, cirrus: 0.0, fog: 0.0 }
    }
}

impl Weather {
    /// A sky forced from the environment, for testing without waiting on the
    /// world's own weather. `DWARF_EYE_CLOUDS=cumulus,stratus` or
    /// `DWARF_EYE_CLOUDS=cumulus=0.8,cirrus=0.4`.
    pub fn from_env() -> Option<Self> {
        let spec = std::env::var("DWARF_EYE_CLOUDS").ok()?;
        let mut weather = Weather::default();
        for part in spec.split(',').filter(|p| !p.trim().is_empty()) {
            let (name, amount) = match part.split_once('=') {
                Some((n, v)) => (n.trim(), v.trim().parse().unwrap_or(0.6)),
                None => (part.trim(), 0.6f32),
            };
            match name {
                "cumulus" => weather.cumulus = amount,
                "stratus" => weather.stratus = amount,
                "cirrus" => weather.cirrus = amount,
                "fog" => weather.fog = amount,
                _ => {}
            }
        }
        Some(weather)
    }

    pub fn is_clear(self) -> bool {
        self.cumulus + self.stratus + self.cirrus + self.fog < 0.01
    }

    pub fn describe(self) -> String {
        if self.is_clear() {
            return "clear".into();
        }
        let mut parts = Vec::new();
        for (name, amount) in [
            ("cumulus", self.cumulus),
            ("stratus", self.stratus),
            ("cirrus", self.cirrus),
            ("fog", self.fog),
        ] {
            if amount > 0.01 {
                parts.push(format!("{name} {:.0}%", amount * 100.0));
            }
        }
        parts.join(", ")
    }
}

#[derive(Component)]
pub struct CloudDeck;

/// The weather the current density texture was built for.
#[derive(Resource, Default)]
pub struct BuiltFor(pub Option<Weather>);

/// Value noise, smoothed and tiling on the grid.
fn hash3(x: i32, y: i32, z: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD8163_841u32)
        ^ (z as u32).wrapping_mul(0xCB1A_B31F);
    h ^= h >> 13;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 16;
    (h & 0xFFFF) as f32 / 65535.0
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Trilinear value noise on a grid of `period` cells, wrapping so the deck can
/// scroll without a seam.
fn noise(p: Vec3, period: i32) -> f32 {
    let scaled = p * period as f32;
    let (i, f) = (scaled.floor(), scaled.fract());
    let (x0, y0, z0) = (i.x as i32, i.y as i32, i.z as i32);
    let wrap = |v: i32| v.rem_euclid(period);

    let (u, v, w) = (smooth(f.x), smooth(f.y), smooth(f.z));
    let mut total = 0.0;
    for (dz, wz) in [(0, 1.0 - w), (1, w)] {
        for (dy, wy) in [(0, 1.0 - v), (1, v)] {
            for (dx, wx) in [(0, 1.0 - u), (1, u)] {
                total += hash3(wrap(x0 + dx), wrap(y0 + dy), wrap(z0 + dz)) * wx * wy * wz;
            }
        }
    }
    total
}

/// Several octaves of value noise, for cloud edges that are ragged at more than
/// one scale.
fn fbm(p: Vec3, base_period: i32) -> f32 {
    let mut total = 0.0;
    let mut amplitude = 0.5;
    let mut period = base_period;
    for _ in 0..4 {
        total += noise(p, period) * amplitude;
        amplitude *= 0.5;
        period *= 2;
    }
    total
}

/// Maps a value through a soft threshold, so `coverage` behaves like the
/// fraction of sky filled rather than a brightness.
fn cover(value: f32, coverage: f32, softness: f32) -> f32 {
    if coverage <= 0.0 {
        return 0.0;
    }
    // A higher coverage lowers the bar a sample has to clear.
    let threshold = 1.0 - coverage;
    ((value - threshold) / softness).clamp(0.0, 1.0)
}

/// A band that fades in and out over `feather` at each edge.
fn band(v: f32, low: f32, high: f32, feather: f32) -> f32 {
    let rise = ((v - low) / feather).clamp(0.0, 1.0);
    let fall = ((high - v) / feather).clamp(0.0, 1.0);
    rise * fall
}

/// Builds the density volume for a given sky.
///
/// The three layers differ in the proportions a real sky gives them: stratus is
/// a near-total sheet an eighth as deep as it is broad, cumulus occupies a third
/// of the column but only patches of the ground plane, cirrus is thin, high and
/// stretched.
pub fn build_density(weather: Weather) -> Image {
    let mut data = vec![0u8; NX * NY * NZ];

    for z in 0..NZ {
        for y in 0..NY {
            for x in 0..NX {
                let p = Vec3::new(
                    x as f32 / NX as f32,
                    y as f32 / NY as f32,
                    z as f32 / NZ as f32,
                );
                let altitude = p.y;
                let mut density: f32 = 0.0;

                // Stratus: a flat sheet low down, almost total where present.
                if weather.stratus > 0.0 {
                    let shape = band(altitude, 0.10, 0.22, 0.035);
                    let n = fbm(Vec3::new(p.x, p.y * 3.0, p.z), 3);
                    density = density.max(cover(n, weather.stratus * 0.92, 0.22) * shape * 0.8);
                }

                // Cumulus: tall lumps. Density falls off toward the top, which
                // is what gives them a flat base and a domed crown.
                if weather.cumulus > 0.0 {
                    let shape = band(altitude, 0.26, 0.62, 0.02);
                    let dome = 1.0 - ((altitude - 0.30) / 0.34).clamp(0.0, 1.0).powf(1.7);
                    // Squashing y makes the lumps wider than they are tall.
                    let n = fbm(Vec3::new(p.x, p.y * 0.55, p.z), 4);
                    density = density.max(cover(n, weather.cumulus * 0.62, 0.3) * shape * dome);
                }

                // Cirrus: high, thin, drawn out along the wind.
                if weather.cirrus > 0.0 {
                    let shape = band(altitude, 0.80, 0.90, 0.03);
                    let n = fbm(Vec3::new(p.x * 0.35, p.y * 6.0, p.z * 1.6), 5);
                    density = density.max(cover(n, weather.cirrus * 0.5, 0.34) * shape * 0.35);
                }

                // Fog clings to the bottom of the volume.
                if weather.fog > 0.0 {
                    let shape = 1.0 - (altitude / 0.06).clamp(0.0, 1.0);
                    let n = fbm(Vec3::new(p.x * 2.0, p.y * 4.0, p.z * 2.0), 4);
                    density = density.max(cover(n, weather.fog, 0.5) * shape * 0.6);
                }

                data[(z * NY + y) * NX + x] = (density.clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
    }

    let mut image = Image::new(
        Extent3d { width: NX as u32, height: NY as u32, depth_or_array_layers: NZ as u32 },
        TextureDimension::D3,
        data,
        TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    // Repeat, so the deck can scroll on the wind without running out.
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    image
}

pub fn setup(mut commands: Commands) {
    commands.spawn((
        FogVolume {
            density_factor: 0.0,
            absorption: 0.42,
            scattering: 0.65,
            ..default()
        },
        Transform::from_xyz(0.0, DECK_BASE, 0.0)
            .with_scale(Vec3::new(DECK_WIDTH, DECK_HEIGHT, DECK_WIDTH)),
        CloudDeck,
    ));
}

/// Rebuilds the deck when the weather changes and drifts it on the wind.
pub fn drive(
    time: Res<Time>,
    weather: Res<Weather>,
    mut built: ResMut<BuiltFor>,
    mut images: ResMut<Assets<Image>>,
    ground: Res<GroundLevel>,
    camera: Query<&Transform, (With<crate::camera::FlyCamera>, Without<CloudDeck>)>,
    mut deck: Query<(&mut FogVolume, &mut Transform), With<CloudDeck>>,
) {
    let Ok((mut volume, mut transform)) = deck.single_mut() else { return };

    if built.0 != Some(*weather) {
        built.0 = Some(*weather);
        volume.density_texture = (!weather.is_clear()).then(|| images.add(build_density(*weather)));
        volume.density_factor = if weather.is_clear() { 0.0 } else { 0.09 };
    }

    // Follow the camera across the ground plane only. The altitude is fixed to
    // the terrain, so the deck stays overhead however high the camera climbs.
    if let Ok(view) = camera.single() {
        transform.translation.x = view.translation.x;
        transform.translation.z = view.translation.z;
    }
    transform.translation.y = ground.0 + DECK_BASE + DECK_HEIGHT * 0.5;
    volume.density_texture_offset += Vec3::new(0.004, 0.0, 0.0015) * time.delta_secs();
}
