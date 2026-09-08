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

use bevy::asset::{AssetPath, embedded_asset, embedded_path};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

/// Embedded, so the binary carries its own shader instead of hunting for a file
/// next to wherever it happens to be run from.
///
/// `embedded_path!` keys the shader on the binary that embedded it and on this
/// file's path inside it, which is not the same in the viewer as in a bench
/// that includes this module from `bin/`. Asking for it the same way it was
/// registered keeps both binaries finding it.
macro_rules! embedded_shader {
    ($name: expr) => {
        ShaderRef::Path(AssetPath::from_path_buf(embedded_path!($name)).with_source("embedded"))
    };
}

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
    /// Non-zero on a horizon material, all of which yield to loaded blocks,
    /// and which of the two it is:
    ///
    /// - `1` the coarse ground, whose UV names an atlas cell rather than a
    ///   point; `cloud_shadow.wgsl:horizon_texel` wraps that sprite across the
    ///   surface by world position, one sprite to a world tile;
    /// - `2` the far crowns, whose UVs are already world-space on a leaf
    ///   texture of their own and want the standard material's own sampling.
    pub horizon: f32,
    /// Block coordinate of the mask's first texel.
    pub mask_origin: Vec2,
    /// How much of the sky's indirect light foliage keeps, 0 on the ground.
    ///
    /// The sky fills from every direction, which leaves a crown lit all round
    /// and with no shaded side. Foliage scales that fill down and weights it
    /// toward the sky, so the sun is what decides which side of a tree is lit.
    pub canopy: f32,
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

/// How much of the sky's fill a crown keeps, weighted toward the faces that
/// look up. The sky lights every side of a tree alike, so at full strength a
/// crown has no shaded side at all and the sun stops reading as the light.
/// `DWARF_EYE_CANOPY_SKY` overrides, for finding the right level.
///
/// The floor is above zero because zero would switch the term off rather than
/// black the shade out.
pub fn canopy_sky() -> f32 {
    std::env::var("DWARF_EYE_CANOPY_SKY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.45f32)
        .clamp(0.005, 2.0)
}

/// How much light a leaf passes from behind: enough to glow against a low sun,
/// little enough that the sky no longer lights the whole crown through it.
/// `DWARF_EYE_LEAF_LIGHT` overrides.
pub fn leaf_transmission() -> f32 {
    std::env::var("DWARF_EYE_LEAF_LIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.1f32)
        .clamp(0.0, 1.0)
}

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
        embedded_shader!("cloud_shadow.wgsl")
    }

    /// The prepass writes depth before the main pass runs, so the horizon has
    /// to yield there too or its depth would hide the fine ground behind it.
    fn prepass_fragment_shader() -> ShaderRef {
        embedded_shader!("cloud_shadow_prepass.wgsl")
    }
}
