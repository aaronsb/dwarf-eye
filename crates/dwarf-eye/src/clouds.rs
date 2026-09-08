//! Clouds, shaped by Dwarf Fortress's own weather.
//!
//! DF reports a cloud *kind* per world tile — cumulus, stratus, cirrus and fog —
//! rather than a coverage number, and the kinds differ mostly in how they occupy
//! height. Each shapes a field of spherical puffs.
//!
//! Those puffs feed two consumers, so the clouds you see and the shadows they
//! throw describe the same sky: the visible geometry, and a 3D density texture
//! the terrain shader marches toward the sun.

use crate::cloud_material::{CloudMaterial, CloudSubsurface};
use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Density volume resolution. Height gets fewer samples because the layers are
/// broad and flat relative to their thickness.
const NX: usize = 96;
const NY: usize = 48;
const NZ: usize = 96;

/// Deck extent in tiles. Wider than the camera's far plane, so its edge never
/// comes into view.
pub const DECK_WIDTH: f32 = 2400.0;
pub const DECK_HEIGHT: f32 = 320.0;
/// Height of the deck's base above the terrain.
pub const DECK_BASE: f32 = 40.0;

/// The z-level the terrain sits at, so the deck can hold a fixed altitude.
///
/// Tying the deck to the camera instead would put the viewer inside it.
#[derive(Resource, Default)]
pub struct GroundLevel(pub f32);

/// Cloud cover as Dwarf Fortress reports it, reduced to what a sky needs.
#[derive(Resource, Clone, Copy, PartialEq, Debug, Default)]
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

impl Weather {
    /// A sky forced from the environment, for testing without waiting on the
    /// world. `DWARF_EYE_CLOUDS=cumulus=0.8,cirrus=0.4`.
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

/// The weather the current geometry was built for.
#[derive(Resource, Default)]
pub struct BuiltFor(pub Option<Weather>);

/// Where the deck sits and what shadows it casts, for the terrain shader.
#[derive(Resource, Default)]
pub struct CloudField {
    pub density: Option<Handle<Image>>,
    pub centre: Vec3,
    pub size: Vec3,
    pub offset: Vec3,
}

/// One sphere of cloud.
struct Puff {
    /// Centre in deck-local space, the deck's own centre being the origin.
    centre: Vec3,
    radius: f32,
    /// 0 at a wispy edge, 1 in the core.
    depth: f32,
    /// 0 at the cloud's flat base, 1 at its crown.
    height: f32,
}

#[derive(Clone, Copy)]
enum Layer {
    Cumulus = 0,
    Stratus = 1,
    Cirrus = 2,
}

/// The proportions that tell one cloud kind from another.
struct Profile {
    /// Height above the deck's base.
    altitude: f32,
    puffs: i32,
    radius: f32,
    spread_x: f32,
    spread_z: f32,
    /// How far the crown rises above the base.
    rise: f32,
    jitter_y: f32,
}

impl Layer {
    fn profile(self) -> Profile {
        match self {
            // Tall and lumpy: a lot of column, only patches of sky.
            Layer::Cumulus => Profile {
                altitude: 130.0,
                puffs: 26,
                radius: 30.0,
                spread_x: 62.0,
                spread_z: 62.0,
                rise: 66.0,
                jitter_y: 30.0,
            },
            // A near-total sheet, a fraction as deep as it is broad.
            Layer::Stratus => Profile {
                altitude: 70.0,
                puffs: 16,
                radius: 36.0,
                spread_x: 96.0,
                spread_z: 96.0,
                rise: 9.0,
                jitter_y: 6.0,
            },
            // High, thin, drawn out along the wind.
            Layer::Cirrus => Profile {
                altitude: 236.0,
                puffs: 7,
                radius: 13.0,
                spread_x: 130.0,
                spread_z: 26.0,
                rise: 4.0,
                jitter_y: 14.0,
            },
        }
    }
}

/// A small deterministic generator, so the same sky comes back every run.
fn hash3(x: i32, y: i32, z: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD816_3841)
        ^ (z as u32).wrapping_mul(0xCB1A_B31F);
    h ^= h >> 13;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 16;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Lays out the cloud field: clusters on a jittered grid, puffs within each.
fn generate_puffs(weather: Weather) -> Vec<Puff> {
    const CELLS: i32 = 15;
    let cell = DECK_WIDTH / CELLS as f32;
    let mut puffs = Vec::new();

    for cz in 0..CELLS {
        for cx in 0..CELLS {
            let jitter = |salt: i32| hash3(cx, cz, salt);
            let base_x = (cx as f32 + 0.5 - CELLS as f32 * 0.5) * cell + (jitter(1) - 0.5) * cell;
            let base_z = (cz as f32 + 0.5 - CELLS as f32 * 0.5) * cell + (jitter(2) - 0.5) * cell;

            for (layer, coverage) in [
                (Layer::Cumulus, weather.cumulus),
                (Layer::Stratus, weather.stratus),
                (Layer::Cirrus, weather.cirrus),
            ] {
                let index = layer as i32;
                if coverage <= 0.01 || jitter(index + 10) > coverage {
                    continue;
                }
                let profile = layer.profile();
                let scale = 0.75 + jitter(index + 20) * 0.6;
                let centre = Vec3::new(
                    base_x,
                    // Deck-local: the deck's centre is the origin.
                    profile.altitude - DECK_HEIGHT * 0.5
                        + (jitter(index + 30) - 0.5) * profile.jitter_y,
                    base_z,
                );

                for puff in 0..profile.puffs {
                    let h = |salt: i32| hash3(cx * 131 + puff, cz * 197 + puff, salt + index);
                    let angle = h(1) * std::f32::consts::TAU;
                    // Square root spreads the puffs evenly over the disc rather
                    // than bunching them in the middle.
                    let reach = h(2).sqrt();
                    // A cloud is a few big masses with small lumps riding on
                    // them, not a heap of equal balls.
                    let mass = h(5).powf(2.4);
                    let rise = (1.0 - reach) * profile.rise * scale * (0.35 + h(4) * 0.9);
                    let offset = Vec3::new(
                        angle.cos() * reach * profile.spread_x * scale,
                        rise,
                        angle.sin() * reach * profile.spread_z * scale,
                    );
                    puffs.push(Puff {
                        centre: centre + offset,
                        radius: profile.radius
                            * scale
                            * (0.32 + mass * 1.15)
                            * (0.5 + (1.0 - reach) * 0.85),
                        depth: 1.0 - reach,
                        height: (rise / profile.rise.max(1.0)).clamp(0.0, 1.0),
                    });
                }
            }
        }
    }
    puffs
}

/// Turns the puffs into geometry, one low-poly sphere each.
fn build_mesh(puffs: &[Puff]) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );

    let unit = Sphere::new(1.0).mesh().ico(1).expect("icosphere");
    let unit_positions: Vec<[f32; 3]> = unit
        .attribute(Mesh::ATTRIBUTE_POSITION)
        .and_then(|v| v.as_float3())
        .map(<[[f32; 3]]>::to_vec)
        .unwrap_or_default();
    let unit_indices: Vec<u32> = match unit.indices() {
        Some(Indices::U32(v)) => v.clone(),
        Some(Indices::U16(v)) => v.iter().map(|&i| i as u32).collect(),
        None => Vec::new(),
    };

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for puff in puffs {
        let base = positions.len() as u32;
        for p in &unit_positions {
            let dir = Vec3::from_array(*p);
            positions.push((puff.centre + dir * puff.radius).to_array());
            normals.push(dir.to_array());
            // Red: how deep in the body. Green: how far up the cloud.
            colors.push([puff.depth, puff.height, 0.0, 1.0]);
        }
        indices.extend(unit_indices.iter().map(|i| i + base));
    }

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    if !indices.is_empty() {
        mesh.insert_indices(Indices::U32(indices));
    }
    mesh
}

/// Splats the same puffs into a density volume, so the shadows on the ground
/// match the clouds in the sky.
fn build_density(puffs: &[Puff]) -> Image {
    let mut data = vec![0u8; NX * NY * NZ];
    let size = Vec3::new(DECK_WIDTH, DECK_HEIGHT, DECK_WIDTH);
    let resolution = Vec3::new(NX as f32, NY as f32, NZ as f32);

    for puff in puffs {
        // Voxel range the sphere touches.
        let lo = ((puff.centre - Vec3::splat(puff.radius)) / size + Vec3::splat(0.5)) * resolution;
        let hi = ((puff.centre + Vec3::splat(puff.radius)) / size + Vec3::splat(0.5)) * resolution;

        for z in lo.z.floor().max(0.0) as usize..(hi.z.ceil() as usize).min(NZ) {
            for y in lo.y.floor().max(0.0) as usize..(hi.y.ceil() as usize).min(NY) {
                for x in lo.x.floor().max(0.0) as usize..(hi.x.ceil() as usize).min(NX) {
                    let world = (Vec3::new(x as f32, y as f32, z as f32) / resolution
                        - Vec3::splat(0.5))
                        * size;
                    let distance = world.distance(puff.centre) / puff.radius;
                    if distance >= 1.0 {
                        continue;
                    }
                    // Soft-edged sphere, so shadow edges are not faceted.
                    let value = (1.0 - distance * distance).powf(1.5);
                    let index = (z * NY + y) * NX + x;
                    data[index] = data[index].max((value * 255.0) as u8);
                }
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
    image.sampler = repeating();
    image
}

/// A 1x1x1 empty volume, so the terrain material always has a 3D texture bound
/// even before any weather arrives.
pub fn empty_density() -> Image {
    let mut image = Image::new(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D3,
        vec![0u8],
        TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = repeating();
    image
}

fn repeating() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    })
}

pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    commands.spawn((
        Mesh3d(meshes.add(build_mesh(&[]))),
        MeshMaterial3d(materials.add(CloudMaterial {
            base: StandardMaterial {
                base_color: Color::WHITE,
                // The shader replaces the lighting; the base is only carrying
                // the vertex path and the prepass.
                unlit: true,
                ..default()
            },
            extension: CloudSubsurface::default(),
        })),
        Transform::from_xyz(0.0, DECK_BASE + DECK_HEIGHT * 0.5, 0.0),
        CloudDeck,
    ));
}

/// Rebuilds the field when the weather changes, and drifts it on the wind.
pub fn drive(
    time: Res<Time>,
    weather: Res<Weather>,
    ground: Res<GroundLevel>,
    mut built: ResMut<BuiltFor>,
    mut field: ResMut<CloudField>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut deck: Query<(&mut Mesh3d, &mut Transform), With<CloudDeck>>,
) {
    let Ok((mut mesh, mut transform)) = deck.single_mut() else { return };

    if built.0 != Some(*weather) {
        built.0 = Some(*weather);
        let puffs = generate_puffs(*weather);
        mesh.0 = meshes.add(build_mesh(&puffs));
        field.density = (!puffs.is_empty()).then(|| images.add(build_density(&puffs)));
    }

    // The deck holds a fixed altitude above the terrain and drifts sideways.
    let drift = time.elapsed_secs() * 0.9;
    transform.translation = Vec3::new(
        drift,
        ground.0 + DECK_BASE + DECK_HEIGHT * 0.5,
        drift * 0.35,
    );

    field.centre = transform.translation;
    field.size = Vec3::new(DECK_WIDTH, DECK_HEIGHT, DECK_WIDTH);
    field.offset = Vec3::ZERO;
}
