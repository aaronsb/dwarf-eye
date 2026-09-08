//! Tree crowns as voxel clumps and branch rods.
//!
//! DF grows a crown as a filled block of tiles, and extruding each of those
//! tiles from its 32x32 sprite put 98.8% of the live window's triangles in
//! trees. Drawing the block as one smooth surface only traded that for green
//! marshmallows. So the tiles are treated as an envelope rather than as
//! matter: limbs become rods along the connections DF reports, leaves become
//! clumps scattered where the crown opens onto air, and the inside is left
//! empty. Sky comes through the gaps, and the cost follows the leaf surface
//! rather than the tile count.
//!
//! Everything is rasterised into a sub-tile voxel grid and meshed with the same
//! face emitter the rest of the renderer uses, with coplanar faces of one
//! colour merged greedily. `DETAIL` sub-voxels per tile edge is the one knob:
//! six now, three for a distant level of detail later.
//!
//! Determinism: every clump is seeded from its tree's origin and its own tile,
//! and every element is placed from the tile it belongs to alone, so the chunks
//! either side of a seam agree without talking to each other.

use crate::library::TileLibrary;
use crate::mesh::{MeshData, MeshOptions, Z_SCALE};
use crate::skeleton::{Part, Skeleton, step};
use crate::world::{BLOCK, Chunk, World};
use dwarf_eye_art::raws;
use std::collections::HashMap;

/// Sub-voxels per tile edge, and where the override lives.
///
/// Four is what the live window affords: six costs 2.0M triangles across it and
/// four costs 977k, and four reads blockier, which is the look. Three is the
/// intended far level of detail. `DWARF_EYE_CANOPY_DETAIL` overrides it.
pub const DEFAULT_DETAIL: i32 = 4;

pub fn detail() -> i32 {
    std::env::var("DWARF_EYE_CANOPY_DETAIL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_DETAIL)
        .clamp(2, 16)
}

/// Tiles of world beyond the chunk whose elements can still reach into it.
///
/// A rod runs half a tile out and a clump sits within half a tile of its own
/// centre, so two tiles of margin covers the one voxel of halo the face culling
/// needs.
const REACH: i32 = 2;

/// Which part of a crown a tile is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum CanopyPart {
    /// A limb, including the heavy branches DF calls trunk branches.
    Branch,
    /// The outermost, thinnest growth.
    Twig,
    /// The solid treetop of a cap tree, which DF reports as floor, wall or ramp.
    Cap,
}

impl CanopyPart {
    /// Radius of a leaf clump, in tiles.
    fn clump_radius(self) -> f32 {
        match self {
            CanopyPart::Branch => 0.38,
            CanopyPart::Twig => 0.31,
            CanopyPart::Cap => 0.44,
        }
    }

    /// Most clumps a tile of this part ever scatters.
    fn clump_limit(self) -> f32 {
        match self {
            CanopyPart::Branch => 3.0,
            CanopyPart::Twig => 4.0,
            CanopyPart::Cap => 4.0,
        }
    }

    /// Radius of the rod along a limb, in tiles.
    ///
    /// DF's own BRANCH_RADIUS is how far a branch reaches, not how thick it is,
    /// so thickness is set here. TODO: take it from MAX_TRUNK_DIAMETER once the
    /// L-system grows real tapered limbs.
    fn rod_radius(self) -> f32 {
        match self {
            CanopyPart::Branch => 0.13,
            CanopyPart::Twig => 0.07,
            CanopyPart::Cap => 0.0,
        }
    }
}

/// A deterministic value in 0..1 from four integers.
fn hash01(a: i32, b: i32, c: i32, d: i32) -> f32 {
    let mut h = (a as u32).wrapping_mul(0x9E3779B1)
        ^ (b as u32).wrapping_mul(0x85EBCA77)
        ^ (c as u32).wrapping_mul(0xC2B2AE3D)
        ^ (d as u32).wrapping_mul(0x27D4EB2F);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545F491);
    h ^= h >> 13;
    (h & 0xFFFF) as f32 / 65535.0
}

fn to_linear(rgb: [u8; 3]) -> [f32; 3] {
    let f = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    [f(rgb[0]), f(rgb[1]), f(rgb[2])]
}

/// A sub-tile voxel grid over one chunk, with a voxel of halo so faces at the
/// chunk's edge can be culled against what the neighbour holds.
struct Volume {
    detail: i32,
    /// Own voxels per horizontal axis, and vertically.
    nx: i32,
    ny: i32,
    /// Zero is empty; anything else is a palette entry plus one.
    cells: Vec<u8>,
    palette: Vec<[f32; 3]>,
    keys: HashMap<u64, u8>,
    origin: (i32, i32, i32),
}

impl Volume {
    fn new(chunk: &Chunk, detail: i32) -> Self {
        let (nx, ny) = (BLOCK * detail, detail);
        let cells = vec![0u8; ((nx + 2) * (ny + 2) * (nx + 2)) as usize];
        Self {
            detail,
            nx,
            ny,
            cells,
            palette: Vec::new(),
            keys: HashMap::new(),
            origin: chunk.origin(),
        }
    }

    fn index(&self, i: i32, j: i32, k: i32) -> usize {
        (((i + 1) * (self.ny + 2) + (j + 1)) * (self.nx + 2) + (k + 1)) as usize
    }

    fn get(&self, i: i32, j: i32, k: i32) -> u8 {
        if i < -1 || j < -1 || k < -1 || i > self.nx || j > self.ny || k > self.nx {
            return 0;
        }
        self.cells[self.index(i, j, k)]
    }

    /// Interns a colour, so faces of one shade can merge into one rectangle.
    fn shade(&mut self, key: u64, color: [f32; 3]) -> u8 {
        if let Some(&found) = self.keys.get(&key) {
            return found;
        }
        // 255 shades is far more than a chunk of forest ever asks for; beyond
        // that, reuse rather than lose the voxel.
        if self.palette.len() >= 255 {
            return 1;
        }
        self.palette.push(color);
        let slot = self.palette.len() as u8;
        self.keys.insert(key, slot);
        slot
    }

    /// Centre of a voxel, in render space.
    fn centre(&self, i: i32, j: i32, k: i32) -> [f32; 3] {
        let d = self.detail as f32;
        [
            self.origin.0 as f32 + (i as f32 + 0.5) / d,
            (self.origin.2 as f32 + (j as f32 + 0.5) / d) * Z_SCALE,
            self.origin.1 as f32 + (k as f32 + 0.5) / d,
        ]
    }

    /// Voxel index range a world-space box covers, clipped to the halo.
    fn span(&self, lo: [f32; 3], hi: [f32; 3]) -> [(i32, i32); 3] {
        let d = self.detail as f32;
        let axis = |a: f32, b: f32, origin: f32, limit: i32| -> (i32, i32) {
            let first = ((a - origin) * d - 0.5).floor() as i32;
            let last = ((b - origin) * d - 0.5).ceil() as i32;
            (first.max(-1), last.min(limit))
        };
        [
            axis(lo[0], hi[0], self.origin.0 as f32, self.nx),
            axis(lo[1] / Z_SCALE, hi[1] / Z_SCALE, self.origin.2 as f32, self.ny),
            axis(lo[2], hi[2], self.origin.1 as f32, self.nx),
        ]
    }

    /// Fills the voxels inside an ellipsoid.
    fn ellipsoid(&mut self, centre: [f32; 3], radius: [f32; 3], shade: u8) {
        let lo = [centre[0] - radius[0], centre[1] - radius[1], centre[2] - radius[2]];
        let hi = [centre[0] + radius[0], centre[1] + radius[1], centre[2] + radius[2]];
        let [(i0, i1), (j0, j1), (k0, k1)] = self.span(lo, hi);
        for i in i0..=i1 {
            for j in j0..=j1 {
                for k in k0..=k1 {
                    let p = self.centre(i, j, k);
                    let mut d = 0.0;
                    for a in 0..3 {
                        let t = (p[a] - centre[a]) / radius[a];
                        d += t * t;
                    }
                    if d <= 1.0 {
                        let at = self.index(i, j, k);
                        self.cells[at] = shade;
                    }
                }
            }
        }
    }

    /// Fills the voxels within `radius` of the segment `a`..`b`.
    fn rod(&mut self, a: [f32; 3], b: [f32; 3], radius: f32, shade: u8) {
        // Below about three quarters of a voxel a rod falls between the voxel
        // centres and vanishes, so thin growth still draws a hairline.
        let r = radius.max(0.75 / self.detail as f32);
        let lo = [a[0].min(b[0]) - r, a[1].min(b[1]) - r, a[2].min(b[2]) - r];
        let hi = [a[0].max(b[0]) + r, a[1].max(b[1]) + r, a[2].max(b[2]) + r];
        let [(i0, i1), (j0, j1), (k0, k1)] = self.span(lo, hi);

        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let len2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
        for i in i0..=i1 {
            for j in j0..=j1 {
                for k in k0..=k1 {
                    let p = self.centre(i, j, k);
                    let ap = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
                    let t = if len2 > 1e-9 {
                        ((ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / len2).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let mut d2 = 0.0;
                    for c in 0..3 {
                        let e = ap[c] - t * ab[c];
                        d2 += e * e;
                    }
                    if d2 <= r * r {
                        let at = self.index(i, j, k);
                        self.cells[at] = shade;
                    }
                }
            }
        }
    }
}

/// How the six faces are lit, so a voxel crown reads as a lit solid.
const FACE_SHADE: [f32; 6] = [0.70, 0.70, 0.50, 1.0, 0.82, 0.82];

/// Triangles a chunk's crown cost, with and without merging.
#[derive(Default, Debug, Clone, Copy)]
pub struct CanopyBudget {
    pub triangles: usize,
    /// What the same faces would have cost one quad each.
    pub unmerged: usize,
}

/// Builds one chunk's crown geometry.
pub fn build(
    world: &World,
    chunk: &Chunk,
    opts: MeshOptions,
    library: &mut TileLibrary,
) -> MeshData {
    build_budgeted(world, chunk, opts, library, &mut CanopyBudget::default())
}

/// `build`, counting what merging saved.
pub fn build_budgeted(
    world: &World,
    chunk: &Chunk,
    opts: MeshOptions,
    library: &mut TileLibrary,
    budget: &mut CanopyBudget,
) -> MeshData {
    let mut mesh = MeshData::default();
    if chunk.z > opts.z_ceiling {
        return mesh;
    }
    let skeleton = Skeleton::build(world, chunk, opts, library, REACH);
    if skeleton.is_empty() {
        return mesh;
    }

    let mut volume = Volume::new(chunk, detail());
    for part in &skeleton.parts {
        limbs(&mut volume, &skeleton, part, library);
        leaves(&mut volume, &skeleton, part, library);
    }
    // A trunk standing among the crown gets leaves over it too, so its sawn-off
    // top does not show through the gaps.
    for (pos, tree, species) in skeleton.crowned_trunks() {
        let part = Part { pos, kind: CanopyPart::Branch, links: 0, tree, species };
        leaves(&mut volume, &skeleton, &part, library);
    }
    emit(&volume, &mut mesh, budget);
    mesh
}

/// The centre of a tile, in render space.
fn tile_centre(pos: (i32, i32, i32)) -> [f32; 3] {
    [pos.0 as f32 + 0.5, (pos.2 as f32 + 0.5) * Z_SCALE, pos.1 as f32 + 0.5]
}

/// Lays one tile's limbs into the volume.
///
/// Each tile draws its own half of every joint, out to the tile boundary, so
/// two joined tiles meet in the middle whichever chunk each of them lands in.
fn limbs(volume: &mut Volume, skeleton: &Skeleton, part: &Part, library: &mut TileLibrary) {
    let (x, y, z) = part.pos;
    let centre = tile_centre(part.pos);
    let bark = to_linear(library.bark_color(part.species));
    let rod_radius = part.kind.rod_radius();
    if rod_radius > 0.0 {
        let shade = volume.shade(u64::MAX ^ (part.species as u32 as u64), bark);
        let mut drawn = false;
        for bit in [raws::NORTH, raws::SOUTH, raws::WEST, raws::EAST] {
            if part.links & bit == 0 {
                continue;
            }
            let (dx, dy) = step(bit);
            let end = [
                centre[0] + dx as f32 * 0.5,
                centre[1],
                centre[2] + dy as f32 * 0.5,
            ];
            volume.rod(centre, end, rod_radius, shade);
            drawn = true;
        }
        // A limb that joins nothing, and the stub that meets the trunk below,
        // still need a body.
        if !drawn || skeleton.is_trunk((x, y, z - 1)) {
            let below = [centre[0], centre[1] - 0.5 * Z_SCALE, centre[2]];
            volume.rod(centre, below, rod_radius, shade);
        }
    }
}

/// Scatters one tile's leaf clumps.
///
/// Only where the tile opens onto air: a tile boxed in by more crown is never
/// seen, so it stays an empty frame of limbs and the sky reaches the ground
/// through the gaps.
fn leaves(volume: &mut Volume, skeleton: &Skeleton, part: &Part, library: &mut TileLibrary) {
    let (x, y, z) = part.pos;
    let centre = tile_centre(part.pos);
    let (tx, ty, tz) = part.tree;
    let growth = library.growth(part.species);
    let openness = skeleton.openness(part.pos);
    if openness <= 0.0 {
        return;
    }
    // A species that grows few branches carries fewer leaves.
    let density = if growth.branch_density == 0 {
        1.0
    } else {
        0.6 + 0.8 * (growth.branch_density as f32 / 100.0)
    };
    let spread = 0.85 + 0.1 * growth.branch_radius.clamp(1, 4) as f32;
    let count = (part.kind.clump_limit() * (0.25 + 0.75 * openness) * density).round() as i32;

    let above = (x, y, z + 1);
    let crown = !skeleton.holds(above) && !skeleton.is_trunk(above);
    let leaf = to_linear(library.canopy_color(part.species));
    for n in 0..count.max(1) {
        let jitter = |slot: i32| hash01(tx ^ x, ty ^ y, tz ^ z, n * 8 + slot);
        let offset = [
            (jitter(0) - 0.5) * 0.62,
            (jitter(1) - 0.5) * 0.62 * Z_SCALE,
            (jitter(2) - 0.5) * 0.62,
        ];
        let scale = part.kind.clump_radius() * spread * (0.8 + 0.45 * jitter(3));
        // Four shades, quantised so a clump's faces still merge with itself.
        // Leaves at the top of the crown catch more sky, so they run lighter.
        let level = (jitter(4) * 3.0) as i32 + crown as i32;
        let lift = 0.84 + 0.08 * level as f32;
        let key = ((part.species as u32 as u64) << 8) | level as u64;
        let shade = volume.shade(key, [leaf[0] * lift, leaf[1] * lift, leaf[2] * lift]);
        volume.ellipsoid(
            [centre[0] + offset[0], centre[1] + offset[1], centre[2] + offset[2]],
            [scale, scale * 0.82 * Z_SCALE, scale],
            shade,
        );
    }
}

/// Turns the volume's surface into quads, merging coplanar runs of one shade.
fn emit(volume: &Volume, mesh: &mut MeshData, budget: &mut CanopyBudget) {
    let d = volume.detail as f32;
    let (ox, oy, oz) = volume.origin;
    let mut mask: Vec<u8> = Vec::new();

    for face in 0..6usize {
        // Axis 0 is x, 1 is y, 2 is z; even faces look toward the negative end.
        let axis = face / 2;
        let positive = face % 2 == 1;
        let along = if axis == 1 { volume.ny } else { volume.nx };
        let (wide, tall) = match axis {
            0 => (volume.ny, volume.nx),
            1 => (volume.nx, volume.nx),
            _ => (volume.nx, volume.ny),
        };

        for slice in 0..along {
            mask.clear();
            mask.resize((wide * tall) as usize, 0);
            let next = if positive { slice + 1 } else { slice - 1 };
            for a in 0..wide {
                for b in 0..tall {
                    let (here, beyond) = match axis {
                        0 => ((slice, a, b), (next, a, b)),
                        1 => ((a, slice, b), (a, next, b)),
                        _ => ((a, b, slice), (a, b, next)),
                    };
                    let cell = volume.get(here.0, here.1, here.2);
                    if cell == 0 || volume.get(beyond.0, beyond.1, beyond.2) != 0 {
                        continue;
                    }
                    mask[(a * tall + b) as usize] = cell;
                }
            }

            budget.unmerged += mask.iter().filter(|&&c| c != 0).count() * 2;

            for a in 0..wide {
                let mut b = 0;
                while b < tall {
                    let cell = mask[(a * tall + b) as usize];
                    if cell == 0 {
                        b += 1;
                        continue;
                    }
                    // Grow along b, then along a while every row matches.
                    let mut w = 1;
                    while b + w < tall && mask[(a * tall + b + w) as usize] == cell {
                        w += 1;
                    }
                    let mut h = 1;
                    'grow: while a + h < wide {
                        for t in 0..w {
                            if mask[((a + h) * tall + b + t) as usize] != cell {
                                break 'grow;
                            }
                        }
                        h += 1;
                    }
                    for ra in a..a + h {
                        for rb in b..b + w {
                            mask[(ra * tall + rb) as usize] = 0;
                        }
                    }

                    let plane = if positive { slice + 1 } else { slice };
                    let color = volume.palette[(cell - 1) as usize];
                    let lit = FACE_SHADE[face];
                    let rgba = [color[0] * lit, color[1] * lit, color[2] * lit, 1.0];
                    push_face(
                        mesh, face, ox, oy, oz, d, plane, a, a + h, b, b + w, rgba,
                    );
                    b += w;
                }
            }
        }
    }
    budget.triangles += mesh.indices.len() / 3;
}

/// Appends one merged rectangle, wound so it faces out of the crown.
#[allow(clippy::too_many_arguments)]
fn push_face(
    mesh: &mut MeshData,
    face: usize,
    ox: i32,
    oy: i32,
    oz: i32,
    d: f32,
    plane: i32,
    a0: i32,
    a1: i32,
    b0: i32,
    b1: i32,
    color: [f32; 4],
) {
    // Voxel index to world coordinate, per axis.
    let wx = |i: i32| ox as f32 + i as f32 / d;
    let wy = |j: i32| (oz as f32 + j as f32 / d) * Z_SCALE;
    let wz = |k: i32| oy as f32 + k as f32 / d;

    let (corners, normal) = match face {
        // -x, with the free axes running (y, z).
        0 => {
            let (x, y0, y1, z0, z1) = (wx(plane), wy(a0), wy(a1), wz(b0), wz(b1));
            (
                [[x, y0, z1], [x, y1, z1], [x, y1, z0], [x, y0, z0]],
                [-1.0, 0.0, 0.0],
            )
        }
        1 => {
            let (x, y0, y1, z0, z1) = (wx(plane), wy(a0), wy(a1), wz(b0), wz(b1));
            (
                [[x, y0, z0], [x, y1, z0], [x, y1, z1], [x, y0, z1]],
                [1.0, 0.0, 0.0],
            )
        }
        // -y and +y, with the free axes running (x, z).
        2 => {
            let (y, x0, x1, z0, z1) = (wy(plane), wx(a0), wx(a1), wz(b0), wz(b1));
            (
                [[x0, y, z0], [x1, y, z0], [x1, y, z1], [x0, y, z1]],
                [0.0, -1.0, 0.0],
            )
        }
        3 => {
            let (y, x0, x1, z0, z1) = (wy(plane), wx(a0), wx(a1), wz(b0), wz(b1));
            (
                [[x0, y, z0], [x0, y, z1], [x1, y, z1], [x1, y, z0]],
                [0.0, 1.0, 0.0],
            )
        }
        // -z and +z, with the free axes running (x, y).
        4 => {
            let (z, x0, x1, y0, y1) = (wz(plane), wx(a0), wx(a1), wy(b0), wy(b1));
            (
                [[x0, y0, z], [x0, y1, z], [x1, y1, z], [x1, y0, z]],
                [0.0, 0.0, -1.0],
            )
        }
        _ => {
            let (z, x0, x1, y0, y1) = (wz(plane), wx(a0), wx(a1), wy(b0), wy(b1));
            (
                [[x1, y0, z], [x1, y1, z], [x0, y1, z], [x0, y0, z]],
                [0.0, 0.0, 1.0],
            )
        }
    };
    mesh.push_quad(corners, normal, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk() -> Chunk {
        Chunk { block_x: 0, block_y: 0, z: 0, voxels: Vec::new() }
    }

    #[test]
    fn a_clump_fills_voxels_around_its_centre() {
        let mut volume = Volume::new(&chunk(), 6);
        let shade = volume.shade(1, [0.2, 0.5, 0.2]);
        volume.ellipsoid([8.0, 0.5, 8.0], [0.3, 0.3, 0.3], shade);
        let filled = volume.cells.iter().filter(|&&c| c != 0).count();
        assert!(filled > 4, "a third-of-a-tile clump should hold several voxels, got {filled}");
        assert_eq!(volume.get(48, 3, 48), shade);
    }

    #[test]
    fn a_rod_stays_connected_along_its_length() {
        let mut volume = Volume::new(&chunk(), 6);
        let shade = volume.shade(1, [0.4, 0.3, 0.2]);
        volume.rod([8.0, 0.5, 8.0], [9.0, 0.5, 8.0], 0.05, shade);
        // Every voxel column the rod passes through must hold something.
        for i in 48..54 {
            let any = (0..6).any(|j| (0..6).any(|k| volume.get(i, j, 45 + k) != 0));
            assert!(any, "the rod broke at voxel column {i}");
        }
    }

    #[test]
    fn merging_a_flat_slab_costs_two_triangles_a_face() {
        let mut volume = Volume::new(&chunk(), 6);
        let shade = volume.shade(1, [0.3, 0.3, 0.3]);
        // A 4x1x4 block of voxels: six flat faces, each one merged rectangle.
        for i in 10..14 {
            for k in 10..14 {
                let at = volume.index(i, 3, k);
                volume.cells[at] = shade;
            }
        }
        let mut mesh = MeshData::default();
        let mut budget = CanopyBudget::default();
        emit(&volume, &mut mesh, &mut budget);
        assert_eq!(budget.triangles, 12, "six merged rectangles");
        assert_eq!(budget.unmerged, 96, "before merging: 48 faces, two triangles each");
    }
}
