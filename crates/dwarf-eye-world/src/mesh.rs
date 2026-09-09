//! Turns chunks into triangle soup. Engine-agnostic: the renderer only has to
//! copy the four output arrays into its own mesh type.

use crate::factory::{Extent, Style};
use crate::heightfield::{Ground, Surface, grounded};
use crate::library::TileLibrary;
use crate::model::{Caps, RenderMode};
use crate::palette::{Rgb, Solid};
use crate::wall;
use crate::world::{BLOCK, Chunk, World};
use dwarf_eye_art::atlas::Rect;

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
    /// Water surfaces, which want a translucent material of their own. Carried
    /// here so a chunk still travels as one value; the renderer splits it off
    /// before uploading (`main.rs:upload_chunks`).
    pub water: Option<Box<MeshData>>,
}

impl Default for MeshData {
    fn default() -> Self {
        Self {
            positions: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
            water: None,
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

    /// Lifts the water surfaces out, leaving the terrain behind.
    pub fn take_water(&mut self) -> MeshData {
        self.water.take().map(|w| *w).unwrap_or_default()
    }

    /// A quad whose corners each carry their own colour, for a surface that
    /// shades across a tile rather than in one step.
    pub fn push_quad_colors(
        &mut self,
        corners: [[f32; 3]; 4],
        normal: [f32; 3],
        colors: [[f32; 4]; 4],
    ) {
        let base = self.positions.len() as u32;
        for (c, color) in corners.into_iter().zip(colors) {
            self.positions.push(c);
            self.normals.push(normal);
            self.colors.push(color);
            self.uvs.push(dwarf_eye_art::atlas::WHITE_UV);
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
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

    /// Appends a box that samples the atlas: `top` on the lid and the bottom,
    /// `side` on the four walls.
    ///
    /// The lid is laid out the way a flat tile is, north at `v0`, so a wall's
    /// top face lines up with the floor beside it. A side runs `v0` at the top
    /// of the box to `v1` at its foot, one cell of texture per z-level, so a
    /// cliff stacks without stretching.
    fn textured_cuboid(
        &mut self,
        lo: [f32; 3],
        hi: [f32; 3],
        color: [f32; 4],
        skip: Faces,
        top: Rect,
        side: Rect,
    ) {
        let [x0, y0, z0] = lo;
        let [x1, y1, z1] = hi;
        // Every side is wound bottom, top, top, bottom, so one set of UVs
        // serves all four.
        let face = [
            [side.u0, side.v1],
            [side.u0, side.v0],
            [side.u1, side.v0],
            [side.u1, side.v1],
        ];

        if !skip.top {
            self.push_textured_quad(
                [[x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]],
                [0.0, 1.0, 0.0],
                shade(color, 1.0),
                [
                    [top.u0, top.v0],
                    [top.u0, top.v1],
                    [top.u1, top.v1],
                    [top.u1, top.v0],
                ],
            );
        }
        if !skip.bottom {
            self.push_textured_quad(
                [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
                [0.0, -1.0, 0.0],
                shade(color, 0.55),
                [
                    [top.u0, top.v0],
                    [top.u1, top.v0],
                    [top.u1, top.v1],
                    [top.u0, top.v1],
                ],
            );
        }
        if !skip.north {
            self.push_textured_quad(
                [[x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0]],
                [0.0, 0.0, -1.0],
                shade(color, 0.8),
                face,
            );
        }
        if !skip.south {
            self.push_textured_quad(
                [[x1, y0, z1], [x1, y1, z1], [x0, y1, z1], [x0, y0, z1]],
                [0.0, 0.0, 1.0],
                shade(color, 0.8),
                face,
            );
        }
        if !skip.west {
            self.push_textured_quad(
                [[x0, y0, z1], [x0, y1, z1], [x0, y1, z0], [x0, y0, z0]],
                [-1.0, 0.0, 0.0],
                shade(color, 0.68),
                face,
            );
        }
        if !skip.east {
            self.push_textured_quad(
                [[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]],
                [1.0, 0.0, 0.0],
                shade(color, 0.68),
                face,
            );
        }
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
pub(crate) fn shade(color: [f32; 4], factor: f32) -> [f32; 4] {
    [color[0] * factor, color[1] * factor, color[2] * factor, color[3]]
}

/// Pulls a colour partway toward its own brightness.
///
/// DF's material colours are far more saturated than its rendering of them:
/// rock salt is `[255, 192, 203]`, and a floor of it in-game reads as grey
/// stone, not pink. Damping keeps one stone distinguishable from another
/// without painting the ground in raw material colour.
pub(crate) fn damp(rgb: [f32; 3], keep: f32) -> [f32; 3] {
    let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    [
        luma + (rgb[0] - luma) * keep,
        luma + (rgb[1] - luma) * keep,
        luma + (rgb[2] - luma) * keep,
    ]
}

/// The bottom sliver of a wall's side strip, `height` of a z-level tall.
///
/// A side strip runs one cell of texture per z-level, `v0` at the top of the
/// box and `v1` at its foot. A floor slab sits at the bottom of its own level,
/// so it takes the last sliver of that run: the course of masonry the wall
/// below it would have shown had it carried on, at the wall's own texel
/// density rather than a whole sheet squeezed into a thin band.
fn strip_foot(side: Rect, height: f32) -> Rect {
    Rect { v0: side.v1 - (side.v1 - side.v0) * (height / Z_SCALE).clamp(0.0, 1.0), ..side }
}

/// A deterministic per-tile brightness wobble, so a hillside of one material
/// does not read as a single painted plane.
pub(crate) fn jitter(x: i32, y: i32, z: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9E3779B1)
        ^ (y as u32).wrapping_mul(0x85EBCA77)
        ^ (z as u32).wrapping_mul(0xC2B2AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545F491);
    h ^= h >> 13;
    0.94 + (h & 0xFFF) as f32 / 4095.0 * 0.12
}

pub(crate) fn to_linear(rgb: Rgb, alpha: f32) -> [f32; 4] {
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
    /// The smoothed ground sheet and its rim (`heightfield.rs`).
    pub surface: usize,
    pub foliage: usize,
    pub liquids: usize,
    pub buildings: usize,
    pub items: usize,
    pub other: usize,
}

/// Buildings and item piles: a second pass over the chunk, drawn on top of
/// whatever the tile itself drew.
///
/// A building is a thing standing in a tile, not a tile: DF reports it apart
/// from the tiletype, and the floor under a door is still a floor. So the box
/// goes on afterwards rather than in place of the tile, and a hatch over a
/// shaft still lands where the tile itself is empty.
///
/// The factory decides the class and the treatment; this only draws what it
/// says, inside the footprint DF gave it and never outside.
fn build_furnishings(
    chunk: &Chunk,
    opts: MeshOptions,
    library: Option<&TileLibrary>,
    surface: Option<&Surface>,
    mesh: &mut MeshData,
    budget: &mut Budget,
) {
    if chunk.z > opts.z_ceiling {
        return;
    }
    let (ox, oy, oz) = chunk.origin();

    for ly in 0..BLOCK {
        for lx in 0..BLOCK {
            let voxel = chunk.get(lx, ly);
            if voxel.hidden && !opts.show_hidden {
                continue;
            }
            let (fx, fy, fz) = ((ox + lx) as f32, oz as f32 * Z_SCALE, (oy + ly) as f32);

            if voxel.built_over() {
                let class = crate::factory::classify_building(voxel.building as i32);
                let kind = crate::factory::building_kind(voxel.building as i32);
                if let (crate::factory::Treatment::Massing(height), true) =
                    (crate::factory::resolve(class, Style::current()), kind.drawn())
                {
                    let built = chunk.built(lx, ly);
                    let lid = library.and_then(|lib| {
                        lib.building_cell(
                            voxel.building as i32,
                            voxel.building_sub as i32,
                            built.sheet,
                            voxel.building_offset(),
                        )
                    });
                    let before = mesh.indices.len();
                    let colour = to_linear(built.color, 1.0);
                    // Grounding: the lid rides the ground at the tile's centre
                    // and the box stretches down to the tile's lowest corner,
                    // so a workshop on a slope shows a skirt rather than a
                    // gap. Never raised (`heightfield.rs:grounded`).
                    let stepped = fy + FLOOR_HEIGHT;
                    let standing = grounded(stepped, surface.and_then(|s| s.height(lx, ly)));
                    let fy = grounded(stepped, surface.and_then(|s| s.low(lx, ly)));
                    let top = standing + height * Z_SCALE;
                    // The sides are the material — DF draws no side of a chair
                    // and inventing one would be a lie — and the lid is DF's
                    // own picture of the thing, seen from above, which is the
                    // only view it ever drew.
                    mesh.cuboid(
                        [fx, fy, fz],
                        [fx + 1.0, top, fz + 1.0],
                        colour,
                        Faces { bottom: true, top: lid.is_some(), ..Default::default() },
                    );
                    if let Some((rect, pattern)) = lid {
                        // A near-grey sheet is a pattern the material colours,
                        // as the ground is; one with colour of its own keeps it.
                        let tint = if pattern {
                            let d = damp([colour[0], colour[1], colour[2]], 0.35);
                            [d[0], d[1], d[2], 1.0]
                        } else {
                            [1.0, 1.0, 1.0, 1.0]
                        };
                        mesh.push_textured_quad(
                            [
                                [fx, top, fz],
                                [fx, top, fz + 1.0],
                                [fx + 1.0, top, fz + 1.0],
                                [fx + 1.0, top, fz],
                            ],
                            [0.0, 1.0, 0.0],
                            tint,
                            [
                                [rect.u0, rect.v0],
                                [rect.u0, rect.v1],
                                [rect.u1, rect.v1],
                                [rect.u1, rect.v0],
                            ],
                        );
                    }
                    budget.buildings += (mesh.indices.len() - before) / 3;
                }
            }

            // A stockpile square, and nothing smaller: one dropped sock costs
            // a box for nothing.
            if let Some(pile) = chunk.pile(lx, ly) {
                let before = mesh.indices.len();
                let stepped = fy + FLOOR_HEIGHT;
                let lid = grounded(stepped, surface.and_then(|s| s.height(lx, ly)));
                let base = grounded(stepped, surface.and_then(|s| s.low(lx, ly)));
                mesh.cuboid(
                    [fx + 0.1, base, fz + 0.1],
                    [fx + 0.9, lid + crate::factory::PILE_HEIGHT * Z_SCALE, fz + 0.9],
                    to_linear(pile.color, 1.0),
                    Faces { bottom: true, ..Default::default() },
                );
                budget.items += (mesh.indices.len() - before) / 3;
            }
        }
    }
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
    library: Option<&mut TileLibrary>,
    budget: &mut Budget,
) -> MeshData {
    build_chunk_in(world, chunk, opts, library, budget, Ground::current())
}

/// The build pass, with the surface model named rather than read from the
/// environment, so both can be meshed side by side in a test.
pub fn build_chunk_in(
    world: &World,
    chunk: &Chunk,
    opts: MeshOptions,
    mut library: Option<&mut TileLibrary>,
    budget: &mut Budget,
    ground: Ground,
) -> MeshData {
    let mut mesh = MeshData::default();
    let tally = |mesh: &MeshData, before: usize, slot: &mut usize| {
        *slot += (mesh.indices.len() - before) / 3;
    };
    if chunk.z > opts.z_ceiling {
        return mesh;
    }
    let style = Style::current();
    // The ground of every natural tile in this chunk as one smoothed sheet.
    // Read once: the tile loop asks per tile whether it is covered, and the
    // sheet itself is emitted after the loop.
    let surface = ground.smooth().then(|| Surface::build(world, chunk, opts));
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

            // The heightfield draws this tile's ground: no slab, no wedge and
            // no top face here (`heightfield.rs:Surface::emit`).
            let covered = surface.as_ref().is_some_and(|s| s.covers(lx, ly));

            // A sprite-derived model, when this tiletype has one.
            if let Some(lib) = library.as_deref_mut() {
                // The factory decides who draws a tile. What it grows — a
                // tree's wood and crown, a plant standing in its own tile —
                // comes back as canopy geometry on its own materials, so
                // nothing of it is drawn from a sprite here.
                if let Some(plan) = lib.plan(voxel.tile_id) {
                    if plan.grown(style) {
                        // A standing plant occupies its tile outright: DF
                        // reports no floor under it, so it keeps the ground the
                        // billboard used to stand on.
                        if plan.extent == Extent::Tile && !covered {
                            if let Some(ground) = lib.ground_beneath(voxel.tile_id) {
                                let before = mesh.indices.len();
                                let wobble = jitter(x, y, z);
                                let tint = if ground.tint {
                                    let base = damp([color[0], color[1], color[2]], 0.35);
                                    [base[0] * wobble, base[1] * wobble, base[2] * wobble]
                                } else {
                                    [wobble, wobble, wobble]
                                };
                                mesh.stamp(&ground.mesh, [fx, fy, fz], tint);
                                let rims = rim_faces();
                                mesh.cuboid(
                                    [fx, fy, fz],
                                    [fx + 1.0, fy + FLOOR_HEIGHT, fz + 1.0],
                                    color,
                                    Faces { top: true, ..rims },
                                );
                                tally(&mesh, before, &mut budget.ground_under);
                            }
                        }
                        continue;
                    }
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
                        // A natural slope is the heightfield's; a constructed
                        // one keeps DF's wedge.
                        if covered {
                            continue;
                        }
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
                    if let Some(ground) = lib.ground_beneath(voxel.tile_id).filter(|_| !covered) {
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

                    if covered && lib.mode(voxel.tile_id) == Some(RenderMode::FlatTile) {
                        continue;
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
                            let (lo, hi) =
                                ([fx, fy, fz], [fx + 1.0, fy + FLOOR_HEIGHT, fz + 1.0]);
                            match lib.floor_rim(voxel.tile_id) {
                                // A built floor is the lid of a built wall, so
                                // where its slab faces a drop it shows the same
                                // masonry rather than a blank skirt. The model
                                // already drew the lid and the underside is
                                // never seen, so only the four sides are cut,
                                // and both rects are the strip's foot.
                                Some((side, _)) => {
                                    let foot = strip_foot(side, FLOOR_HEIGHT);
                                    mesh.textured_cuboid(
                                        lo,
                                        hi,
                                        [tint[0], tint[1], tint[2], 1.0],
                                        Faces { top: true, ..rims },
                                        foot,
                                        foot,
                                    )
                                }
                                None => {
                                    mesh.cuboid(lo, hi, color, Faces { top: true, ..rims })
                                }
                            }
                            tally(&mesh, before, &mut budget.floors);
                        } else {
                            tally(&mesh, before, &mut budget.models);
                        }
                        continue;
                    }
                }
            }
            // A wall wears DF's wall sheet: the variant for the walls it joins
            // on its top face, and a strip cut from the same sprite on its
            // sides.
            //
            // The set comes from the neighbours, not from DF's own
            // `direction()`. Only smoothed and constructed walls carry one, and
            // it links a wall to the masonry it was cut with rather than to the
            // rock it stands in, so a smoothed wall against a rough one would
            // report an open side where a voxel view shows solid stone.
            let skin = matches!(voxel.solid, Solid::Cube | Solid::Fortification)
                .then(|| {
                    let lib = library.as_deref()?;
                    if !lib.is_wall(voxel.tile_id) {
                        return None;
                    }
                    let mut mask = 0;
                    for (bit, dx, dy) in wall::NEIGHBOURS {
                        if world.voxel(x + dx, y + dy, z).is_some_and(|n| {
                            matches!(n.solid, Solid::Cube | Solid::Fortification)
                        }) {
                            mask |= bit;
                        }
                    }
                    lib.wall_skin(voxel.tile_id, mask)
                })
                .flatten();

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
                    let (lo, hi) = ([fx, fy, fz], [fx + 1.0, fy + Z_SCALE, fz + 1.0]);
                    match skin {
                        // A near-grey sheet is a pattern the material colours,
                        // as the ground is; a sheet with colour of its own
                        // keeps it and takes only the tile's brightness.
                        Some(skin) => {
                            let wobble = jitter(x, y, z);
                            let tint = if skin.tint {
                                damp([color[0], color[1], color[2]], 0.35)
                            } else {
                                [wobble, wobble, wobble]
                            };
                            mesh.textured_cuboid(
                                lo,
                                hi,
                                [tint[0], tint[1], tint[2], 1.0],
                                full,
                                skin.top,
                                skin.side,
                            );
                        }
                        None => mesh.cuboid(lo, hi, color, full),
                    }
                }
                Solid::Floor if !covered => {
                    let rims = rim_faces();
                    mesh.cuboid(
                        [fx, fy, fz],
                        [fx + 1.0, fy + FLOOR_HEIGHT, fz + 1.0],
                        color,
                        Faces { top: false, ..rims },
                    );
                }
                Solid::Floor => {}
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

            // Magma sits inside the cell at a height set by its fill level and
            // stays opaque. Water is a surface, meshed apart from here.
            if voxel.magma > 0 {
                let h = (voxel.magma as f32 / 7.0).clamp(0.15, 1.0) * Z_SCALE;
                let tint = to_linear([255, 90, 20], 1.0);
                mesh.cuboid([fx, fy, fz], [fx + 1.0, fy + h, fz + 1.0], tint, Faces::default());
            }
            tally(&mesh, before, &mut budget.liquids);
        }
    }

    if let Some(surface) = surface.as_ref() {
        budget.surface += surface.emit(world, chunk, opts, library.as_deref_mut(), &mut mesh);
    }

    build_furnishings(chunk, opts, library.as_deref(), surface.as_ref(), &mut mesh, budget);

    // Water is a sheet across tiles rather than a box inside one, and it is
    // meshed whatever else a tile is drawing: a pool's rim tiles are ramps, and
    // the sprite paths above have already moved on from them.
    let mut water = MeshData::default();
    crate::water::build_chunk(world, chunk, opts, &mut water);
    budget.liquids += water.triangle_count();
    if !water.is_empty() {
        mesh.water = Some(Box::new(water));
    }

    mesh
}
