//! The rasterised stages of the far band's tree chain: a grown tree, cut as
//! finely as the camera's distance asks for.
//!
//! A region-sourced tree can stand a few tiles from the camera — the live
//! window is 144 tiles square and the camera walks to its edge — so the chain
//! cannot start at a box, and it cannot start at one voxel to a tile either: a
//! tree just outside the window would then be visibly cruder than one just
//! inside it at the same distance, which draws the window's boundary across the
//! canopy. The instances therefore take **the same cuts the window's canopy
//! bands take**, at the same distances: four, three, two and one voxel to a
//! tile (`factory::Chain::window`), each with the near band's own limb widths.
//!
//! Growth is the expensive half and there is no per-tree data out here to
//! preserve, so a handful of shapes serve thousands of instances: a tree's seed
//! picks one of [`VARIANTS`] canonical growths per species, and the instance
//! transform gives it its height and its yaw. One growth feeds every cut, so a
//! tree keeps its shape as it hands over. Grown once and kept for the process,
//! since the set is small and a window move rebuilds the scatter around it.
//!
//! These cuts are the stages [`super::batch`] leaves instanced: a copy per tree
//! runs to hundreds of thousands of triangles, so the transform stays on the
//! entity rather than being baked into the vertices.

use std::sync::{Mutex, OnceLock};

use dwarf_eye_trees::params::TreeParams;
use dwarf_eye_trees::{Cut, Preset, TreeMesh};

use crate::factory::{self, Detail, StageCache};
use crate::mesh::MeshData;
use crate::tree::DETAIL;

/// Canonical growths per species. Enough that a stand does not read as one
/// tree repeated, few enough that the whole set grows in well under a second
/// and is then never grown again.
pub const VARIANTS: u32 = 6;

/// The height every grown variant is built at, in tiles. The instance scales
/// from here, so a voxel stays the fraction of a tile its cut says it is
/// whatever height the scatter gave the tree.
pub const GROWN_HEIGHT: f32 = super::scatter::DF_TREE_HEIGHT;

type Cache = StageCache<(Preset, u32), TreeMesh>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

/// One species' `variant`th growth, cut at `stage`.
///
/// A miss grows the skeleton and rasterises every cut off it, so a variant's
/// whole chain costs one growth.
pub fn mesh(preset: Preset, variant: u32, stage: Detail) -> MeshData {
    let key = (preset, variant % VARIANTS);
    if let Some(found) = cache().lock().unwrap().get(key, stage) {
        return convert(found.clone());
    }
    let cuts = grow(key.0, key.1);
    let mut held = cache().lock().unwrap();
    for (detail, mesh) in cuts {
        held.insert(key, detail, mesh);
    }
    held.get(key, stage).map(|m| convert(m.clone())).unwrap_or_default()
}

/// Every rasterised cut of one variant, off a single growth.
fn grow(preset: Preset, variant: u32) -> Vec<(Detail, TreeMesh)> {
    let mut params = TreeParams::preset(preset);
    // A DF tree is a fraction of the lab preset's height, and these stages are
    // meshed at that size rather than shrunk into it.
    params.height = GROWN_HEIGHT;
    let seed = 0x_5EED_0000 ^ ((variant as u64) << 8) ^ preset.name().len() as u64;
    let skeleton = dwarf_eye_trees::grow(&params, seed, None);
    factory::tree_chain()
        .window()
        .iter()
        .filter_map(|stage| {
            let per_tile = stage.detail.per_tile()? as u32;
            // The near band's limb widths, whatever this cut's own resolution:
            // the same correction `canopy::Band::cut` makes, so a coarse
            // instance does not fill with bark the fine one never showed.
            let cut = Cut { wood_like: DETAIL as u32 };
            let voxels = dwarf_eye_trees::rasterise_cut(&skeleton, per_tile, cut);
            Some((stage.detail, dwarf_eye_trees::mesh(&voxels)))
        })
        .collect()
}

/// The tree crate's mesh in this crate's buffers. Its UVs are world-space at
/// one repeat per tile, but an instanced crown is scaled by its transform, so
/// the horizon's crown material derives them from the world position instead
/// (`cloud_shadow.wgsl:horizon_leaf`) and these are carried only so the vertex
/// buffers line up.
fn convert(tree: TreeMesh) -> MeshData {
    MeshData {
        uvs: tree.uvs,
        positions: tree.positions,
        normals: tree.normals,
        colors: tree.colors,
        indices: tree.indices,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cuts this module builds are exactly the chain's window run, so an
    /// instance is cut the way a chunk is.
    fn cuts() -> Vec<Detail> {
        factory::tree_chain().window().iter().map(|s| s.detail).collect()
    }

    /// Every species grows at every cut, and the variants differ.
    #[test]
    fn a_species_has_a_few_distinct_growths() {
        for preset in [Preset::Oak, Preset::Pine, Preset::Willow, Preset::MushroomTree] {
            for cut in cuts() {
                let a = mesh(preset, 0, cut);
                let b = mesh(preset, 1, cut);
                assert!(!a.indices.is_empty(), "{preset:?} grew nothing at {cut:?}");
                assert_ne!(a.positions, b.positions, "{preset:?} variants are the same tree");
            }
        }
    }

    /// A finer cut of one variant draws more than a coarser one, and no two
    /// cuts draw the same tree.
    #[test]
    fn the_cuts_run_fine_to_coarse() {
        let mut counts: Vec<usize> =
            cuts().iter().map(|cut| mesh(Preset::Oak, 0, *cut).indices.len()).collect();
        let mut sorted = counts.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(counts, sorted, "cuts are not ordered fine to coarse: {counts:?}");
        counts.dedup();
        assert_eq!(counts.len(), cuts().len(), "two cuts drew the same tree");
    }

    /// A grown tree stands on zero and reaches about the height it was asked
    /// for, so the instance transform is a plain scale.
    #[test]
    fn a_growth_stands_on_zero_at_the_height_it_was_asked_for() {
        let coarsest = *cuts().last().unwrap();
        for preset in [Preset::Oak, Preset::Birch, Preset::Spruce] {
            let m = mesh(preset, 2, coarsest);
            let lo = m.positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
            let hi = m.positions.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
            // At one voxel to a tile the base voxel spans the ground tile
            // itself, so a foot a tile under zero is the trunk in the soil.
            assert!((-1.6..=0.6).contains(&lo), "{preset:?} foot at {lo}");
            assert!(hi > GROWN_HEIGHT * 0.5, "{preset:?} top {hi} well under {GROWN_HEIGHT}");
            assert!(hi < GROWN_HEIGHT * 1.6, "{preset:?} top {hi} well over {GROWN_HEIGHT}");
        }
    }

    /// The cache answers with the same mesh rather than growing again, and one
    /// growth fills every cut of that variant.
    #[test]
    fn growths_are_cached_per_stage() {
        let fine = cuts()[0];
        let a = mesh(Preset::Oak, 3, fine);
        let b = mesh(Preset::Oak, 3, fine);
        assert_eq!(a.positions, b.positions);
        // A variant past the set wraps into it.
        assert_eq!(mesh(Preset::Oak, VARIANTS + 3, fine).positions, a.positions);
        let held = cache().lock().unwrap();
        for cut in cuts() {
            assert!(held.contains((Preset::Oak, 3), cut), "{cut:?} was not cached");
        }
    }
}
