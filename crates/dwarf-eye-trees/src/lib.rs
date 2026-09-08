//! Procedural voxel trees and other vegetation.
//!
//! Three stages, each usable on its own: [`grow`] builds a skeleton of segments
//! and leaf clusters, [`rasterise`] turns that into voxels, and [`mesh`] merges
//! the visible faces into triangles. Everything is a pure function of the
//! parameters and the seed, so the same plant can be rebuilt anywhere without
//! storing it.
//!
//! ```
//! use dwarf_eye_trees::{grow, mesh, oak, rasterise};
//! let tree = grow(&oak(), 7, None);
//! let voxels = rasterise(&tree, 4);
//! let mesh = mesh(&voxels);
//! assert!(!mesh.indices.is_empty());
//! ```
//!
//! # Driving it from Dwarf Fortress
//!
//! [`build`] is the single call a treatment registry needs: it takes the
//! grammar, the species knobs, a seed and an optional envelope, and returns
//! triangles. The main app is expected to fill those in like this.
//!
//! - **Kind** comes from the classified tile. A DF tile carries a plant shape
//!   (sapling, shrub, trunk, branch, twig, cap) which maps onto
//!   [`VegetationKind`]; a dead plant maps to [`VegetationKind::DeadTree`].
//! - **Params** come from the plant raws. Height and trunk width from the
//!   plant's growth data, [`Habit`] from whether it is a conifer, and
//!   [`Palette`] from the sprite colours DF already gives for wood and leaves,
//!   two or three shades sampled per material. Porosity is per species:
//!   [`TreeParams::leaf_density`] is the coarse air between clusters and
//!   [`TreeParams::cutout_openness`] the fine air inside a leaf face, which is
//!   what makes a conifer read as open next to a solid oak.
//! - **Seed** is a hash of the tree's origin tile, so a tree is the same every
//!   time the block is streamed in and neighbouring trees differ.
//! - **Envelope** is built from the DF tiles the tree actually occupies: one
//!   [`Footprint`] per z-level, `true` where a tile belongs to this tree.
//!   Growth then bends toward each level's centroid and stops at the edge, so
//!   the mesh stays inside the space the fortress map gave it.
//! - **Resolution** is `voxels_per_tile`, matched to the rest of the map.

pub mod grow;
pub mod math;
pub mod mesh;
pub mod params;
pub mod raster;
pub mod rng;

pub use grow::{LeafCluster, Segment, Skeleton, grow};
pub use math::{IVec3, Vec3, ivec3, vec3};
pub use mesh::{Stats, TreeMesh, mesh, mesh_of, stats};
pub use params::{
    Envelope, Footprint, Habit, Palette, Preset, Rgb, TreeParams, VegetationKind, birch, bush,
    dead_tree, mushroom_tree, oak, pine, sapling, shrub, spruce, tall_grass, willow,
};
pub use raster::{Kind, Voxel, VoxelTree, rasterise};

/// Grow, voxelise and mesh in one call: the entry point a treatment registry
/// maps a classified tile onto.
///
/// `kind` overrides [`TreeParams::kind`], so one species' parameters can be
/// reused for its sapling, its shrub form and its dead stump.
pub fn build(
    kind: VegetationKind,
    params: &TreeParams,
    seed: u64,
    envelope: Option<&Envelope>,
    voxels_per_tile: u32,
) -> TreeMesh {
    let mut params = params.clone();
    params.kind = kind;
    mesh(&rasterise(&grow(&params, seed, envelope), voxels_per_tile))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presets() -> Vec<(Preset, TreeParams)> {
        Preset::ALL.iter().map(|p| (*p, TreeParams::preset(*p))).collect()
    }

    #[test]
    fn growth_is_deterministic() {
        for (name, params) in presets() {
            let a = grow(&params, 12345, None);
            let b = grow(&params, 12345, None);
            assert_eq!(a.segments.len(), b.segments.len(), "{name:?} segment count");
            assert_eq!(a.segments, b.segments, "{name:?} segments");
            assert_eq!(a.leaves, b.leaves, "{name:?} leaves");
        }
    }

    #[test]
    fn voxels_and_mesh_are_deterministic() {
        for (name, params) in presets() {
            let a = mesh(&rasterise(&grow(&params, 99, None), 4));
            let b = mesh(&rasterise(&grow(&params, 99, None), 4));
            assert_eq!(a.positions, b.positions, "{name:?} positions");
            assert_eq!(a.colors, b.colors, "{name:?} colors");
            assert_eq!(a.indices, b.indices, "{name:?} indices");
        }
    }

    #[test]
    fn seeds_differ() {
        let a = rasterise(&grow(&oak(), 1, None), 4);
        let b = rasterise(&grow(&oak(), 2, None), 4);
        assert_ne!(a.voxels.len(), 0);
        assert_ne!(a.voxels, b.voxels);
    }

    #[test]
    fn bark_stays_under_a_fifth_of_leaf() {
        for (name, params) in presets() {
            let counts = rasterise(&grow(&params, 7, None), 4).counts();
            if params.leaf_density <= 0.0 {
                assert_eq!(counts.leaf, 0, "{name:?} is meant to be bare");
                continue;
            }
            assert!(counts.leaf > 0, "{name:?} grew no leaves");
            assert!(
                counts.bark * 5 < counts.leaf,
                "{name:?}: {} bark vs {} leaf",
                counts.bark,
                counts.leaf
            );
        }
    }

    #[test]
    fn height_lands_near_the_request() {
        for (name, params) in presets() {
            let tree = grow(&params, 4, None);
            let error = (tree.height - params.height).abs() / params.height;
            assert!(error < 0.12, "{name:?} grew to {} not {}", tree.height, params.height);
        }
    }

    #[test]
    fn an_envelope_bounds_the_tree() {
        let envelope = Envelope::box_of(7, 10);
        let tree = grow(&oak(), 3, Some(&envelope));
        for segment in &tree.segments {
            assert!(segment.b.y <= 10.5, "grew above the envelope: {}", segment.b.y);
            assert!(segment.b.x.abs() <= 4.0 && segment.b.z.abs() <= 4.0, "grew out the side");
        }
    }

    #[test]
    fn build_matches_the_stages() {
        let params = shrub();
        let one = build(VegetationKind::Shrub, &params, 21, None, 4);
        let staged = mesh(&rasterise(&grow(&params, 21, None), 4));
        assert_eq!(one.positions, staged.positions);
        assert!(!one.indices.is_empty());
    }

    #[test]
    fn every_preset_builds() {
        for (name, params) in presets() {
            let built = build(params.kind, &params, 5, None, 4);
            assert!(!built.indices.is_empty(), "{name:?} meshed to nothing");
        }
    }

    #[test]
    fn meshing_culls_interior_faces() {
        // A solid 4x4x4 block has 6 sides; greedy merging should give 12
        // triangles, not 6 per voxel face.
        let mut voxels = std::collections::BTreeMap::new();
        for z in 0..4 {
            for y in 0..4 {
                for x in 0..4 {
                    voxels.insert(
                        ivec3(x, y, z),
                        Voxel { kind: Kind::Bark, color: Rgb(10, 20, 30) },
                    );
                }
            }
        }
        let tree = VoxelTree { voxels, voxels_per_tile: 1 };
        assert_eq!(mesh(&tree).indices.len() / 3, 12);
    }
}
