//! Voxelising grown trees and slicing them into chunks.
//!
//! `tree.rs` grows a tree inside the envelope DF gives it. Here that tree is
//! rasterised once into its own voxel volume, cached by origin, and copied into
//! whichever chunks it reaches. Growing per tree rather than per chunk is what
//! keeps a chunk boundary from changing a tree's shape.
//!
//! The surface is meshed with coplanar faces of one shade merged greedily, and
//! split by the material it wants: bark, broadleaf cutout, needle cutout. Leaf
//! faces carry world-space UVs, so a procedural cutout repeats at Dwarf
//! Fortress's own texel density however finely a tile is cut into voxels, and
//! the shadow the crown casts is dappled at that scale.
//!
//! Weeping species also hang streamers, which are quads rather than voxels;
//! they are meshed by the growth crate and handed to whichever chunk holds
//! them.

use crate::library::TileLibrary;
use crate::mesh::{MeshData, MeshOptions, Z_SCALE};
use crate::tree::{DETAIL, Envelope, Habit};
use dwarf_eye_art::raws::TreeGrowth;
use crate::world::{BLOCK, Chunk, World};
use dwarf_eye_trees as trees;
use dwarf_eye_trees::{Kind, TreeMesh};
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

/// How the six faces are lit, so a voxel tree reads as a lit solid.
const FACE_SHADE: [f32; 6] = [0.70, 0.70, 0.50, 1.0, 0.82, 0.82];

/// Tiles of world beyond a chunk whose trees can still reach into it.
const REACH: i32 = 2;

/// Z-levels above the top of a loaded column that its chunk still draws, so a
/// crown is never shorn off at the ceiling of what has been sent.
const OVERHEAD: i32 = 24;

/// Which of the canopy materials a face wants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    Bark,
    Broadleaf,
    Needle,
}

impl Surface {
    fn slot(self) -> usize {
        match self {
            Surface::Bark => 0,
            Surface::Broadleaf => 1,
            Surface::Needle => 2,
        }
    }
}

/// One shade in a volume's palette.
#[derive(Clone, Copy)]
struct Tone {
    color: [f32; 3],
    surface: Surface,
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
    /// The tree's weeping strands, already meshed in render space. Quads, not
    /// voxels, so they ride alongside the volume rather than in it.
    streamers: TreeMesh,
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
        if self.tones[(tone - 1) as usize].surface == Surface::Bark {
            self.bark_voxels += 1;
        } else {
            self.leaf_voxels += 1;
        }
    }

}

/// Grows one tree with the shared generator and lays its voxels out in render
/// space.
///
/// The generator works in tiles with the trunk's base at its own origin, so all
/// that happens here is an offset onto the tile the trunk stands on, and the
/// colours it chose interned into a small palette.
fn voxelise(
    env: &Envelope,
    library: &mut TileLibrary,
    growth: TreeGrowth,
    habit: Habit,
    world_origin: (i32, i32, i32),
) -> TreeVoxels {
    let grown = crate::tree::grow(env, growth, habit, library, world_origin);
    let leaf_surface =
        if habit == Habit::Conifer { Surface::Needle } else { Surface::Broadleaf };

    // The generator's origin is the centre of the base tile, at its floor.
    let anchor = [
        env.base.0 * DETAIL + DETAIL / 2,
        env.base.2 * DETAIL,
        env.base.1 * DETAIL + DETAIL / 2,
    ];
    let world = crate::tree::anchor(env);

    let Some((lo, hi)) = grown.bounds() else {
        return TreeVoxels {
            gx: anchor[0],
            gy: anchor[1],
            gz: anchor[2],
            nx: 0,
            ny: 0,
            nz: 0,
            cells: Vec::new(),
            tones: Vec::new(),
            streamers: TreeMesh::default(),
            leaf_voxels: 0,
            bark_voxels: 0,
        };
    };
    let (nx, ny, nz) = (hi.x - lo.x + 1, hi.y - lo.y + 1, hi.z - lo.z + 1);
    let mut volume = TreeVoxels {
        gx: anchor[0] + lo.x,
        gy: anchor[1] + lo.y,
        gz: anchor[2] + lo.z,
        nx,
        ny,
        nz,
        cells: vec![0u8; (nx * ny * nz) as usize],
        tones: Vec::new(),
        streamers: strands(&grown, world),
        leaf_voxels: 0,
        bark_voxels: 0,
    };

    let mut seen: HashMap<([u8; 3], bool), u8> = HashMap::new();
    for (at, voxel) in &grown.voxels {
        let bark = voxel.kind == Kind::Bark;
        let key = ([voxel.color.0, voxel.color.1, voxel.color.2], bark);
        let tone = match seen.get(&key) {
            Some(&tone) => tone,
            None => {
                if volume.tones.len() >= 255 {
                    continue;
                }
                let surface = if bark { Surface::Bark } else { leaf_surface };
                volume.tones.push(Tone { color: to_linear(key.0), surface });
                let tone = volume.tones.len() as u8;
                seen.insert(key, tone);
                tone
            }
        };
        volume.set(at.x - lo.x, at.y - lo.y, at.z - lo.z, tone);
    }
    volume
}

/// The tree's weeping strands, meshed once and moved into render space.
fn strands(grown: &trees::VoxelTree, at: [f32; 3]) -> TreeMesh {
    let mut mesh = trees::mesh_of(grown, Some(Kind::Streamer));
    for p in &mut mesh.positions {
        p[0] += at[0];
        p[1] = p[1] * Z_SCALE + at[1];
        p[2] += at[2];
    }
    mesh
}

/// Trees that have been grown, kept so a chunk never regrows one.
#[derive(Default)]
pub struct Forest {
    trees: HashMap<(i32, i32, i32), Option<Arc<TreeVoxels>>>,
}

/// One chunk's trees, split by the material each surface wants. Empty meshes
/// are normal: most chunks hold no conifer, and only weeping species hang
/// strands.
#[derive(Default)]
pub struct CanopyMeshes {
    pub bark: MeshData,
    pub broadleaf: MeshData,
    pub needle: MeshData,
    pub streamers: MeshData,
}

impl CanopyMeshes {
    pub fn is_empty(&self) -> bool {
        self.iter().all(|m| m.is_empty())
    }

    pub fn triangle_count(&self) -> usize {
        self.iter().map(|m| m.triangle_count()).sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = &MeshData> {
        [&self.bark, &self.broadleaf, &self.needle, &self.streamers].into_iter()
    }

    fn slot(&mut self, surface: Surface) -> &mut MeshData {
        match surface {
            Surface::Bark => &mut self.bark,
            Surface::Broadleaf => &mut self.broadleaf,
            Surface::Needle => &mut self.needle,
        }
    }
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
        world_origin: (i32, i32, i32),
    ) -> CanopyMeshes {
        self.build_budgeted(
            world,
            chunk,
            opts,
            library,
            world_origin,
            &mut CanopyBudget::default(),
        )
    }

    /// `build_chunk`, counting what it cost.
    #[allow(clippy::too_many_arguments)]
    pub fn build_budgeted(
        &mut self,
        world: &World,
        chunk: &Chunk,
        opts: MeshOptions,
        library: &mut TileLibrary,
        world_origin: (i32, i32, i32),
        budget: &mut CanopyBudget,
    ) -> CanopyMeshes {
        let mut meshes = CanopyMeshes::default();
        if chunk.z > opts.z_ceiling {
            return meshes;
        }
        let mut origins = self.nearby(world, chunk, opts, library);
        if origins.is_empty() {
            return meshes;
        }
        origins.sort_unstable();
        origins.dedup();

        // Only the top of a column carries what rises above it.
        let (cx, cy) = (chunk.block_x, chunk.block_y);
        let above = if world.chunk(cx, cy, chunk.z + 1).is_some() { 0 } else { OVERHEAD };
        let mut volume = Volume::new(chunk, above);
        for (origin, species) in origins {
            let grown = self.tree(world, library, origin, species, world_origin);
            let Some(grown) = grown else { continue };
            budget.trees += 1;
            volume.absorb(&grown);
            hang(&grown.streamers, chunk, &mut meshes.streamers);
        }
        emit(&volume, &mut meshes, budget);
        meshes
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
        world_origin: (i32, i32, i32),
    ) -> Option<Arc<TreeVoxels>> {
        if let Some(found) = self.trees.get(&origin) {
            return found.clone();
        }
        let built = Envelope::read(world, library, origin, species).map(|env| {
            let growth = library.growth(species);
            let habit = env.habit(growth);
            let grown = voxelise(&env, library, growth, habit, world_origin);
            if std::env::var("DWARF_EYE_TREE_LOG").is_ok() {
                let (bx, by) = (env.base.0.div_euclid(BLOCK), env.base.1.div_euclid(BLOCK));
                let mut loaded = env.z1;
                while world.chunk(bx, by, loaded + 1).is_some() {
                    loaded += 1;
                }
                eprintln!(
                    "tree {:?} species {species} envelope z {}..{} truncated {} loaded top {loaded} voxels y {}..{}",
                    env.origin,
                    env.z0,
                    env.z1,
                    env.truncated,
                    grown.gy as f32 / DETAIL as f32,
                    (grown.gy + grown.ny) as f32 / DETAIL as f32,
                );
            }
            Arc::new(grown)
        });
        self.trees.insert(origin, built.clone());
        built
    }
}

/// Copies the strands that hang inside this chunk.
///
/// A quad goes wherever its middle falls, so a strand crossing a chunk floor is
/// split between the two rather than drawn twice.
fn hang(strands: &TreeMesh, chunk: &Chunk, out: &mut MeshData) {
    if strands.indices.is_empty() {
        return;
    }
    let (ox, oy, oz) = chunk.origin();
    let inside = |p: [f32; 3]| {
        p[0] >= ox as f32
            && p[0] < (ox + BLOCK) as f32
            && p[2] >= oy as f32
            && p[2] < (oy + BLOCK) as f32
            && p[1] >= oz as f32 * Z_SCALE
            && p[1] < (oz + 1) as f32 * Z_SCALE
    };
    for quad in strands.positions.chunks_exact(4).enumerate() {
        let (n, corners) = quad;
        let mid = [
            (corners[0][0] + corners[2][0]) * 0.5,
            (corners[0][1] + corners[2][1]) * 0.5,
            (corners[0][2] + corners[2][2]) * 0.5,
        ];
        if !inside(mid) {
            continue;
        }
        let base = n * 4;
        out.push_textured_quad(
            [corners[0], corners[1], corners[2], corners[3]],
            strands.normals[base],
            strands.colors[base],
            [
                strands.uvs[base],
                strands.uvs[base + 1],
                strands.uvs[base + 2],
                strands.uvs[base + 3],
            ],
        );
    }
}

/// One chunk's slice of whatever trees reach it, with a voxel of halo so faces
/// at its edge can be culled against what the neighbour holds.
struct Volume {
    nx: i32,
    ny: i32,
    cells: Vec<u8>,
    palette: Vec<Tone>,
    keys: HashMap<(u32, u32, u32, u32), u8>,
    origin: (i32, i32, i32),
}

impl Volume {
    /// `above` is how many further z-levels this chunk has to carry because
    /// nothing is loaded over it. A crown reaching past the top of its column
    /// would otherwise have no chunk to be drawn in and would end flat at the
    /// loaded ceiling.
    fn new(chunk: &Chunk, above: i32) -> Self {
        let (nx, ny) = (BLOCK * DETAIL, DETAIL * (1 + above.max(0)));
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
        // Shades come from a handful of palette entries, so exact bits are a
        // fine key, and the surface has to be part of it: the same colour on
        // bark and on leaves needs two slots or one would take the other's
        // cutout.
        let key = (
            tone.color[0].to_bits(),
            tone.color[1].to_bits(),
            tone.color[2].to_bits(),
            tone.surface.slot() as u32,
        );
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

/// Turns the volume's surface into quads, merging coplanar runs of one shade
/// and sorting them into the mesh for the material each shade wants.
fn emit(volume: &Volume, out: &mut CanopyMeshes, budget: &mut CanopyBudget) {
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
                        out.slot(tone.surface),
                        face,
                        ox,
                        oy,
                        oz,
                        d,
                        plane,
                        a,
                        a + h,
                        b,
                        b + w,
                        rgba,
                    );
                    b += w;
                }
            }
        }
    }
    budget.triangles += out.bark.triangle_count()
        + out.broadleaf.triangle_count()
        + out.needle.triangle_count();
}

/// Appends one merged rectangle, wound so it faces out of the tree.
///
/// UVs are the face's own world coordinates, one texture repeat to the tile, so
/// bark and leaves show the same texel size as the ground whatever the voxel
/// resolution and however far a merged run reaches.
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

    // Vertical faces take v from world height, so bark fissures run up a trunk.
    let uv = |p: [f32; 3]| match face {
        0 | 1 => [p[2], p[1] / Z_SCALE],
        2 | 3 => [p[0], p[2]],
        _ => [p[0], p[1] / Z_SCALE],
    };
    mesh.push_textured_quad(
        corners,
        normal,
        color,
        [uv(corners[0]), uv(corners[1]), uv(corners[2]), uv(corners[3])],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_chunk(block_x: i32, block_y: i32, z: i32) -> Chunk {
        Chunk { block_x, block_y, z, voxels: Vec::new() }
    }

    #[test]
    fn a_shade_is_interned_once_per_surface() {
        let mut volume = Volume::new(&test_chunk(0, 0, 0), 0);
        let bark = Tone { color: [0.4, 0.3, 0.2], surface: Surface::Bark };
        let leaf = Tone { surface: Surface::Broadleaf, ..bark };
        assert_eq!(volume.intern(bark), volume.intern(bark));
        // The same colour on two materials has to stay two palette entries, or
        // bark would be drawn with the leaf cutout.
        assert_ne!(volume.intern(bark), volume.intern(leaf));
    }

    #[test]
    fn strands_go_to_the_chunk_that_holds_them() {
        let mut strands = TreeMesh::default();
        let quad = |x: f32, y: f32| {
            [[x, y, 0.5], [x + 0.2, y, 0.5], [x + 0.2, y - 0.2, 0.5], [x, y - 0.2, 0.5]]
        };
        for corner in quad(4.0, 3.5).into_iter().chain(quad(40.0, 3.5)) {
            strands.positions.push(corner);
            strands.normals.push([0.0, 0.0, 1.0]);
            strands.colors.push([1.0, 1.0, 1.0, 1.0]);
            strands.uvs.push([0.0, 0.0]);
        }
        strands.indices.extend(0..12u32);

        let mut mesh = MeshData::default();
        hang(&strands, &test_chunk(0, 0, 3), &mut mesh);
        assert_eq!(mesh.triangle_count(), 2, "only the near strand belongs here");

        let mut far = MeshData::default();
        hang(&strands, &test_chunk(2, 0, 3), &mut far);
        assert_eq!(far.triangle_count(), 2, "the far strand belongs two blocks over");
    }
}
