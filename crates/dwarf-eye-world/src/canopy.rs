//! Tree crowns as merged volumes.
//!
//! A branch tile extruded from its sprite costs a few hundred triangles and a
//! grown tree holds dozens of them, so a forest is most of the frame's
//! geometry. Here every canopy tile instead drops a sphere of density into a
//! field, and the isosurface through that field is meshed once for the whole
//! chunk. Neighbouring tiles merge into one rounded crown, and the cost falls
//! with the crown's surface area rather than its tile count.
//!
//! The mesher is naive surface nets: one vertex per cell that straddles the
//! isolevel, one quad per grid edge that crosses it. It needs no lookup table,
//! and it emits about half the triangles marching cubes would for the same
//! surface.
//!
//! Chunks are single z-levels, so a crown is meshed in slices. The field is a
//! function of world position alone, and a chunk owns exactly the grid edges
//! whose low corner lies inside it, so the slices meet without seams or
//! duplicates as long as both sides can see the same tiles. That is what the
//! `REACH` halo is for.

use crate::library::TileLibrary;
use crate::mesh::{MeshData, MeshOptions, Z_SCALE};
use crate::world::{BLOCK, Chunk, World};
use std::collections::HashSet;

/// Grid cells per tile edge. Two gives half-tile resolution.
const RES: i32 = 2;

/// Density at which the surface sits.
const LEVEL: f32 = 0.5;

/// Support radius of the sphere each canopy tile adds to the summed field.
///
/// DF grows a crown as a filled box — the trees here are seven tiles across and
/// nine levels tall — so the roundness has to come from the field. A sphere
/// this wide smooths a corner away over about a tile while a flat face barely
/// moves, which is what turns the box into a crown.
const SUPPORT: f32 = 2.4;

/// How much of a sphere a tile contributes, by which part of the crown it is.
/// Twigs are thin growth at the edge and press outward less than a limb.
const BRANCH_WEIGHT: f32 = 1.0;
const TWIG_WEIGHT: f32 = 0.85;
const CAP_WEIGHT: f32 = 1.0;

/// Weight kept by a tile with none of its twenty-six neighbours filled.
///
/// DF grows a crown as a squared-off block — the trees here are seven tiles
/// across, solid to a flat underside — so weighting a tile by how enclosed it
/// is erodes the corners, edges and that flat bottom while leaving the middle
/// untouched. It is what turns the block into something crown-shaped.
const EXPOSED_WEIGHT: f32 = 0.3;

/// Summed density a crown reaches just outside its outermost tiles.
///
/// Dividing by it puts the surface there, so a crown clears its own footprint
/// by about half a tile however wide the smoothing sphere is.
const CROWN_FILL: f32 = 2.05;

/// Support radius of the sphere a tile keeps to itself.
///
/// The summed field needs a high divisor to stay near the crown's footprint,
/// and a tile standing alone never reaches it. Taking the larger of the two
/// fields draws that tile as its own small ball, just inside its own tile, so
/// a lone sprig is not erased.
const LONE_SUPPORT: f32 = 0.9;

/// Tiles of world beyond the chunk that can still reach into its grid.
const REACH: i32 = 3;

/// Corners along one tile-space axis, per tile.
const NX: i32 = BLOCK * RES;
const NY: i32 = RES;

/// Corner counts including the one-cell halo on each side.
const CX: usize = (NX + 2) as usize;
const CY: usize = (NY + 2) as usize;

/// Which part of a crown a tile is, which sets how far its sphere reaches.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum CanopyPart {
    /// A limb: the bulk of the crown.
    Branch,
    /// The outermost, thinnest growth.
    Twig,
    /// The solid treetop, which DF reports as floor, wall or ramp.
    Cap,
}

impl CanopyPart {
    fn weight(self) -> f32 {
        match self {
            CanopyPart::Branch => BRANCH_WEIGHT,
            CanopyPart::Twig => TWIG_WEIGHT,
            CanopyPart::Cap => CAP_WEIGHT,
        }
    }
}

/// A canopy tile reduced to the sphere it contributes.
struct Blob {
    /// Centre, in render space.
    centre: [f32; 3],
    weight: f32,
    /// Linear colour of this species' foliage.
    color: [f32; 3],
}

/// The sampled field over one chunk plus its halo.
struct Field {
    /// Every tile's sphere added together, which is what merges a crown.
    sum: Vec<f32>,
    /// The largest single sphere, which is what keeps a lone tile visible.
    lone: Vec<f32>,
    /// Colour summed against the same weights, so a vertex reads the species
    /// that actually built it.
    color: Vec<[f32; 3]>,
}

impl Field {
    fn new() -> Self {
        let n = CX * CY * CX;
        Self { sum: vec![0.0; n], lone: vec![0.0; n], color: vec![[0.0; 3]; n] }
    }

    /// Corner indices run -1..=N; the halo is folded into the offset.
    fn index(i: i32, j: i32, k: i32) -> usize {
        ((i + 1) as usize * CY + (j + 1) as usize) * CX + (k + 1) as usize
    }

    fn at(&self, i: i32, j: i32, k: i32) -> f32 {
        let at = Self::index(i, j, k);
        (self.sum[at] * (LEVEL / CROWN_FILL)).max(self.lone[at])
    }
}

fn to_linear(rgb: [u8; 3]) -> [f32; 3] {
    let f = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    [f(rgb[0]), f(rgb[1]), f(rgb[2])]
}

/// A hash in 0..1 from three integers, for variation that survives a reload.
fn hash01(a: i32, b: i32, c: i32) -> f32 {
    let mut h = (a as u32).wrapping_mul(0x9E3779B1)
        ^ (b as u32).wrapping_mul(0x85EBCA77)
        ^ (c as u32).wrapping_mul(0xC2B2AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545F491);
    h ^= h >> 13;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Collects every canopy tile that can reach into this chunk's grid.
///
/// Trunk tiles standing among the crown join it, so the surface closes over the
/// top of the trunk instead of dipping around it and leaving a capped stump in
/// the open air.
///
/// Sorted by position, so the field is summed in the same order however the
/// chunks arrived.
fn gather(world: &World, chunk: &Chunk, opts: MeshOptions, library: &mut TileLibrary) -> Vec<Blob> {
    let (ox, oy, oz) = chunk.origin();
    // Position, part, species, and whether it is a trunk drawn into the crown.
    let mut found: Vec<(i32, i32, i32, CanopyPart, i32, bool)> = Vec::new();
    let mut trunks: Vec<(i32, i32, i32, i32)> = Vec::new();

    // One tile wider than the blobs need, so every blob can count all of its
    // neighbours and the chunks either side of a seam agree on the answer.
    let edge = REACH + 1;
    for z in (oz - edge)..=(oz + edge) {
        if z > opts.z_ceiling {
            continue;
        }
        for bx in (ox - edge).div_euclid(BLOCK)..=(ox + BLOCK - 1 + edge).div_euclid(BLOCK) {
            for by in (oy - edge).div_euclid(BLOCK)..=(oy + BLOCK - 1 + edge).div_euclid(BLOCK) {
                let Some(near) = world.chunk(bx, by, z) else { continue };
                for ly in 0..BLOCK {
                    for lx in 0..BLOCK {
                        let (x, y) = (bx * BLOCK + lx, by * BLOCK + ly);
                        if x < ox - edge
                            || x >= ox + BLOCK + edge
                            || y < oy - edge
                            || y >= oy + BLOCK + edge
                        {
                            continue;
                        }
                        let v = near.get(lx, ly);
                        if v.hidden && !opts.show_hidden {
                            continue;
                        }
                        match library.canopy_part(v.tile_id) {
                            Some(part) => found.push((x, y, z, part, v.mat_index, false)),
                            None if library.is_trunk(v.tile_id) => {
                                trunks.push((x, y, z, v.mat_index))
                            }
                            None => {}
                        }
                    }
                }
            }
        }
    }
    let mut crown: HashSet<(i32, i32, i32)> =
        found.iter().map(|&(x, y, z, ..)| (x, y, z)).collect();
    for (x, y, z, mat_index) in trunks {
        let among = (-1..=1).any(|dx| {
            (-1..=1).any(|dy| (-1..=1).any(|dz| crown.contains(&(x + dx, y + dy, z + dz))))
        });
        if among {
            found.push((x, y, z, CanopyPart::Branch, mat_index, true));
        }
    }
    crown.extend(found.iter().map(|&(x, y, z, ..)| (x, y, z)));
    found.sort_unstable();

    // How enclosed a tile is, over its twenty-six neighbours.
    let enclosure = |x: i32, y: i32, z: i32| -> f32 {
        let mut n = 0;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if (dx, dy, dz) != (0, 0, 0) && crown.contains(&(x + dx, y + dy, z + dz)) {
                        n += 1;
                    }
                }
            }
        }
        let f = n as f32 / 26.0;
        EXPOSED_WEIGHT + (1.0 - EXPOSED_WEIGHT) * f * f.sqrt()
    };

    found
        .into_iter()
        .filter(|&(x, y, z, ..)| {
            x >= ox - REACH
                && x < ox + BLOCK + REACH
                && y >= oy - REACH
                && y < oy + BLOCK + REACH
                && z >= oz - REACH
                && z <= oz + REACH
        })
        .map(|(x, y, z, part, mat_index, trunk)| {
            let color = to_linear(library.canopy_color(mat_index));
            // Vary each tree's crown a little, seeded from where it stands.
            let (tx, ty, tz) = world
                .voxel(x, y, z)
                .map(|v| v.tree_origin(x, y, z))
                .unwrap_or((x, y, z));
            let wobble = 0.85 + 0.3 * hash01(tx, ty, tz);
            Blob {
                centre: [x as f32 + 0.5, (z as f32 + 0.5) * Z_SCALE, y as f32 + 0.5],
                // Wood does not erode: a trunk standing in a gap in the crown
                // keeps enough weight to be covered rather than left as a
                // capped stump in the open.
                weight: part.weight() * enclosure(x, y, z).max(if trunk { 0.8 } else { 0.0 }) * wobble,
                color,
            }
        })
        .collect()
}

/// Sums the blobs into the chunk's corner grid.
fn splat(blobs: &[Blob], chunk: &Chunk) -> Field {
    let (ox, oy, oz) = chunk.origin();
    let mut field = Field::new();
    let step = 1.0 / RES as f32;

    for blob in blobs {
        // Corner index range a sphere of `radius` around this blob can touch.
        let span = |centre: f32, origin: f32, limit: i32, radius: f32| -> (i32, i32) {
            let lo = ((centre - radius - origin) * RES as f32).ceil() as i32;
            let hi = ((centre + radius - origin) * RES as f32).floor() as i32;
            (lo.max(-1), hi.min(limit))
        };

        for (radius, merged) in [(SUPPORT, true), (LONE_SUPPORT, false)] {
            let r2 = radius * radius;
            let (i0, i1) = span(blob.centre[0], ox as f32, NX, radius);
            let (j0, j1) = span(blob.centre[1] / Z_SCALE, oz as f32, NY, radius);
            let (k0, k1) = span(blob.centre[2], oy as f32, NX, radius);

            for i in i0..=i1 {
                let dx = ox as f32 + i as f32 * step - blob.centre[0];
                for j in j0..=j1 {
                    let dy = (oz as f32 + j as f32 * step) * Z_SCALE - blob.centre[1];
                    for k in k0..=k1 {
                        let dz = oy as f32 + k as f32 * step - blob.centre[2];
                        let d2 = dx * dx + dy * dy + dz * dz;
                        if d2 >= r2 {
                            continue;
                        }
                        let t = 1.0 - d2 / r2;
                        let w = t * t * blob.weight;
                        let at = Field::index(i, j, k);
                        if merged {
                            field.sum[at] += w;
                            for c in 0..3 {
                                field.color[at][c] += w * blob.color[c];
                            }
                        } else {
                            field.lone[at] = field.lone[at].max(w);
                        }
                    }
                }
            }
        }
    }
    field
}

/// The eight corners of a cell, as offsets.
const CORNERS: [[i32; 3]; 8] = [
    [0, 0, 0], [1, 0, 0], [0, 1, 0], [1, 1, 0],
    [0, 0, 1], [1, 0, 1], [0, 1, 1], [1, 1, 1],
];

/// The twelve edges of a cell, as corner index pairs.
const EDGES: [(usize, usize); 12] = [
    (0, 1), (2, 3), (4, 5), (6, 7),
    (0, 2), (1, 3), (4, 6), (5, 7),
    (0, 4), (1, 5), (2, 6), (3, 7),
];

/// Builds one chunk's canopy geometry into `mesh`.
pub fn build(
    world: &World,
    chunk: &Chunk,
    opts: MeshOptions,
    library: &mut TileLibrary,
    mesh: &mut MeshData,
) {
    let blobs = gather(world, chunk, opts, library);
    if blobs.is_empty() {
        return;
    }
    let field = splat(&blobs, chunk);
    let (ox, oy, oz) = chunk.origin();
    let step = 1.0 / RES as f32;

    // One vertex per straddling cell. Cells run -1..=N-1, one halo cell on the
    // low side of each axis so the chunk can close the edges it owns.
    let cell_index = |ci: i32, cj: i32, ck: i32| -> usize {
        ((ci + 1) as usize * (CY - 1) + (cj + 1) as usize) * (CX - 1) + (ck + 1) as usize
    };
    let mut vertex = vec![u32::MAX; (CX - 1) * (CY - 1) * (CX - 1)];

    for ci in -1..NX {
        for cj in -1..NY {
            for ck in -1..NX {
                let mut d = [0.0f32; 8];
                let mut inside = 0;
                for (n, c) in CORNERS.iter().enumerate() {
                    d[n] = field.at(ci + c[0], cj + c[1], ck + c[2]);
                    if d[n] >= LEVEL {
                        inside += 1;
                    }
                }
                if inside == 0 || inside == 8 {
                    continue;
                }

                // The vertex sits at the mean of the crossings on the cell's
                // edges, which is what rounds the surface off.
                let (mut sum, mut count) = ([0.0f32; 3], 0.0f32);
                for &(a, b) in &EDGES {
                    if (d[a] >= LEVEL) == (d[b] >= LEVEL) {
                        continue;
                    }
                    let t = ((LEVEL - d[a]) / (d[b] - d[a])).clamp(0.0, 1.0);
                    for c in 0..3 {
                        sum[c] += CORNERS[a][c] as f32 + t * (CORNERS[b][c] - CORNERS[a][c]) as f32;
                    }
                    count += 1.0;
                }
                let local = [sum[0] / count, sum[1] / count, sum[2] / count];

                // The field falls away from the crown, so its gradient points
                // inward and the outward normal is its negation.
                let face = |lo: [usize; 4]| -> f32 { lo.iter().map(|&n| d[n]).sum() };
                let grad = [
                    face([1, 3, 5, 7]) - face([0, 2, 4, 6]),
                    face([2, 3, 6, 7]) - face([0, 1, 4, 5]),
                    face([4, 5, 6, 7]) - face([0, 1, 2, 3]),
                ];
                let len = (grad[0] * grad[0] + grad[1] * grad[1] + grad[2] * grad[2]).sqrt();
                let normal = if len > 1e-6 {
                    [-grad[0] / len, -grad[1] / len, -grad[2] / len]
                } else {
                    [0.0, 1.0, 0.0]
                };

                // Colour comes from whichever species weighs most here.
                let (mut csum, mut wsum) = ([0.0f32; 3], 0.0f32);
                for c in CORNERS {
                    let at = Field::index(ci + c[0], cj + c[1], ck + c[2]);
                    wsum += field.sum[at];
                    for n in 0..3 {
                        csum[n] += field.color[at][n];
                    }
                }
                let base = if wsum > 1e-6 {
                    [csum[0] / wsum, csum[1] / wsum, csum[2] / wsum]
                } else {
                    [0.0, 0.0, 0.0]
                };

                // Underside darker than crown, plus a per-vertex wobble so the
                // surface does not read as one painted shell.
                let lit = 0.52 + 0.48 * (0.5 + 0.5 * normal[1]);
                let wobble = 0.92 + 0.16 * hash01(ox * RES + ci, oz * RES + cj, oy * RES + ck);
                let shade = lit * wobble;

                vertex[cell_index(ci, cj, ck)] = mesh.positions.len() as u32;
                mesh.positions.push([
                    ox as f32 + (ci as f32 + local[0]) * step,
                    (oz as f32 + (cj as f32 + local[1]) * step) * Z_SCALE,
                    oy as f32 + (ck as f32 + local[2]) * step,
                ]);
                mesh.normals.push(normal);
                mesh.colors.push([base[0] * shade, base[1] * shade, base[2] * shade, 1.0]);
                mesh.uvs.push(dwarf_eye_art::atlas::WHITE_UV);
            }
        }
    }

    // One quad per crossing edge, from the four cells that ring it. The chunk
    // owns the edges whose low corner is its own, so the slice above and the
    // block beside it close their own halves and nothing is drawn twice.
    for i in 0..NX {
        for j in 0..NY {
            for k in 0..NX {
                let here = field.at(i, j, k) >= LEVEL;
                // Ring of cells around each axis, wound counter-clockwise as
                // seen from the positive end of that axis.
                let rings: [(f32, [[i32; 3]; 4]); 3] = [
                    (
                        field.at(i + 1, j, k),
                        [[i, j - 1, k - 1], [i, j, k - 1], [i, j, k], [i, j - 1, k]],
                    ),
                    (
                        field.at(i, j + 1, k),
                        [[i - 1, j, k - 1], [i - 1, j, k], [i, j, k], [i, j, k - 1]],
                    ),
                    (
                        field.at(i, j, k + 1),
                        [[i - 1, j - 1, k], [i, j - 1, k], [i, j, k], [i - 1, j, k]],
                    ),
                ];
                for (far, cells) in rings {
                    if (far >= LEVEL) == here {
                        continue;
                    }
                    let mut quad = [0u32; 4];
                    let mut complete = true;
                    for (n, c) in cells.iter().enumerate() {
                        let v = vertex[cell_index(c[0], c[1], c[2])];
                        complete &= v != u32::MAX;
                        quad[n] = v;
                    }
                    if !complete {
                        continue;
                    }
                    // Wind so the face turns away from the dense side.
                    let [a, b, c, e] = if here { quad } else { [quad[3], quad[2], quad[1], quad[0]] };
                    mesh.indices.extend_from_slice(&[a, b, c, a, c, e]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field is dense at a blob's centre and empty well away from it.
    #[test]
    fn blob_fills_its_centre() {
        let chunk = Chunk { block_x: 0, block_y: 0, z: 0, voxels: Vec::new() };
        let blobs = vec![Blob {
            centre: [8.0, 0.5, 8.0],
            weight: 1.0,
            color: [0.2, 0.5, 0.2],
        }];
        let field = splat(&blobs, &chunk);
        assert!(field.at(16, 1, 16) > LEVEL, "centre of the blob must be inside");
        assert!(field.at(0, 0, 0) < LEVEL, "the corner must be outside");
    }

    #[test]
    fn tree_origin_combines_axes_differently() {
        let v = crate::world::Voxel { tree_dx: 2, tree_dy: -1, tree_dz: 3, ..Default::default() };
        assert_eq!(v.tree_origin(10, 10, 10), (8, 11, 13));
    }
}
