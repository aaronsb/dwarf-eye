//! Turning the scatter's per-tree instances into per-cell meshes.
//!
//! The far band places one canonical crown per tree and used to spawn **one
//! entity per tree per stage**, each carrying its own `VisibilityRange`. Over
//! 22.7k trees and a six-stage chain that is 136k entities, plus a blob shadow
//! each: Bevy's per-entity visibility, range and transform work ran over 159k
//! entities every frame and cost far more than the triangles they drew. The
//! geometry was never the bill.
//!
//! So the cheap stages are **baked**. A cell's trees are merged into one mesh
//! with each instance's yaw, scale and position folded into the vertices, and
//! the cell draws as one entity at the identity transform. Nothing a texture
//! sees changes: `cloud_shadow.wgsl:horizon_leaf` derives a far crown's UVs
//! from the world position precisely because an instance's own scale would
//! otherwise take them with it, so a baked transform is the case it was written
//! for.
//!
//! Baking costs a copy of the mesh per tree, so it only pays where the mesh is
//! small. A stage merges when it costs at most [`MERGE_MAX_TRIANGLES`] a tree —
//! the canonical crown at 30 and the box at 12 — and the rasterised cuts, at
//! 780 to 16000 triangles a tree, stay instanced: baking the one-voxel cut over
//! the trees that can reach it would hold four million triangles where forty
//! thousand do now, and rebuild them on every window move.
//!
//! What makes the cuts affordable as entities is [`reach`]: a tree is only
//! given a stage the camera can still ask for. The camera stands somewhere in
//! the live window, so the nearest it can ever come to a region tile is that
//! tile's own distance to the window's rectangle, and a stage that hands over
//! nearer than that is never spawned at all. The stage behind it already covers
//! everything the camera can reach, so nothing has to be stretched to fill in.

use std::collections::HashMap;

use dwarf_eye_trees::Preset;

use crate::mesh::MeshData;

use super::REGION_TILE;
use super::scatter::{CrownInstance, Stage};

/// How many triangles a tree may cost at a stage for that stage to be merged
/// into its cell's mesh rather than instanced one entity a tree.
///
/// The canonical crown is 20 to 40 triangles and the box is 12, so both merge;
/// the coarsest rasterised cut is 780 and merging it would hold, and rebuild on
/// every window move, a hundred times the geometry it holds now.
pub const MERGE_MAX_TRIANGLES: usize = 64;

/// How many region tiles across one merged cell is, per stage.
///
/// A cell hands over as a whole, at its own centre's distance (Bevy's
/// `VisibilityRange::use_aabb` measures to the bounding box's centre), so the
/// error a cell costs is half its width. One region tile for anything that
/// hands over near the window; the crown and the box hand over at four and
/// eight times the near band, where a wider cell is the same fraction of the
/// distance it happens at and halves the entity count again.
pub fn cell_span(stage: Stage) -> i32 {
    match stage {
        Stage::Crown => 2,
        Stage::Box => 4,
        _ => 1,
    }
}

/// The cell the blob shadows are merged over, in region tiles. A blob has one
/// edge and it is a near one — where the cuts stop casting into the cascades —
/// and past it the blob runs to the far plane with no further hand-off.
pub const BLOB_CELL: i32 = 2;

/// Where the blob sits above the ground it lies on, and how far it may stretch
/// as a multiple of the crown's width. Uncapped, a sun near the horizon would
/// throw a shadow across half the map.
pub const BLOB_LIFT: f32 = 0.06;
pub const BLOB_MAX_STRETCH: f32 = 4.0;

/// One tree's ground shadow, kept so the merged mesh can be re-aimed when the
/// sun moves without going back to the scatter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blob {
    pub pos: [f32; 3],
    /// The crown's own half-width in tiles, after the instance's scale.
    pub radius: f32,
    /// How tall the tree stands, which is what the sun's slant multiplies.
    pub height: f32,
}

/// One cell's trees at one stage, as a single mesh in render space.
pub struct Merged {
    pub stage: Stage,
    /// The merge cell, in units of [`cell_span`] region tiles.
    pub cell: (i32, i32),
    /// The nearest the camera can come to this cell while it stands anywhere in
    /// the live window, in tiles. A stage handing over inside this never draws.
    pub reach: f32,
    pub trees: usize,
    pub mesh: MeshData,
}

/// One cell's blob shadows: the quads, and what they were made from, so a sun
/// two degrees along rewrites the positions in place.
pub struct Blobs {
    pub cell: (i32, i32),
    pub reach: f32,
    pub shadows: Vec<Blob>,
}

/// One species at one growth variant and one detail stage, still instanced: the
/// mesh, and every place it stands.
///
/// The stages left here are the rasterised cuts, whose meshes are too big to
/// copy per tree. Each instance carries its own [`CrownInstance::reach`], so a
/// tree is only spawned at a cut the camera can still ask for.
pub struct CrownBatch {
    pub preset: Preset,
    pub stage: Stage,
    /// Which of the species' canonical growths this mesh is.
    pub variant: u32,
    /// How tall the mesh itself stands, in tiles: an instance's transform is
    /// scaled by its own height over this.
    pub mesh_height: f32,
    pub mesh: MeshData,
    pub instances: Vec<CrownInstance>,
}

/// How near the camera can come to a region tile while it stands anywhere in
/// the live window: the distance from the tile's own square to the window's
/// rectangle, in tiles, zero where the two touch.
///
/// This is the whole of the culling rule. A stage that hands over inside this
/// distance can never be asked for, so it is not spawned; the stage behind it
/// starts where that hand-off was and therefore already covers every distance
/// the camera can reach, which is why nothing needs stretching to cover a gap.
pub fn reach(tile: (i32, i32), origin: (i32, i32), live: (i32, i32, i32, i32)) -> f32 {
    let x0 = (tile.0 * REGION_TILE - origin.0) as f32;
    let z0 = (tile.1 * REGION_TILE - origin.1) as f32;
    let (x1, z1) = (x0 + REGION_TILE as f32, z0 + REGION_TILE as f32);
    let dx = (live.0 as f32 - x1).max(x0 - live.2 as f32).max(0.0);
    let dz = (live.1 as f32 - z1).max(z0 - live.3 as f32).max(0.0);
    (dx * dx + dz * dz).sqrt()
}

/// The region tile a render-space position falls in. The render origin is a
/// block position times [`REGION_TILE`], so the two grids line up.
pub fn tile_of(pos: [f32; 3], origin: (i32, i32)) -> (i32, i32) {
    (
        (pos[0] as i32 + origin.0).div_euclid(REGION_TILE),
        (pos[2] as i32 + origin.1).div_euclid(REGION_TILE),
    )
}

/// Appends one instance of `src` to `out` with its yaw, scale and position
/// folded into the vertices.
///
/// The scale is uniform, so a normal only turns; the colours and UVs come
/// through untouched, and the far crown material derives its own UVs from the
/// world position in any case.
pub fn bake(src: &MeshData, instance: &CrownInstance, mesh_height: f32, out: &mut MeshData) {
    let base = out.positions.len() as u32;
    let scale = instance.height / mesh_height;
    let (sin, cos) = instance.yaw.sin_cos();
    let turn = |v: [f32; 3]| [cos * v[0] + sin * v[2], v[1], -sin * v[0] + cos * v[2]];
    out.positions.extend(src.positions.iter().map(|p| {
        let t = turn([p[0] * scale, p[1] * scale, p[2] * scale]);
        [t[0] + instance.pos[0], t[1] + instance.pos[1], t[2] + instance.pos[2]]
    }));
    out.normals.extend(src.normals.iter().map(|n| turn(*n)));
    out.colors.extend_from_slice(&src.colors);
    out.uvs.extend_from_slice(&src.uvs);
    out.indices.extend(src.indices.iter().map(|i| i + base));
}

/// The four ground-plane corners of one blob, in render space.
///
/// A blob lies on the ground under its tree, turned to the sun's azimuth and
/// stretched along it by the tree's height over the tangent of the sun's
/// elevation, capped at [`BLOB_MAX_STRETCH`] crown widths. `aim` is the
/// direction the sun's light travels.
pub fn blob_corners(blob: &Blob, aim: [f32; 3]) -> [[f32; 3]; 4] {
    let width = blob.radius * 2.0;
    let centre = [blob.pos[0], blob.pos[1] + BLOB_LIFT, blob.pos[2]];
    let flat = (aim[0] * aim[0] + aim[2] * aim[2]).sqrt();
    let corner = |sx: f32, sz: f32, dir: (f32, f32), depth: f32, shift: (f32, f32)| {
        let (x, z) = (sx * width, sz * depth);
        [
            centre[0] + dir.1 * x + dir.0 * z + shift.0,
            centre[1],
            centre[2] - dir.0 * x + dir.1 * z + shift.1,
        ]
    };
    // Sun overhead, or below the ground: the blob lies square under the crown.
    if aim[1] >= -0.02 || flat < 1e-4 {
        return [
            corner(-0.5, -0.5, (0.0, 1.0), width, (0.0, 0.0)),
            corner(-0.5, 0.5, (0.0, 1.0), width, (0.0, 0.0)),
            corner(0.5, 0.5, (0.0, 1.0), width, (0.0, 0.0)),
            corner(0.5, -0.5, (0.0, 1.0), width, (0.0, 0.0)),
        ];
    }
    let dir = (aim[0] / flat, aim[2] / flat);
    let cast = (blob.height * flat / -aim[1]).min(width * BLOB_MAX_STRETCH);
    let depth = width + cast;
    let shift = (dir.0 * cast * 0.5, dir.1 * cast * 0.5);
    [
        corner(-0.5, -0.5, dir, depth, shift),
        corner(-0.5, 0.5, dir, depth, shift),
        corner(0.5, 0.5, dir, depth, shift),
        corner(0.5, -0.5, dir, depth, shift),
    ]
}

/// One cell's blobs as a single mesh.
pub fn blob_mesh(shadows: &[Blob], aim: [f32; 3]) -> MeshData {
    let mut mesh = MeshData::default();
    for blob in shadows {
        mesh.push_quad(blob_corners(blob, aim), [0.0, 1.0, 0.0], [1.0, 1.0, 1.0, 1.0]);
    }
    mesh
}

/// Re-aims a cell's blob mesh in place, which is what a sun two degrees along
/// costs: four positions a tree rewritten, against rebuilding the mesh and its
/// indices, normals, colours and UVs with them. Everything but the positions is
/// the same whatever the sun is doing.
pub fn aim_blobs(shadows: &[Blob], aim: [f32; 3], positions: &mut [[f32; 3]]) {
    for (blob, slot) in shadows.iter().zip(positions.chunks_exact_mut(4)) {
        slot.copy_from_slice(&blob_corners(blob, aim));
    }
}

/// What one horizon's trees come out as: merged cells for the stages worth
/// baking, instanced batches for the cuts that are not, and the blob shadows
/// merged the same way.
#[derive(Default)]
pub struct Batched {
    pub merged: Vec<Merged>,
    pub crowns: Vec<CrownBatch>,
    pub blobs: Vec<Blobs>,
}

/// Groups the placed trees for the renderer.
///
/// `mesh_for` is the canonical mesh of a species at a stage and growth variant.
/// A stage under [`MERGE_MAX_TRIANGLES`] a tree is baked into one mesh a cell;
/// the rest keep their instances, and every tree carries the reach that decides
/// which cuts it is spawned at.
pub fn assemble(
    instances: &[(Preset, CrownInstance)],
    stages: &[Stage],
    mut mesh_for: impl FnMut(Preset, Stage, u32) -> MeshData,
) -> Batched {
    // Only the rasterised cuts vary by growth variant; the crown and the box
    // have one mesh a species.
    fn key_of(preset: Preset, stage: Stage, instance: &CrownInstance) -> (Preset, Stage, u32) {
        (preset, stage, if stage.per_tile().is_some() { instance.variant } else { 0 })
    }

    // One canonical mesh per species, stage and variant, built once here and
    // shared by everything below.
    let mut canonical: HashMap<(Preset, Stage, u32), (MeshData, f32)> = HashMap::new();
    for &(preset, instance) in instances {
        for &stage in stages {
            let key = key_of(preset, stage, &instance);
            canonical.entry(key).or_insert_with(|| {
                let mesh = mesh_for(key.0, key.1, key.2);
                let height = mesh_height(&mesh);
                (mesh, height)
            });
        }
    }

    let merges: HashMap<Stage, bool> = stages
        .iter()
        .map(|&stage| {
            let worst = canonical
                .iter()
                .filter(|((_, s, _), _)| *s == stage)
                .map(|(_, (m, _))| m.triangle_count())
                .max()
                .unwrap_or(0);
            (stage, worst <= MERGE_MAX_TRIANGLES)
        })
        .collect();

    let mut merged: HashMap<(Stage, i32, i32), Merged> = HashMap::new();
    let mut batches: HashMap<(Preset, Stage, u32), CrownBatch> = HashMap::new();
    let mut blobs: HashMap<(i32, i32), Blobs> = HashMap::new();

    for &(preset, instance) in instances {
        for &stage in stages {
            let key = key_of(preset, stage, &instance);
            let (mesh, height) = &canonical[&key];
            if merges[&stage] {
                let span = cell_span(stage);
                let cell = (instance.tile.0.div_euclid(span), instance.tile.1.div_euclid(span));
                let slot = merged.entry((stage, cell.0, cell.1)).or_insert_with(|| Merged {
                    stage,
                    cell,
                    reach: f32::MAX,
                    trees: 0,
                    mesh: MeshData::default(),
                });
                slot.reach = slot.reach.min(instance.reach);
                slot.trees += 1;
                bake(mesh, &instance, *height, &mut slot.mesh);
            } else {
                batches
                    .entry(key)
                    .or_insert_with(|| CrownBatch {
                        preset: key.0,
                        stage: key.1,
                        variant: key.2,
                        mesh_height: *height,
                        mesh: copy(mesh),
                        instances: Vec::new(),
                    })
                    .instances
                    .push(instance);
            }
        }
        // One blob a tree, whichever stage is standing over it.
        let crown = &canonical[&key_of(preset, Stage::Box, &instance)].0;
        let unit = canonical[&key_of(preset, Stage::Box, &instance)].1;
        let radius = crown
            .positions
            .iter()
            .map(|p| p[0].abs().max(p[2].abs()))
            .fold(0.5f32, f32::max)
            * (instance.height / unit);
        let cell =
            (instance.tile.0.div_euclid(BLOB_CELL), instance.tile.1.div_euclid(BLOB_CELL));
        let slot = blobs
            .entry(cell)
            .or_insert_with(|| Blobs { cell, reach: f32::MAX, shadows: Vec::new() });
        slot.reach = slot.reach.min(instance.reach);
        slot.shadows.push(Blob { pos: instance.pos, radius, height: instance.height });
    }

    let mut out = Batched {
        merged: merged.into_values().collect(),
        crowns: batches.into_values().collect(),
        blobs: blobs.into_values().collect(),
    };
    // A stable order, so a rebuild spawns the same things in the same order.
    out.merged.sort_by_key(|m| (m.stage, m.cell));
    out.crowns.sort_by_key(|b| (b.preset.name(), b.stage, b.variant));
    out.blobs.sort_by_key(|b| b.cell);
    out
}

/// A stage's canonical mesh, kept alongside the instances that draw it.
/// `MeshData` is not `Clone` — a chunk's buffers are big enough that copying
/// one should be spelled out.
fn copy(mesh: &MeshData) -> MeshData {
    MeshData {
        positions: mesh.positions.clone(),
        normals: mesh.normals.clone(),
        colors: mesh.colors.clone(),
        uvs: mesh.uvs.clone(),
        indices: mesh.indices.clone(),
        ..Default::default()
    }
}

/// How tall a stage's mesh stands, so an instance can be scaled to the height
/// the scatter gave it whichever stage is drawing.
pub fn mesh_height(mesh: &MeshData) -> f32 {
    mesh.positions.iter().map(|p| p[1]).fold(0.0f32, f32::max).max(0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_mesh() -> MeshData {
        let mut mesh = MeshData::default();
        // A one-tile box standing on zero, two tiles tall.
        mesh.push_quad(
            [[-0.5, 0.0, -0.5], [-0.5, 2.0, -0.5], [0.5, 2.0, -0.5], [0.5, 0.0, -0.5]],
            [0.0, 0.0, -1.0],
            [1.0, 1.0, 1.0, 1.0],
        );
        mesh
    }

    fn tree(x: f32, z: f32, yaw: f32, height: f32) -> CrownInstance {
        CrownInstance {
            pos: [x, 0.0, z],
            height,
            yaw,
            variant: 0,
            tile: (0, 0),
            reach: 0.0,
        }
    }

    /// Baking an instance keeps every vertex and every triangle, and puts each
    /// one exactly where the instance transform would have.
    #[test]
    fn baking_preserves_the_mesh_and_places_it() {
        let src = unit_mesh();
        let mut out = MeshData::default();
        let a = tree(10.0, 20.0, 0.0, 4.0);
        let b = tree(-3.0, 7.0, std::f32::consts::FRAC_PI_2, 2.0);
        bake(&src, &a, 2.0, &mut out);
        bake(&src, &b, 2.0, &mut out);
        assert_eq!(out.positions.len(), src.positions.len() * 2);
        assert_eq!(out.triangle_count(), src.triangle_count() * 2);
        assert_eq!(out.indices.len(), src.indices.len() * 2);
        // The second copy's indices point at its own vertices.
        assert!(out.indices[src.indices.len()..].iter().all(|&i| i as usize >= src.positions.len()));

        // A tree at twice the mesh's height doubles it and stands it on its
        // own spot.
        let foot = out.positions[0];
        assert!((foot[0] - (10.0 - 1.0)).abs() < 1e-4, "{foot:?}");
        assert!((foot[1] - 0.0).abs() < 1e-4, "{foot:?}");
        assert!((foot[2] - (20.0 - 1.0)).abs() < 1e-4, "{foot:?}");
        let top = out.positions[1];
        assert!((top[1] - 4.0).abs() < 1e-4, "a 4-tile tree off a 2-tile mesh: {top:?}");

        // A quarter turn takes the mesh's -z face to -x, about the tree's own
        // position.
        let turned = out.positions[src.positions.len()];
        assert!((turned[0] - (-3.0 - 0.5)).abs() < 1e-4, "{turned:?}");
        assert!((turned[2] - (7.0 + 0.5)).abs() < 1e-4, "{turned:?}");
    }

    /// A baked normal turns with the mesh and stays a unit vector: the scale is
    /// uniform, so it never shears.
    #[test]
    fn baking_turns_the_normals() {
        let src = unit_mesh();
        let mut out = MeshData::default();
        bake(&src, &tree(0.0, 0.0, std::f32::consts::FRAC_PI_2, 6.0), 2.0, &mut out);
        let n = out.normals[0];
        assert!((n[0] - (-1.0)).abs() < 1e-4, "{n:?}");
        assert!(n[1].abs() < 1e-4 && n[2].abs() < 1e-4, "{n:?}");
    }

    /// The reach is zero inside the live window and grows with the gap to it,
    /// measured from the tile's own square.
    #[test]
    fn reach_is_the_gap_to_the_live_window() {
        let live = (0, 0, 144, 144);
        assert_eq!(reach((0, 0), (0, 0), live), 0.0, "a tile inside the window");
        assert_eq!(reach((3, 0), (0, 0), live), 0.0, "a tile touching the window");
        // The fourth region tile out starts at 192, so its near edge is 48
        // tiles clear of the window's east side.
        assert!((reach((4, 0), (0, 0), live) - 48.0).abs() < 1e-4);
        // Diagonally, both gaps count.
        let d = reach((4, 4), (0, 0), live);
        assert!((d - (48.0f32 * 48.0 * 2.0).sqrt()).abs() < 1e-3, "{d}");
    }

    /// A blob lies square under its crown with the sun overhead and stretches
    /// away from the sun as it sets, never past the cap.
    #[test]
    fn a_blob_stretches_along_the_sun() {
        let blob = Blob { pos: [0.0, 0.0, 0.0], radius: 1.0, height: 8.0 };
        let overhead = blob_corners(&blob, [0.0, -1.0, 0.0]);
        let span = |c: [[f32; 3]; 4], axis: usize| {
            let lo = c.iter().map(|p| p[axis]).fold(f32::MAX, f32::min);
            let hi = c.iter().map(|p| p[axis]).fold(f32::MIN, f32::max);
            hi - lo
        };
        assert!((span(overhead, 0) - 2.0).abs() < 1e-4);
        assert!((span(overhead, 2) - 2.0).abs() < 1e-4);
        assert!((overhead[0][1] - BLOB_LIFT).abs() < 1e-4, "the blob lies above the ground");

        // A low sun to the west throws the shadow east and stretches it.
        let low = [1.0f32, -0.25, 0.0];
        let cast = blob_corners(&blob, low);
        assert!(span(cast, 0) > 2.0, "{cast:?}");
        assert!((span(cast, 2) - 2.0).abs() < 1e-3, "the width across the sun is unchanged");
        let centre: f32 = cast.iter().map(|p| p[0]).sum::<f32>() / 4.0;
        assert!(centre > 0.0, "the shadow fell toward the sun: {centre}");

        // And never past the cap, however low the sun gets.
        let grazing = blob_corners(&blob, [1.0, -0.001, 0.0]);
        assert!(span(grazing, 0) <= 2.0 * BLOB_MAX_STRETCH + 2.0 + 1e-3, "{grazing:?}");
    }

    /// Re-aiming rewrites the same positions the mesh would have been built
    /// with, and touches nothing else.
    #[test]
    fn re_aiming_matches_a_rebuild() {
        let shadows = vec![
            Blob { pos: [3.0, 1.0, 4.0], radius: 1.5, height: 9.0 },
            Blob { pos: [-8.0, 2.0, 1.0], radius: 0.8, height: 5.0 },
        ];
        let dawn = [0.9f32, -0.3, 0.2];
        let noon = [0.05f32, -0.99, 0.0];
        let mut mesh = blob_mesh(&shadows, dawn);
        let indices = mesh.indices.clone();
        aim_blobs(&shadows, noon, &mut mesh.positions);
        assert_eq!(mesh.positions, blob_mesh(&shadows, noon).positions);
        assert_eq!(mesh.indices, indices, "the winding never changes");
        assert_eq!(mesh.positions.len(), shadows.len() * 4);
    }
}
