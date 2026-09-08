//! Procedural voxel trees.
//!
//! Three stages, each usable on its own: [`grow`] builds a skeleton of segments
//! and leaf clusters, [`rasterise`] turns that into voxels, and [`mesh`] merges
//! the visible faces into triangles. Everything is a pure function of the
//! parameters and the seed.
//!
//! ```
//! use dwarf_eye_trees::{grow, mesh, oak, rasterise};
//! let tree = grow(&oak(), 7, None);
//! let voxels = rasterise(&tree, 4);
//! let mesh = mesh(&voxels);
//! assert!(!mesh.indices.is_empty());
//! ```

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
    Envelope, Footprint, Habit, Palette, Preset, Rgb, TreeParams, birch, bush, oak, pine, spruce,
    willow,
};
pub use raster::{Kind, Voxel, VoxelTree, rasterise};

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
