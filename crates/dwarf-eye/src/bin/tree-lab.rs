//! A bench for the procedural tree generator: a row of the presets on a green
//! plane under the same sky the main viewer uses.
//!
//! Trees on the back row, the smaller vegetation in front. R reseeds, 1..0
//! forces one preset on every slot and Tab cycles through the rest, `[` and `]`
//! change the voxel resolution, `-` and `=` halve and double the texel density,
//! F12 saves a screenshot. `DWARF_EYE_SHOT=path[:seconds]` saves one after a
//! delay and exits; `DWARF_EYE_TEXELS` sets the starting texel density,
//! `TREE_LAB_CAM=x,y,z,yaw,pitch` the camera,
//! `TREE_LAB_SUN=azimuth,elevation` (degrees) where the sun stands, and
//! `TREE_LAB_WOOD` the resolution whose limb widths the cut shows, which
//! otherwise follows the detail band of the chosen resolution.

#[path = "../camera.rs"]
mod camera;
// The canopy material itself, so the lab shades leaves exactly as the viewer
// does: same extension, same sky-fill occlusion, same knobs.
// The lab binds the whole material but reads none of the map's own knobs.
#[allow(dead_code)]
#[path = "../shadow.rs"]
mod shadow;

use bevy::asset::RenderAssetUsages;
use bevy::camera::Exposure;
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::{
    Atmosphere, AtmosphereEnvironmentMapLight, SunDisk, atmosphere::ScatteringMedium,
    light_consts::lux,
};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{AtmosphereMode, AtmosphereSettings};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use camera::FlyCamera;
use shadow::{
    CloudShadow, ShadowUniform, TerrainMaterial as TerrainMat, canopy_sky, leaf_transmission,
};
use dwarf_eye_trees::texture::{self, Texels};
use dwarf_eye_trees::{Cut, Habit, Kind, Preset, TreeParams, grow, mesh_of, rasterise_cut};
use dwarf_eye_world::canopy;

/// Tiles between trunks along a row.
const SPACING: f32 = 16.0;
/// Slots per row; the rest spill onto a second row nearer the camera.
const PER_ROW: usize = 6;
/// Tiles between the rows.
const ROW_DEPTH: f32 = 22.0;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window { title: "tree-lab".into(), ..default() }),
            ..default()
        }))
        .add_plugins(shadow::CloudShadowPlugin)
        .init_resource::<Lab>()
        .init_resource::<TexelDensity>()
        .init_resource::<Report>()
        .add_systems(Startup, setup)
        .add_systems(Update, (handle_input, rebuild, retexture, update_hud, screenshot))
        // An unattended shot must not follow the keyboard: the window can take
        // focus from whatever the operator is doing and fly the camera away.
        .add_systems(Update, camera::fly.run_if(|| std::env::var("DWARF_EYE_SHOT").is_err()))
        .run();
}

#[derive(Resource)]
struct Lab {
    /// `None` shows one of each preset.
    forced: Option<Preset>,
    seed: u64,
    voxels_per_tile: u32,
    /// `TREE_LAB_WOOD`, overriding the resolution whose limb widths this cut
    /// shows. Unset, the lab shows what the viewer draws; set to the cut's own
    /// resolution, it shows the uncorrected coarse cut the bands were measured
    /// against.
    wood_like: Option<u32>,
    dirty: bool,
}

impl Lab {
    /// What this cut asks the rasteriser for: the band's own correction at
    /// this resolution, unless `TREE_LAB_WOOD` overrides it.
    fn cut(&self) -> Cut {
        match self.wood_like {
            Some(wood_like) => Cut { wood_like },
            None => canopy::cut_at(self.voxels_per_tile as i32),
        }
    }
}

impl Default for Lab {
    fn default() -> Self {
        let forced = std::env::var("TREE_LAB_PRESET")
            .ok()
            .and_then(|name| Preset::ALL.iter().find(|p| p.name() == name).copied());
        let seed = std::env::var("TREE_LAB_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        let voxels_per_tile =
            std::env::var("TREE_LAB_VPT").ok().and_then(|v| v.parse().ok()).unwrap_or(4);
        let wood_like = std::env::var("TREE_LAB_WOOD").ok().and_then(|v| v.parse().ok());
        Self { forced, seed, voxels_per_tile, wood_like, dirty: true }
    }
}

/// Where the sun stands, from `TREE_LAB_SUN=azimuth,elevation` in degrees:
/// azimuth 0 puts it behind the camera, 90 to its right, 180 beyond the trees.
/// The viewer takes its sun from the game clock, so this is the only place a
/// low sun can be aimed at a crown on purpose.
fn sun_direction() -> Vec3 {
    let Some(spec) = std::env::var("TREE_LAB_SUN").ok() else {
        return Vec3::new(55.0, 62.0, 85.0).normalize();
    };
    let n: Vec<f32> = spec.split(',').filter_map(|v| v.trim().parse::<f32>().ok()).collect();
    if n.len() != 2 {
        return Vec3::new(55.0, 62.0, 85.0).normalize();
    }
    let (azimuth, elevation) = (n[0].to_radians(), n[1].to_radians());
    // The camera looks up -Z, so azimuth turns from +Z, the way it faces.
    Vec3::new(azimuth.sin() * elevation.cos(), elevation.sin(), azimuth.cos() * elevation.cos())
        .normalize()
}

/// `TREE_LAB_CAM=x,y,z,yaw_deg,pitch_deg` places the camera for an unattended
/// shot.
fn requested_camera() -> Option<(Vec3, f32, f32)> {
    let spec = std::env::var("TREE_LAB_CAM").ok()?;
    let n: Vec<f32> = spec.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    (n.len() == 5).then(|| {
        (Vec3::new(n[0], n[1], n[2]), n[3].to_radians(), n[4].to_radians())
    })
}

/// Texels per world tile. Dwarf Fortress's art is 32 to a tile and the main
/// app draws the ground at that density, so tree surfaces match it by default.
#[derive(Resource)]
struct TexelDensity {
    per_tile: u32,
    dirty: bool,
}

impl Default for TexelDensity {
    fn default() -> Self {
        let per_tile = std::env::var("DWARF_EYE_TEXELS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(32)
            .clamp(4, 128);
        Self { per_tile, dirty: false }
    }
}

#[derive(Resource, Default)]
struct Report(String);

/// Bark is shared; foliage is one cutout per species, because porosity is a
/// species trait.
#[derive(Resource)]
struct Materials {
    bark: Handle<TerrainMat>,
    leaves: HashMap<Preset, Handle<TerrainMat>>,
    /// Weeping strands, on their own leaflet strip.
    streamers: Handle<TerrainMat>,
}

#[derive(Component)]
struct Tree;

#[derive(Component)]
struct Hud;

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut canopy: ResMut<Assets<TerrainMat>>,
    mut images: ResMut<Assets<Image>>,
    mut mediums: ResMut<Assets<ScatteringMedium>>,
    texels: Res<TexelDensity>,
) {
    commands.spawn(Atmosphere::earth(mediums.add(ScatteringMedium::earth(256, 256))));

    let (place, yaw, pitch) =
        requested_camera().unwrap_or((Vec3::new(0.0, 10.0, 74.0), 0.0, -0.03));

    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection { far: 4000.0, ..default() }),
        // Rotation is set here as well as in the fly camera, because unattended
        // shots run with the fly camera switched off.
        Transform::from_translation(place)
            .with_rotation(Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0)),
        AtmosphereSettings {
            rendering_method: AtmosphereMode::Raymarched,
            sky_max_samples: 32,
            ..default()
        },
        Exposure { ev100: 13.0 },
        Tonemapping::AcesFitted,
        DebandDither::Enabled,
        AtmosphereEnvironmentMapLight { intensity: 2.6, size: UVec2::splat(1024), ..default() },
        FlyCamera { yaw, pitch, ..default() },
    ));

    commands.spawn((
        DirectionalLight {
            illuminance: lux::RAW_SUNLIGHT,
            shadow_maps_enabled: true,
            shadow_depth_bias: 0.04,
            shadow_normal_bias: 1.2,
            ..default()
        },
        SunDisk::EARTH,
        Transform::from_translation(sun_direction() * 4000.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(20000.0, 20000.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.22, 0.34, 0.13),
            perceptual_roughness: 0.96,
            reflectance: 0.02,
            ..default()
        })),
    ));

    let tex = texels.per_tile;
    let bark_texture = images.add(pixels(texture::bark(tex)));
    let streamer_texture = images.add(pixels(texture::streamer_strip(tex)));

    // The cloud shadow lookup is switched off here: the lab has no weather.
    // What the extension is carried for is its canopy term, which is what
    // gives a crown a shaded side.
    let flat = images.add(one_texel(255));
    let unmasked = images.add(one_texel(0));
    let canopy_material = |base: StandardMaterial| TerrainMat {
        base,
        extension: CloudShadow {
            uniform: ShadowUniform { canopy: canopy_sky(), ..default() },
            map: flat.clone(),
            mask: unmasked.clone(),
        },
    };

    let foliage = |texture: Handle<Image>| StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(texture),
        // A cutout, so the leaf faces read as leaves and not as cubes.
        alpha_mode: AlphaMode::Mask(0.5),
        double_sided: true,
        cull_mode: None,
        perceptual_roughness: 0.97,
        reflectance: 0.02,
        // Leaves pass a little light, enough to glow when the sun is behind
        // them. More than that and the sky lights every side of the crown.
        diffuse_transmission: leaf_transmission(),
        thickness: 0.12,
        ..default()
    };

    let mut leaves = HashMap::new();
    for preset in Preset::ALL {
        let params = TreeParams::preset(preset);
        let needle = params.habit == Habit::Conifer;
        let cutout = images.add(pixels(texture::leaf_cutout(needle, params.cutout_openness, tex)));
        leaves.insert(preset, canopy.add(canopy_material(foliage(cutout))));
    }

    commands.insert_resource(Materials {
        streamers: canopy.add(canopy_material(foliage(streamer_texture))),
        bark: canopy.add(canopy_material(StandardMaterial {
            base_color: Color::WHITE,
            base_color_texture: Some(bark_texture),
            perceptual_roughness: 0.95,
            reflectance: 0.02,
            ..default()
        })),
        leaves,
    });

    commands.spawn((
        Text::new(""),
        TextFont { font_size: FontSize::Px(13.0), ..default() },
        Node { position_type: PositionType::Absolute, left: px(8), top: px(8), ..default() },
        Hud,
    ));
}

fn handle_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut lab: ResMut<Lab>,
    mut texels: ResMut<TexelDensity>,
) {
    if keys.just_pressed(KeyCode::Minus) && texels.per_tile > 4 {
        texels.per_tile /= 2;
        texels.dirty = true;
    }
    if keys.just_pressed(KeyCode::Equal) && texels.per_tile < 128 {
        texels.per_tile *= 2;
        texels.dirty = true;
    }
    if keys.just_pressed(KeyCode::KeyR) {
        lab.seed = lab.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        lab.dirty = true;
    }
    const DIGITS: [KeyCode; 10] = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
        KeyCode::Digit0,
    ];
    for (key, preset) in DIGITS.iter().zip(Preset::ALL) {
        if keys.just_pressed(*key) {
            lab.forced = if lab.forced == Some(preset) { None } else { Some(preset) };
            lab.dirty = true;
        }
    }
    // Tab reaches the presets past the digits, and back to the whole set.
    if keys.just_pressed(KeyCode::Tab) {
        lab.forced = match lab.forced {
            None => Some(Preset::ALL[0]),
            Some(current) => {
                let next = Preset::ALL.iter().position(|p| *p == current).unwrap_or(0) + 1;
                Preset::ALL.get(next).copied()
            }
        };
        lab.dirty = true;
    }
    if keys.just_pressed(KeyCode::BracketLeft) && lab.voxels_per_tile > 1 {
        lab.voxels_per_tile -= 1;
        lab.dirty = true;
    }
    if keys.just_pressed(KeyCode::BracketRight) && lab.voxels_per_tile < 8 {
        lab.voxels_per_tile += 1;
        lab.dirty = true;
    }
}

fn rebuild(
    mut commands: Commands,
    mut lab: ResMut<Lab>,
    mut report: ResMut<Report>,
    mut meshes: ResMut<Assets<Mesh>>,
    materials: Res<Materials>,
    existing: Query<Entity, With<Tree>>,
) {
    if !lab.dirty {
        return;
    }
    lab.dirty = false;
    for entity in &existing {
        commands.entity(entity).despawn();
    }

    let mut lines = Vec::new();
    for (i, slot) in Preset::ALL.iter().enumerate() {
        let preset = lab.forced.unwrap_or(*slot);
        let mut params = TreeParams::preset(preset);
        // TREE_LAB_HEIGHT grows every preset at one height, for comparing a
        // preset here against the same species in the game.
        if let Some(height) = std::env::var("TREE_LAB_HEIGHT").ok().and_then(|v| v.parse().ok()) {
            params.height = height;
        }
        let seed = lab.seed.wrapping_add(i as u64 * 0x9E3779B97F4A7C15);
        let skeleton = grow(&params, seed, None);
        let voxels = rasterise_cut(&skeleton, lab.voxels_per_tile, lab.cut());
        let counts = voxels.counts();
        let row = (i / PER_ROW) as f32;
        // Rows nearer the camera hold the smaller vegetation, offset half a slot
        // so nothing hides behind the row behind it.
        let x = ((i % PER_ROW) as f32 - (PER_ROW as f32 - 1.0) * 0.5) * SPACING
            + row * SPACING * 0.5;
        let z = row * ROW_DEPTH;

        let mut triangles = 0;
        for (kind, material) in [
            (Kind::Bark, materials.bark.clone()),
            (Kind::Leaf, materials.leaves[&preset].clone()),
            (Kind::Streamer, materials.streamers.clone()),
        ] {
            let built = mesh_of(&voxels, Some(kind));
            if built.indices.is_empty() {
                continue;
            }
            triangles += built.indices.len() / 3;
            commands.spawn((
                Mesh3d(meshes.add(to_bevy(&built))),
                MeshMaterial3d(material),
                Transform::from_xyz(x, 0.0, z),
                Tree,
            ));
        }
        lines.push(format!(
            "{}: {} bark + {} leaf voxels, {} strands, {triangles} tris",
            preset.name(),
            counts.bark,
            counts.leaf,
            voxels.streamers.len(),
        ));
    }
    report.0 = lines.join("\n");
    info!("\n{}", report.0);
}

/// Rebuild the procedural textures at a new density, in place, so the meshes
/// and the camera stay put while the pixel scale changes.
fn retexture(
    mut texels: ResMut<TexelDensity>,
    materials: Res<Materials>,
    mut images: ResMut<Assets<Image>>,
    mut standard: ResMut<Assets<TerrainMat>>,
) {
    if !texels.dirty {
        return;
    }
    texels.dirty = false;
    let tex = texels.per_tile;

    let bark = images.add(pixels(texture::bark(tex)));
    if let Some(mut material) = standard.get_mut(&materials.bark) {
        material.base.base_color_texture = Some(bark);
    }
    let strip = images.add(pixels(texture::streamer_strip(tex)));
    if let Some(mut material) = standard.get_mut(&materials.streamers) {
        material.base.base_color_texture = Some(strip);
    }
    for preset in Preset::ALL {
        let params = TreeParams::preset(preset);
        let needle = params.habit == Habit::Conifer;
        let leaf = images.add(pixels(texture::leaf_cutout(needle, params.cutout_openness, tex)));
        if let Some(mut material) = standard.get_mut(&materials.leaves[&preset]) {
            material.base.base_color_texture = Some(leaf);
        }
    }
    info!("texel density now {tex} per tile");
}

fn to_bevy(source: &dwarf_eye_trees::TreeMesh) -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, source.positions.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, source.normals.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, source.uvs.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, source.colors.clone())
        .with_inserted_indices(Indices::U32(source.indices.clone()))
}

fn update_hud(
    lab: Res<Lab>,
    texels: Res<TexelDensity>,
    report: Res<Report>,
    hud: Query<&mut Text, With<Hud>>,
) {
    if !lab.is_changed() && !report.is_changed() && !texels.is_changed() {
        return;
    }
    for mut text in hud {
        text.0 = format!(
            "seed {}  {} voxels/tile  wood at {}  {} texels/tile  {}\nR reseed  1..0 and Tab preset  [ ] voxels  - = texels  F12 shot\n{}",
            lab.seed,
            lab.voxels_per_tile,
            lab.cut().wood_like,
            texels.per_tile,
            lab.forced.map(|p| p.name()).unwrap_or("all presets"),
            report.0,
        );
    }
}

/// F12 at any time, and `DWARF_EYE_SHOT=path[:seconds]` for an unattended run.
fn screenshot(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut commands: Commands,
    mut elapsed: Local<f32>,
    mut exit: MessageWriter<AppExit>,
) {
    if keys.just_pressed(KeyCode::F12) {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk("tree-lab.png"));
    }
    let Some(spec) = std::env::var("DWARF_EYE_SHOT").ok() else { return };
    let (path, delay) = match spec.rsplit_once(':') {
        Some((p, d)) if d.parse::<f32>().is_ok() => (p.to_string(), d.parse().unwrap_or(6.0)),
        _ => (spec, 6.0),
    };
    let before = *elapsed;
    *elapsed += time.delta_secs();
    if before < delay && *elapsed >= delay {
        info!("saving screenshot to {path}");
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *elapsed > delay + 1.5 {
        exit.write(AppExit::Success);
    }
}

/// A one-texel red image, for the bindings the canopy material always carries
/// but the lab never reads: there is no weather here and no coarse horizon.
fn one_texel(value: u8) -> Image {
    Image::new(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        vec![value],
        TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn pixels(texels: Texels) -> Image {
    let mut image = Image::new(
        Extent3d { width: texels.width, height: texels.height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        texels.rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    // World-space UVs run well past 0..1, so the textures have to wrap.
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        // Nearest keeps the cutout's edge hard, which is the whole point of it.
        mag_filter: ImageFilterMode::Nearest,
        min_filter: ImageFilterMode::Nearest,
        ..default()
    });
    image
}
