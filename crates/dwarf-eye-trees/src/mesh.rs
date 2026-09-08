//! Greedy meshing: cull the faces between neighbours, then merge the survivors
//! into the largest rectangles of one colour and kind.

use crate::math::{IVec3, ivec3};
use crate::params::Rgb;
use crate::raster::{Kind, VoxelTree};

#[derive(Clone, Debug, Default)]
pub struct TreeMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    /// World-space, one texture repeat per tile: a face's two off-axis world
    /// coordinates. Every surface in the scene then shares one texel density
    /// whatever the voxel resolution, and a merged run simply spans more
    /// repeats. Needs a Repeat sampler.
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    /// Per vertex: 0 bark, 1 leaf.
    pub kinds: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub bark_voxels: usize,
    pub leaf_voxels: usize,
    pub bark_triangles: usize,
    pub leaf_triangles: usize,
}

impl Stats {
    pub fn voxels(&self) -> usize {
        self.bark_voxels + self.leaf_voxels
    }

    pub fn triangles(&self) -> usize {
        self.bark_triangles + self.leaf_triangles
    }
}

/// Voxel and triangle counts per kind. Meshing runs once per kind, which is how
/// the lab draws bark and leaves with different materials anyway.
pub fn stats(tree: &VoxelTree) -> Stats {
    let counts = tree.counts();
    Stats {
        bark_voxels: counts.bark,
        leaf_voxels: counts.leaf,
        bark_triangles: mesh_of(tree, Some(Kind::Bark)).indices.len() / 3,
        leaf_triangles: mesh_of(tree, Some(Kind::Leaf)).indices.len() / 3,
    }
}

/// Mesh everything, bark and leaves in one buffer.
pub fn mesh(tree: &VoxelTree) -> TreeMesh {
    mesh_of(tree, None)
}

/// Mesh one kind, or all of it when `only` is `None`.
///
/// Positions are in tiles, with the trunk's base at the origin.
pub fn mesh_of(tree: &VoxelTree, only: Option<Kind>) -> TreeMesh {
    let mut out = TreeMesh::default();
    let Some((lo, hi)) = tree.bounds() else { return out };

    let dims = [hi.x - lo.x + 3, hi.y - lo.y + 3, hi.z - lo.z + 3];
    let size = (dims[0] * dims[1] * dims[2]) as usize;
    // One voxel of padding on every side, so faces on the outer shell survive
    // the neighbour test without bounds checks.
    let mut grid: Vec<Option<(u8, Rgb)>> = vec![None; size];
    let index = |x: i32, y: i32, z: i32| ((z * dims[1] + y) * dims[0] + x) as usize;
    for (at, voxel) in &tree.voxels {
        if only.is_some_and(|k| k != voxel.kind) {
            continue;
        }
        let kind = if voxel.kind == Kind::Bark { 0 } else { 1 };
        grid[index(at.x - lo.x + 1, at.y - lo.y + 1, at.z - lo.z + 1)] = Some((kind, voxel.color));
    }

    let inv = 1.0 / tree.voxels_per_tile as f32;
    let origin = ivec3(lo.x - 1, lo.y - 1, lo.z - 1);

    for axis in 0..3usize {
        let u = (axis + 1) % 3;
        let v = (axis + 2) % 3;
        let (du, dv) = (dims[u], dims[v]);
        let mut mask: Vec<Option<(u8, Rgb)>> = vec![None; (du * dv) as usize];

        for sign in [1i32, -1] {
            for layer in 0..dims[axis] {
                let mut any = false;
                for j in 0..dv {
                    for i in 0..du {
                        let mut here = [0i32; 3];
                        here[axis] = layer;
                        here[u] = i;
                        here[v] = j;
                        let mut there = here;
                        there[axis] = layer + sign;
                        let cell = grid[index(here[0], here[1], here[2])];
                        let cover = if there[axis] < 0 || there[axis] >= dims[axis] {
                            None
                        } else {
                            grid[index(there[0], there[1], there[2])]
                        };
                        let face = match (cell, cover) {
                            (Some(c), None) => Some(c),
                            _ => None,
                        };
                        any |= face.is_some();
                        mask[(j * du + i) as usize] = face;
                    }
                }
                if !any {
                    continue;
                }

                // Merge the mask into maximal rectangles.
                let mut j = 0;
                while j < dv {
                    let mut i = 0;
                    while i < du {
                        let Some(cell) = mask[(j * du + i) as usize] else {
                            i += 1;
                            continue;
                        };
                        let mut w = 1;
                        while i + w < du && mask[(j * du + i + w) as usize] == Some(cell) {
                            w += 1;
                        }
                        let mut h = 1;
                        'grow: while j + h < dv {
                            for k in 0..w {
                                if mask[((j + h) * du + i + k) as usize] != Some(cell) {
                                    break 'grow;
                                }
                            }
                            h += 1;
                        }
                        for b in 0..h {
                            for a in 0..w {
                                mask[((j + b) * du + i + a) as usize] = None;
                            }
                        }
                        emit(
                            &mut out, axis, u, v, sign, layer, i, j, w, h, cell, origin, inv,
                        );
                        i += w;
                    }
                    j += 1;
                }
            }
        }
    }

    out
}

#[allow(clippy::too_many_arguments)]
fn emit(
    out: &mut TreeMesh,
    axis: usize,
    u: usize,
    v: usize,
    sign: i32,
    layer: i32,
    i: i32,
    j: i32,
    w: i32,
    h: i32,
    cell: (u8, Rgb),
    origin: IVec3,
    inv: f32,
) {
    let base = [origin.x as f32, origin.y as f32, origin.z as f32];
    let mut corner = [0.0f32; 3];
    corner[axis] = layer as f32 + if sign > 0 { 1.0 } else { 0.0 };
    corner[u] = i as f32;
    corner[v] = j as f32;

    let mut edge_u = [0.0f32; 3];
    edge_u[u] = w as f32;
    let mut edge_v = [0.0f32; 3];
    edge_v[v] = h as f32;

    let point = |a: f32, b: f32| {
        [
            (base[0] + corner[0] + edge_u[0] * a + edge_v[0] * b) * inv,
            (base[1] + corner[1] + edge_u[1] * a + edge_v[1] * b) * inv,
            (base[2] + corner[2] + edge_u[2] * a + edge_v[2] * b) * inv,
        ]
    };

    let mut normal = [0.0f32; 3];
    normal[axis] = sign as f32;

    let start = out.positions.len() as u32;
    // (axis, u, v) is cyclic, so edge_u x edge_v points along +axis.
    let quad = [point(0.0, 0.0), point(1.0, 0.0), point(1.0, 1.0), point(0.0, 1.0)];
    // Side faces take v from world Y, so bark fissures run up the trunk.
    let uv = |p: [f32; 3]| match axis {
        0 => [p[2], p[1]],
        1 => [p[0], p[2]],
        _ => [p[0], p[1]],
    };
    let uvs = [uv(quad[0]), uv(quad[1]), uv(quad[2]), uv(quad[3])];
    let color = cell.1.to_linear();
    for k in 0..4 {
        out.positions.push(quad[k]);
        out.normals.push(normal);
        out.colors.push(color);
        out.uvs.push(uvs[k]);
        out.kinds.push(cell.0);
    }
    let order: [u32; 6] =
        if sign > 0 { [0, 1, 2, 0, 2, 3] } else { [0, 2, 1, 0, 3, 2] };
    out.indices.extend(order.iter().map(|o| start + o));
}
