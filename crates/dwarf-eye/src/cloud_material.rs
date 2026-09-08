//! Shading for cloud geometry.

use bevy::asset::embedded_asset;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

/// Cloud geometry: the standard vertex path, with the lighting replaced.
pub type CloudMaterial = ExtendedMaterial<StandardMaterial, CloudSubsurface>;

const SHADER: &str = "embedded://dwarf_eye/cloud.wgsl";

/// How a cloud takes light.
#[derive(Clone, Copy, Debug, Reflect, ShaderType)]
pub struct CloudUniform {
    pub sun: Vec3,
    /// How far past the terminator light wraps. 1.0 lights the whole sphere.
    pub wrap: f32,
    pub sun_color: Vec3,
    /// Strength of light bleeding through thin parts.
    pub translucency: f32,
    pub sky_color: Vec3,
    /// Tightness of the forward-scattering lobe.
    pub scatter_power: f32,
    pub ground_color: Vec3,
    pub brightness: f32,
}

impl Default for CloudUniform {
    fn default() -> Self {
        Self {
            sun: Vec3::Y,
            wrap: 0.38,
            sun_color: Vec3::new(1.0, 0.97, 0.92),
            translucency: 1.6,
            sky_color: Vec3::new(0.52, 0.62, 0.78),
            scatter_power: 6.0,
            ground_color: Vec3::new(0.30, 0.32, 0.30),
            brightness: 1.0,
        }
    }
}

/// Bindings start at 100; the base `StandardMaterial` owns everything below.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct CloudSubsurface {
    #[uniform(100)]
    pub uniform: CloudUniform,
}

impl MaterialExtension for CloudSubsurface {
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }
}

pub struct CloudMaterialPlugin;

impl Plugin for CloudMaterialPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "cloud.wgsl");
        app.add_plugins(MaterialPlugin::<CloudMaterial>::default());
    }
}
