# Level of detail

Status: fine chunks landed (`crates/dwarf-eye-world/src/mesh.rs`); near, mid and
far canopy bands landed (`crates/dwarf-eye-world/src/canopy.rs`, issue #10); the
far band from region data landed (`crates/dwarf-eye-world/src/horizon/`, issue
#31); mid terrain heightfield planned (issue #10); seam skirt past the region
details planned (issue #9).

## What it does

Draws three tiers of ground: full voxel detail where DF has tiles, nothing yet
in between, and, out to the horizon, a terraced heightfield from the region and
world maps carrying forests, rivers and sites. Crowns have three bands of their
own, because the canopy is where the triangles are: about five thousand a chunk,
nearly all of them alpha-masked leaf voxels.

## Principle

One instance vocabulary, three data sources chosen by availability per
location:

- cached fine tiles where the character has been;
- the region tile (48-tile pitch: elevation, biome, vegetation, rainfall, tree
  and plant materials, rivers, site footprints) where not;
- the world tile (768-tile pitch) beyond the region details.

The factory classifies from whichever source it has, so a tree is a tree whether
it came from a tiletype or from a region tile's tree materials and vegetation
density. Every band renders the same objects: L-system trees and bushes at the
resolution the band needs, in five stages — full voxels, quarter-resolution
opaque, one canonical crown per species
(`dwarf_eye_trees::crown`), its bounding box (`crown_box`), and finally the
canopy colour baked into the heightfield; building prefabs from site footprints or from construction tiles;
water and rivers as surfaces. Approximation lives in the placement rule, seeded
from absolute coordinates, so a horizon tree keeps its place as the camera
approaches and is replaced in place when fine data arrives. The band is chosen
by projected tile size on screen, not by which source fed it.

## The tiers

| Tier | Source | Spacing | Status |
|---|---|---|---|
| fine | `GetBlockList` and the disk cache | 1 tile | landed |
| mid | simplified cached chunks | 4 tiles | planned, issue #10 |
| near coarse | `GetRegionMapsNew`, interpolated | 6 tiles | landed |
| middle coarse | `GetRegionMapsNew`, interpolated | 24 tiles | landed |
| region | `GetRegionMapsNew` | 48 tiles | landed |
| far world | `GetWorldMap` | 768 tiles | landed |

The three coarse bands are terraced to whole z-levels, so they step the way fine
tiles do; only the world grid, where a level is under a pixel, stays smooth.

Canopy bands, all built from the same growth and spawned together. The edge is
where that band's own leaf voxel falls to two pixels (`canopy.rs:Band::edge`);
the tile figures are Bevy's 45-degree lens into a 720-tall window:

| Band | Voxels per tile | Ends at | Carries | Meshes per chunk | Material |
|---|---|---|---|---|---|
| near | 4 (`tree.rs:DETAIL`) | N, 109 tiles | trees, plants, tufts, strands | up to 4 | bark, broadleaf and needle cutouts, leaflet strip |
| mid | 2 (`canopy.rs:MID_DETAIL`) | 2N, 217 tiles | trees and strands | up to 4 | the same four |
| far | 1 (`canopy.rs:FAR_DETAIL`) | far plane | trees only | 1 | one opaque leaf material, bark included |

The mid band is a half-resolution crown still wearing the cutout, so sun and sky
keep coming through a canopy well past the first hand-off; only the far band
trades the holes away. `canopy.rs:Band::slot` is what merges the far band into
one mesh: one mesh on one material is one entity and one draw call for a chunk's
whole crown, and at that distance there is no bark grain or leaf hole left to
tell apart. `canopy.rs:Band::coats` says what each mesh wears.

Fine terrain stays at full detail however far away, out to
`worker.rs:RETAIN_RADIUS` 40 blocks horizontally. What remains of issue #10 is
the terrain mid tier: a surface-only 4-tile heightfield coloured from the top
voxels, keeping the block mask over it, after which retention can grow.

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

Every later hand-off is the same rule on that band's own leaf voxel, which is
`DETAIL / detail` times as wide and so stays two pixels that many times further
out (`canopy.rs:Band::edge`): the mid band's half-tile voxel reaches 2N, 217
tiles at 720, and the far band runs from there to the camera's far plane.

The bands are a list, not a pair: `canopy.rs:BANDS` orders them nearest first,
`main.rs:band_edges` gives one handover distance per gap and
`main.rs:band_ranges` turns those into one `VisibilityRange` per band. A coarser
stage — a canonical crown per species, a green box — is one more entry in
`BANDS`, one more mesh per chunk from the worker, and nothing else: the edges
follow from the detail.

`main.rs:size_bands` recomputes N from the window and the camera's own
projection, and rewrites the ranges already on the GPU when either changes.
Bevy does the swapping: each canopy entity carries a `VisibilityRange` — near
`0..N`, mid `N..2N`, far `2N..far` — and each hand-off shares its margin
`E..E*1.15` so one band dithers into the next rather than popping. The ranges
measure from the mesh's bounds (`use_aabb: true`), since chunk meshes hold
world-space vertices at an identity transform and would otherwise all sit at the
world origin.

Terrain and water carry no `VisibilityRange` at all: they are drawn wherever
they are retained, and the bands beyond that are the heightfield's job.

## GPU occlusion culling

The camera carries `OcclusionCulling` alongside the `DepthPrepass` the clouds
already need, so Bevy splits the prepass in two and tests each mesh's bounding
box against a depth pyramid before transforming its vertices
(`main.rs:setup`). `DWARF_EYE_OCCLUSION=0` turns it off, which is how its worth
was measured.

Bevy ignores `OcclusionCulling` on a device whose GPU preprocessing cannot
cull, and says nothing about it, so `main.rs:report_culling` reads
`GpuPreprocessingSupport` in the render app and logs one line at startup:
`GPU preprocessing available; occlusion culling on`.

Full detail outside the live window is not obtainable. DF discards the local map
on offload and regenerates it from region details, and no DFHack call reaches
tiles beyond `world.map.block_index`
([docs/dfhack-horizon-notes.md](../../dfhack-horizon-notes.md)).

## Pages

| Page | |
|---|---|
| [horizon.md](horizon.md) | the terraced far band: region and world maps, crowns, rivers, sites, the block mask and the stitch |

## Invariants and gotchas

- The block mask is the whole of the arbitration between tiers. Fine geometry
  never yields; the coarse mesh discards over any block whose fine chunks reach
  the ground, and at the rim of it a coarse cell snaps to the fine column's own
  z-level (`horizon/terrace.rs:cell_level`), so the two surfaces are one plane.
- `worker.rs:grounded_blocks` decides that: the lowest loaded chunk of a column
  must be at least half non-empty. A sparse lowest chunk is canopy with no
  ground under it.
- Retention is horizontal only, so a mid tier has to keep the same rule.
- A tree is cached per origin **and** per resolution (`canopy.rs:Forest.trees`),
  and every cut comes off one growth: the skeleton is the expensive half, so
  rasterising three times costs a fraction of growing three times. Retiring a
  tree takes all of its cuts.
- The worker builds every band for every chunk it meshes and ships them in one
  `Event::Chunks` entry; a chunk arrives whole or not at all.
- The mid band drops plants and tufts but keeps the strands, which are quads the
  growth crate has already meshed.
- Only the far band's leaves are opaque: a cutout costs a masked pass, a discard
  in the depth prepass and the overdraw behind every hole, which is worth paying
  while a hole is still about a pixel and not after.
- A crossfading range is not free: Bevy compiles every mesh that carries one
  with `VISIBILITY_RANGE_DITHER`, which discards, whenever it draws and not
  only inside the margin. An abrupt range would keep the far band's opaque
  shader opaque, at the cost of popping.
- `cargo run --release -p dwarf-eye-world --example budget` reports where the
  triangles go, and `--example horizon` reports what DFHack knows beyond the
  window.

## Related issues

#10 (mid detail), #9 (the seam skirt where the terraces meet the world grid),
#6 (a smoothed fine tier would turn the terracing off and keep the boundary
snap), #31 (the far band, landed).
