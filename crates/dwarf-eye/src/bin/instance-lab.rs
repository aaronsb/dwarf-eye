//! A bench for GPU instancing, with no Dwarf Fortress connection: `n` synthetic
//! crowns scattered over a plane, drawn through the instanced path alone.
//!
//! `DWARF_EYE_INSTANCE_TEST=n` sets how many (default 100000),
//! `INSTANCE_LAB_SPREAD` how many tiles across they are scattered,
//! `INSTANCE_LAB_CAM=x,y,z,yaw,pitch` where the camera starts. The HUD reports
//! the instance count, how many the cull keeps and the frame rate; turning the
//! camera is what shows the cull working. `DWARF_EYE_SHOT=path[:seconds]` saves
//! a shot and exits.
//!
//! Nothing here is read back off the GPU. The drawn count is the same rule the
//! compute shader runs, evaluated on the CPU (`instancing::survives`), which the
//! unit tests pin against the shader's own arithmetic.

#[path = "../camera.rs"]
mod camera;
#[allow(dead_code)]
#[path = "../capture.rs"]
mod capture;
#[allow(dead_code)]
#[path = "../instancing.rs"]
mod instancing;
#[allow(dead_code)]
#[path = "../shadow.rs"]
mod shadow;
#[allow(dead_code)]
#[path = "../texture.rs"]
mod texture;

use bevy::asset::RenderAssetUsages;
use bevy::camera::Exposure;
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::{
    Atmosphere, AtmosphereEnvironmentMapLight, SunDisk, atmosphere::ScatteringMedium,
    light_consts::lux,
};
use bevy::pbr::{AtmosphereMode, AtmosphereSettings};
use bevy::prelude::*;
use bevy::render::occlusion_culling::OcclusionCulling;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::text::FontSize;

use camera::FlyCamera;
use dwarf_eye_trees::{Preset, texture as tree_texture};
use instancing::{FAR, Instance, InstanceCounts, Instanced, InstancingPlugin, dither_of};

/// How many instances the bench spawns unless told otherwise: the acceptance
/// figure from issue #34.
const DEFAULT_COUNT: usize = 100_000;
/// How many tiles across they are scattered. A hundred thousand trees over
/// nine hundred tiles is a dense wood, not a lawn of dots.
const DEFAULT_SPREAD: f32 = 900.0;

/// The band a synthetic crown draws in: everything, so the count the HUD shows
/// is the frustum's own work rather than the chain's.
const BAND: [f32; 4] = [0.0, 0.0, FAR, FAR];

fn env<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "instance-lab".into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(FrameTimeDiagnosticsPlugin::default())
        .add_plugins(shadow::CloudShadowPlugin)
        .add_plugins(capture::CapturePlugin)
        .add_plugins(InstancingPlugin)
        .add_systems(Startup, setup)
        .add_systems(Update, (camera::fly, hud))
        .run();
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut mediums: ResMut<Assets<ScatteringMedium>>,
    mut images: ResMut<Assets<Image>>,
    mut instanced: ResMut<Instanced>,
) {
    commands.spawn(Atmosphere::earth(
        mediums.add(ScatteringMedium::earth(256, 256)),
    ));

    let (x, y, z, yaw, pitch) = env::<String>("INSTANCE_LAB_CAM")
        .and_then(|s| {
            let v: Vec<f32> = s.split(',').filter_map(|p| p.parse().ok()).collect();
            (v.len() == 5).then(|| (v[0], v[1], v[2], v[3], v[4]))
        })
        .unwrap_or((0.0, 3.0, 300.0, 0.0, -2.0));
    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            far: 40000.0,
            ..default()
        }),
        Transform::from_xyz(x, y, z),
        AtmosphereSettings {
            rendering_method: AtmosphereMode::Raymarched,
            ..default()
        },
        AtmosphereEnvironmentMapLight::default(),
        Exposure::SUNLIGHT,
        // The same prepass and occlusion culling the viewer's camera carries,
        // so the bench exercises the depth path and not only the main pass.
        DepthPrepass,
        OcclusionCulling,
        // `camera::fly` rewrites the rotation from these every frame, so the
        // aim has to live here rather than on the transform.
        FlyCamera {
            yaw: yaw.to_radians(),
            pitch: pitch.to_radians(),
            ..default()
        },
    ));

    commands.spawn((
        DirectionalLight {
            illuminance: lux::FULL_DAYLIGHT,
            shadow_maps_enabled: true,
            ..default()
        },
        SunDisk::EARTH,
        Transform::from_xyz(60.0, 100.0, 40.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // A plain green plane, drawn the ordinary way: the instanced path is what
    // is under test, and something has to catch the shadows.
    let spread: f32 = env("INSTANCE_LAB_SPREAD").unwrap_or(DEFAULT_SPREAD);
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(spread * 3.0, spread * 3.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.24, 0.32, 0.16),
            perceptual_roughness: 1.0,
            ..default()
        })),
        Transform::IDENTITY,
    ));

    // The surfaces the instanced pipeline binds. The leaf cutout is the
    // viewer's own; the cloud shadow and the block mask are the blanks a scene
    // with no weather and no fine map would carry.
    let texels = tree_texture::DEFAULT_TEXELS;
    let leaf = images.add(tiled(
        tree_texture::leaf_cutout(false, 0.34, texels).rgba,
        texels,
    ));
    let cloud = images.add(single(255));
    let mask = images.add(single(0));

    let count: usize = env("DWARF_EYE_INSTANCE_TEST").unwrap_or(DEFAULT_COUNT);
    instanced.leaf = Some(leaf);
    instanced.cloud = Some(cloud);
    instanced.mask = Some(mask);
    instanced.scene = shadow::ShadowUniform {
        canopy: shadow::canopy_sky(),
        period: 1.0,
        ..default()
    };
    instanced.replace(vec![scatter(count, spread)]);
    info!("instance-lab: {count} instances over {spread} tiles");

    commands.spawn((
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(14.0),
            ..default()
        },
        Node {
            position_type: PositionType::Absolute,
            top: px(10),
            left: px(12),
            ..default()
        },
        Hud,
    ));
}

#[derive(Component)]
struct Hud;

/// One batch: the canonical oak crown, everywhere.
///
/// A canonical crown is what the far band's middle stage draws, so this is the
/// same geometry the integration will push through the same path.
fn scatter(count: usize, spread: f32) -> instancing::Batch {
    let mesh = dwarf_eye_trees::crown(Preset::Oak);
    let height = mesh.positions.iter().map(|p| p[1]).fold(1.0f32, f32::max);
    let radius = mesh
        .positions
        .iter()
        .map(|p| p[0].abs().max(p[2].abs()))
        .fold(0.5f32, f32::max);

    let mut instances = Vec::with_capacity(count);
    // A cheap lattice with a hashed jitter: a grid reads as a grid, and a
    // hashed position is what the real scatter hands over anyway.
    let side = (count as f32).sqrt().ceil() as usize;
    let pitch = spread / side as f32;
    for i in 0..count {
        let (gx, gz) = (i % side, i / side);
        let jitter = |k: u32| {
            let mut h = (i as u32).wrapping_mul(0x9e37_79b9) ^ k.wrapping_mul(0x85eb_ca6b);
            h ^= h >> 15;
            h = h.wrapping_mul(0x2545_f491);
            (h >> 8) as f32 / (1u32 << 24) as f32
        };
        let pos = Vec3::new(
            (gx as f32 - side as f32 / 2.0 + jitter(1)) * pitch,
            0.0,
            (gz as f32 - side as f32 / 2.0 + jitter(2)) * pitch,
        );
        let scale = 0.72 + jitter(3) * 0.63;
        instances.push(Instance {
            pos,
            yaw: jitter(4) * std::f32::consts::TAU,
            scale,
            radius: radius * scale,
            centre_y: height * 0.5 * scale,
            dither: dither_of(pos),
            band: BAND,
            tint: [1.0; 4],
        });
    }

    instancing::Batch {
        positions: mesh.positions,
        normals: mesh.normals,
        uvs: mesh.uvs,
        colors: mesh.colors,
        indices: mesh.indices,
        instances,
        // A canonical crown bakes a lit top and darker sides into its vertex
        // colours, so it takes no sky term; the same rule the viewer applies.
        canopy: 0.0,
        roughness: 0.97,
        casts: true,
    }
}

fn hud(
    counts: Res<InstanceCounts>,
    diagnostics: Res<DiagnosticsStore>,
    camera: Query<&Transform, With<FlyCamera>>,
    mut hud: Query<&mut Text, With<Hud>>,
) {
    let (Ok(mut text), Ok(transform)) = (hud.single_mut(), camera.single()) else {
        return;
    };
    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);
    text.0 = format!(
        "instances {}   drawn {}   culled {}   triangles {}   {fps:.0} fps\n\
         camera ({:.0}, {:.0}, {:.0})\n\
         WASD move   QE up/down   shift boost   right-drag look   wheel speed",
        counts.instances,
        counts.drawn,
        counts.culled(),
        counts.triangles,
        transform.translation.x,
        transform.translation.y,
        transform.translation.z,
    );
}

/// A one-texel image, for the cloud shadow and the block mask a bench has none
/// of: an unbound texture drops its binding from the pipeline layout entirely.
fn single(value: u8) -> Image {
    let mut image = Image::new(
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        vec![value, value, value, 255],
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::linear();
    image
}

/// The leaf cutout, wrapped and mipped the way the far band wears it.
fn tiled(rgba: Vec<u8>, texels: u32) -> Image {
    let mut image = Image::new(
        Extent3d {
            width: texels,
            height: texels,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Nearest,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    image
}
