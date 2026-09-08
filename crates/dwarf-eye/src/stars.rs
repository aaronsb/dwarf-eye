//! A night sky that turns with Dwarf Fortress's clock.
//!
//! The stars are one mesh of small unlit quads on a far sphere, spun by a
//! parent transform and faded by the sun's elevation. Drawing them as geometry
//! rather than into the sky texture keeps them independent of the atmosphere.

use crate::sky::Clock;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;

/// How far out the stars sit: beyond the horizon mesh, which reaches some nine
/// thousand tiles, and well inside the camera's 40000-unit far plane. Nearer
/// than the terrain they would show through distant hills.
const SPHERE_RADIUS: f32 = 20000.0;
const STAR_COUNT: usize = 1800;
/// Angular size on that sphere, in world units.
const STAR_SIZE: f32 = 13.6;

/// The axis the sky turns about, tilted off vertical so stars arc rather than
/// spin flat overhead.
const AXIS_TILT: f32 = 0.62;

#[derive(Component)]
pub struct StarField;

#[derive(Resource)]
pub struct StarMaterial(pub Handle<StandardMaterial>);

/// A small deterministic generator, so the sky is the same every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as f32) / (u32::MAX as f32 / 2.0)
    }
}

/// Builds the star mesh and hangs it on a pivot that the clock will turn.
pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Unlit takes its colour from base_color alone — emissive belongs to the
    // lit path — and the mesh's vertex colours carry each star's brightness.
    // Being unlit also keeps them clear of the exposure, so they hold steady
    // while the scene's own stop opens at night.
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 1.0, 1.0, 0.0),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    commands.insert_resource(StarMaterial(material.clone()));

    commands.spawn((
        Mesh3d(meshes.add(build_star_mesh())),
        MeshMaterial3d(material),
        Transform::from_rotation(Quat::from_rotation_z(AXIS_TILT)),
        // The sky is a backdrop, not part of the scene's lighting.
        NotShadowCaster,
        NotShadowReceiver,
        StarField,
    ));
}

fn build_star_mesh() -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(STAR_COUNT * 4);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(STAR_COUNT * 4);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(STAR_COUNT * 4);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(STAR_COUNT * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(STAR_COUNT * 6);
    let mut rng = Rng(0x5EED_5741);

    for _ in 0..STAR_COUNT {
        // Uniform on the sphere: z spread evenly, angle spread evenly.
        let z = rng.next() * 2.0 - 1.0;
        let phi = rng.next() * std::f32::consts::TAU;
        let r = (1.0 - z * z).max(0.0).sqrt();
        let dir = Vec3::new(r * phi.cos(), z, r * phi.sin());
        let centre = dir * SPHERE_RADIUS;

        // A quad facing the origin.
        let right = dir.cross(Vec3::Y).normalize_or(Vec3::X);
        let up = right.cross(dir).normalize();

        // Brightness follows a steep curve, so a few stars dominate.
        let magnitude = rng.next().powf(3.2);
        let size = STAR_SIZE * (0.45 + magnitude * 1.4);
        // Hotter stars run blue, cooler ones amber.
        let warmth = rng.next();
        let tint = [
            0.72 + warmth * 0.28,
            0.78 + warmth * 0.14,
            1.0 - warmth * 0.22,
        ];
        let brightness = 0.25 + magnitude * 3.2;
        let color = [tint[0] * brightness, tint[1] * brightness, tint[2] * brightness, 1.0];

        let base = positions.len() as u32;
        for (dx, dy, u, v) in [
            (-1.0, -1.0, 0.0, 0.0),
            (-1.0, 1.0, 0.0, 1.0),
            (1.0, 1.0, 1.0, 1.0),
            (1.0, -1.0, 1.0, 0.0),
        ] {
            positions.push((centre + right * (dx * size) + up * (dy * size)).to_array());
            normals.push((-dir).to_array());
            colors.push(color);
            uvs.push([u, v]);
        }
        // Wound so the face the viewer sees is the front one: the quads look
        // inward from the sphere, and back-face culling eats the other order.
        indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Turns the sky with the clock, keeps it centred on the viewer, and fades it
/// out as the sun climbs.
pub fn drive(
    clock: Res<Clock>,
    material: Res<StarMaterial>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    camera: Query<&Transform, (With<crate::camera::FlyCamera>, Without<StarField>)>,
    mut field: Query<&mut Transform, With<StarField>>,
) {
    let Ok(mut transform) = field.single_mut() else { return };

    let spin = clock.day_fraction() * std::f32::consts::TAU;
    transform.rotation = Quat::from_rotation_z(AXIS_TILT) * Quat::from_rotation_y(spin);
    if let Ok(view) = camera.single() {
        transform.translation = view.translation;
    }

    // Twilight fades them in; the sun blots them out well before it rises.
    let elevation = clock.sun_direction().y;
    let visibility = (-elevation * 6.0).clamp(0.0, 1.0);
    if let Some(mut m) = materials.get_mut(&material.0) {
        m.base_color = Color::srgba(1.0, 1.0, 1.0, visibility);
    }
}
