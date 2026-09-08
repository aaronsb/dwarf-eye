//! Turns chunks into triangle soup. Engine-agnostic: the renderer only has to
//! copy the four output arrays into its own mesh type.

use crate::library::TileLibrary;
use crate::model::{Caps, RenderMode};
use crate::palette::{Rgb, Solid};
use crate::world::{BLOCK, Chunk, World};

/// Vertical exaggeration of a z-level relative to a tile's width.
///
/// DF z-levels read as taller than one tile is wide; 1.0 gives Minecraft cubes.
pub const Z_SCALE: f32 = 1.0;
/// Thickness of a floor slab, in cells.
pub const FLOOR_HEIGHT: f32 = 0.12 * Z_SCALE;

/// Buffers for one chunk's geometry, in Bevy's Y-up convention.
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl Default for MeshData {
    fn default() -> Self {
        Self {
            positions: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
        }
    }
}

impl MeshData {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Appends a quad wound counter-clockwise when seen from `normal`.
    pub fn push_quad(&mut self, corners: [[f32; 3]; 4], normal: [f32; 3], color: [f32; 4]) {
        self.push_textured_quad(corners, normal, color, [dwarf_eye_art::atlas::WHITE_UV; 4]);
    }

    /// Appends a quad that samples the atlas, for geometry whose look comes from
    /// a sprite rather than from vertex colour.
    pub fn push_textured_quad(
        &mut self,
        corners: [[f32; 3]; 4],
        normal: [f32; 3],
        color: [f32; 4],
        uvs: [[f32; 2]; 4],
    ) {
        let base = self.positions.len() as u32;
        for (c, uv) in corners.into_iter().zip(uvs) {
            self.positions.push(c);
            self.normals.push(normal);
            self.colors.push(color);
            self.uvs.push(uv);
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// Appends another mesh translated by `offset`, for stamping a cached tile
    /// model into a chunk.
    pub fn stamp(&mut self, model: &MeshData, offset: [f32; 3], tint: [f32; 3]) {
        let base = self.positions.len() as u32;
        self.positions.extend(model.positions.iter().map(|p| {
            [p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]]
        }));
        self.normals.extend_from_slice(&model.normals);
        self.uvs.extend_from_slice(&model.uvs);
        self.colors.extend(
            model
                .colors
                .iter()
                .map(|c| [c[0] * tint[0], c[1] * tint[1], c[2] * tint[2], c[3]]),
        );
        self.indices.extend(model.indices.iter().map(|i| i + base));
    }

    /// Appends a box spanning `lo`..`hi`, skipping the faces marked in `skip`.
    fn cuboid(&mut self, lo: [f32; 3], hi: [f32; 3], color: [f32; 4], skip: Faces) {
        let [x0, y0, z0] = lo;
        let [x1, y1, z1] = hi;

        if !skip.top {
            self.push_quad(
                [[x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]],
                [0.0, 1.0, 0.0],
                shade(color, 1.0),
            );
        }
        if !skip.bottom {
            self.push_quad(
                [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
                [0.0, -1.0, 0.0],
                shade(color, 0.55),
            );
        }
        if !skip.north {
            self.push_quad(
                [[x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0]],
                [0.0, 0.0, -1.0],
                shade(color, 0.8),
            );
        }
        if !skip.south {
            self.push_quad(
                [[x1, y0, z1], [x1, y1, z1], [x0, y1, z1], [x0, y0, z1]],
                [0.0, 0.0, 1.0],
                shade(color, 0.8),
            );
        }
        if !skip.west {
            self.push_quad(
                [[x0, y0, z1], [x0, y1, z1], [x0, y1, z0], [x0, y0, z0]],
                [-1.0, 0.0, 0.0],
                shade(color, 0.68),
            );
        }
        if !skip.east {
            self.push_quad(
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

/// Pulls a colour partway toward its own brightness.
///
/// DF's material colours are far more saturated than its rendering of them:
/// rock salt is `[255, 192, 203]`, and a floor of it in-game reads as grey
/// stone, not pink. Damping keeps one stone distinguishable from another
/// without painting the ground in raw material colour.
fn damp(rgb: [f32; 3], keep: f32) -> [f32; 3] {
    let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    [
        luma + (rgb[0] - luma) * keep,
        luma + (rgb[1] - luma) * keep,
        luma + (rgb[2] - luma) * keep,
    ]
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
///
/// When `library` is given, tiles that have a Dwarf Fortress sprite are stamped
/// from that sprite's extruded mask instead of drawn as plain blocks.
/// Where a chunk's triangles went, for aiming reductions.
#[derive(Default, Debug, Clone, Copy)]
pub struct Budget {
    pub models: usize,
    pub ramps: usize,
    pub ground_under: usize,
    pub cubes: usize,
    pub floors: usize,
    pub foliage: usize,
    pub liquids: usize,
    pub other: usize,
    /// Tree crowns, meshed as merged volumes.
    pub canopy: usize,
}

pub fn build_chunk(
    world: &World,
    chunk: &Chunk,
    opts: MeshOptions,
    library: Option<&mut TileLibrary>,
) -> MeshData {
    build_chunk_budgeted(world, chunk, opts, library, &mut Budget::default())
}

/// `build_chunk`, tallying triangles by kind into `budget`.
pub fn build_chunk_budgeted(
    world: &World,
    chunk: &Chunk,
    opts: MeshOptions,
    mut library: Option<&mut TileLibrary>,
    budget: &mut Budget,
) -> MeshData {
    let mut mesh = MeshData::default();
    let tally = |mesh: &MeshData, before: usize, slot: &mut usize| {
        *slot += (mesh.indices.len() - before) / 3;
    };
    if chunk.z > opts.z_ceiling {
        return mesh;
    }
    let (ox, oy, oz) = chunk.origin();

    // Tree crowns are one merged surface per chunk rather than one model per
    // tile, so they are built before the tile loop and skipped inside it.
    if let Some(lib) = library.as_deref_mut() {
        let before = mesh.indices.len();
        crate::canopy::build(world, chunk, opts, lib, &mut mesh);
        tally(&mesh, before, &mut budget.canopy);
    }

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
                    Some(n) => {
                        let visible = opts.show_hidden || !n.hidden;
                        // A floor above hides the top of whatever it rests on.
                        let lid = dz == 1 && n.solid == Solid::Floor;
                        visible && (n.solid.occludes() || lid)
                    }
                    // Nothing is ever seen from under the lowest loaded level.
                    // Sideways, unloaded neighbours stay open so chunk borders
                    // keep their walls.
                    None => dz == -1,
                }
            };

            // Sides of a floor slab that face a drop, where alone they show.
            let rim_faces = || {
                let drop = |dx: i32, dy: i32| {
                    let beside = world.voxel(x + dx, y + dy, z);
                    let below = world.voxel(x + dx, y + dy, z - 1);
                    beside.is_some_and(|n| n.solid.is_empty())
                        && !below.is_some_and(|n| n.solid.occludes())
                };
                Faces {
                    top: false,
                    bottom: true,
                    north: !drop(0, -1),
                    south: !drop(0, 1),
                    west: !drop(-1, 0),
                    east: !drop(1, 0),
                }
            };

            let color = shade(to_linear(voxel.color, 1.0), jitter(x, y, z));

            // A sprite-derived model, when this tiletype has one.
            if let Some(lib) = library.as_deref_mut() {
                if lib.canopy_part(voxel.tile_id).is_some() {
                    continue;
                }
                if lib.handles(voxel.tile_id) {
                    // A trunk with more trunk above it has no visible top. This
                    // is the whole reason a forest stays affordable.
                    let extruded = lib.mode(voxel.tile_id) == Some(RenderMode::Extrude);
                    let continues = |dz: i32| {
                        extruded
                            && world
                                .voxel(x, y, z + dz)
                                .is_some_and(|n| lib.mode(n.tile_id) == Some(RenderMode::Extrude))
                    };
                    let caps = Caps { top: !continues(1), bottom: !continues(-1) };

                    // Terrain ramps carry no direction, so the slope comes from
                    // whichever neighbour is a wall.
                    if lib.mode(voxel.tile_id) == Some(RenderMode::Ramp) {
                        let mut high = 0u8;
                        for (bit, dx, dy) in crate::ramp::NEIGHBOURS {
                            if world.voxel(x + dx, y + dy, z).is_some_and(|n| {
                                matches!(n.solid, Solid::Cube | Solid::Fortification)
                            }) {
                                high |= bit;
                            }
                        }
                        if let Some(model) = lib.ramp(voxel.tile_id, high) {
                            let wobble = jitter(x, y, z);
                            let tint = if model.tint {
                                let base = damp([color[0], color[1], color[2]], 0.35);
                                [base[0] * wobble, base[1] * wobble, base[2] * wobble]
                            } else {
                                [wobble, wobble, wobble]
                            };
                            let before = mesh.indices.len();
                            mesh.stamp(&model.mesh, [fx, fy, fz], tint);
                            tally(&mesh, before, &mut budget.ramps);
                            continue;
                        }
                    }

                    // A shrub or boulder fills its tile outright, with no floor
                    // tile of its own, so give it ground to stand on.
                    if let Some(ground) = lib.ground_beneath(voxel.tile_id) {
                        let wobble = jitter(x, y, z);
                        let tint = if ground.tint {
                            let base = damp([color[0], color[1], color[2]], 0.35);
                            [base[0] * wobble, base[1] * wobble, base[2] * wobble]
                        } else {
                            [wobble, wobble, wobble]
                        };
                        let before = mesh.indices.len();
                        mesh.stamp(&ground.mesh, [fx, fy, fz], tint);
                        let rims = rim_faces();
                        mesh.cuboid([fx, fy, fz], [fx + 1.0, fy + FLOOR_HEIGHT, fz + 1.0], color, Faces { top: true, ..rims });
                        tally(&mesh, before, &mut budget.ground_under);
                    }

                    if let Some(model) = lib.model(voxel.tile_id, voxel.mat_index, caps) {
                        let wobble = jitter(x, y, z);
                        // A near-grey sprite is a pattern; the tile's material
                        // supplies the colour, so granite still reads unlike
                        // marble under the same stone texture.
                        let tint = if model.tint {
                            let base = damp([color[0], color[1], color[2]], 0.35);
                            [base[0] * wobble, base[1] * wobble, base[2] * wobble]
                        } else {
                            [wobble, wobble, wobble]
                        };
                        let before = mesh.indices.len();
                        mesh.stamp(&model.mesh, [fx, fy, fz], tint);
                        if lib.mode(voxel.tile_id) == Some(RenderMode::FlatTile) {
                            let rims = rim_faces();
                            mesh.cuboid([fx, fy, fz], [fx + 1.0, fy + FLOOR_HEIGHT, fz + 1.0], color, Faces { top: true, ..rims });
                            tally(&mesh, before, &mut budget.floors);
                        } else {
                            tally(&mesh, before, &mut budget.models);
                        }
                        continue;
                    }
                }
            }
            let full = Faces {
                top: occluded(0, 0, 1),
                bottom: occluded(0, 0, -1),
                north: occluded(0, -1, 0),
                south: occluded(0, 1, 0),
                east: occluded(1, 0, 0),
                west: occluded(-1, 0, 0),
            };

            let before = mesh.indices.len();
            match voxel.solid {
                Solid::Empty => {}
                Solid::Cube | Solid::Fortification => {
                    mesh.cuboid([fx, fy, fz], [fx + 1.0, fy + Z_SCALE, fz + 1.0], color, full);
                }
                Solid::Floor => {
                    let rims = rim_faces();
                    mesh.cuboid(
                        [fx, fy, fz],
                        [fx + 1.0, fy + FLOOR_HEIGHT, fz + 1.0],
                        color,
                        Faces { top: false, ..rims },
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

            match voxel.solid {
                Solid::Cube | Solid::Fortification => tally(&mesh, before, &mut budget.cubes),
                Solid::Floor => tally(&mesh, before, &mut budget.floors),
                Solid::Foliage => tally(&mesh, before, &mut budget.foliage),
                _ => tally(&mesh, before, &mut budget.other),
            }
            let before = mesh.indices.len();

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
            tally(&mesh, before, &mut budget.liquids);
        }
    }

    mesh
}
