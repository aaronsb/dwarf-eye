# Level of detail

Status: fine chunks landed (`crates/dwarf-eye-world/src/mesh.rs`); coarse
horizon landed (`crates/dwarf-eye-world/src/horizon.rs`); near and mid canopy
bands landed (`crates/dwarf-eye-world/src/canopy.rs`, issue #10); mid terrain
heightfield planned (issue #10); seam skirt planned (issue #9).

## What it does

Draws three tiers of ground: full voxel detail where DF has tiles, nothing yet
in between, and a coarse heightfield from the region and world maps out to the
horizon. Crowns are the exception: they already have two bands, because the
canopy is where the triangles are — about five thousand a chunk, nearly all of
them alpha-masked leaf voxels.

## The tiers

| Tier | Source | Spacing | Status |
|---|---|---|---|
| fine | `GetBlockList` and the disk cache | 1 tile | landed |
| mid | simplified cached chunks | 4 tiles | planned, issue #10 |
| coarse ring | `GetRegionMapsNew` | 48 tiles | landed |
| far world | `GetWorldMap`, interpolated | 768 tiles | landed |

Canopy bands, both built from the same growth and spawned together:

| Band | Voxels per tile | Carries | Meshes per chunk | Material |
|---|---|---|---|---|
| near | 4 (`tree.rs:DETAIL`) | trees, plants, tufts, strands | up to 4 | bark, broadleaf and needle cutouts, leaflet strip |
| mid | 1 (`canopy.rs:MID_DETAIL`) | trees only | 1 | one opaque leaf material, bark included |

`canopy.rs:Band::slot` is what merges the mid band into one mesh: one mesh on
one material is one entity and one draw call for a chunk's whole crown, and at
that distance there is no bark grain or leaf hole left to tell apart.
`canopy.rs:Band::coats` says what each mesh wears.

## The projected-size rule

Which band a chunk draws is decided by how large a near-detail leaf voxel would
be on screen, not by raw distance. A voxel is a tile over `DETAIL` on a side,
and a length `L` at distance `d` covers `L * h / (2 d tan(fov/2))` pixels of a
viewport `h` pixels tall, so the band ends at

    N = L * h / (2 * MIN_LEAF_PIXELS * tan(fov/2))

with `MIN_LEAF_PIXELS` 2 (`canopy.rs:near_band`). At Bevy's default 45-degree
lens that is 109 tiles (6.8 blocks) into a 720-tall window and 180 tiles (11.3
blocks) into a 1190-tall one: a taller window or a longer lens pushes the band
out, which is the point of measuring in pixels. `DWARF_EYE_LOD_NEAR` overrides
it, in blocks.

`main.rs:size_bands` recomputes N from the window and the camera's own
projection, and rewrites the ranges already on the GPU when either changes.
Bevy does the swapping: each canopy entity carries a `VisibilityRange`, near
`0..N` and mid `N..far`, sharing the margin `N..N*1.15` so one band dithers into
the other rather than popping. The ranges measure from the mesh's bounds
(`use_aabb: true`), since chunk meshes hold world-space vertices at an identity
transform and would otherwise all sit at the world origin.

Terrain and water carry no `VisibilityRange` at all: they are drawn wherever
they are retained, and the bands beyond that are the heightfield's job.

## GPU occlusion culling

The camera carries `OcclusionCulling` alongside the `DepthPrepass` the clouds
already need, so Bevy splits the prepass in two and tests each mesh's bounding
box against a depth pyramid before transforming its vertices
(`main.rs:setup`). `DWARF_EYE_OCCLUSION=0` turns it off, which is how its worth
was measured.

Fine terrain stays at full detail however far away, out to
`worker.rs:RETAIN_RADIUS` 40 blocks horizontally. What remains of issue #10 is
the terrain mid tier: a surface-only 4-tile heightfield coloured from the top
voxels, keeping the block mask over it, after which retention can grow.

Full detail outside the live window is not obtainable. DF discards the local map
on offload and regenerates it from region details, and no DFHack call reaches
tiles beyond `world.map.block_index`
([docs/dfhack-horizon-notes.md](../../dfhack-horizon-notes.md)).

## Pages

| Page | |
|---|---|
| [horizon.md](horizon.md) | region and world maps, rivers, sites, the block mask and the seam |

## Invariants and gotchas

- The block mask is the whole of the arbitration between tiers. Fine geometry
  never yields; the coarse mesh discards over any block whose fine chunks reach
  the ground.
- `worker.rs:grounded_blocks` decides that: the lowest loaded chunk of a column
  must be at least half non-empty. A sparse lowest chunk is canopy with no
  ground under it.
- Retention is horizontal only, so a mid tier has to keep the same rule.
- A tree is cached per origin **and** per resolution (`canopy.rs:Forest.trees`),
  and both cuts come off one growth: the skeleton is the expensive half, so
  rasterising twice costs a fraction of growing twice. Retiring a tree takes
  both cuts.
- The worker builds both bands for every chunk it meshes and ships them in one
  `Event::Chunks` entry; a chunk arrives whole or not at all.
- The mid band drops plants, tufts and strands, and its leaves are opaque: a
  cutout costs a masked pass, a discard in the depth prepass and the overdraw
  behind every hole, for holes that are under a pixel there.
- A crossfading range is not free: Bevy compiles every mesh that carries one
  with `VISIBILITY_RANGE_DITHER`, which discards, whenever it draws and not
  only inside the margin. An abrupt range would keep the mid band's opaque
  shader opaque, at the cost of popping.
- `cargo run --release -p dwarf-eye-world --example budget` reports where the
  triangles go, and `--example horizon` reports what DFHack knows beyond the
  window.

## Related issues

#10 (mid detail), #9 (confirm rivers and sites, then the seam skirt), #6
(heightfield ground would change what the fine tier is).
