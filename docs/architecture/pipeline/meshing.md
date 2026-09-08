# Meshing

Status: landed (`crates/dwarf-eye-world/src/mesh.rs`,
`crates/dwarf-eye-world/src/canopy.rs`, `crates/dwarf-eye/src/main.rs`).

## What it does

Turns one decoded chunk into triangle arrays, split by the material each surface
wants.

## How

`mesh.rs:build_chunk` walks the chunk's 256 tiles, culls faces against
neighbouring chunks through `World::voxel`, and emits either a sprite-derived
model or a plain cuboid. `MeshData` holds positions, normals, colours, UVs and
indices; the renderer copies them straight into a Bevy mesh
(`main.rs:to_bevy_mesh`). `mesh.rs:build_chunk_budgeted` is the same pass with a
`Budget` tally, which `make budget` prints.

Crowns are meshed separately by `canopy.rs:Forest::build_chunk` into
`CanopyMeshes`, four buffers: bark, broadleaf, needle, streamers. Each becomes
its own entity with its own material (`main.rs:CanopyMaterials`,
`main.rs:upload_chunks`).

Shading is baked into vertex colour per face (`mesh.rs:shade`), material colour
is pulled toward its own brightness (`mesh.rs:damp`), and a per-tile hash gives
a brightness wobble so a hillside of one material is not a painted plane
(`mesh.rs:jitter`).

## Geometry per tile

| `Solid` | Geometry |
|---|---|
| `Cube`, `Fortification` | full cuboid, faces culled by `occluded` |
| `Floor` | slab `FLOOR_HEIGHT` 0.12 thick, rim faces only at a drop |
| `Ramp` | wedge from the ramp sheet, or a half-height block with no sprite |
| `Stair` | two stacked boxes |
| `Foliage` | inset box, 0.85 tall |
| liquids | box whose height is the fill level over 7, vertex alpha, opaque material |

## Invariants and gotchas

- Sideways, an unloaded neighbour counts as open, so a chunk border keeps its
  walls; below the lowest loaded level nothing is ever seen
  (`mesh.rs:build_chunk_budgeted`, the `occluded` closure).
- A floor above hides the top of whatever it rests on, which is the `lid` case
  in the same closure.
- Tiles that belong to a tree are skipped here outright: `TileLibrary::is_trunk`
  or `TileLibrary::canopy_part` returning something means the canopy path owns
  that tile. Those two early-outs are what the entity factory replaces
  ([../factory/README.md](../factory/README.md), issue #5).
- A trunk with more trunk above it drops its top cap (`model.rs:Caps`). This is
  what keeps a forest affordable.
- Terrain ramps carry no direction, so the high side comes from whichever of
  `ramp.rs:NEIGHBOURS` is a wall.
- Water and magma get vertex alpha but the material is opaque, so they render
  solid. That is issue #16, along with greedy-merging terrain cubes; the merge
  already exists for crowns in `canopy.rs:emit` and in `dwarf-eye-trees::mesh`.
- Natural ground is stepped terraces, not a heightfield. Issue #6.
- Walls draw flat-coloured. `SoilWall` alone was 73,666 tiles in one view, which
  is issue #13.

## Related issues

#5 (registry lookups in place of the scattered checks), #6 (smoothed
heightfield), #13 (wall textures), #16 (transparency and greedy meshing), #10
(mid LOD).
