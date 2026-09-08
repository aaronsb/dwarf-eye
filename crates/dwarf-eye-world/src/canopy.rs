//! Voxelising grown trees and slicing them into chunks.
//!
//! `tree.rs` grows a tree inside the envelope DF gives it. Here that tree is
//! rasterised once into its own voxel volume, cached by origin, and copied into
//! whichever chunks it reaches. Growing per tree rather than per chunk is what
//! keeps a chunk boundary from changing a tree's shape.
//!
//! The surface is meshed with the same face emitter as the rest of the
//! renderer, with coplanar faces of one shade merged greedily. Leaf faces carry
//! the species' twig sprite as an alpha cutout, so light comes through the
//! crown at texel scale and the shadow it casts is finely dappled.

use crate::library::TileLibrary;
use crate::mesh::{MeshData, MeshOptions, Z_SCALE};
use crate::tree::{DETAIL, Envelope, Grown, hash01};
use crate::world::{BLOCK, Chunk, World};
use dwarf_eye_art::atlas::Rect;
use std::collections::HashMap;
use std::sync::Arc;

/// Which part of a crown a tile is. DF's own classification, still used to
/// decide which tiles make up a tree's envelope.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum CanopyPart {
    /// A limb, including the heavy branches DF calls trunk branches.
    Branch,
    /// The outermost, thinnest growth.
    Twig,
    /// The solid treetop of a cap tree, which DF reports as floor, wall or ramp.
    Cap,
}

/// Voxels per leaf cluster edge, so leaves gather rather than speckle.
const CLUSTER: i32 = 2;

/// How the six faces are lit, so a voxel tree reads as a lit solid.
const FACE_SHADE: [f32; 6] = [0.70, 0.70, 0.50, 1.0, 0.82, 0.82];

/// Tiles of world beyond a chunk whose trees can still reach into it.
const REACH: i32 = 2;

/// One shade in a volume's palette.
#[derive(Clone, Copy)]
struct Tone {
    color: [f32; 3],
    /// Where its sprite sits in the atlas, for leaves; bark carries none.
    leaf: Option<Rect>,
}

fn to_linear(rgb: [u8; 3]) -> [f32; 3] {
    let f = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    [f(rgb[0]), f(rgb[1]), f(rgb[2])]
}

/// A tree, grown and voxelised, in global voxel coordinates.
pub struct TreeVoxels {
    /// Corner of the volume, in voxels: tile times `DETAIL`.
    gx: i32,
    gy: i32,
    gz: i32,
    nx: i32,
    ny: i32,
    nz: i32,
    /// Zero is empty; anything else is a tone plus one.
    cells: Vec<u8>,
    tones: Vec<Tone>,
    pub leaf_voxels: usize,
    pub bark_voxels: usize,
}

impl TreeVoxels {
    fn index(&self, i: i32, j: i32, k: i32) -> usize {
        ((i * self.ny + j) * self.nz + k) as usize
    }

    /// Reads a voxel by global voxel coordinates.
    fn at(&self, gi: i32, gj: i32, gk: i32) -> u8 {
        let (i, j, k) = (gi - self.gx, gj - self.gy, gk - self.gz);
        if i < 0 || j < 0 || k < 0 || i >= self.nx || j >= self.ny || k >= self.nz {
            return 0;
        }
        self.cells[self.index(i, j, k)]
    }

    fn set(&mut self, i: i32, j: i32, k: i32, tone: u8) {
        if i < 0 || j < 0 || k < 0 || i >= self.nx || j >= self.ny || k >= self.nz {
            return;
        }
        let at = self.index(i, j, k);
        if self.cells[at] != 0 {
            return;
        }
        self.cells[at] = tone;
        if self.tones[(tone - 1) as usize].leaf.is_some() {
            self.leaf_voxels += 1;
        } else {
            self.bark_voxels += 1;
        }
    }

    /// Centre of a voxel, in render space.
    fn centre(&self, i: i32, j: i32, k: i32) -> [f32; 3] {
        let d = DETAIL as f32;
        [
            (self.gx + i) as f32 / d + 0.5 / d,
            ((self.gy + j) as f32 / d + 0.5 / d) * Z_SCALE,
            (self.gz + k) as f32 / d + 0.5 / d,
        ]
    }

    /// Voxel index range a world-space box covers, clipped to the volume.
    fn span(&self, lo: [f32; 3], hi: [f32; 3]) -> [(i32, i32); 3] {
        let d = DETAIL as f32;
        let axis = |a: f32, b: f32, base: i32, limit: i32| -> (i32, i32) {
            let first = (a * d - 0.5).floor() as i32 - base;
            let last = (b * d - 0.5).ceil() as i32 - base;
            (first.max(0), last.min(limit - 1))
        };
        [
            axis(lo[0], hi[0], self.gx, self.nx),
            axis(lo[1] / Z_SCALE, hi[1] / Z_SCALE, self.gy, self.ny),
            axis(lo[2], hi[2], self.gz, self.nz),
        ]
    }

    /// The voxel a point falls in.
    fn nearest(&self, p: [f32; 3]) -> (i32, i32, i32) {
        let d = DETAIL as f32;
        (
            (p[0] * d - 0.5).round() as i32 - self.gx,
            (p[1] / Z_SCALE * d - 0.5).round() as i32 - self.gy,
            (p[2] * d - 0.5).round() as i32 - self.gz,
        )
    }

    /// Fills the voxels within a tapering distance of the segment `a`..`b`.
    ///
    /// A limb thin enough to read as a twig is thinner than the gap between
    /// voxel centres, so the radius alone would leave it as a row of dashes.
    /// Walking the segment and claiming the voxel under each step is what keeps
    /// it unbroken at one voxel wide.
    fn rod(&mut self, a: [f32; 3], b: [f32; 3], r0: f32, r1: f32, tone: u8) {
        let run = ((b[0] - a[0]).powi(2)
            + ((b[1] - a[1]) / Z_SCALE).powi(2)
            + (b[2] - a[2]).powi(2))
        .sqrt();
        let steps = (run * DETAIL as f32 * 2.0).ceil().max(1.0) as i32;
        for n in 0..=steps {
            let t = n as f32 / steps as f32;
            let p = [
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
            ];
            let (i, j, k) = self.nearest(p);
            self.set(i, j, k, tone);
        }
        let floor = 0.35 / DETAIL as f32;
        let (r0, r1) = (r0.max(floor), r1.max(floor));
        let wide = r0.max(r1);
        let lo = [a[0].min(b[0]) - wide, a[1].min(b[1]) - wide, a[2].min(b[2]) - wide];
        let hi = [a[0].max(b[0]) + wide, a[1].max(b[1]) + wide, a[2].max(b[2]) + wide];
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
                    let r = r0 + (r1 - r0) * t;
                    if d2 <= r * r {
                        self.set(i, j, k, tone);
                    }
                }
            }
        }
    }
}

/// Grows and voxelises one tree.
fn voxelise(env: &Envelope, grown: &Grown, library: &mut TileLibrary) -> TreeVoxels {
    let leaf_uv = library.leaf_uv(env.species);
    let leaf_tones: Vec<Tone> = library
        .leaf_tones(env.species, 3)
        .into_iter()
        .map(|c| Tone { color: to_linear(c), leaf: leaf_uv })
        .collect();
    let bark_tones: Vec<Tone> = library
        .bark_tones(env.species, 2)
        .into_iter()
        .map(|c| Tone { color: to_linear(c), leaf: None })
        .collect();
    let mut tones = bark_tones.clone();
    tones.extend(leaf_tones.iter().copied());
    let bark_n = bark_tones.len();

    // The volume covers the envelope's footprint and height with a tile of
    // margin, which is all the half-tile dilation and the root flare can reach.
    let (gx, gy, gz) = (
        (env.x0 - 1) * DETAIL,
        (env.z0 - 1) * DETAIL,
        (env.y0 - 1) * DETAIL,
    );
    let (nx, ny, nz) = (
        (env.w + 2) * DETAIL,
        (env.height() + 2) * DETAIL,
        (env.d + 2) * DETAIL,
    );
    let mut volume = TreeVoxels {
        gx,
        gy,
        gz,
        nx,
        ny,
        nz,
        cells: vec![0u8; (nx * ny * nz) as usize],
        tones,
        leaf_voxels: 0,
        bark_voxels: 0,
    };

    let (ox, oy, oz) = env.origin;
    for (n, seg) in grown.wood.iter().enumerate() {
        let pick = hash01(ox, oy, oz, 1100 + n as i32);
        let tone = 1 + (pick * bark_n as f32) as u8 % bark_n.max(1) as u8;
        volume.rod(seg.a, seg.b, seg.r0, seg.r1, tone);
    }

    if leaf_tones.is_empty() {
        return volume;
    }
    let growth = library.growth(env.species);
    let density = if growth.branch_density == 0 {
        1.0
    } else {
        0.7 + 0.6 * growth.branch_density as f32 / 100.0
    };
    for (n, anchor) in grown.leaves.iter().enumerate() {
        scatter(&mut volume, env, *anchor, density, bark_n, n as i32);
    }
    volume
}

/// Gathers leaves around one point on the outermost wood.
///
/// Density falls under the point and rises toward the top of the tree and the
/// outside of its footprint, which is what leaves the underside open and the
/// crown thickest where it meets the sky.
fn scatter(
    volume: &mut TreeVoxels,
    env: &Envelope,
    anchor: [f32; 3],
    density: f32,
    bark_n: usize,
    n: i32,
) {
    let leaf_n = volume.tones.len() - bark_n;
    if leaf_n == 0 {
        return;
    }
    let reach = 0.95;
    let lo = [anchor[0] - reach, anchor[1] - reach * Z_SCALE, anchor[2] - reach];
    let hi = [anchor[0] + reach, anchor[1] + reach * Z_SCALE, anchor[2] + reach];
    let [(i0, i1), (j0, j1), (k0, k1)] = volume.span(lo, hi);

    let crown = (env.z1 + 1) as f32;
    let base = env.crown_z0 as f32;
    for i in i0..=i1 {
        for j in j0..=j1 {
            for k in k0..=k1 {
                let p = volume.centre(i, j, k);
                if !env.contains(p) {
                    continue;
                }
                let far = ((p[0] - anchor[0]).powi(2)
                    + ((p[1] - anchor[1]) / Z_SCALE).powi(2)
                    + (p[2] - anchor[2]).powi(2))
                .sqrt();
                if far > reach {
                    continue;
                }
                // Thinner below the anchor than above it, and thinner deep
                // inside the crown than out at its shell.
                let under = if p[1] < anchor[1] { 0.66 } else { 1.0 };
                let high = 0.68 + 0.32 * ((p[1] / Z_SCALE - base) / (crown - base)).clamp(0.0, 1.0);
                let c = env.centre_at(p[1]);
                let out = ((p[0] - c[0]).powi(2) + (p[2] - c[1]).powi(2)).sqrt()
                    / env.radius_at(p[1]).max(0.5);
                let shell = 0.72 + 0.42 * out.clamp(0.0, 1.0);
                // Capped, so even the thickest part of a crown keeps some air
                // and the sky reaches through it.
                let fill = (1.9 * (1.0 - 0.75 * far / reach) * under * high * shell * density)
                    .min(0.86);

                let (ci, cj, ck) = (
                    (volume.gx + i).div_euclid(CLUSTER),
                    (volume.gy + j).div_euclid(CLUSTER),
                    (volume.gz + k).div_euclid(CLUSTER),
                );
                if hash01(ci, cj, ck, 3) >= fill {
                    continue;
                }
                let pick = hash01(ci, cj, ck, 11 + n % 3);
                let tone = bark_n + (pick * leaf_n as f32) as usize % leaf_n;
                volume.set(i, j, k, tone as u8 + 1);
            }
        }
    }
}

/// Trees that have been grown, kept so a chunk never regrows one.
#[derive(Default)]
pub struct Forest {
    trees: HashMap<(i32, i32, i32), Option<Arc<TreeVoxels>>>,
}

/// What a chunk's trees cost.
#[derive(Default, Debug, Clone, Copy)]
pub struct CanopyBudget {
    pub triangles: usize,
    /// What the same faces would have cost one quad each.
    pub unmerged: usize,
    pub leaf_voxels: usize,
    pub bark_voxels: usize,
    pub trees: usize,
}

impl Forest {
    /// Forgets the trees near a set of chunks, so blocks that have just arrived
    /// can lengthen an envelope that was cut short.
    pub fn retire_near(&mut self, keys: &[(i32, i32, i32)]) {
        if keys.is_empty() {
            return;
        }
        self.trees.retain(|origin, _| {
            !keys.iter().any(|&(bx, by, z)| {
                (origin.0.div_euclid(BLOCK) - bx).abs() <= 1
                    && (origin.1.div_euclid(BLOCK) - by).abs() <= 1
                    && (origin.2 - z).abs() <= 20
            })
        });
    }

    pub fn tree_count(&self) -> usize {
        self.trees.values().filter(|t| t.is_some()).count()
    }

    /// Leaf and bark voxels across every tree grown, each counted once however
    /// many chunks it reaches.
    pub fn voxel_counts(&self) -> (usize, usize) {
        self.trees
            .values()
            .flatten()
            .fold((0, 0), |(l, b), t| (l + t.leaf_voxels, b + t.bark_voxels))
    }

    /// Builds one chunk's tree geometry.
    pub fn build_chunk(
        &mut self,
        world: &World,
        chunk: &Chunk,
        opts: MeshOptions,
        library: &mut TileLibrary,
    ) -> MeshData {
        self.build_budgeted(world, chunk, opts, library, &mut CanopyBudget::default())
    }

    /// `build_chunk`, counting what it cost.
    pub fn build_budgeted(
        &mut self,
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
        let mut origins = self.nearby(world, chunk, opts, library);
        if origins.is_empty() {
            return mesh;
        }
        origins.sort_unstable();
        origins.dedup();

        let mut volume = Volume::new(chunk);
        for (origin, species) in origins {
            let grown = self.tree(world, library, origin, species);
            let Some(grown) = grown else { continue };
            budget.trees += 1;
            volume.absorb(&grown);
        }
        emit(&volume, &mut mesh, budget);
        mesh
    }

    /// The trees whose tiles reach into a chunk.
    fn nearby(
        &self,
        world: &World,
        chunk: &Chunk,
        opts: MeshOptions,
        library: &TileLibrary,
    ) -> Vec<((i32, i32, i32), i32)> {
        let (ox, oy, oz) = chunk.origin();
        let mut found = Vec::new();
        for z in (oz - REACH)..=(oz + REACH) {
            for x in (ox - REACH)..(ox + BLOCK + REACH) {
                for y in (oy - REACH)..(oy + BLOCK + REACH) {
                    let Some(v) = world.voxel(x, y, z) else { continue };
                    if v.hidden && !opts.show_hidden {
                        continue;
                    }
                    if !library.is_trunk(v.tile_id) && library.canopy_part(v.tile_id).is_none() {
                        continue;
                    }
                    found.push((v.tree_origin(x, y, z), v.mat_index));
                }
            }
        }
        found
    }

    fn tree(
        &mut self,
        world: &World,
        library: &mut TileLibrary,
        origin: (i32, i32, i32),
        species: i32,
    ) -> Option<Arc<TreeVoxels>> {
        if let Some(found) = self.trees.get(&origin) {
            return found.clone();
        }
        let built = Envelope::read(world, library, origin, species).map(|env| {
            let growth = library.growth(species);
            let habit = env.habit(growth);
            let grown = crate::tree::grow(&env, growth, habit);
            Arc::new(voxelise(&env, &grown, library))
        });
        self.trees.insert(origin, built.clone());
        built
    }
}

/// One chunk's slice of whatever trees reach it, with a voxel of halo so faces
/// at its edge can be culled against what the neighbour holds.
struct Volume {
    nx: i32,
    ny: i32,
    cells: Vec<u8>,
    palette: Vec<Tone>,
    keys: HashMap<u32, u8>,
    origin: (i32, i32, i32),
}

impl Volume {
    fn new(chunk: &Chunk) -> Self {
        let (nx, ny) = (BLOCK * DETAIL, DETAIL);
        Self {
            nx,
            ny,
            cells: vec![0u8; ((nx + 2) * (ny + 2) * (nx + 2)) as usize],
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

    /// Copies the part of one tree that lands in this chunk.
    fn absorb(&mut self, tree: &TreeVoxels) {
        let (ox, oy, oz) = self.origin;
        let (bx, by, bz) = (ox * DETAIL, oz * DETAIL, oy * DETAIL);
        let mut map: Vec<Option<u8>> = vec![None; tree.tones.len()];
        for i in -1..=self.nx {
            for j in -1..=self.ny {
                for k in -1..=self.nx {
                    let cell = tree.at(bx + i, by + j, bz + k);
                    if cell == 0 {
                        continue;
                    }
                    let n = (cell - 1) as usize;
                    let slot = match map[n] {
                        Some(slot) => slot,
                        None => {
                            let slot = self.intern(tree.tones[n]);
                            map[n] = Some(slot);
                            slot
                        }
                    };
                    let at = self.index(i, j, k);
                    if self.cells[at] == 0 {
                        self.cells[at] = slot;
                    }
                }
            }
        }
    }

    fn intern(&mut self, tone: Tone) -> u8 {
        let key = ((tone.color[0] * 4095.0) as u32) << 20
            | ((tone.color[1] * 4095.0) as u32) << 8
            | ((tone.color[2] * 255.0) as u32)
            | if tone.leaf.is_some() { 1 << 31 } else { 0 };
        if let Some(&found) = self.keys.get(&key) {
            return found;
        }
        if self.palette.len() >= 255 {
            return 1;
        }
        self.palette.push(tone);
        let slot = self.palette.len() as u8;
        self.keys.insert(key, slot);
        slot
    }
}

/// Turns the volume's surface into quads, merging coplanar runs of one shade.
fn emit(volume: &Volume, mesh: &mut MeshData, budget: &mut CanopyBudget) {
    let d = DETAIL as f32;
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
                    let tone = volume.palette[(cell - 1) as usize];
                    let lit = FACE_SHADE[face];
                    let rgba =
                        [tone.color[0] * lit, tone.color[1] * lit, tone.color[2] * lit, 1.0];
                    push_face(
                        mesh, face, ox, oy, oz, d, plane, a, a + h, b, b + w, rgba, tone.leaf,
                    );
                    b += w;
                }
            }
        }
    }
    budget.triangles += mesh.indices.len() / 3;
}

/// Appends one merged rectangle, wound so it faces out of the tree.
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
    leaf: Option<Rect>,
) {
    let wx = |i: i32| ox as f32 + i as f32 / d;
    let wy = |j: i32| (oz as f32 + j as f32 / d) * Z_SCALE;
    let wz = |k: i32| oy as f32 + k as f32 / d;

    let (corners, normal) = match face {
        0 => {
            let (x, y0, y1, z0, z1) = (wx(plane), wy(a0), wy(a1), wz(b0), wz(b1));
            ([[x, y0, z1], [x, y1, z1], [x, y1, z0], [x, y0, z0]], [-1.0, 0.0, 0.0])
        }
        1 => {
            let (x, y0, y1, z0, z1) = (wx(plane), wy(a0), wy(a1), wz(b0), wz(b1));
            ([[x, y0, z0], [x, y1, z0], [x, y1, z1], [x, y0, z1]], [1.0, 0.0, 0.0])
        }
        2 => {
            let (y, x0, x1, z0, z1) = (wy(plane), wx(a0), wx(a1), wz(b0), wz(b1));
            ([[x0, y, z0], [x1, y, z0], [x1, y, z1], [x0, y, z1]], [0.0, -1.0, 0.0])
        }
        3 => {
            let (y, x0, x1, z0, z1) = (wy(plane), wx(a0), wx(a1), wz(b0), wz(b1));
            ([[x0, y, z0], [x0, y, z1], [x1, y, z1], [x1, y, z0]], [0.0, 1.0, 0.0])
        }
        4 => {
            let (z, x0, x1, y0, y1) = (wz(plane), wx(a0), wx(a1), wy(b0), wy(b1));
            ([[x0, y0, z], [x0, y1, z], [x1, y1, z], [x1, y0, z]], [0.0, 0.0, -1.0])
        }
        _ => {
            let (z, x0, x1, y0, y1) = (wz(plane), wx(a0), wx(a1), wy(b0), wy(b1));
            ([[x1, y0, z], [x1, y1, z], [x0, y1, z], [x0, y0, z]], [0.0, 0.0, 1.0])
        }
    };

    match leaf {
        // The sprite stretches over a merged run rather than repeating, because
        // the atlas has no wrap inside a cell. A leaf cutout survives that: the
        // holes widen but they stay holes.
        Some(uv) => mesh.push_textured_quad(
            corners,
            normal,
            color,
            [[uv.u0, uv.v0], [uv.u0, uv.v1], [uv.u1, uv.v1], [uv.u1, uv.v0]],
        ),
        None => mesh.push_quad(corners, normal, color),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_limb_is_at_least_one_voxel_wide_and_unbroken() {
        let mut volume = TreeVoxels {
            gx: 0,
            gy: 0,
            gz: 0,
            nx: 64,
            ny: 16,
            nz: 64,
            cells: vec![0; 64 * 16 * 64],
            tones: vec![Tone { color: [0.4, 0.3, 0.2], leaf: None }],
            leaf_voxels: 0,
            bark_voxels: 0,
        };
        volume.rod([2.0, 1.0, 2.0], [4.0, 1.0, 2.0], 0.05, 0.05, 1);
        for i in (2 * DETAIL)..(4 * DETAIL) {
            let any = (0..16).any(|j| (0..64).any(|k| volume.at(i, j, k) != 0));
            assert!(any, "the limb broke at voxel column {i}");
        }
        assert!(volume.bark_voxels > 0);
    }
}
