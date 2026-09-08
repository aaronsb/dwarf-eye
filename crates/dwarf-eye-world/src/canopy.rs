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
//! Leaves are scattered as small clusters rather than solid blobs, so a third
//! or more of the crown's shell is air and the limbs show through it. Habit
//! comes from the plant raws: a species with no heavy branches is a conifer and
//! carries its foliage in tiers with the trunk showing between them, and
//! anything else spreads.
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
/// Six is the scale the reference art works at. Three is the intended far level
/// of detail. `DWARF_EYE_CANOPY_DETAIL` overrides it.
pub const DEFAULT_DETAIL: i32 = 6;

/// Voxels per leaf cluster edge. Two gives clusters of one to eight voxels,
/// which is the scattered look; the grid is world-aligned so clusters carry
/// across tile and chunk boundaries unbroken.
const CLUSTER: i32 = 2;

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
    /// The share of a tile's volume this part fills with leaves when the tile
    /// is fully exposed. Everything else scales this down.
    fn leaf_fill(self) -> f32 {
        match self {
            CanopyPart::Branch => 0.42,
            CanopyPart::Twig => 0.55,
            CanopyPart::Cap => 0.70,
        }
    }

    /// Whether this part carries a woody rod at all.
    ///
    /// Limbs must stay far slimmer than the leaves around them, so a rod is one
    /// voxel wide wherever it runs and only a limb meeting the trunk widens.
    /// DF's own BRANCH_RADIUS is how far a branch reaches rather than how thick
    /// it is, so thickness is set here. TODO: taper from MAX_TRUNK_DIAMETER once
    /// the L-system grows real limbs.
    fn woody(self) -> bool {
        matches!(self, CanopyPart::Branch | CanopyPart::Twig)
    }
}

/// How a species carries its crown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Habit {
    /// Limbs fork and rise, leaves everywhere the crown opens onto air.
    Spreading,
    /// Foliage in tiers around the trunk, with air and bare trunk between them,
    /// narrowing to a spike.
    Conifer,
}

/// Reads a species' habit out of its growth tokens.
///
/// DF gives pine, cedar and larch no heavy branches at all and the broadleaves
/// a quarter density of them, which separates the two habits exactly.
pub fn habit(growth: raws::TreeGrowth) -> Habit {
    if growth.heavy_branch_density == 0 && growth.branch_density > 0 {
        Habit::Conifer
    } else {
        Habit::Spreading
    }
}

/// Everything about how one species is drawn, resolved once per chunk.
struct Look {
    /// Leaf tones darkest first, with the last reserved for new growth at the
    /// tips.
    leaf: Vec<u8>,
    bark: Vec<u8>,
    habit: Habit,
    growth: raws::TreeGrowth,
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
    /// Voxels set so far, so the budget can weigh bark against leaves.
    filled: usize,
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
            filled: 0,
            palette: Vec::new(),
            keys: HashMap::new(),
            origin: chunk.origin(),
        }
    }

    fn index(&self, i: i32, j: i32, k: i32) -> usize {
        (((i + 1) * (self.ny + 2) + (j + 1)) * (self.nx + 2) + (k + 1)) as usize
    }

    fn set(&mut self, i: i32, j: i32, k: i32, shade: u8) {
        if i < -1 || j < -1 || k < -1 || i > self.nx || j > self.ny || k > self.nx {
            return;
        }
        let at = self.index(i, j, k);
        if self.cells[at] == 0 {
            self.filled += 1;
        }
        self.cells[at] = shade;
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

    /// The voxel centre nearest a point, so a rod runs down a line of voxels
    /// instead of falling between two.
    fn snap(&self, p: [f32; 3]) -> [f32; 3] {
        let d = self.detail as f32;
        let axis = |v: f32, origin: f32| -> f32 {
            origin + ((v - origin) * d - 0.5).round() / d + 0.5 / d
        };
        [
            axis(p[0], self.origin.0 as f32),
            axis(p[1] / Z_SCALE, self.origin.2 as f32) * Z_SCALE,
            axis(p[2], self.origin.1 as f32),
        ]
    }

    /// Fills the voxels within `radius` of the segment `a`..`b`.
    ///
    /// Endpoints snap to voxel centres, so a radius under one voxel draws a
    /// single line of voxels rather than a two-wide smear or nothing at all.
    fn rod(&mut self, a: [f32; 3], b: [f32; 3], radius: f32, shade: u8) {
        let (a, b) = (self.snap(a), self.snap(b));
        let r = radius.max(0.28 / self.detail as f32);
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
                        self.set(i, j, k, shade);
                    }
                }
            }
        }
    }
}

/// How the six faces are lit, so a voxel crown reads as a lit solid.
const FACE_SHADE: [f32; 6] = [0.70, 0.70, 0.50, 1.0, 0.82, 0.82];

/// What a chunk's crowns cost.
#[derive(Default, Debug, Clone, Copy)]
pub struct CanopyBudget {
    pub triangles: usize,
    /// What the same faces would have cost one quad each.
    pub unmerged: usize,
    pub leaf_voxels: usize,
    pub bark_voxels: usize,
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

    // Palettes and habit are per species, so they are resolved once and the
    // geometry then runs without touching the sprite sheets again.
    let mut volume = Volume::new(chunk, detail());
    let mut looks: HashMap<i32, Look> = HashMap::new();
    for part in &skeleton.parts {
        if looks.contains_key(&part.species) {
            continue;
        }
        let leaf: Vec<u8> = library
            .leaf_tones(part.species, 3)
            .into_iter()
            .map(|c| volume.shade(key(part.species, 0, &c), to_linear(c)))
            .collect();
        let bark: Vec<u8> = library
            .bark_tones(part.species, 2)
            .into_iter()
            .map(|c| volume.shade(key(part.species, 1, &c), to_linear(c)))
            .collect();
        let growth = library.growth(part.species);
        looks.insert(part.species, Look { leaf, bark, habit: habit(growth), growth });
    }

    for part in &skeleton.parts {
        let Some(look) = looks.get(&part.species) else { continue };
        limbs(&mut volume, world, library, part, look, budget);
        leaves(&mut volume, world, library, &skeleton, part, look, budget);
    }
    // A trunk standing among the crown gets leaves over it too, so its sawn-off
    // top does not show through the gaps, and a trunk meeting the ground gets a
    // flare of roots.
    for (pos, tree, species) in skeleton.crowned_trunks() {
        let Some(look) = looks.get(&species) else { continue };
        let part = Part { pos, kind: CanopyPart::Branch, links: 0, tree, species };
        leaves(&mut volume, world, library, &skeleton, &part, look, budget);
    }
    for (pos, tree, species) in skeleton.trunk_feet(world, library) {
        let Some(look) = looks.get(&species) else { continue };
        root_flare(&mut volume, pos, tree, look, budget);
    }
    emit(&volume, &mut mesh, budget);
    mesh
}

/// The centre of a tile, in render space.
fn tile_centre(pos: (i32, i32, i32)) -> [f32; 3] {
    [pos.0 as f32 + 0.5, (pos.2 as f32 + 0.5) * Z_SCALE, pos.1 as f32 + 0.5]
}

/// A palette key that keeps one species' leaves and wood apart.
fn key(species: i32, slot: u64, color: &[u8; 3]) -> u64 {
    ((species as u32 as u64) << 32)
        | (slot << 24)
        | ((color[0] as u64) << 16)
        | ((color[1] as u64) << 8)
        | color[2] as u64
}

/// How many crown tiles stand directly above one, up to four.
///
/// Read from the world rather than from the chunk's own gather, so the answer
/// does not depend on which chunk is asking.
fn cover(world: &World, library: &TileLibrary, pos: (i32, i32, i32)) -> i32 {
    let (x, y, z) = pos;
    (1..=4)
        .take_while(|n| {
            world
                .voxel(x, y, z + n)
                .is_some_and(|v| library.canopy_part(v.tile_id).is_some())
        })
        .count() as i32
}

/// Lays one tile's limbs into the volume.
///
/// Each tile draws its own half of every joint, out to the tile boundary, so
/// two joined tiles meet in the middle whichever chunk each of them lands in.
/// A limb runs one voxel wide everywhere except where it meets the trunk.
fn limbs(
    volume: &mut Volume,
    world: &World,
    library: &TileLibrary,
    part: &Part,
    look: &Look,
    budget: &mut CanopyBudget,
) {
    if !part.kind.woody() || look.bark.is_empty() {
        return;
    }
    let (x, y, z) = part.pos;
    let centre = tile_centre(part.pos);
    let before = volume.filled;

    // Two bark tones along the limb, so it does not read as one painted stick.
    let tone = |n: i32| -> u8 {
        let pick = hash01(part.tree.0 ^ x, part.tree.1 ^ y, z, 40 + n);
        look.bark[(pick * look.bark.len() as f32) as usize % look.bark.len()]
    };

    let d = volume.detail as f32;
    // One voxel along a limb, a little more where it lands on the trunk.
    let hairline = 0.3 / d;
    let thick = 0.95 / d;
    let mut drawn = false;
    for (n, bit) in [raws::NORTH, raws::SOUTH, raws::WEST, raws::EAST].into_iter().enumerate() {
        if part.links & bit == 0 {
            continue;
        }
        let (dx, dy) = step(bit);
        let end = [centre[0] + dx as f32 * 0.5, centre[1], centre[2] + dy as f32 * 0.5];
        volume.rod(centre, end, hairline, tone(n as i32));
        drawn = true;
    }

    // A limb standing on the trunk widens where it meets it; one that joins
    // nothing still needs a body.
    let on_trunk = world
        .voxel(x, y, z - 1)
        .is_some_and(|v| library.is_trunk(v.tile_id));
    if on_trunk || !drawn {
        let below = [centre[0], centre[1] - 0.5 * Z_SCALE, centre[2]];
        let width = if on_trunk { thick } else { hairline };
        volume.rod(centre, below, width, tone(9));
    }
    budget.bark_voxels += volume.filled - before;
}

/// Scatters one tile's leaves.
///
/// Clusters sit on a world-aligned grid, so they carry across tiles unbroken,
/// and the share of a tile they fill falls with how enclosed the tile is and
/// how much crown stands over it. That is what leaves the inside of a crown an
/// open frame of limbs, opens the underside, and keeps the leaf mass at the top
/// and the outside where it belongs.
#[allow(clippy::too_many_arguments)]
fn leaves(
    volume: &mut Volume,
    world: &World,
    library: &TileLibrary,
    skeleton: &Skeleton,
    part: &Part,
    look: &Look,
    budget: &mut CanopyBudget,
) {
    if look.leaf.is_empty() {
        return;
    }
    let (x, y, z) = part.pos;
    let openness = skeleton.openness(part.pos);
    if openness <= 0.0 {
        return;
    }

    // A conifer carries its foliage in tiers. Every other level of the tree is
    // left bare so the trunk shows between them.
    let tier = (z - part.tree.2).rem_euclid(2) == 0;
    if look.habit == Habit::Conifer && !tier {
        return;
    }

    let under = cover(world, library, part.pos);
    let depth = 1.0 - under as f32 / 4.0;
    // Each tree varies a little, so a stand does not repeat one silhouette.
    let vary = 0.85 + 0.3 * hash01(part.tree.0, part.tree.1, part.tree.2, 7);
    let density = if look.growth.branch_density == 0 {
        1.0
    } else {
        0.65 + 0.7 * (look.growth.branch_density as f32 / 100.0)
    };
    let fill = part.kind.leaf_fill() * (0.35 + 0.65 * openness) * (0.55 + 0.45 * depth)
        * density
        * vary;

    // New growth at the outer tips runs lighter than the leaves behind it.
    let tips = openness > 0.45 && under == 0;
    let d = volume.detail;
    let before = volume.filled;
    for a in 0..d {
        for b in 0..d {
            for c in 0..d {
                let (i, j, k) = ((x - volume.origin.0) * d + a,
                                 (z - volume.origin.2) * d + b,
                                 (y - volume.origin.1) * d + c);
                if i < -1 || j < -1 || k < -1 || i > volume.nx || j > volume.ny || k > volume.nx {
                    continue;
                }
                // Cluster on a world-aligned grid so the pattern is continuous.
                let (gi, gj, gk) = (x * d + a, z * d + b, y * d + c);
                let cluster = (
                    gi.div_euclid(CLUSTER),
                    gj.div_euclid(CLUSTER),
                    gk.div_euclid(CLUSTER),
                );
                // Ease off toward the tile's corners, so a crown does not read
                // as a stack of cubes.
                let off = |v: i32| (v as f32 + 0.5) / d as f32 - 0.5;
                let reach = (off(a) * off(a) + off(b) * off(b) + off(c) * off(c)).sqrt() / 0.87;
                let here = fill * (1.15 - 0.5 * reach);
                if hash01(cluster.0, cluster.1, cluster.2, 3) >= here {
                    continue;
                }
                let pick = hash01(cluster.0, cluster.1, cluster.2, 11);
                let last = look.leaf.len() - 1;
                let tone = if tips && pick > 0.66 {
                    look.leaf[last]
                } else {
                    look.leaf[(pick * last as f32) as usize % look.leaf.len()]
                };
                volume.set(i, j, k, tone);
            }
        }
    }
    budget.leaf_voxels += volume.filled - before;
}

/// A flare of roots where a trunk meets the ground.
fn root_flare(
    volume: &mut Volume,
    pos: (i32, i32, i32),
    tree: (i32, i32, i32),
    look: &Look,
    budget: &mut CanopyBudget,
) {
    if look.bark.is_empty() {
        return;
    }
    let centre = tile_centre(pos);
    let before = volume.filled;
    let foot = [centre[0], centre[1] - 0.5 * Z_SCALE + 0.5 / volume.detail as f32, centre[2]];
    for n in 0..5 {
        let angle = std::f32::consts::TAU
            * (n as f32 + hash01(tree.0, tree.1, tree.2, 20 + n)) / 5.0;
        let reach = 0.32 + 0.22 * hash01(tree.0, tree.1, pos.2, 30 + n);
        let end = [foot[0] + angle.cos() * reach, foot[1], foot[2] + angle.sin() * reach];
        let tone = look.bark[n as usize % look.bark.len()];
        volume.rod(foot, end, 0.6 / volume.detail as f32, tone);
    }
    budget.bark_voxels += volume.filled - before;
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

    /// A limb has to be one voxel wide and unbroken: thinner and it vanishes
    /// between the voxel centres, thicker and the bark outweighs the leaves.
    #[test]
    fn a_limb_is_one_voxel_wide_and_unbroken() {
        let mut volume = Volume::new(&chunk(), 6);
        let shade = volume.shade(1, [0.4, 0.3, 0.2]);
        volume.rod([8.0, 0.5, 8.0], [9.0, 0.5, 8.0], 0.3 / 6.0, shade);
        for i in 48..54 {
            let across: usize = (0..8)
                .map(|j| (0..8).filter(|k| volume.get(i, j, 44 + k) != 0).count())
                .sum();
            assert_eq!(across, 1, "voxel column {i} should hold exactly one limb voxel");
        }
    }

    #[test]
    fn a_root_flare_reaches_out_from_the_foot() {
        let mut volume = Volume::new(&chunk(), 6);
        let shade = volume.shade(1, [0.4, 0.3, 0.2]);
        let look = Look {
            leaf: Vec::new(),
            bark: vec![shade],
            habit: Habit::Spreading,
            growth: raws::TreeGrowth::default(),
        };
        let mut budget = CanopyBudget::default();
        root_flare(&mut volume, (8, 8, 0), (8, 8, 0), &look, &mut budget);
        assert!(budget.bark_voxels > 6, "five roots should cost several voxels");
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
