//! Turning a skeleton into voxels: wood along the segments, foliage as ragged
//! clusters that thin out toward the crown's core and underside.
//!
//! Foliage is offered to the volume far more often than it is kept: a crown of
//! a few thousand clusters proposes millions of candidate cells and settles
//! tens of thousands of them. So the volume being written is a dense box, where
//! "is this cell taken?" is one array index, and a candidate is rejected on a
//! squared distance before it is asked to hash anything.

use std::collections::BTreeMap;

use crate::grow::{Skeleton, Streamer, shell_distance};
use crate::math::{IVec3, Vec3, ivec3};
use crate::params::Rgb;
use crate::rng::{hash3, hash_unit};

/// Voxels per patch of one shade. Bigger than one, so a crown reads as blocks
/// of colour and coplanar faces can merge.
const SHADE_PATCH: i32 = 2;

/// The most a cluster's wobble can widen it. `hash_unit` stays under one, so no
/// leaf is ever kept further out than this multiple of its cluster's radius.
const WOBBLE_MAX: f32 = 0.72 + 0.55;

/// The thinnest a limb can be drawn, in voxels: past the trunk a limb is a
/// thread, and a thread still has to fill the cell it is in.
const THREAD: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Bark,
    Leaf,
    /// A hanging strand. Never the kind of a [`Voxel`]: streamers are quads,
    /// and the kind exists so `mesh_of` can hand them to the leaf material.
    Streamer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Voxel {
    pub kind: Kind,
    pub color: Rgb,
}

#[derive(Clone, Debug)]
pub struct VoxelTree {
    pub voxels: BTreeMap<IVec3, Voxel>,
    /// Carried through from the skeleton with their colours resolved; drawn as
    /// quads by the mesher, not as voxels.
    pub streamers: Vec<Streamer>,
    pub voxels_per_tile: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VoxelCounts {
    pub bark: usize,
    pub leaf: usize,
}

impl VoxelTree {
    pub fn counts(&self) -> VoxelCounts {
        let mut counts = VoxelCounts::default();
        for v in self.voxels.values() {
            match v.kind {
                Kind::Bark => counts.bark += 1,
                // Streamers are never voxels, so this only ever sees leaves.
                Kind::Leaf | Kind::Streamer => counts.leaf += 1,
            }
        }
        counts
    }

    /// Inclusive min and max over the occupied voxels.
    pub fn bounds(&self) -> Option<(IVec3, IVec3)> {
        let mut it = self.voxels.keys();
        let first = *it.next()?;
        let mut lo = first;
        let mut hi = first;
        for k in self.voxels.keys() {
            lo.x = lo.x.min(k.x);
            lo.y = lo.y.min(k.y);
            lo.z = lo.z.min(k.z);
            hi.x = hi.x.max(k.x);
            hi.y = hi.y.max(k.y);
            hi.z = hi.z.max(k.z);
        }
        Some((lo, hi))
    }
}

/// The box a skeleton writes into, laid out so a linear walk of it visits
/// coordinates in [`IVec3`]'s own order: y, then z, then x. That is what lets
/// the finished voxels be handed to a `BTreeMap` already sorted.
struct Grid {
    lo: IVec3,
    /// Cells along each axis.
    span: IVec3,
    cells: Vec<Option<Voxel>>,
}

impl Grid {
    fn new(lo: IVec3, hi: IVec3) -> Self {
        let span = ivec3(hi.x - lo.x + 1, hi.y - lo.y + 1, hi.z - lo.z + 1);
        let n = span.x as usize * span.y as usize * span.z as usize;
        Grid { lo, span, cells: vec![None; n] }
    }

    fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let (i, j, k) = (x - self.lo.x, y - self.lo.y, z - self.lo.z);
        if i < 0 || j < 0 || k < 0 || i >= self.span.x || j >= self.span.y || k >= self.span.z {
            return None;
        }
        Some(((j * self.span.z + k) * self.span.x + i) as usize)
    }

    fn taken(&self, x: i32, y: i32, z: i32) -> bool {
        self.index(x, y, z).is_some_and(|at| self.cells[at].is_some())
    }

    /// Writes over whatever is there. Wood does that: a later limb crossing an
    /// earlier one takes the cell.
    fn set(&mut self, x: i32, y: i32, z: i32, voxel: Voxel) {
        if let Some(at) = self.index(x, y, z) {
            self.cells[at] = Some(voxel);
        }
    }

    /// Every occupied cell with its coordinate, in ascending [`IVec3`] order.
    fn drain(self) -> Vec<(IVec3, Voxel)> {
        let mut out = Vec::new();
        let mut at = 0usize;
        for j in 0..self.span.y {
            for k in 0..self.span.z {
                for i in 0..self.span.x {
                    if let Some(voxel) = self.cells[at] {
                        out.push((ivec3(self.lo.x + i, self.lo.y + j, self.lo.z + k), voxel));
                    }
                    at += 1;
                }
            }
        }
        out
    }
}

/// The box every ball and every cluster of a skeleton can reach, in voxels.
///
/// Two voxels of slack on each side covers the rounding the fill loops do: they
/// centre on a rounded coordinate and reach a ceiled radius from it.
fn extent(skeleton: &Skeleton, scale: f32) -> Option<(IVec3, IVec3)> {
    const SLACK: f32 = 2.0;
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    let mut any = false;
    let mut widen = |p: Vec3, r: f32| {
        any = true;
        lo.x = lo.x.min(p.x - r);
        lo.y = lo.y.min(p.y - r);
        lo.z = lo.z.min(p.z - r);
        hi.x = hi.x.max(p.x + r);
        hi.y = hi.y.max(p.y + r);
        hi.z = hi.z.max(p.z + r);
    };

    for segment in &skeleton.segments {
        // The widest ball the sweep draws: the radius is interpolated and then
        // clamped, and both are monotone, so the end radii bound it.
        let wide = segment.radius_a.max(segment.radius_b) * scale;
        let r = if segment.depth == 0 { wide.max(0.55) } else { wide.clamp(0.5, 1.05) };
        widen(segment.a * scale, r + SLACK);
        widen(segment.b * scale, r + SLACK);
    }
    for cluster in &skeleton.leaves {
        let r = (cluster.radius * scale).max(0.45);
        widen(cluster.center * scale, r + 1.0 + SLACK);
    }
    if !any {
        return None;
    }
    Some((
        ivec3(lo.x.floor() as i32, lo.y.floor() as i32, lo.z.floor() as i32),
        ivec3(hi.x.ceil() as i32, hi.y.ceil() as i32, hi.z.ceil() as i32),
    ))
}

/// What a cut owes a finer one, so the two read alike.
///
/// A voxel cannot be thinner than itself: a limb narrower than the cut's own
/// cell is still drawn a whole cell wide. That is what makes a coarse crown
/// darker and denser than the fine crown it stands in for — the wood in it
/// grows with the cell — and it is what a detail hand-off shows as a jump.
/// Foliage needs no such correction: a crown is cells deep, so whatever hole a
/// thinner fill opens the layer behind it closes, and thinning the leaves moves
/// a crown's mean colour by under a percent. Measured in the tree lab;
/// `docs/architecture/lod/README.md` carries the figures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cut {
    /// The resolution whose wood this cut should show, in voxels per tile.
    ///
    /// A limb thinner than a voxel is kept only as often as its true width
    /// asks for, measured against what a cut at this resolution would draw, so
    /// the bark showing through a crown stays where the finer cut put it. Equal
    /// to the cut's own resolution, nothing is dropped and the cut is exactly
    /// what it always was.
    pub wood_like: u32,
}

impl Cut {
    /// The cut that changes nothing: every limb the skeleton has, at this
    /// resolution.
    pub fn fine(voxels_per_tile: u32) -> Self {
        Cut { wood_like: voxels_per_tile }
    }
}

/// Voxelise a skeleton at `voxels_per_tile` resolution.
///
/// Wood is drawn first and foliage never overwrites it, so limbs stay readable
/// through the canopy.
pub fn rasterise(skeleton: &Skeleton, voxels_per_tile: u32) -> VoxelTree {
    rasterise_cut(skeleton, voxels_per_tile, Cut::fine(voxels_per_tile))
}

/// [`rasterise`], compensated for how coarse the cut is (see [`Cut`]).
pub fn rasterise_cut(skeleton: &Skeleton, voxels_per_tile: u32, cut: Cut) -> VoxelTree {
    let scale = voxels_per_tile.max(1) as f32;
    let reference = cut.wood_like.max(1) as f32;
    let palette = &skeleton.params.palette;
    let Some((lo, hi)) = extent(skeleton, scale) else {
        return VoxelTree {
            voxels: BTreeMap::new(),
            streamers: streamers(skeleton, scale),
            voxels_per_tile: voxels_per_tile.max(1),
        };
    };
    let mut grid = Grid::new(lo, hi);

    for segment in &skeleton.segments {
        // How much wider than the truth this cut draws the limb, over how much
        // wider the reference cut draws it: one where the cut is the reference
        // or the limb is thicker than a cell either way, and below one where a
        // coarse cell has fattened a twig the reference kept thin. Keeping the
        // twig that often leaves the same bark showing through the crown. The
        // draw is on the limb's place in the world, not on the cut, so a twig
        // one cut drops is dropped by every coarser one as well.
        if segment.depth > 0 && scale < reference {
            let widest = segment.radius_a.max(segment.radius_b);
            let fine = (widest * reference / THREAD).min(1.0);
            let keep = if fine > 0.0 { (widest * scale / THREAD).min(1.0) / fine } else { 1.0 };
            let at = segment.a * 64.0;
            if keep < 1.0 && hash_unit(at.x as i32, at.y as i32, at.z as i32, 0x7BC1) > keep {
                continue;
            }
        }
        let a = segment.a * scale;
        let b = segment.b * scale;
        let span = (b - a).length();
        let steps = (span / 0.4).ceil().max(1.0) as i32;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let p = a.lerp(b, t);
            let mut r = (segment.radius_a + (segment.radius_b - segment.radius_a) * t) * scale;
            // Past the trunk a limb is a thread: one or two voxels, no more.
            r = if segment.depth == 0 { r.max(0.55) } else { r.clamp(0.5, 1.05) };
            fill_ball(&mut grid, p, r, |v| Voxel {
                kind: Kind::Bark,
                color: pick(
                    &palette.bark,
                    ivec3(
                        v.x.div_euclid(SHADE_PATCH),
                        v.y.div_euclid(SHADE_PATCH),
                        v.z.div_euclid(SHADE_PATCH),
                    ),
                    0x8A17,
                ),
            });
        }
    }

    let crown = skeleton.crown_center * scale;
    let crown_radius = (skeleton.crown_radius * scale).max(1.0);
    let stretch = skeleton.params.crown_stretch;
    let hollow = skeleton.params.crown_hollow.clamp(0.0, 1.0);
    let density = skeleton.params.leaf_density;
    // The crown ends in a dome: over the top fifth of it the leaves thin to
    // tufts, so no tree finishes in a flat slab of foliage.
    let apex = skeleton.crown_top * scale;
    let dome = (skeleton.dome() * scale).max(1.0);

    for cluster in &skeleton.leaves {
        let center = cluster.center * scale;
        // A blade of grass is allowed to be a single voxel wide.
        let radius = (cluster.radius * scale).max(0.45);
        let ri = radius.ceil() as i32 + 1;
        let cx = center.x.round() as i32;
        let cy = center.y.round() as i32;
        let cz = center.z.round() as i32;
        // No wobble reaches past this, so a candidate beyond it is dropped
        // before it costs a hash or a square root.
        let reach = (radius * WOBBLE_MAX).powi(2);
        for z in (cz - ri)..=(cz + ri) {
            let dz = z as f32 - center.z;
            let flat = reach - dz * dz;
            if flat < 0.0 {
                continue;
            }
            for y in (cy - ri)..=(cy + ri) {
                let dy = y as f32 - center.y;
                let left = flat - dy * dy;
                if left < 0.0 {
                    continue;
                }
                // The row's own span, widened by a voxel so the exact test
                // inside is still what decides.
                let half = left.sqrt() + 1.0;
                let x0 = ((center.x - half).floor() as i32).max(cx - ri);
                let x1 = ((center.x + half).ceil() as i32).min(cx + ri);
                for x in x0..=x1 {
                    let p = Vec3 { x: x as f32, y: y as f32, z: z as f32 };
                    let away = p - center;
                    let d2 = away.dot(away);
                    if d2 > reach || grid.taken(x, y, z) {
                        continue;
                    }
                    // A wobbly edge, so clusters read as foliage not spheres.
                    let wobble = 0.72 + hash_unit(x, y, z, cluster.seed ^ 0x51E1) * 0.55;
                    let d = d2.sqrt();
                    if d > radius * wobble {
                        continue;
                    }
                    let shell = (shell_distance(p, crown, stretch) / crown_radius).clamp(0.0, 1.2);
                    // Dense at the crown's surface, open in its core, as far as
                    // the species is hollow at all.
                    let mut keep = density * (1.0 - hollow + hollow * (0.28 + shell));
                    let into_dome = ((p.y - (apex - dome)) / dome).clamp(0.0, 1.0);
                    keep *= 1.0 - 0.88 * into_dome * into_dome;
                    // The underside is thinner than the top, as light dictates.
                    if p.y < crown.y {
                        keep *= 1.0 - hollow * 0.38;
                    } else {
                        keep *= 1.0 + hollow * 0.15;
                    }
                    if hash_unit(x, y, z, cluster.seed) > keep.clamp(0.02, 0.98) {
                        continue;
                    }
                    // Shade is chosen per cluster of voxels, not per voxel: it
                    // reads as foliage in patches rather than static, and it
                    // lets the mesher merge runs of one colour instead of
                    // breaking every face apart.
                    let patch = ivec3(
                        x.div_euclid(SHADE_PATCH),
                        y.div_euclid(SHADE_PATCH),
                        z.div_euclid(SHADE_PATCH),
                    );
                    let mut color = pick(&palette.leaf, patch, 0x2C71);
                    // The outermost leaves catch the light.
                    let lit = cluster.shell * 0.6 + shell * 0.4;
                    if lit > 0.6 && hash_unit(patch.x, patch.y, patch.z, 0x77A3) < (lit - 0.6) * 2.2
                    {
                        color = color.lerp(palette.tip, 0.85);
                    }
                    grid.set(x, y, z, Voxel { kind: Kind::Leaf, color });
                }
            }
        }
    }

    // The grid walks in key order, so the map is bulk-built rather than grown
    // one insertion at a time.
    let voxels = BTreeMap::from_iter(grid.drain());
    VoxelTree {
        voxels,
        streamers: streamers(skeleton, scale),
        voxels_per_tile: voxels_per_tile.max(1),
    }
}

/// Streamers are geometry, not voxels; all they need here is a leaf colour.
fn streamers(skeleton: &Skeleton, scale: f32) -> Vec<Streamer> {
    let palette = &skeleton.params.palette;
    skeleton
        .streamers
        .iter()
        .map(|s| {
            let at = ivec3(
                (s.anchor.x * scale) as i32,
                (s.anchor.y * scale) as i32,
                (s.anchor.z * scale) as i32,
            );
            let mut color = pick(&palette.leaf, at, 0x2C71);
            if hash_unit(at.x, at.y, at.z, 0x77A3) < 0.3 {
                color = color.lerp(palette.tip, 0.6);
            }
            Streamer { color, ..*s }
        })
        .collect()
}

fn fill_ball(grid: &mut Grid, center: Vec3, radius: f32, make: impl Fn(IVec3) -> Voxel) {
    let ri = radius.ceil() as i32;
    let cx = center.x.round() as i32;
    let cy = center.y.round() as i32;
    let cz = center.z.round() as i32;
    for z in (cz - ri)..=(cz + ri) {
        for y in (cy - ri)..=(cy + ri) {
            for x in (cx - ri)..=(cx + ri) {
                let d = Vec3 { x: x as f32, y: y as f32, z: z as f32 } - center;
                if d.length() <= radius {
                    grid.set(x, y, z, make(ivec3(x, y, z)));
                }
            }
        }
    }
}

/// Pick a palette shade from the voxel's own coordinate, so the choice does not
/// depend on the order voxels were written in.
fn pick(shades: &[Rgb], at: IVec3, salt: u64) -> Rgb {
    if shades.is_empty() {
        return Rgb(255, 0, 255);
    }
    let h = hash3(at.x, at.y, at.z, salt);
    shades[(h % shades.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{Preset, TreeParams};

    /// What each preset rasterises to, pinned so that a faster rasteriser has
    /// to be the same rasteriser. These came off the map-and-tree-walk
    /// implementation the dense grid replaced.
    const PINNED: [(Preset, usize, u64); 11] = [
        (Preset::Oak, 43873, 0x9b2f234d2de46d7f),
        (Preset::Birch, 12072, 0x3c9e9ae7252e2889),
        (Preset::Pine, 17262, 0xf81518a98dedcf1a),
        (Preset::Spruce, 24741, 0xc5b0f59e979e8826),
        (Preset::Willow, 29834, 0xded0713dfc5428f8),
        (Preset::Bush, 9867, 0xdd92f7e2ca5fadf9),
        (Preset::Shrub, 1261, 0xbde1fd554357ce2e),
        (Preset::Sapling, 117, 0xdc6115b79ec6be74),
        (Preset::TallGrass, 67, 0x530a6f2ec6adbe5c),
        (Preset::DeadTree, 1544, 0x637f2efcd28c1172),
        (Preset::MushroomTree, 13361, 0x1391c392a0b3750e),
    ];

    fn checksum(tree: &VoxelTree) -> u64 {
        let mut h: u64 = 0;
        for (at, voxel) in &tree.voxels {
            h = h.wrapping_mul(0x100000001B3)
                ^ hash3(at.x, at.y, at.z, voxel.color.0 as u64)
                ^ (voxel.color.1 as u64) << 8
                ^ (voxel.color.2 as u64) << 16
                ^ (voxel.kind as u64) << 32;
        }
        h
    }

    #[test]
    fn every_preset_rasterises_to_what_it_always_did() {
        for (preset, count, sum) in PINNED {
            let tree = rasterise(&crate::grow(&TreeParams::preset(preset), 11, None), 4);
            assert_eq!(tree.voxels.len(), count, "{preset:?} voxel count");
            assert_eq!(checksum(&tree), sum, "{preset:?} voxels");
        }
    }
}
