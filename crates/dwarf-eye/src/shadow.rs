//! The terrain material, extended so the clouds shade the ground and the
//! coarse horizon gives way to the detailed map.
//!
//! The clouds are a transparent volume, so they never enter the shadow map.
//! Instead `clouds::bake_shadow` marches the cloud field toward the sun on the
//! CPU and hands the result here as a transmittance map, which the terrain
//! shader looks up along the sun's slant.
//!
//! The horizon mesh is a smooth surface through coarse samples, so wherever
//! fine chunks exist it would cut through them. A block mask marks every
//! loaded block, and horizon fragments over a marked block are discarded, in
//! the main pass and the depth prepass alike.

use bevy::asset::embedded_asset;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

/// Embedded, so the binary carries its own shader instead of hunting for a file
/// next to wherever it happens to be run from.
const SHADER: &str = "embedded://dwarf_eye/cloud_shadow.wgsl";

/// Terrain shading, plus a cloud shadow lookup.
pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, CloudShadow>;

/// Where the shadow map came from, and how to read it.
#[derive(Clone, Copy, Debug, Default, Reflect, ShaderType)]
pub struct ShadowUniform {
    /// Direction toward the sun the map was baked for.
    pub sun: Vec3,
    /// Fraction of light a fully opaque cloud takes away, 0..1.
    pub strength: f32,
    /// Wind offset in tiles: the field is sampled at world + wind.
    pub wind: Vec2,
    /// The map covers one period of the field.
    pub period: f32,
    /// Height the map was baked at.
    pub ground: f32,
    /// Zero when the sky is clear, so the lookup is skipped.
    pub enabled: f32,
    /// One on the horizon material, which yields to loaded blocks.
    pub horizon: f32,
    /// Block coordinate of the mask's first texel.
    pub mask_origin: Vec2,
}

/// Bindings start at 100; the base `StandardMaterial` owns everything below.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct CloudShadow {
    #[uniform(100)]
    pub uniform: ShadowUniform,
    /// Never optional: leaving a texture unbound drops its binding from the
    /// pipeline layout, and the shader then fails validation.
    #[texture(101)]
    #[sampler(102)]
    pub map: Handle<Image>,
    /// One texel per 16-tile block, non-zero where a fine chunk is loaded.
    #[texture(103)]
    pub mask: Handle<Image>,
}

/// Blocks covered by the mask on each side.
pub const MASK_BLOCKS: u32 = 512;

/// Registers the material and embeds its shader.
pub struct CloudShadowPlugin;

impl Plugin for CloudShadowPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "cloud_shadow.wgsl");
        embedded_asset!(app, "cloud_shadow_prepass.wgsl");
        app.add_plugins(MaterialPlugin::<TerrainMaterial>::default());
    }
}

impl MaterialExtension for CloudShadow {
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }

    /// The prepass writes depth before the main pass runs, so the horizon has
    /// to yield there too or its depth would hide the fine ground behind it.
    fn prepass_fragment_shader() -> ShaderRef {
        "embedded://dwarf_eye/cloud_shadow_prepass.wgsl".into()
    }
}
