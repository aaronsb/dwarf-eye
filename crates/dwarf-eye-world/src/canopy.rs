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
//! them, clipped to the level that holds each quad so a strand ends at a
//! hidden level instead of hanging through it.
//!
//! Plants that stand in a single tile — shrubs, saplings, dead stems — are
//! grown here too, but on their own terms: one plant per tile, grown at the
//! preset's natural size and then fitted into the tile DF gave it, cached by
//! that tile so a re-mesh never regrows one. They are not sliced into the
//! chunk's voxel volume, because a tile's worth of plant at the volume's
//! resolution is a blob; they are meshed by the growth crate at its own
//! resolution and stamped in.
//!
//! Every chunk is built once per [`Band`]: the near band is all of the above,
//! the mid band the same trees at half the voxel resolution and still under the
//! leaf cutout, the far band a quarter of it and opaque. They hand over at the
//! projected sizes [`near_band`] and [`Band::edge`] say, and Bevy swaps between
//! them.

use crate::factory::{self, Class, Extent, Style, Treatment};
use crate::heightfield::{self, Ground};
use crate::library::TileLibrary;
use crate::mesh::{FLOOR_HEIGHT, MeshData, MeshOptions, Z_SCALE};
use crate::tree::{DETAIL, Envelope, Habit};
use crate::world::{BLOCK, Chunk, Voxel, World};
use dwarf_eye_trees as trees;
use dwarf_eye_trees::{Kind, TreeMesh};
use std::collections::HashMap;
use std::sync::Arc;

pub mod timing;

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
///
/// The cut plane (issue #8) reuses this same headroom: the chunk sitting at
/// `opts.z_ceiling` is the last one ever built while the plane sits there, so
/// it is treated as if it were the top of the loaded column and allowed the
/// same reach upward, which is what lets a whole tree clear the plane instead
/// of being shorn off at it.
const OVERHEAD: i32 = 24;

/// Whether the cut plane draws a tree whole rather than slicing its canopy
/// flat at the ceiling. `DWARF_EYE_CUT_TREES=slice` restores the old slicing;
/// anything else, including unset, is whole.
fn cut_trees_whole() -> bool {
    static WHOLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WHOLE.get_or_init(|| std::env::var("DWARF_EYE_CUT_TREES").ok().as_deref() != Some("slice"))
}

/// How the cut plane treats one tree, given the tile its trunk stands on and
/// the ceiling (`i32::MAX` when there is no cut plane).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CutTreatment {
    /// No cut plane, or `DWARF_EYE_CUT_TREES=slice`: chunks above the ceiling
    /// simply are never built, which flattens whatever crown reached them.
    Slice,
    /// The cut plane is active, whole-tree mode is on, and this tree's base is
    /// at or below the ceiling: it draws entire, in the chunk sitting at the
    /// ceiling, instead of being flattened there.
    Whole,
    /// The cut plane is active, whole-tree mode is on, and this tree's base is
    /// above the ceiling: a tree that has not sprouted yet, from here, so none
    /// of it is drawn.
    Hidden,
}

/// Decides [`CutTreatment`] for a tree rooted at `base_z`, under a cut plane
/// at `ceiling`. `whole` is [`cut_trees_whole`], threaded through rather than
/// read here so the decision stays a pure function to test.
fn cut_treatment(base_z: i32, ceiling: i32, whole: bool) -> CutTreatment {
    if ceiling == i32::MAX || !whole {
        return CutTreatment::Slice;
    }
    if base_z > ceiling { CutTreatment::Hidden } else { CutTreatment::Whole }
}

/// How many z-levels beyond its own a chunk's tree volume reaches: past the
/// top of the loaded column as before, and now also past the cut plane's
/// ceiling when whole-tree mode needs the same headroom there.
fn chunk_above(top_of_load: bool, chunk_z: i32, ceiling: i32, whole: bool) -> i32 {
    let top_of_cut = ceiling != i32::MAX && chunk_z == ceiling && whole;
    if top_of_load || top_of_cut { OVERHEAD } else { 0 }
}

/// Sub-voxels per tile the close band cuts a tree into: one step down from
/// [`DETAIL`], so no hand-off doubles the voxel size and the first one — the
/// one the eye is nearest to and most likely to catch — is the smallest.
pub const CLOSE_DETAIL: i32 = DETAIL - 1;

/// Sub-voxels per tile the mid band cuts a tree into: half of [`DETAIL`], so a
/// crown holds an eighth of the near band's cells and its leaves still wear the
/// cutout.
pub const MID_DETAIL: i32 = DETAIL / 2;

/// Sub-voxels per tile the far band cuts a tree into: a quarter of [`DETAIL`],
/// so a crown holds a sixty-fourth of the near band's cells and its merged
/// faces are whole tiles across.
pub const FAR_DETAIL: i32 = DETAIL / 4;

/// How finely one chunk's crowns are cut, and what rides along with them.
///
/// The near band is the full tree, its plants and its hanging strands. The
/// close and mid bands are the same tree at [`CLOSE_DETAIL`] and
/// [`MID_DETAIL`], without the ground cover but still under the leaf cutout,
/// so light keeps coming through a crown well past the first hand-off. The far
/// band is [`FAR_DETAIL`] and opaque: a tuft is under a pixel there and a hole
/// in the cutout costs a masked pass to draw air.
///
/// The steps are 4, 3, 2, 1 rather than 4, 2, 1: no hand-off doubles the voxel
/// size, and the first one, nearest the eye and the one the player is most
/// likely to be looking at, is the gentlest of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Band {
    Near,
    Close,
    Mid,
    Far,
}

/// The bands a chunk is built in, nearest first.
///
/// The list is the whole of the ordering: a coarser stage — a canonical crown
/// per species, a green box — is one more entry here, one more mesh per chunk
/// and one more handover distance, with nothing else to restructure.
pub const BANDS: [Band; 4] = [Band::Near, Band::Close, Band::Mid, Band::Far];

impl Band {
    pub fn detail(self) -> i32 {
        match self {
            Band::Near => DETAIL,
            Band::Close => CLOSE_DETAIL,
            Band::Mid => MID_DETAIL,
            Band::Far => FAR_DETAIL,
        }
    }

    /// What this band asks the rasteriser for: the near band's wood, however
    /// coarse its own cells.
    ///
    /// A limb thinner than a cell is drawn a cell wide, so an uncorrected
    /// coarse cut fattens every twig and its crown fills with bark the fine one
    /// does not show — over a crown of oak, 1.4% of the near band's pixels
    /// against 13% of the far band's, which is most of what makes a hand-off
    /// read as a jump. Every band therefore cuts its wood like [`DETAIL`].
    /// Pinned by `every_band_shows_the_near_band_s_wood`.
    pub fn cut(self) -> trees::Cut {
        trees::Cut { wood_like: DETAIL as u32 }
    }

    /// Where this band hands over to the next, in tiles, given where the near
    /// band ends.
    ///
    /// [`near_band`]'s rule applied to this band's own leaf voxel: a voxel
    /// `DETAIL / detail` times as wide still covers [`MIN_LEAF_PIXELS`] that
    /// many times further out. The last band hands over to nothing and its edge
    /// is never asked for.
    pub fn edge(self, near: f32) -> f32 {
        near * DETAIL as f32 / self.detail() as f32
    }

    /// Whether this band carries the ground cover: standing plants, tufts and
    /// the strands a weeping crown hangs.
    fn undergrowth(self) -> bool {
        self == Band::Near
    }

    /// Whether a weeping crown's strands come with this band. They are quads
    /// the growth crate has already meshed, so carrying them one band further
    /// out costs a copy rather than a rasterisation.
    fn strands(self) -> bool {
        self != Band::Far
    }

    /// Which of [`CanopyMeshes`]'s four meshes a surface goes into.
    ///
    /// The cutout bands split by material, since each wants its own cutout.
    /// The far band puts everything in the first, because one mesh on one
    /// material is one entity and one draw call per chunk, and a chunk that far
    /// off has no bark grain or leaf holes left to tell apart.
    fn slot(self, surface: Surface) -> usize {
        match self {
            Band::Far => 0,
            _ => surface.slot(),
        }
    }

    /// What each of [`CanopyMeshes`]'s four meshes is drawn with.
    ///
    /// Only the far band's leaves are opaque: a cutout costs a masked pass, a
    /// discard in the depth prepass and the overdraw behind every hole, which is
    /// worth paying while a hole is still a pixel wide and not after. Only the
    /// far band's first slot is ever filled.
    pub fn coats(self) -> [Coat; 4] {
        match self {
            Band::Near | Band::Close | Band::Mid => [
                Coat::Bark,
                Coat::Cutout(Surface::Broadleaf),
                Coat::Cutout(Surface::Needle),
                Coat::Strip,
            ],
            Band::Far => [Coat::Leaf; 4],
        }
    }
}

/// What a canopy mesh is drawn with: bark, an alpha-masked leaf cutout, the
/// leaflet strip a strand hangs from, or plain opaque foliage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Coat {
    Bark,
    Cutout(Surface),
    Strip,
    Leaf,
}

impl Coat {
    /// Whether this coat is alpha masked, so it costs a discard wherever it is
    /// drawn.
    pub fn masked(self) -> bool {
        matches!(self, Coat::Cutout(_) | Coat::Strip)
    }
}

/// Pixels a near-band leaf voxel must still cover for the near band to be worth
/// drawing. Below this the cutout is sampling noise, so the coarse crown reads
/// the same and costs a fraction.
const MIN_LEAF_PIXELS: f32 = 2.0;

/// Where the near band ends, in tiles, for a camera of this vertical field of
/// view drawing into a viewport this many pixels tall.
///
/// A near leaf voxel is one tile over [`DETAIL`] on a side. A length `L` at
/// distance `d` covers `L * h / (2 d tan(fov/2))` pixels of a viewport `h`
/// pixels tall, so the distance at which it falls to [`MIN_LEAF_PIXELS`] is
/// `L * h / (2 * MIN_LEAF_PIXELS * tan(fov/2))`. Projected size, not raw
/// distance: a taller window or a narrower lens pushes the band out.
pub fn near_band(fov_y: f32, viewport_height: f32) -> f32 {
    let leaf = 1.0 / DETAIL as f32;
    leaf * viewport_height / (2.0 * MIN_LEAF_PIXELS * (fov_y * 0.5).tan())
}

/// [`near_band`], with `DWARF_EYE_LOD_NEAR` overriding it in blocks.
pub fn near_band_override(fov_y: f32, viewport_height: f32) -> f32 {
    match std::env::var("DWARF_EYE_LOD_NEAR").ok().and_then(|v| v.parse::<f32>().ok()) {
        Some(blocks) => blocks * BLOCK as f32,
        None => near_band(fov_y, viewport_height),
    }
}

/// What the band cut at this resolution asks the rasteriser for, so the tree
/// lab shows a crown exactly as the band that draws it will.
pub fn cut_at(detail: i32) -> trees::Cut {
    BANDS
        .iter()
        .find(|b| b.detail() == detail)
        .map(|b| b.cut())
        .unwrap_or(trees::Cut::fine(detail.max(1) as u32))
}

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
    /// Sub-voxels per tile this copy was cut at, which is what its coordinates
    /// are in. A chunk may only absorb a tree cut at its own band's detail.
    detail: i32,
    /// Corner of the volume, in voxels: tile times `detail`.
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

/// Lays one rasterised tree's voxels out in render space.
///
/// The generator works in tiles with the trunk's base at its own origin, so all
/// that happens here is an offset onto the tile the trunk stands on, and the
/// colours it chose interned into a small palette. `band` says which detail the
/// tree was cut at and whether its strands come with it.
fn voxelise(
    grown: &trees::VoxelTree,
    env: &Envelope,
    habit: Habit,
    band: Band,
) -> TreeVoxels {
    let started = std::time::Instant::now();
    let detail = band.detail();
    let leaf_surface =
        if habit == Habit::Conifer { Surface::Needle } else { Surface::Broadleaf };

    // The generator's origin is the centre of the base tile, at its floor.
    let anchor = [
        env.base.0 * detail + detail / 2,
        env.base.2 * detail,
        env.base.1 * detail + detail / 2,
    ];
    let world = crate::tree::anchor(env);

    let Some((lo, hi)) = grown.bounds() else {
        timing::PHASES.voxelise.since(started);
        return TreeVoxels {
            detail,
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
        detail,
        gx: anchor[0] + lo.x,
        gy: anchor[1] + lo.y,
        gz: anchor[2] + lo.z,
        nx,
        ny,
        nz,
        cells: vec![0u8; (nx * ny * nz) as usize],
        tones: Vec::new(),
        streamers: if band.strands() { strands(grown, world) } else { TreeMesh::default() },
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
    timing::PHASES.voxelise.since(started);
    volume
}

/// The tree's weeping strands, meshed once and moved into render space.
fn strands(grown: &trees::VoxelTree, at: [f32; 3]) -> TreeMesh {
    let mut mesh =
        trees::mesh_of_texels(grown, Some(Kind::Streamer), trees::texture::live_texels());
    for p in &mut mesh.positions {
        p[0] += at[0];
        p[1] = p[1] * Z_SCALE + at[1];
        p[2] += at[2];
    }
    mesh
}

/// Sub-voxels per tile of a standing plant's own height, before it is fitted
/// into its tile.
///
/// A shrub is a bit over two tiles tall in the preset's own units, so at two it
/// lands at about the four voxels per tile a tree is cut into: a shrub is then
/// drawn at the same density as the crown above it, which is what keeps a
/// meadow of them affordable — they are small but there are thousands.
///
/// `DWARF_EYE_PLANT_VOXELS` overrides it. Three is visibly rounder close up and
/// costs about half a second more on the first mesh of a cached map, and half
/// again as many triangles.
const DEFAULT_PLANT_DETAIL: u32 = 2;

fn plant_detail() -> u32 {
    static DETAIL: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *DETAIL.get_or_init(|| {
        std::env::var("DWARF_EYE_PLANT_VOXELS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_PLANT_DETAIL)
            .clamp(1, 8)
    })
}

/// Plants kept before the cache is dropped and regrown.
///
/// Every plant is a pure function of its tile and its species, so forgetting
/// one costs time and never changes what is drawn. Without a bound the cache
/// would hold every plant ever walked past.
const PLANT_CACHE: usize = 40_000;

/// One plant standing in one tile, grown once and kept by the tile it stands
/// on.
///
/// Held in tile-local space — x and z span the tile, y rises from its floor —
/// so stamping it into a chunk is a translation. The texture repeats once per
/// world tile and the translation is whole tiles, so the surface lands at the
/// same texel phase wherever it is stamped.
pub struct Plant {
    bark: MeshData,
    leaf: MeshData,
    pub triangles: usize,
}

/// Grows one standing plant and fits it into its tile.
///
/// Dwarf Fortress gives a shrub or a sapling exactly one tile: this is the
/// whole of what it contributes. The shape inside that is the preset's, grown
/// at the size its grammar expects and then scaled down as a whole, because a
/// grammar asked for a plant one tile tall grows a stub rather than a small
/// plant.
fn sprout(
    library: &mut TileLibrary,
    class: Class,
    voxel: Voxel,
    at: (i32, i32, i32),
) -> Option<Plant> {
    let Treatment::Grown(kind, preset) = factory::resolve(class, Style::Grown) else {
        return None;
    };
    let mut params = *preset;
    params.kind = kind;
    params.height = factory::standing_height(class, &params);
    // A sapling is a tree that has not grown up yet, so its species has real
    // foliage art to read; a shrub's sheet is DF's one generic sprite, and the
    // colour DF paints its tile with is all that separates one from another.
    if class == Class::Sapling {
        let leaf: Vec<trees::Rgb> = library
            .leaf_tones(voxel.mat_index, 3)
            .into_iter()
            .map(|c| trees::Rgb(c[0], c[1], c[2]))
            .collect();
        if let Some(&tip) = leaf.last() {
            params.palette.tip = tip;
            params.palette.leaf = leaf;
        }
        let bark: Vec<trees::Rgb> = library
            .bark_tones(voxel.mat_index, 2)
            .into_iter()
            .map(|c| trees::Rgb(c[0], c[1], c[2]))
            .collect();
        if !bark.is_empty() {
            params.palette.bark = bark;
        }
    } else {
        factory::recolour(&mut params, class, voxel.color);
    }

    let seed = factory::seed(at.0, at.1, at.2, voxel.mat_index);
    let started = std::time::Instant::now();
    let grown = trees::rasterise(&trees::grow(&params, seed, None), plant_detail());
    let bark = trees::mesh_of(&grown, Some(Kind::Bark));
    let leaf = trees::mesh_of(&grown, Some(Kind::Leaf));
    timing::PHASES.plants.since(started);

    let (scale, floor, middle) = fitted(&[&bark, &leaf])?;
    let bark = tile_local(&bark, scale, floor, middle);
    let leaf = tile_local(&leaf, scale, floor, middle);
    let triangles = bark.triangle_count() + leaf.triangle_count();
    Some(Plant { bark, leaf, triangles })
}

/// How much to shrink a grown plant so it stands inside one tile, the height it
/// starts from and the middle of its footprint.
fn fitted(meshes: &[&TreeMesh]) -> Option<(f32, f32, [f32; 2])> {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for mesh in meshes {
        for p in &mesh.positions {
            for axis in 0..3 {
                lo[axis] = lo[axis].min(p[axis]);
                hi[axis] = hi[axis].max(p[axis]);
            }
        }
    }
    if lo[0] > hi[0] {
        return None;
    }
    let span = |axis: usize| (hi[axis] - lo[axis]).max(0.01);
    // Never enlarged: a plant DF gives one tile may be smaller than one, and a
    // tuft of grass blown up to a tile tall is a hedge.
    let scale = (1.0 / span(1)).min(1.0 / span(0).max(span(2))).min(1.0);
    Some((scale, lo[1], [(lo[0] + hi[0]) * 0.5, (lo[2] + hi[2]) * 0.5]))
}

/// Moves a grown plant into its tile: scaled, stood on the floor and centred.
///
/// The texture coordinates are scaled with the geometry, so a shrunk plant
/// still shows the same texel size as the ground it stands on.
fn tile_local(mesh: &TreeMesh, scale: f32, floor: f32, middle: [f32; 2]) -> MeshData {
    let mut out = MeshData::default();
    for (n, p) in mesh.positions.iter().enumerate() {
        out.positions.push([
            (p[0] - middle[0]) * scale + 0.5,
            (p[1] - floor) * scale * Z_SCALE,
            (p[2] - middle[1]) * scale + 0.5,
        ]);
        let normal = mesh.normals[n];
        out.normals.push(normal);
        let lit = face_shade(normal);
        let c = mesh.colors[n];
        out.colors.push([c[0] * lit, c[1] * lit, c[2] * lit, c[3]]);
        let uv = mesh.uvs[n];
        out.uvs.push([uv[0] * scale, uv[1] * scale]);
    }
    out.indices.extend_from_slice(&mesh.indices);
    out
}

/// How a face is lit, from the way it points: the same six shades a sliced
/// tree's faces get, so a shrub sits in the same light as the crown above it.
fn face_shade(normal: [f32; 3]) -> f32 {
    if normal[1] > 0.5 {
        FACE_SHADE[3]
    } else if normal[1] < -0.5 {
        FACE_SHADE[2]
    } else if normal[0].abs() > 0.5 {
        FACE_SHADE[0]
    } else {
        FACE_SHADE[4]
    }
}

/// A tree's origin tile and the detail its copy was cut at.
type TreeKey = ((i32, i32, i32), i32);

/// Trees that have been grown, kept so a chunk never regrows one.
///
/// Keyed by origin and by the detail the copy was cut at: the bands are the
/// same tree sampled once each, and all of them are wanted for as long as the
/// chunk is.
#[derive(Default)]
pub struct Forest {
    trees: HashMap<TreeKey, Option<Arc<TreeVoxels>>>,
    /// Standing plants, by the absolute tile each one stands on.
    plants: HashMap<(i32, i32, i32), Option<Arc<Plant>>>,
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

    fn at(&mut self, slot: usize) -> &mut MeshData {
        match slot {
            0 => &mut self.bark,
            1 => &mut self.broadleaf,
            2 => &mut self.needle,
            _ => &mut self.streamers,
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
    /// Standing plants grown into this chunk.
    pub plants: usize,
}

impl Forest {
    /// Forgets the trees near a set of chunks, so blocks that have just arrived
    /// can lengthen an envelope that was cut short.
    pub fn retire_near(&mut self, keys: &[(i32, i32, i32)]) {
        if keys.is_empty() {
            return;
        }
        self.trees.retain(|(origin, _), _| {
            !keys.iter().any(|&(bx, by, z)| {
                (origin.0.div_euclid(BLOCK) - bx).abs() <= 1
                    && (origin.1.div_euclid(BLOCK) - by).abs() <= 1
                    && (origin.2 - z).abs() <= 20
            })
        });
    }

    pub fn plant_count(&self) -> usize {
        self.plants.values().filter(|p| p.is_some()).count()
    }

    /// Trees grown, counted once each rather than once per band.
    pub fn tree_count(&self) -> usize {
        self.trees.iter().filter(|((_, d), t)| *d == DETAIL && t.is_some()).count()
    }

    /// Leaf and bark voxels across every tree grown at near detail, each counted
    /// once however many chunks it reaches.
    pub fn voxel_counts(&self) -> (usize, usize) {
        self.trees
            .iter()
            .filter(|((_, d), _)| *d == DETAIL)
            .filter_map(|(_, t)| t.as_ref())
            .fold((0, 0), |(l, b), t| (l + t.leaf_voxels, b + t.bark_voxels))
    }

    /// Builds one chunk's tree geometry for one band.
    pub fn build_chunk(
        &mut self,
        world: &World,
        chunk: &Chunk,
        opts: MeshOptions,
        library: &mut TileLibrary,
        world_origin: (i32, i32, i32),
        band: Band,
    ) -> CanopyMeshes {
        self.build_budgeted(
            world,
            chunk,
            opts,
            library,
            world_origin,
            band,
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
        band: Band,
        budget: &mut CanopyBudget,
    ) -> CanopyMeshes {
        let mut meshes = CanopyMeshes::default();
        if chunk.z > opts.z_ceiling {
            return meshes;
        }
        timing::PHASES.chunks.tick();
        let started = std::time::Instant::now();
        let mut origins = self.nearby(world, chunk, opts, library);
        origins.sort_unstable();
        origins.dedup();
        timing::PHASES.nearby.since(started);

        let whole = cut_trees_whole();

        // A tree that has not sprouted yet from here — its base above the cut
        // plane — never draws, whole-tree mode or not. No-op when there is no
        // cut plane or the plane is in slice mode: `cut_treatment` only ever
        // says `Hidden` under whole-tree mode with an active ceiling.
        origins
            .retain(|(origin, _)| cut_treatment(origin.2, opts.z_ceiling, whole) != CutTreatment::Hidden);

        // Only the top of a column carries what rises above it. The chunk
        // sitting at the cut plane's ceiling is, while the plane is there, the
        // last one that will ever be built above it, so a tree drawn whole
        // needs the same reach there as the true top of the loaded column
        // gets.
        let (cx, cy) = (chunk.block_x, chunk.block_y);
        let top_of_load = world.chunk(cx, cy, chunk.z + 1).is_none();
        let above = chunk_above(top_of_load, chunk.z, opts.z_ceiling, whole);

        if band.undergrowth() {
            // A plant stands on the ground, and under the smooth model the
            // ground is a sheet rather than the tile's own slab.
            let surface = Ground::current()
                .smooth()
                .then(|| heightfield::Surface::build(world, chunk, opts));
            self.sow(chunk, opts, library, world_origin, surface.as_ref(), &mut meshes, budget);
        }
        if origins.is_empty() {
            return meshes;
        }

        let mut volume = Volume::new(chunk, above, band);
        for (origin, species) in origins {
            let grown = self.tree(world, library, origin, species, world_origin, band);
            let Some(grown) = grown else { continue };
            budget.trees += 1;
            let started = std::time::Instant::now();
            volume.absorb(&grown);
            timing::PHASES.absorb.since(started);
            // What a strand may not hang through: built work and rock. Its own
            // tree is the exception, since a strand starts inside the crown.
            let solid = |x: i32, y: i32, z: i32| {
                world.voxel(x, y, z).is_some_and(|v| {
                    !library.of_tree(v.tile_id)
                        && (library.is_built(v.tile_id) || v.solid.occludes())
                })
            };
            hang(&grown.streamers, chunk, above, &solid, &mut meshes.streamers);
        }
        let started = std::time::Instant::now();
        emit(&volume, &mut meshes, budget);
        timing::PHASES.emit.since(started);
        meshes
    }

    /// Grows the plants standing in this chunk's own tiles.
    ///
    /// A standing plant never leaves its tile, so a chunk's plants are exactly
    /// the ones its own tiles hold: no halo to scan, and nothing to slice.
    #[allow(clippy::too_many_arguments)]
    fn sow(
        &mut self,
        chunk: &Chunk,
        opts: MeshOptions,
        library: &mut TileLibrary,
        world_origin: (i32, i32, i32),
        surface: Option<&heightfield::Surface>,
        meshes: &mut CanopyMeshes,
        budget: &mut CanopyBudget,
    ) {
        let style = Style::current();
        if style != Style::Grown {
            return;
        }
        let (ox, oy, oz) = chunk.origin();
        for ly in 0..BLOCK {
            for lx in 0..BLOCK {
                let voxel = chunk.get(lx, ly);
                if voxel.hidden && !opts.show_hidden {
                    continue;
                }
                let Some(plan) = library.plan(voxel.tile_id) else { continue };
                if plan.extent != Extent::Tile || !plan.grown(style) {
                    continue;
                }
                let at = (ox + lx, oy + ly, oz);
                let Some(plant) = self.plant(library, plan.class, voxel, at, world_origin) else {
                    continue;
                };
                // Grounding: a plant fills one tile, so its base drops to the
                // ground at that tile's centre and is never lifted off the
                // slab it stood on (`heightfield.rs:grounded`).
                let base = heightfield::grounded(
                    at.2 as f32 * Z_SCALE + FLOOR_HEIGHT,
                    surface.and_then(|s| s.height(lx, ly)),
                ) - FLOOR_HEIGHT;
                let offset = [at.0 as f32, base, at.1 as f32];
                meshes.bark.stamp(&plant.bark, offset, [1.0; 3]);
                meshes.broadleaf.stamp(&plant.leaf, offset, [1.0; 3]);
                budget.plants += 1;
                budget.triangles += plant.triangles;
            }
        }
    }

    /// One tile's plant, grown on first sight and kept.
    fn plant(
        &mut self,
        library: &mut TileLibrary,
        class: Class,
        voxel: Voxel,
        at: (i32, i32, i32),
        world_origin: (i32, i32, i32),
    ) -> Option<Arc<Plant>> {
        if let Some(found) = self.plants.get(&at) {
            return found.clone();
        }
        if self.plants.len() >= PLANT_CACHE {
            self.plants.clear();
        }
        // Absolute tiles, so a plant is the same plant wherever the render
        // origin sits.
        let absolute = (at.0 + world_origin.0, at.1 + world_origin.1, at.2 + world_origin.2);
        let built = sprout(library, class, voxel, absolute).map(Arc::new);
        self.plants.insert(at, built.clone());
        built
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
                    // The factory says which tiles belong to a tree; a plant
                    // standing in one tile is not one of them.
                    if !library.of_tree(v.tile_id) {
                        continue;
                    }
                    found.push((v.tree_origin(x, y, z), v.mat_index));
                }
            }
        }
        found
    }

    /// One tree at one band's detail, grown on first sight and kept.
    ///
    /// Every band is cut from a single growth: the skeleton is the expensive
    /// half and a coarser band is the same tree, only sampled coarsely, so
    /// rasterising three times costs a fraction of growing three times.
    fn tree(
        &mut self,
        world: &World,
        library: &mut TileLibrary,
        origin: (i32, i32, i32),
        species: i32,
        world_origin: (i32, i32, i32),
        band: Band,
    ) -> Option<Arc<TreeVoxels>> {
        if let Some(found) = self.trees.get(&(origin, band.detail())) {
            return found.clone();
        }
        let started = std::time::Instant::now();
        let read = Envelope::read(world, library, origin, species);
        timing::PHASES.envelope.since(started);
        let Some(env) = read else {
            for band in BANDS {
                self.trees.insert((origin, band.detail()), None);
            }
            return None;
        };

        let growth = library.growth(species);
        let habit = env.habit(growth);
        let skeleton = crate::tree::skeleton(&env, growth, habit, library, world_origin);
        let mut wanted = None;
        for cut in BANDS {
            let rastered = crate::tree::rasterise(&skeleton, cut.detail(), cut.cut());
            let grown = Arc::new(voxelise(&rastered, &env, habit, cut));
            if cut == band {
                wanted = Some(Arc::clone(&grown));
            }
            if cut == Band::Near && std::env::var("DWARF_EYE_TREE_LOG").is_ok() {
                let (bx, by) = (env.base.0.div_euclid(BLOCK), env.base.1.div_euclid(BLOCK));
                let mut loaded = env.z1;
                while world.chunk(bx, by, loaded + 1).is_some() {
                    loaded += 1;
                }
                eprintln!(
                    "tree {:?} species {species} envelope z {}..{} truncated {} loaded top {loaded} \
                     voxels y {}..{} leaf {} bark {} height {} cap {:.1}",
                    env.origin,
                    env.z0,
                    env.z1,
                    env.truncated,
                    grown.gy as f32 / DETAIL as f32,
                    (grown.gy + grown.ny) as f32 / DETAIL as f32,
                    grown.leaf_voxels,
                    grown.bark_voxels,
                    env.height(),
                    crate::tree::cap_radius(&env),
                );
            }
            self.trees.insert((origin, cut.detail()), Some(grown));
        }
        wanted
    }
}

/// Copies the strands that hang inside this chunk, cut at its ceiling, its
/// floor, and anything solid they meet.
///
/// A strand is a chain of quads, and where the chain crosses a level the quad
/// on the boundary is clipped rather than given to one side whole. That is
/// what makes a curtain end flush with a hidden level instead of hanging
/// through it, and it keeps neighbouring chunks from drawing the same quad
/// twice. `above` is the extra levels this chunk carries because nothing is
/// loaded over it, the same overhead the voxel volume gets.
///
/// A strand also stops at whatever it hangs into. A willow leaning over a
/// roof would otherwise trail its curtain straight through the building, which
/// reads as vegetation growing out of the masonry; wood and crown are the
/// exception, because a strand starts inside its own tree.
fn hang(
    strands: &TreeMesh,
    chunk: &Chunk,
    above: i32,
    solid: &dyn Fn(i32, i32, i32) -> bool,
    out: &mut MeshData,
) {
    if strands.indices.is_empty() {
        return;
    }
    let (ox, oy, oz) = chunk.origin();
    let floor = oz as f32 * Z_SCALE;
    let ceiling = (oz + 1 + above.max(0)) as f32 * Z_SCALE;
    let over = |p: [f32; 3]| {
        p[0] >= ox as f32
            && p[0] < (ox + BLOCK) as f32
            && p[2] >= oy as f32
            && p[2] < (oy + BLOCK) as f32
    };
    for (n, corners) in strands.positions.chunks_exact(4).enumerate() {
        // A quad hangs plumb: its first two corners are the top edge, the last
        // two the bottom, and it stands over one spot on the ground.
        let (top, bottom) = (corners[0][1], corners[3][1]);
        let mid = [(corners[0][0] + corners[2][0]) * 0.5, 0.0, (corners[0][2] + corners[2][2]) * 0.5];
        if !over(mid) || top <= bottom {
            continue;
        }
        let (kept_top, kept_bottom) = (top.min(ceiling), bottom.max(floor));
        if kept_top <= kept_bottom {
            continue;
        }
        let level = ((kept_top + kept_bottom) * 0.5 / Z_SCALE).floor() as i32;
        if solid(mid[0].floor() as i32, mid[2].floor() as i32, level) {
            continue;
        }
        let base = n * 4;
        // How far down the quad each cut falls, so the corner that moves takes
        // its texture with it.
        let along = |y: f32| (top - y) / (top - bottom);
        let cut = |from: usize, to: usize, t: f32| {
            let (a, b) = (corners[from], corners[to]);
            let (ua, ub) = (strands.uvs[base + from], strands.uvs[base + to]);
            (
                [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t],
                [ua[0] + (ub[0] - ua[0]) * t, ua[1] + (ub[1] - ua[1]) * t],
            )
        };
        let (high, low) = (along(kept_top), along(kept_bottom));
        // Corner 0 hangs to corner 3, corner 1 to corner 2.
        let (p0, uv0) = cut(0, 3, high);
        let (p1, uv1) = cut(1, 2, high);
        let (p2, uv2) = cut(1, 2, low);
        let (p3, uv3) = cut(0, 3, low);
        out.push_textured_quad(
            [p0, p1, p2, p3],
            strands.normals[base],
            strands.colors[base],
            [uv0, uv1, uv2, uv3],
        );
    }
}

/// One chunk's slice of whatever trees reach it, with a voxel of halo so faces
/// at its edge can be culled against what the neighbour holds.
struct Volume {
    band: Band,
    /// Sub-voxels per tile, the band's own detail.
    detail: i32,
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
    fn new(chunk: &Chunk, above: i32, band: Band) -> Self {
        let detail = band.detail();
        let (nx, ny) = (BLOCK * detail, detail * (1 + above.max(0)));
        Self {
            band,
            detail,
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
        debug_assert_eq!(tree.detail, self.detail, "a tree cut for another band");
        let (ox, oy, oz) = self.origin;
        let (bx, by, bz) = (ox * self.detail, oz * self.detail, oy * self.detail);
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
                        out.at(volume.band.slot(tone.surface)),
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
        Chunk { block_x, block_y, z, ..Default::default() }
    }

    /// One tree's worth of bark voxels, `height` sub-voxels tall, one wide,
    /// its base at global voxel y `base_y` (one sub-voxel a tile, so this is
    /// also its base z).
    fn straight_trunk(base_y: i32, height: i32) -> TreeVoxels {
        TreeVoxels {
            detail: 1,
            gx: 0,
            gy: base_y,
            gz: 0,
            nx: 1,
            ny: height,
            nz: 1,
            cells: vec![1u8; height as usize],
            tones: vec![Tone { color: [0.5, 0.3, 0.1], surface: Surface::Bark }],
            streamers: TreeMesh::default(),
            leaf_voxels: 0,
            bark_voxels: height as usize,
        }
    }

    fn count_filled(volume: &Volume) -> usize {
        let mut n = 0;
        for i in 0..volume.nx {
            for j in 0..volume.ny {
                for k in 0..volume.nx {
                    if volume.get(i, j, k) != 0 {
                        n += 1;
                    }
                }
            }
        }
        n
    }

    #[test]
    fn a_tree_rooted_at_or_below_the_ceiling_draws_whole() {
        assert_eq!(cut_treatment(5, 10, true), CutTreatment::Whole);
        assert_eq!(cut_treatment(10, 10, true), CutTreatment::Whole, "at the ceiling still counts");
    }

    #[test]
    fn a_tree_rooted_above_the_ceiling_is_hidden_entirely() {
        assert_eq!(cut_treatment(11, 10, true), CutTreatment::Hidden);
    }

    #[test]
    fn slice_mode_is_unchanged() {
        // Slice mode never exempts and never hides by itself: a tree above the
        // ceiling is kept off-screen the old way, by its chunk never being
        // built at all, not by this decision.
        assert_eq!(cut_treatment(5, 10, false), CutTreatment::Slice);
        assert_eq!(cut_treatment(15, 10, false), CutTreatment::Slice);
        assert_eq!(chunk_above(false, 10, 10, false), 0, "slice mode reaches only its own chunk");
        // The true top of the loaded column is untouched by either mode.
        assert_eq!(chunk_above(true, 10, 10, false), OVERHEAD);
        assert_eq!(chunk_above(true, 10, 10, true), OVERHEAD);
    }

    #[test]
    fn no_cut_plane_never_hides_or_reaches_further() {
        assert_eq!(cut_treatment(50, i32::MAX, true), CutTreatment::Slice);
        assert_eq!(chunk_above(false, 10, i32::MAX, true), 0);
    }

    #[test]
    fn a_tree_rooted_below_the_ceiling_keeps_all_its_voxels() {
        // A trunk ten sub-voxels tall, based right at the ceiling: whole-tree
        // mode has to reach all the way up it from the chunk sitting there.
        let ceiling = 0;
        let tree = straight_trunk(ceiling, 10);
        let chunk = test_chunk(0, 0, ceiling);
        let above = chunk_above(false, chunk.z, ceiling, true);
        assert_eq!(above, OVERHEAD, "the ceiling chunk did not get the extra reach");

        let mut volume = Volume::new(&chunk, above, Band::Near);
        volume.absorb(&tree);
        assert_eq!(
            count_filled(&volume),
            tree.bark_voxels + tree.leaf_voxels,
            "whole-tree mode lost voxels that a full-height volume should hold"
        );
    }

    #[test]
    fn a_tree_rooted_above_the_ceiling_emits_no_voxels() {
        // Its base is one level above the ceiling: `Hidden` is what makes
        // `build_budgeted` drop it from `origins` before it is ever grown or
        // absorbed. That filtering is load-bearing — the ceiling chunk's own
        // headroom reaches OVERHEAD levels past the ceiling for the trees that
        // do qualify, comfortably far enough to have swallowed this one too
        // had it not been filtered out first.
        let ceiling = 0;
        let base_z = 1;
        assert_eq!(cut_treatment(base_z, ceiling, true), CutTreatment::Hidden);

        let tree = straight_trunk(base_z, 10);
        let chunk = test_chunk(0, 0, ceiling);
        let above = chunk_above(false, chunk.z, ceiling, true);
        let mut volume = Volume::new(&chunk, above, Band::Near);
        volume.absorb(&tree);
        assert!(
            count_filled(&volume) > 0,
            "the volume's own headroom would have drawn this tree anyway; \
             only the origin filter keeps a tree rooted above the ceiling off-screen"
        );
    }

    /// Open air everywhere: nothing for a strand to hang into.
    fn open(_: i32, _: i32, _: i32) -> bool {
        false
    }

    /// One strand quad, hanging from `top` down to `bottom` over one tile.
    fn strand(x: f32, top: f32, bottom: f32) -> TreeMesh {
        let mut strands = TreeMesh::default();
        let quad = [
            [x, top, 0.5],
            [x + 0.2, top, 0.5],
            [x + 0.2, bottom, 0.5],
            [x, bottom, 0.5],
        ];
        for (n, corner) in quad.into_iter().enumerate() {
            strands.positions.push(corner);
            strands.normals.push([0.0, 0.0, 1.0]);
            strands.colors.push([1.0, 1.0, 1.0, 1.0]);
            strands.uvs.push([0.0, if n < 2 { 0.0 } else { 1.0 }]);
        }
        strands.indices.extend(0..6u32);
        strands
    }

    #[test]
    fn the_near_band_ends_where_a_leaf_voxel_is_two_pixels() {
        // Bevy's default lens, into a 1080-tall window: a quarter-tile leaf
        // voxel covers two pixels at this distance and less beyond it.
        let (fov, height) = (std::f32::consts::FRAC_PI_4, 1080.0);
        let n = near_band(fov, height);
        let pixels = |d: f32| (1.0 / DETAIL as f32) * height / (2.0 * d * (fov * 0.5).tan());
        assert!((pixels(n) - MIN_LEAF_PIXELS).abs() < 1e-3, "{n} tiles is not the two-pixel range");
        assert!(pixels(n * 2.0) < MIN_LEAF_PIXELS);

        // A taller window resolves the same voxel further out, in proportion;
        // a wider lens pulls the band in.
        assert!((near_band(fov, height * 2.0) - n * 2.0).abs() < 1e-2);
        assert!(near_band(std::f32::consts::FRAC_PI_2, height) < n);
    }

    #[test]
    fn each_band_is_coarser_than_the_one_before_it() {
        assert_eq!(BANDS.len(), 4);
        assert_eq!(BANDS[0], Band::Near);
        assert_eq!(Band::Near.detail(), DETAIL);
        assert_eq!(Band::Far.detail(), 1, "the last band is one voxel a tile");
        let details: Vec<i32> = BANDS.iter().map(|b| b.detail()).collect();
        assert!(details.windows(2).all(|w| w[0] > w[1]), "{details:?} is not coarsening");
        // One step at a time: a hand-off that doubles the voxel is the jump the
        // bands exist to avoid.
        assert!(
            details.windows(2).all(|w| w[0] - w[1] == 1),
            "{details:?} skips a resolution"
        );
        // Ground cover is the near band's alone; strands ride to the last
        // cutout band, since they are already meshed.
        for band in [Band::Close, Band::Mid] {
            assert!(!band.undergrowth(), "{band:?} kept plants and tufts");
            assert!(band.strands(), "{band:?} dropped the strands it is handed");
        }
        assert!(Band::Near.undergrowth());
        assert!(!Band::Far.strands());
    }

    /// The wood, not the leaves, is what a coarse cut gets wrong: a limb
    /// thinner than a cell is drawn a cell wide, so bark grows with the cell
    /// unless the band asks for the near band's limbs.
    #[test]
    fn every_band_shows_the_near_band_s_wood() {
        for band in BANDS {
            assert_eq!(band.cut().wood_like, DETAIL as u32, "{band:?} fattens its twigs");
            assert_eq!(cut_at(band.detail()), band.cut());
        }
        // A resolution no band draws is nobody's stand-in, so it is itself.
        assert_eq!(cut_at(7), trees::Cut::fine(7));

        // What that is worth: the coarse cut keeps a fraction of the twigs the
        // fine one has, in proportion to how much wider it would draw them.
        let skeleton = trees::grow(&trees::oak(), 11, None);
        let bark = |band: Band| {
            trees::rasterise_cut(&skeleton, band.detail() as u32, band.cut()).counts().bark
        };
        let plain = trees::rasterise(&skeleton, FAR_DETAIL as u32).counts().bark;
        assert!(bark(Band::Far) * 3 < plain * 2, "{} against {plain} uncorrected", bark(Band::Far));
    }

    #[test]
    fn only_the_last_voxel_band_wears_no_cutout() {
        for band in [Band::Near, Band::Close, Band::Mid] {
            assert_eq!(
                band.coats().iter().filter(|c| c.masked()).count(),
                3,
                "{band:?} lost the cutout that lets light through a crown"
            );
            for (n, surface) in [Surface::Bark, Surface::Broadleaf, Surface::Needle]
                .into_iter()
                .enumerate()
            {
                assert_eq!(band.slot(surface), n, "{band:?} merged two materials");
            }
        }
        assert!(
            !Band::Far.coats().iter().any(|c| c.masked()),
            "a far mesh asked for a masked material"
        );
        // One mesh, so one entity and one draw call for a chunk's whole crown.
        for surface in [Surface::Bark, Surface::Broadleaf, Surface::Needle] {
            assert_eq!(Band::Far.slot(surface), 0);
        }
    }

    #[test]
    fn the_bands_hand_over_in_order() {
        // Each band ends where its own leaf voxel falls to two pixels, so a
        // coarser cut reaches further: near, then a third further, then twice,
        // then four times.
        let near = near_band(std::f32::consts::FRAC_PI_4, 720.0);
        assert!((Band::Near.edge(near) - near).abs() < 1e-3);
        assert!((Band::Close.edge(near) - near * 4.0 / 3.0).abs() < 1e-3);
        assert!((Band::Mid.edge(near) - near * 2.0).abs() < 1e-3);
        assert!((Band::Far.edge(near) - near * 4.0).abs() < 1e-3);
        let edges: Vec<f32> = BANDS.iter().map(|b| b.edge(near)).collect();
        assert!(edges.windows(2).all(|w| w[0] < w[1]), "{edges:?} is not nearest first");
    }

    #[test]
    fn a_coarser_cut_holds_a_fraction_of_the_voxels() {
        let skeleton = trees::grow(&trees::oak(), 11, None);
        let near = trees::rasterise(&skeleton, DETAIL as u32).counts();
        let close = trees::rasterise(&skeleton, CLOSE_DETAIL as u32).counts();
        let mid = trees::rasterise(&skeleton, MID_DETAIL as u32).counts();
        let far = trees::rasterise(&skeleton, FAR_DETAIL as u32).counts();
        assert!(far.leaf > 0, "the coarse cut lost the crown");
        assert!(close.leaf < near.leaf, "near {} close {}", near.leaf, close.leaf);
        assert!(mid.leaf * 2 < near.leaf, "near {} mid {}", near.leaf, mid.leaf);
        assert!(far.leaf * 8 < near.leaf, "near {} far {}", near.leaf, far.leaf);
    }

    #[test]
    fn a_tree_is_cached_once_per_band() {
        // The cache key carries the detail, so the coarse copy of a tree never
        // stands in for the fine one at the origin they share.
        let mut forest = Forest::default();
        let origin = (4, 5, 6);
        for band in BANDS {
            forest.trees.insert((origin, band.detail()), None);
        }
        assert_eq!(forest.trees.len(), BANDS.len(), "two bands shared a cache key");
        forest.retire_near(&[(0, 0, 6)]);
        assert!(forest.trees.is_empty(), "retiring a tree has to take every cut of it");
    }

    #[test]
    fn a_shade_is_interned_once_per_surface() {
        let mut volume = Volume::new(&test_chunk(0, 0, 0), 0, Band::Near);
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
        hang(&strands, &test_chunk(0, 0, 3), 0, &open, &mut mesh);
        assert_eq!(mesh.triangle_count(), 2, "only the near strand belongs here");

        let mut far = MeshData::default();
        hang(&strands, &test_chunk(2, 0, 3), 0, &open, &mut far);
        assert_eq!(far.triangle_count(), 2, "the far strand belongs two blocks over");
    }

    #[test]
    fn a_strand_is_cut_where_a_level_ends() {
        // One quad hanging from 4.3 down to 3.7, across the floor of level 4.
        let mut strands = TreeMesh::default();
        let quad = [[4.0, 4.3, 0.5], [4.2, 4.3, 0.5], [4.2, 3.7, 0.5], [4.0, 3.7, 0.5]];
        for (n, corner) in quad.into_iter().enumerate() {
            strands.positions.push(corner);
            strands.normals.push([0.0, 0.0, 1.0]);
            strands.colors.push([1.0, 1.0, 1.0, 1.0]);
            strands.uvs.push([0.0, if n < 2 { 0.0 } else { 1.0 }]);
        }
        strands.indices.extend(0..6u32);

        let top = |mesh: &MeshData| {
            mesh.positions.iter().fold(f32::MIN, |t, p| t.max(p[1]))
        };
        let bottom = |mesh: &MeshData| {
            mesh.positions.iter().fold(f32::MAX, |t, p| t.min(p[1]))
        };

        let mut lower = MeshData::default();
        hang(&strands, &test_chunk(0, 0, 3), 0, &open, &mut lower);
        assert_eq!(lower.triangle_count(), 2);
        assert!((top(&lower) - 4.0).abs() < 1e-5, "cut at the level's ceiling");
        assert!((bottom(&lower) - 3.7).abs() < 1e-5, "and hangs to its own tip");

        let mut upper = MeshData::default();
        hang(&strands, &test_chunk(0, 0, 4), 0, &open, &mut upper);
        assert_eq!(upper.triangle_count(), 2);
        assert!((bottom(&upper) - 4.0).abs() < 1e-5, "the rest starts at that cut");
        // The texture goes with the cut, so neither half repeats the other's
        // strand: half the quad's v range on each side.
        let v = |mesh: &MeshData| mesh.uvs.iter().fold(f32::MIN, |t, uv| t.max(uv[1]));
        assert!((v(&upper) - 0.5).abs() < 1e-5, "the upper half keeps the upper texture");
    }

    #[test]
    fn a_strand_stops_at_built_work() {
        // A willow leaning over a roof: the strand over the built tile is
        // dropped, the one beside it still hangs.
        let roof = |x: i32, _y: i32, _z: i32| x == 4;
        let mut into_roof = MeshData::default();
        hang(&strand(4.0, 3.9, 3.4), &test_chunk(0, 0, 3), 0, &roof, &mut into_roof);
        assert_eq!(into_roof.triangle_count(), 0, "a strand hung through the roof");

        let mut beside = MeshData::default();
        hang(&strand(5.0, 3.9, 3.4), &test_chunk(0, 0, 3), 0, &roof, &mut beside);
        assert_eq!(beside.triangle_count(), 2, "the strand beside it was cut too");
    }

    #[test]
    fn a_grown_plant_stands_inside_its_own_tile() {
        // The whole of DF's contribution to a standing plant is the one tile it
        // stands in, so nothing may leave it however the preset grew.
        for preset in [trees::Preset::Shrub, trees::Preset::Sapling, trees::Preset::TallGrass] {
            let params = trees::TreeParams::preset(preset);
            for seed in 0..8u64 {
                let grown = trees::rasterise(&trees::grow(&params, seed, None), plant_detail());
                let bark = trees::mesh_of(&grown, Some(Kind::Bark));
                let leaf = trees::mesh_of(&grown, Some(Kind::Leaf));
                let (scale, floor, middle) = fitted(&[&bark, &leaf]).expect("grew nothing");
                for mesh in [&bark, &leaf] {
                    for p in tile_local(mesh, scale, floor, middle).positions {
                        assert!(
                            (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[2]),
                            "{preset:?} seed {seed} leaned into the next tile at {p:?}"
                        );
                        assert!(
                            (0.0..=Z_SCALE).contains(&p[1]),
                            "{preset:?} seed {seed} left its level at {p:?}"
                        );
                    }
                }
            }
        }
    }
}
