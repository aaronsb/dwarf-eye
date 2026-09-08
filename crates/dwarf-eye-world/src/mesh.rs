//! Turns chunks into triangle soup. Engine-agnostic: the renderer only has to
//! copy the four output arrays into its own mesh type.

use crate::palette::{Rgb, Solid};
use crate::world::{BLOCK, Chunk, World};

/// Vertical exaggeration of a z-level relative to a tile's width.
///
/// DF z-levels read as taller than one tile is wide; 1.0 gives Minecraft cubes.
pub const Z_SCALE: f32 = 1.0;

/// Buffers for one chunk's geometry, in Bevy's Y-up convention.
#[derive(Default)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Appends a quad wound counter-clockwise when seen from `normal`.
    fn quad(&mut self, corners: [[f32; 3]; 4], normal: [f32; 3], color: [f32; 4]) {
        let base = self.positions.len() as u32;
        for c in corners {
            self.positions.push(c);
            self.normals.push(normal);
            self.colors.push(color);
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// Appends a box spanning `lo`..`hi`, skipping the faces marked in `skip`.
    fn cuboid(&mut self, lo: [f32; 3], hi: [f32; 3], color: [f32; 4], skip: Faces) {
        let [x0, y0, z0] = lo;
        let [x1, y1, z1] = hi;

        if !skip.top {
            self.quad(
                [[x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]],
                [0.0, 1.0, 0.0],
                shade(color, 1.0),
            );
        }
        if !skip.bottom {
            self.quad(
                [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
                [0.0, -1.0, 0.0],
                shade(color, 0.55),
            );
        }
        if !skip.north {
            self.quad(
                [[x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0]],
                [0.0, 0.0, -1.0],
                shade(color, 0.8),
            );
        }
        if !skip.south {
            self.quad(
                [[x1, y0, z1], [x1, y1, z1], [x0, y1, z1], [x0, y0, z1]],
                [0.0, 0.0, 1.0],
                shade(color, 0.8),
            );
        }
        if !skip.west {
            self.quad(
                [[x0, y0, z1], [x0, y1, z1], [x0, y1, z0], [x0, y0, z0]],
                [-1.0, 0.0, 0.0],
                shade(color, 0.68),
            );
        }
        if !skip.east {
            self.quad(
                [[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]],
                [1.0, 0.0, 0.0],
                shade(color, 0.68),
            );
        }
    }
}

/// Which faces of a cell are hidden by neighbours.
#[derive(Clone, Copy, Default)]
struct Faces {
    top: bool,
    bottom: bool,
    north: bool,
    south: bool,
    east: bool,
    west: bool,
}

/// Bakes directional lighting into vertex colour so a single unlit-ish material
/// still reads as a lit scene.
fn shade(color: [f32; 4], factor: f32) -> [f32; 4] {
    [color[0] * factor, color[1] * factor, color[2] * factor, color[3]]
}

/// A deterministic per-tile brightness wobble, so a hillside of one material
/// does not read as a single painted plane.
fn jitter(x: i32, y: i32, z: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9E3779B1)
        ^ (y as u32).wrapping_mul(0x85EBCA77)
        ^ (z as u32).wrapping_mul(0xC2B2AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545F491);
    h ^= h >> 13;
    0.94 + (h & 0xFFF) as f32 / 4095.0 * 0.12
}

fn to_linear(rgb: Rgb, alpha: f32) -> [f32; 4] {
    // sRGB -> linear, so colours survive Bevy's tonemapping unmuddied.
    let f = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    [f(rgb[0]), f(rgb[1]), f(rgb[2]), alpha]
}

/// Controls what a build pass includes.
#[derive(Clone, Copy)]
pub struct MeshOptions {
    /// Highest z-level to draw, so the camera can cut into the map.
    pub z_ceiling: i32,
    /// Draw tiles the player has not discovered.
    pub show_hidden: bool,
}

impl Default for MeshOptions {
    fn default() -> Self {
        Self { z_ceiling: i32::MAX, show_hidden: false }
    }
}

/// Builds the geometry for one chunk, culling faces against its neighbours.
pub fn build_chunk(world: &World, chunk: &Chunk, opts: MeshOptions) -> MeshData {
    let mut mesh = MeshData::default();
    if chunk.z > opts.z_ceiling {
        return mesh;
    }
    let (ox, oy, oz) = chunk.origin();

    for ly in 0..BLOCK {
        for lx in 0..BLOCK {
            let voxel = chunk.get(lx, ly);
            if voxel.solid.is_empty() || (voxel.hidden && !opts.show_hidden) {
                continue;
            }

            let (x, y, z) = (ox + lx, oy + ly, oz);
            // DF is x-east / y-south / z-up; Bevy is y-up, so y and z swap.
            let fx = x as f32;
            let fz = y as f32;
            let fy = z as f32 * Z_SCALE;

            let occluded = |dx: i32, dy: i32, dz: i32| {
                if z + dz > opts.z_ceiling {
                    return false;
                }
                match world.voxel(x + dx, y + dy, z + dz) {
                    Some(n) => n.solid.occludes() && (opts.show_hidden || !n.hidden),
                    // Unloaded neighbours stay open so chunk borders keep their walls.
                    None => false,
                }
            };

            let color = shade(to_linear(voxel.color, 1.0), jitter(x, y, z));
            let full = Faces {
                top: occluded(0, 0, 1),
                bottom: occluded(0, 0, -1),
                north: occluded(0, -1, 0),
                south: occluded(0, 1, 0),
                east: occluded(1, 0, 0),
                west: occluded(-1, 0, 0),
            };

            match voxel.solid {
                Solid::Empty => {}
                Solid::Cube | Solid::Fortification => {
                    mesh.cuboid([fx, fy, fz], [fx + 1.0, fy + Z_SCALE, fz + 1.0], color, full);
                }
                Solid::Floor => {
                    let h = 0.12 * Z_SCALE;
                    mesh.cuboid(
                        [fx, fy, fz],
                        [fx + 1.0, fy + h, fz + 1.0],
                        color,
                        Faces { bottom: full.bottom, ..Default::default() },
                    );
                }
                Solid::Ramp => {
                    // A half-height block reads as a slope well enough until the
                    // ramp's facing direction is wired up.
                    mesh.cuboid(
                        [fx, fy, fz],
                        [fx + 1.0, fy + 0.55 * Z_SCALE, fz + 1.0],
                        color,
                        Faces { bottom: full.bottom, ..Default::default() },
                    );
                }
                Solid::Stair => {
                    let h = 0.5 * Z_SCALE;
                    mesh.cuboid([fx, fy, fz], [fx + 1.0, fy + h, fz + 1.0], color, Faces::default());
                    mesh.cuboid(
                        [fx + 0.5, fy + h, fz],
                        [fx + 1.0, fy + Z_SCALE, fz + 1.0],
                        color,
                        Faces::default(),
                    );
                }
                Solid::Foliage => {
                    let inset = 0.18;
                    mesh.cuboid(
                        [fx + inset, fy, fz + inset],
                        [fx + 1.0 - inset, fy + 0.85 * Z_SCALE, fz + 1.0 - inset],
                        color,
                        Faces::default(),
                    );
                }
            }

            // Liquids sit inside the cell at a height set by their fill level.
            let liquid = if voxel.magma > 0 {
                Some((voxel.magma, to_linear([255, 90, 20], 0.9)))
            } else if voxel.water > 0 {
                Some((voxel.water, to_linear([50, 105, 190], 0.65)))
            } else {
                None
            };
            if let Some((level, tint)) = liquid {
                let h = (level as f32 / 7.0).clamp(0.15, 1.0) * Z_SCALE;
                mesh.cuboid([fx, fy, fz], [fx + 1.0, fy + h, fz + 1.0], tint, Faces::default());
            }
        }
    }

    mesh
}
