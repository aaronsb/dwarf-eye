//! Turning a skeleton into voxels: wood along the segments, foliage as ragged
//! clusters that thin out toward the crown's core and underside.

use std::collections::BTreeMap;

use crate::grow::{Skeleton, Streamer, shell_distance};
use crate::math::{IVec3, Vec3, ivec3};
use crate::params::Rgb;
use crate::rng::{hash3, hash_unit};

/// Voxels per patch of one shade. Bigger than one, so a crown reads as blocks
/// of colour and coplanar faces can merge.
const SHADE_PATCH: i32 = 2;

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

/// Voxelise a skeleton at `voxels_per_tile` resolution.
///
/// Wood is drawn first and foliage never overwrites it, so limbs stay readable
/// through the canopy.
pub fn rasterise(skeleton: &Skeleton, voxels_per_tile: u32) -> VoxelTree {
    let scale = voxels_per_tile.max(1) as f32;
    let mut voxels: BTreeMap<IVec3, Voxel> = BTreeMap::new();
    let palette = &skeleton.params.palette;

    for segment in &skeleton.segments {
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
            fill_ball(&mut voxels, p, r, |v| Voxel {
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
        for z in (cz - ri)..=(cz + ri) {
            for y in (cy - ri)..=(cy + ri) {
                for x in (cx - ri)..=(cx + ri) {
                    let key = ivec3(x, y, z);
                    if voxels.contains_key(&key) {
                        continue;
                    }
                    let p = Vec3 { x: x as f32, y: y as f32, z: z as f32 };
                    // A wobbly edge, so clusters read as foliage not spheres.
                    let wobble = 0.72 + hash_unit(x, y, z, cluster.seed ^ 0x51E1) * 0.55;
                    let d = (p - center).length();
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
                    let patch = ivec3(x.div_euclid(SHADE_PATCH), y.div_euclid(SHADE_PATCH), z.div_euclid(SHADE_PATCH));
                    let mut color = pick(&palette.leaf, patch, 0x2C71);
                    // The outermost leaves catch the light.
                    let lit = cluster.shell * 0.6 + shell * 0.4;
                    if lit > 0.6 && hash_unit(patch.x, patch.y, patch.z, 0x77A3) < (lit - 0.6) * 2.2 {
                        color = color.lerp(palette.tip, 0.85);
                    }
                    voxels.insert(key, Voxel { kind: Kind::Leaf, color });
                }
            }
        }
    }

    // Streamers are geometry, not voxels; all they need here is a leaf colour.
    let streamers = skeleton
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
        .collect();

    VoxelTree { voxels, streamers, voxels_per_tile: voxels_per_tile.max(1) }
}

fn fill_ball(
    voxels: &mut BTreeMap<IVec3, Voxel>,
    center: Vec3,
    radius: f32,
    make: impl Fn(IVec3) -> Voxel,
) {
    let ri = radius.ceil() as i32;
    let cx = center.x.round() as i32;
    let cy = center.y.round() as i32;
    let cz = center.z.round() as i32;
    for z in (cz - ri)..=(cz + ri) {
        for y in (cy - ri)..=(cy + ri) {
            for x in (cx - ri)..=(cx + ri) {
                let d = Vec3 { x: x as f32, y: y as f32, z: z as f32 } - center;
                if d.length() <= radius {
                    voxels.insert(ivec3(x, y, z), make(ivec3(x, y, z)));
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
