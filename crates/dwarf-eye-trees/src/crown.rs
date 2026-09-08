//! One canonical crown per species preset, and its bounding box.
//!
//! Stage three and four of the tree chain. [`grow`](crate::grow) plus
//! [`rasterise`](crate::rasterise) draw a tree out of voxels, which is right
//! where a trunk is metres wide on screen and ruinous where a whole forest is.
//! Here the same preset becomes a handful of **axis-aligned boxes** — a trunk
//! under one to three crown boxes whose extents come from the preset's
//! silhouette — and [`crown_box`] reduces that to a single box in the crown's
//! mean colour, for the band where a tree is a few pixels tall.
//!
//! Boxes rather than faceted ellipsoids on purpose: at this range a rounded
//! solid is only ever a dozen flat facets, and their angled edges read as
//! crystal rather than as foliage. A box shares the voxel map's own vocabulary
//! and shades the way the near canopy does, top face lit and sides darker.
//!
//! Faces that can never be seen are left out, so a box is not always twelve
//! triangles: a trunk is four sides (eight triangles), its top being inside the
//! crown and its foot in the ground, and a crown box drops its underside where
//! the box below is at least as wide.
//!
//! Both are pure functions of the preset, so a caller builds one mesh per
//! species and places thousands of instances of it. Units are tiles, matching
//! [`TreeParams::height`], so an instance needs no scaling beyond the size
//! jitter a scatter wants.

use crate::mesh::TreeMesh;
use crate::params::{Habit, Preset, Rgb, TreeParams, VegetationKind};

/// Per-vertex kind, matching [`TreeMesh::kinds`].
const BARK: u8 = 0;
const LEAF: u8 = 1;

/// How the six faces of a box are lit, so a solid reads as a solid under a sky
/// that fills from every direction: top, bottom, then the four sides in two
/// pairs so the form turns.
const TOP_SHADE: f32 = 1.0;
const BOTTOM_SHADE: f32 = 0.5;
const X_SHADE: f32 = 0.74;
const Z_SHADE: f32 = 0.86;

/// One box of a tree: half-extents in x and z, the levels it spans, its colour,
/// whether it is wood or foliage, and which of its horizontal caps are drawn.
#[derive(Clone, Copy)]
struct Slab {
    half_x: f32,
    half_z: f32,
    y0: f32,
    y1: f32,
    color: Rgb,
    kind: u8,
    top: bool,
    bottom: bool,
}

impl Slab {
    fn triangles(&self) -> usize {
        8 + 2 * (self.top as usize + self.bottom as usize)
    }
}

/// The canonical crown for a species preset: a trunk under one to three crown
/// boxes.
pub fn crown(preset: Preset) -> TreeMesh {
    let mut mesh = TreeMesh::default();
    for slab in slabs(&TreeParams::preset(preset)) {
        push_box(&mut mesh, &slab);
    }
    mesh
}

/// The crown reduced to one box in the mean of its foliage colours: twelve
/// triangles, for the band beyond which even a canonical crown is a smudge.
pub fn crown_box(preset: Preset) -> TreeMesh {
    let slabs = slabs(&TreeParams::preset(preset));
    let mut mesh = TreeMesh::default();
    let Some(first) = slabs.first() else { return mesh };
    // The foliage boxes where there are any, so a box reads as a crown rather
    // than as an average of trunk and crown.
    let leafy: Vec<&Slab> = slabs.iter().filter(|s| s.kind == LEAF).collect();
    let counted: Vec<&Slab> = if leafy.is_empty() { slabs.iter().collect() } else { leafy };
    let mut half_x: f32 = 0.0;
    let mut half_z: f32 = 0.0;
    let mut y1 = f32::MIN;
    let mut sum = [0.0f32; 3];
    for slab in &counted {
        half_x = half_x.max(slab.half_x);
        half_z = half_z.max(slab.half_z);
        y1 = y1.max(slab.y1);
        sum[0] += slab.color.0 as f32;
        sum[1] += slab.color.1 as f32;
        sum[2] += slab.color.2 as f32;
    }
    let n = counted.len() as f32;
    push_box(
        &mut mesh,
        &Slab {
            half_x,
            half_z,
            // Down to the foot, so a box stands on the ground like the tree it
            // stands in for.
            y0: first.y0,
            y1,
            color: Rgb((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8),
            kind: LEAF,
            top: true,
            bottom: true,
        },
    );
    mesh
}

/// How many triangles a preset's crown and box cost, for a caller budgeting a
/// scatter.
pub fn crown_triangles(preset: Preset) -> usize {
    slabs(&TreeParams::preset(preset)).iter().map(Slab::triangles).sum()
}

/// The boxes a preset reduces to, trunk first, with the caps that can never be
/// seen already dropped.
fn slabs(p: &TreeParams) -> Vec<Slab> {
    let height = p.height.max(0.5);
    // The grammar clears `clear_frac` of the height before the first fork, and
    // the crown then reaches out by about `limb_frac` of the height; the limbs
    // fork outward, so the crown is wider than one limb length.
    let clear = (p.clear_frac.clamp(0.0, 0.9) * height).max(0.0);
    let radius = (p.limb_frac * height * 1.25).clamp(0.35, height * 0.8);
    let trunk = (p.trunk_width * 0.5).max(0.12);

    let leaf_low = p.palette.leaf.first().copied().unwrap_or(Rgb(70, 120, 50));
    let leaf_high = *p.palette.leaf.last().unwrap_or(&leaf_low);
    let tip = leaf_high.lerp(p.palette.tip, 0.45);
    // A trunk is a couple of pixels wide at this range, so a pale bark — a
    // birch's especially — reads as a bright vertical line rather than as wood.
    // Pull it toward the crown's own shade and take it down.
    let bark = p.palette.bark.first().copied().unwrap_or(Rgb(110, 80, 50)).lerp(leaf_low, 0.3).scale(0.72);

    // The trunk's top is inside the crown and its foot is in the ground, so it
    // is four side faces and nothing else.
    let wood = |y1: f32, half: f32, kind: u8, color: Rgb| Slab {
        half_x: half,
        half_z: half,
        y0: 0.0,
        y1,
        color,
        kind,
        top: false,
        bottom: false,
    };
    // A crown box's underside is drawn only when nothing wider sits under it.
    let foliage = |y0: f32, y1: f32, half: f32, color: Rgb, bottom: bool| Slab {
        half_x: half,
        half_z: half,
        y0,
        y1,
        color,
        kind: LEAF,
        top: true,
        bottom,
    };

    match p.kind {
        // No trunk worth drawing: one low wide box, its foot in the ground.
        VegetationKind::Shrub | VegetationKind::TallGrass => {
            vec![foliage(0.0, height, radius, leaf_low.lerp(leaf_high, 0.5), false)]
        }
        VegetationKind::Sapling => vec![
            wood(height * 0.55, trunk, BARK, bark),
            foliage(height * 0.42, height, radius.min(height * 0.45), tip, true),
        ],
        // Bare limbs: the trunk, and a narrow box of wood standing for them.
        VegetationKind::DeadTree => vec![
            wood(height, trunk, BARK, bark),
            Slab {
                half_x: radius * 0.45,
                half_z: radius * 0.45,
                y0: height * 0.55,
                y1: height * 0.95,
                color: bark,
                kind: BARK,
                top: true,
                bottom: true,
            },
        ],
        // A flat wide cap on a bare stalk.
        VegetationKind::MushroomTree => vec![
            wood(height * 0.78, trunk, BARK, bark),
            foliage(height * 0.72, height, radius, leaf_high, true),
        ],
        VegetationKind::Tree if p.habit == Habit::Conifer => {
            // Three boxes narrowing upward: the whorls step in as the leader
            // rises, which is the whole of a conifer's outline. Each rests on
            // a wider one, so only the lowest keeps an underside.
            let base = clear * 0.6;
            let span = height - base;
            vec![
                wood(base + span * 0.15, trunk, BARK, bark),
                foliage(base, base + span * 0.42, radius, leaf_low, true),
                foliage(base + span * 0.40, base + span * 0.76, radius * 0.7, leaf_low.lerp(leaf_high, 0.6), false),
                foliage(base + span * 0.74, height, radius * 0.4, tip, false),
            ]
        }
        VegetationKind::Tree => {
            // A broadleaf: one wide box with a smaller one on top.
            let base = clear * 0.72;
            let top = base + (height - base) * p.crown_stretch.clamp(0.6, 1.0);
            let span = top - base;
            vec![
                wood(base + span * 0.2, trunk, BARK, bark),
                foliage(base, base + span * 0.66, radius, leaf_low.lerp(leaf_high, 0.4), true),
                foliage(base + span * 0.62, top, radius * 0.62, tip, false),
            ]
        }
    }
}

/// Four sides and whichever caps are drawn, each face flat shaded so the solid
/// turns under a sky that lights it from every direction.
fn push_box(mesh: &mut TreeMesh, slab: &Slab) {
    let (x0, x1) = (-slab.half_x, slab.half_x);
    let (z0, z1) = (-slab.half_z, slab.half_z);
    let (y0, y1) = (slab.y0, slab.y1);
    let mut faces: Vec<([f32; 3], [[f32; 3]; 4], f32)> = vec![
        ([0.0, 0.0, -1.0], [[x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0]], Z_SHADE),
        ([0.0, 0.0, 1.0], [[x1, y0, z1], [x1, y1, z1], [x0, y1, z1], [x0, y0, z1]], Z_SHADE),
        ([-1.0, 0.0, 0.0], [[x0, y0, z1], [x0, y1, z1], [x0, y1, z0], [x0, y0, z0]], X_SHADE),
        ([1.0, 0.0, 0.0], [[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]], X_SHADE),
    ];
    if slab.top {
        faces.push(([0.0, 1.0, 0.0], [[x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]], TOP_SHADE));
    }
    if slab.bottom {
        faces.push(([0.0, -1.0, 0.0], [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]], BOTTOM_SHADE));
    }
    for (normal, corners, shade) in faces {
        let color = slab.color.scale(shade).to_linear();
        let base = mesh.positions.len() as u32;
        for corner in corners {
            mesh.positions.push(corner);
            mesh.normals.push(normal);
            mesh.colors.push(color);
            mesh.uvs.push([0.0, 0.0]);
            mesh.kinds.push(slab.kind);
        }
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_a_few_boxes() {
        for preset in Preset::ALL {
            let triangles = crown(preset).indices.len() / 3;
            assert_eq!(triangles, crown_triangles(preset), "{preset:?} count disagrees");
            assert!((8..=48).contains(&triangles), "{preset:?} is {triangles} triangles");
            assert_eq!(crown_box(preset).indices.len() / 3, 12, "{preset:?} box");
        }
    }

    /// The trunk is four side faces: its top is inside the crown and its foot
    /// is in the ground.
    #[test]
    fn trunks_are_four_sides() {
        for preset in [Preset::Oak, Preset::Birch, Preset::Pine, Preset::MushroomTree] {
            let trunk = slabs(&TreeParams::preset(preset))[0];
            assert_eq!(trunk.kind, BARK, "{preset:?}");
            assert_eq!(trunk.triangles(), 8, "{preset:?}");
        }
    }

    /// The crown stands on the ground and reaches the preset's own height, so
    /// an instance needs no offset.
    #[test]
    fn crowns_stand_on_zero() {
        for preset in Preset::ALL {
            for mesh in [crown(preset), crown_box(preset)] {
                let lo = mesh.positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
                let hi = mesh.positions.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
                let height = TreeParams::preset(preset).height;
                assert!(lo.abs() < 1e-3, "{preset:?} foot at {lo}");
                assert!(hi <= height + 1e-3, "{preset:?} top {hi} over height {height}");
                assert!(hi > height * 0.4, "{preset:?} top {hi} under height {height}");
            }
        }
    }

    /// Every face is axis aligned: no angled facet to read as crystal at a
    /// thousand tiles.
    #[test]
    fn every_face_is_axis_aligned() {
        for preset in Preset::ALL {
            for normal in crown(preset).normals {
                let axes = normal.iter().filter(|c| c.abs() > 1e-4).count();
                assert_eq!(axes, 1, "{preset:?} has a normal {normal:?}");
            }
        }
    }

    #[test]
    fn crowns_are_deterministic() {
        for preset in Preset::ALL {
            assert_eq!(crown(preset).positions, crown(preset).positions, "{preset:?}");
            assert_eq!(crown_box(preset).colors, crown_box(preset).colors, "{preset:?}");
        }
    }
}
