//! The terrain material, extended so the cloud deck shades the ground.
//!
//! Bevy's volumetric fog lights fog but never shadows scene geometry, so
//! without this the clouds float over a fully lit landscape.

use bevy::asset::embedded_asset;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

/// Embedded, so the binary carries its own shader instead of hunting for a file
/// next to wherever it happens to be run from.
const SHADER: &str = "embedded://dwarf_eye/cloud_shadow.wgsl";

/// Terrain shading, plus a cloud shadow pass.
pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, CloudShadow>;

/// What the shader needs to march the cloud volume toward the sun.
#[derive(Clone, Copy, Debug, Default, Reflect, ShaderType)]
pub struct CloudUniform {
    /// Direction from the world toward the sun.
    pub sun: Vec3,
    /// How much light a fully opaque cloud takes away, 0..1.
    pub strength: f32,
    /// Centre of the cloud deck in world space.
    pub centre: Vec3,
    pub density_scale: f32,
    /// Full extent of the deck in world space.
    pub size: Vec3,
    /// Zero when the sky is clear, so the march is skipped outright.
    pub enabled: f32,
    /// Wind offset applied to the density texture.
    pub offset: Vec3,
    pub padding: f32,
}

/// Bindings start at 100; the base `StandardMaterial` owns everything below.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct CloudShadow {
    #[uniform(100)]
    pub uniform: CloudUniform,
    /// Never optional: leaving a texture unbound drops its binding from the
    /// pipeline layout, and the shader then fails validation.
    #[texture(101, dimension = "3d")]
    #[sampler(102)]
    pub density: Handle<Image>,
}

/// Registers the material and embeds its shader.
pub struct CloudShadowPlugin;

impl Plugin for CloudShadowPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "cloud_shadow.wgsl");
        app.add_plugins(MaterialPlugin::<TerrainMaterial>::default());
    }
}

impl MaterialExtension for CloudShadow {
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }
}
