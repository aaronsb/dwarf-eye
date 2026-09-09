//! The nearest stage of the far band's tree chain: a grown tree.
//!
//! A region-sourced tree can stand a few tiles from the camera — the live
//! window is 144 tiles square and the camera walks to its edge — so the chain
//! cannot start at a box. The first stage is the same growth the canopy's far
//! band draws, one voxel to a tile
//! ([`canopy::FAR_DETAIL`](crate::canopy::FAR_DETAIL)), opaque, with the
//! crate's own world-space leaf and bark UVs on it.
//!
//! Growth is the expensive half and there is no per-tree data out here to
//! preserve, so a handful of shapes serve thousands of instances: a tree's seed
//! picks one of [`VARIANTS`] canonical growths per species, and the instance
//! transform gives it its height and its yaw. Grown once and kept for the
//! process, since the set is small and a window move rebuilds the scatter
//! around it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use dwarf_eye_trees::params::TreeParams;
use dwarf_eye_trees::{Preset, TreeMesh};

use crate::mesh::MeshData;

/// Canonical growths per species. Enough that a stand does not read as one
/// tree repeated, few enough that the whole set grows in well under a second
/// and is then never grown again.
pub const VARIANTS: u32 = 6;

/// The height every grown variant is built at, in tiles. The instance scales
/// from here, so a voxel stays about a tile across whatever size the scatter
/// asked for — which is the resolution this stage is meant to be.
pub const GROWN_HEIGHT: f32 = super::scatter::DF_TREE_HEIGHT;

/// Voxels per tile: the far canopy band's own resolution.
const DETAIL: u32 = 1;

type Cache = HashMap<(Preset, u32), TreeMesh>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One species' `variant`th growth, at one voxel to a tile and
/// [`GROWN_HEIGHT`] tall.
pub fn mesh(preset: Preset, variant: u32) -> MeshData {
    let key = (preset, variant % VARIANTS);
    if let Some(found) = cache().lock().unwrap().get(&key) {
        return convert(found.clone());
    }
    let built = grow(key.0, key.1);
    let out = convert(built.clone());
    cache().lock().unwrap().insert(key, built);
    out
}

fn grow(preset: Preset, variant: u32) -> TreeMesh {
    let mut params = TreeParams::preset(preset);
    // A DF tree is a fraction of the lab preset's height, and this stage is
    // meshed at that size rather than shrunk into it: a voxel is then a tile,
    // not a third of one.
    params.height = GROWN_HEIGHT;
    let seed = 0x_5EED_0000 ^ ((variant as u64) << 8) ^ preset.name().len() as u64;
    dwarf_eye_trees::build(params.kind, &params, seed, None, DETAIL)
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

    /// Every species grows at this resolution, and the variants differ.
    #[test]
    fn a_species_has_a_few_distinct_growths() {
        for preset in [Preset::Oak, Preset::Pine, Preset::Willow, Preset::MushroomTree] {
            let a = mesh(preset, 0);
            let b = mesh(preset, 1);
            assert!(!a.indices.is_empty(), "{preset:?} grew nothing");
            assert_ne!(a.positions, b.positions, "{preset:?} variants are the same tree");
        }
    }

    /// A grown tree stands on zero and reaches about the height it was asked
    /// for, so the instance transform is a plain scale.
    #[test]
    fn a_growth_stands_on_zero_at_the_height_it_was_asked_for() {
        for preset in [Preset::Oak, Preset::Birch, Preset::Spruce] {
            let m = mesh(preset, 2);
            let lo = m.positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
            let hi = m.positions.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
            // At one voxel to a tile the base voxel spans the ground tile
            // itself, so a foot a tile under zero is the trunk in the soil.
            assert!((-1.6..=0.6).contains(&lo), "{preset:?} foot at {lo}");
            assert!(hi > GROWN_HEIGHT * 0.5, "{preset:?} top {hi} well under {GROWN_HEIGHT}");
            assert!(hi < GROWN_HEIGHT * 1.6, "{preset:?} top {hi} well over {GROWN_HEIGHT}");
        }
    }

    /// The cache answers with the same mesh rather than growing again.
    #[test]
    fn growths_are_cached() {
        let a = mesh(Preset::Oak, 3);
        let b = mesh(Preset::Oak, 3);
        assert_eq!(a.positions, b.positions);
        // A variant past the set wraps into it.
        assert_eq!(mesh(Preset::Oak, VARIANTS + 3).positions, a.positions);
    }
}
