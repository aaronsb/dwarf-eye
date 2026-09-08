//! The procedural surfaces a tree is drawn with, as raw RGBA8.
//!
//! Everything here is authored in texels per world tile, matching Dwarf
//! Fortress's own art at 32, so a leaf face, a trunk and the ground all show
//! the same pixel size however many voxels a tile is cut into. Mesh UVs are
//! world-space (see [`crate::mesh`]), so these want a Repeat sampler and
//! nearest-neighbour magnification.
//!
//! No image crate and no engine types: the caller wraps the bytes.

use crate::rng::Rng;

/// One generated surface: `width * height` RGBA8 texels.
pub struct Texels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Texels {
    /// Fraction of texels that are not fully transparent.
    pub fn coverage(&self) -> f32 {
        let solid = self.rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
        solid as f32 / (self.width * self.height).max(1) as f32
    }
}

/// A leaf cutout: clumps of leaf with air between them, tiling at `texels` to a
/// world tile. `openness` is the fraction of air, which is the fine porosity
/// that keeps a conifer's spray see-through next to a solid oak crown.
///
/// The needle variant is stringy rather than clumped.
pub fn leaf_cutout(needle: bool, openness: f32, texels: u32) -> Texels {
    let tex = texels.max(4);
    let mut rng = Rng::new(if needle { 0x_1EED_1E5F } else { 0x_B20A_D1E5 });
    let mut cover = vec![0u8; (tex * tex) as usize];

    if needle {
        // Short strokes, an eighth of a tile long, wrapped so the tile repeats.
        let strokes = ((tex * tex) as f32 * 0.107 * (1.0 - openness)).round().max(8.0) as u32;
        for _ in 0..strokes {
            let sx = (rng.unit() * tex as f32) as i32;
            let sy = (rng.unit() * tex as f32) as i32;
            let dir = if rng.chance(0.5) { 1 } else { -1 };
            let len = (tex as i32 / 8).max(2) + (rng.unit() * 2.0) as i32;
            for t in 0..len {
                let y = (sy + t).rem_euclid(tex as i32) as u32;
                for wide in 0..2 {
                    let x = (sx + t * dir + wide).rem_euclid(tex as i32) as u32;
                    cover[(y * tex + x) as usize] = 255;
                }
            }
        }
    } else {
        // Value noise on a coarse grid: clumps about an eighth of a tile across.
        // The threshold is raised until the mask is as open as asked for.
        let grid = (tex / 4).max(2) as usize;
        let mut field = vec![0.0f32; grid * grid];
        for v in field.iter_mut() {
            *v = rng.unit();
        }
        let mut value = vec![0.0f32; (tex * tex) as usize];
        for y in 0..tex {
            for x in 0..tex {
                let fx = x as f32 * grid as f32 / tex as f32;
                let fy = y as f32 * grid as f32 / tex as f32;
                // Grain roughens the edge so no clump is a smooth oval.
                let grain = ((x * 7 + y * 13) % 5) as f32 * 0.03;
                value[(y * tex + x) as usize] = sample(&field, grid, fx, fy) + grain;
            }
        }
        let mut sorted = value.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let cut = sorted[((sorted.len() as f32 * openness) as usize).min(sorted.len() - 1)];
        for (i, v) in value.iter().enumerate() {
            if *v > cut {
                cover[i] = 255;
            }
        }
        // Pinholes, for the light that comes through a real crown.
        for _ in 0..(tex / 4).max(2) {
            let cx = (rng.unit() * tex as f32) as i32;
            let cy = (rng.unit() * tex as f32) as i32;
            let r = (tex as i32 / 32).max(1) + (rng.unit() * 2.0) as i32;
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx * dx + dy * dy > r * r {
                        continue;
                    }
                    let x = (cx + dx).rem_euclid(tex as i32) as u32;
                    let y = (cy + dy).rem_euclid(tex as i32) as u32;
                    cover[(y * tex + x) as usize] = 0;
                }
            }
        }
    }

    Texels { width: tex, height: tex, rgba: shade_mask(&cover, tex, tex) }
}

/// Bark fissures at the same density: stripes a sixteenth of a tile wide
/// running along the trunk axis, opaque and tinted by the vertex colour.
pub fn bark(texels: u32) -> Texels {
    let tex = texels.max(4);
    let mut rng = Rng::new(0xBA2C_0000);
    let mut columns = vec![1.0f32; tex as usize];
    let mut x = 0usize;
    while x < tex as usize {
        let width = (tex as usize / 16).max(1) + (rng.unit() * 2.0) as usize;
        let shade = rng.range(0.6, 1.0);
        for c in columns.iter_mut().skip(x).take(width) {
            *c = shade;
        }
        x += width;
    }
    let mut rgba = Vec::with_capacity((tex * tex * 4) as usize);
    for y in 0..tex {
        for x in 0..tex {
            // Fissures wander a little down the trunk rather than ruling straight.
            let shift = ((y as f32 * 11.2 / tex as f32).sin() * (tex as f32 / 16.0)).round() as i32;
            let column = columns[(x as i32 + shift).rem_euclid(tex as i32) as usize];
            let grain = ((x * 3 + y * 11) % 4) as f32 * 0.04;
            let v = ((column + grain) * 255.0).clamp(0.0, 255.0) as u8;
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    Texels { width: tex, height: tex, rgba }
}

/// Dwarf Fortress's own art density, and this crate's default: 32 texels to a
/// world tile.
pub const DEFAULT_TEXELS: u32 = 32;

/// How many cells [`streamer_strip`] holds: three leaflet variants and a tip.
pub const STREAMER_CELLS: u32 = 4;

/// Where the last cell sits, used for the bottom segment of a strand.
pub const STREAMER_TIP: u32 = STREAMER_CELLS - 1;

/// The hanging-leaflet strip a weeping tree's streamers are drawn with.
///
/// Cells sit side by side in one row, each a quarter tile square at the default
/// four voxels to a tile, so one streamer segment covers exactly one cell at the
/// same texel size as the leaf cutout. Use [`streamer_uv`] to address them.
pub fn streamer_strip(texels: u32) -> Texels {
    let tex = texels.max(8);
    let cell = (tex / STREAMER_CELLS).max(2);
    let (width, height) = (cell * STREAMER_CELLS, cell);
    let mut cover = vec![0u8; (width * height) as usize];
    let mut rng = Rng::new(0x5732_EA31);

    for c in 0..STREAMER_CELLS {
        let x0 = c * cell;
        // A stem down the middle of the cell, so segments chain unbroken.
        let stem = cell / 2;
        let tip = c == STREAMER_TIP;
        // The tip cell stops short, which is what ends a strand cleanly.
        let bottom = if tip { (cell * 5 / 8).max(1) } else { cell };
        for y in 0..bottom {
            cover[(y * width + x0 + stem) as usize] = 255;
        }
        // Leaflets hanging off the stem, alternating sides.
        let leaflets = if tip { cell / 3 } else { cell / 2 };
        for n in 0..leaflets.max(1) {
            let y = (n * bottom / leaflets.max(1) + (rng.unit() * 1.9) as u32).min(bottom - 1);
            let side: i32 = if rng.chance(0.5) { 1 } else { -1 };
            let reach = 1 + (rng.unit() * (cell as f32 * 0.35)) as i32;
            for t in 1..=reach {
                let x = stem as i32 + side * t;
                if x < 0 || x >= cell as i32 {
                    break;
                }
                // Leaflets droop as they reach out.
                let drop = (t / 2) as u32;
                let yy = (y + drop).min(bottom - 1);
                cover[(yy * width + x0 + x as u32) as usize] = 255;
            }
        }
    }

    Texels { width, height, rgba: shade_mask(&cover, width, height) }
}

/// UV rectangle of one [`streamer_strip`] cell, inset half a texel so nearest
/// sampling cannot bleed in from the cell beside it.
pub fn streamer_uv(cell: u32, texels: u32) -> [[f32; 2]; 2] {
    let tex = texels.max(8);
    let size = (tex / STREAMER_CELLS).max(2);
    let width = (size * STREAMER_CELLS) as f32;
    let cell = cell.min(STREAMER_TIP) as f32;
    let inset = 0.5 / width;
    let u0 = cell * size as f32 / width + inset;
    let u1 = (cell + 1.0) * size as f32 / width - inset;
    [[u0, 0.0], [u1, 1.0]]
}

/// Turns a coverage mask into near-white RGBA, so the vertex colour carries the
/// hue and the texture only carries shape and a little grain.
fn shade_mask(cover: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let alpha = cover[(y * width + x) as usize];
            let shade = 200 + ((x * 5 + y * 3) % 7) as u8 * 8;
            rgba.extend_from_slice(&[shade, shade, shade, alpha]);
        }
    }
    rgba
}

fn sample(field: &[f32], grid: usize, x: f32, y: f32) -> f32 {
    let at = |ix: i32, iy: i32| {
        field[(iy.rem_euclid(grid as i32) as usize) * grid + ix.rem_euclid(grid as i32) as usize]
    };
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
    let top = at(x0, y0) + (at(x0 + 1, y0) - at(x0, y0)) * sx;
    let bottom = at(x0, y0 + 1) + (at(x0 + 1, y0 + 1) - at(x0, y0 + 1)) * sx;
    top + (bottom - top) * sy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cutout_is_as_open_as_asked() {
        for openness in [0.2f32, 0.4, 0.6] {
            let cover = leaf_cutout(false, openness, 32).coverage();
            // Pinholes take a little more out on top of the threshold.
            assert!(
                (1.0 - cover - openness).abs() < 0.14,
                "openness {openness} gave {cover} cover"
            );
        }
    }

    #[test]
    fn textures_are_the_size_they_claim() {
        for texels in [16u32, 32, 64] {
            for t in [leaf_cutout(true, 0.5, texels), bark(texels), streamer_strip(texels)] {
                assert_eq!(t.rgba.len(), (t.width * t.height * 4) as usize);
            }
        }
    }

    #[test]
    fn streamer_cells_do_not_overlap() {
        let uv = |c| streamer_uv(c, 32);
        for c in 1..STREAMER_CELLS {
            assert!(uv(c - 1)[1][0] < uv(c)[0][0], "cell {c} overlaps its neighbour");
        }
    }
}
