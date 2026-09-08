//! The ground atlas as a GPU texture.
//!
//! Sampled nearest with no mip chain, pixel art sparkles at distance: each
//! screen pixel picks one texel out of the many it covers. A mip chain gives
//! the sampler pre-averaged levels to pick from. Magnification stays nearest,
//! so the art is crisp up close.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Levels below the base. The atlas pads each 32 px sprite by 16 px of its
/// own edge, which keeps neighbours out of the chain for four halvings.
const MIP_LEVELS: u32 = 4;

/// sRGB bytes to linear and back, so averages are done on light rather than
/// on encoded values.
fn to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let c = if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0 + 0.5) as u8
}

/// Halves an RGBA8 image with a 2x2 box filter, weighting colour by alpha so
/// transparent texels do not darken the edges of sprites.
fn downsample(src: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let (w, h) = ((width / 2).max(1), (height / 2).max(1));
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (mut r, mut g, mut b, mut a, mut weight) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for dy in 0..2 {
                for dx in 0..2 {
                    let sx = (x * 2 + dx).min(width - 1);
                    let sy = (y * 2 + dy).min(height - 1);
                    let i = ((sy * width + sx) * 4) as usize;
                    let alpha = src[i + 3] as f32 / 255.0;
                    r += to_linear(src[i]) * alpha;
                    g += to_linear(src[i + 1]) * alpha;
                    b += to_linear(src[i + 2]) * alpha;
                    a += alpha;
                    weight += 1.0;
                }
            }
            let o = ((y * w + x) * 4) as usize;
            if a > 0.0 {
                out[o] = to_srgb(r / a);
                out[o + 1] = to_srgb(g / a);
                out[o + 2] = to_srgb(b / a);
            }
            out[o + 3] = (a / weight * 255.0 + 0.5) as u8;
        }
    }
    (out, w, h)
}

/// Builds the atlas texture with its mip chain.
pub fn atlas_image(width: u32, height: u32, pixels: Vec<u8>) -> Image {
    let mut data = pixels.clone();
    let (mut level, mut w, mut h) = (pixels, width, height);
    let mut levels = 1;
    // DWARF_EYE_NO_MIPS=1 restores the bare nearest-sampled atlas, for comparison.
    let wanted = if std::env::var("DWARF_EYE_NO_MIPS").is_ok() { 0 } else { MIP_LEVELS };
    while levels <= wanted && w > 1 && h > 1 {
        let (next, nw, nh) = downsample(&level, w, h);
        data.extend_from_slice(&next);
        level = next;
        w = nw;
        h = nh;
        levels += 1;
    }

    let mut image = Image::new(
        Extent3d { width, height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        // Nearest when magnified keeps the pixel art; linear across and
        // between levels stops the sparkle at distance.
        mag_filter: ImageFilterMode::Nearest,
        min_filter: if levels > 1 { ImageFilterMode::Linear } else { ImageFilterMode::Nearest },
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    image
}
