# Level of detail

Status: fine chunks landed (`crates/dwarf-eye-world/src/mesh.rs`); the four
canopy bands landed (`crates/dwarf-eye-world/src/canopy.rs`, issue #10); the
far band from region data landed (`crates/dwarf-eye-world/src/horizon/`, issue
#31); the bands and the far band's instances now come off one chain in the
factory (`factory.rs:Chain`, [../factory/README.md](../factory/README.md), issue
#5); the far band's trees draw through true GPU instancing
(`crates/dwarf-eye/src/instancing.rs`, issue #34); mid terrain heightfield
planned (issue #10); seam skirt past the region
details planned (issue #9).

## What it does

Draws three tiers of ground: full voxel detail where DF has tiles, nothing yet
in between, and, out to the horizon, a heightfield from the region and world
maps carrying forests, rivers and sites. Both the fine and the coarse tier
follow `DWARF_EYE_GROUND`: smooth by default, terraced under `stepped`. Crowns have four bands of their
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
density. Every band renders the same objects, off one chain the factory resolves
(`factory::chain`): L-system trees and bushes cut at four, three, two and one
voxel to a tile, then one canonical crown per species
(`dwarf_eye_trees::crown`), then its bounding box (`crown_box`), and past the
scatter's reach the canopy colour baked into the heightfield; building prefabs
from site footprints or from construction tiles; water and rivers as surfaces.
Approximation lives in the placement rule, seeded from absolute coordinates, so
a horizon tree keeps its place as the camera approaches and is replaced in place
when fine data arrives. **The stage is chosen by projected size on screen, never
by which source fed it** — a chunk mesher draws the tree DF reports and the
horizon draws a canonical growth, but both at the cut the camera's distance
asks for, so the window's boundary does not show in the canopy.

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

The far band draws **the same chain**, all six stages of it, swapped by the
camera's distance to each tree rather than by the window's centre
([horizon.md](horizon.md)). Detail is that distance and never which survey a
tree came from, so a tree just outside the live window is cut exactly as
coarsely as one just inside it and no more.

Canopy bands, all built from the same growth and spawned together. A band *is*
a stage of `factory::chain(Class::Tree, ..)`: its resolution, its cut, what
rides with it and where it ends are all read off that stage
(`canopy.rs:Band::stage`). The edge is where that band's own leaf voxel falls to
the leaf-pixel threshold (`factory::Stage::edge`, through `canopy.rs:Band::edge`); the
crossfade is that edge widened to the distance the dither needs
(`main.rs:band_fades`). The tile figures are Bevy's 45-degree lens into a
720-tall window:

| Band | Voxels per tile | Ends at | Crossfade | Carries | Meshes per chunk | Material |
|---|---|---|---|---|---|---|
| near | 4 (`tree.rs:DETAIL`) | N, 73 tiles | 63..83 | trees, plants, tufts, strands | up to 4 | bark, broadleaf and needle cutouts, leaflet strip |
| close | 3 (`canopy.rs:CLOSE_DETAIL`) | 4N/3, 97 tiles | 85..110 | trees and strands | up to 4 | the same four |
| mid | 2 (`canopy.rs:MID_DETAIL`) | 2N, 145 tiles | 121..174 | trees and strands | up to 4 | the same four |
| far | 1 (`canopy.rs:FAR_DETAIL`) | far plane | — | trees only | 1 | one opaque leaf material, bark included |

The steps are 4, 3, 2, 1 rather than 4, 2, 1: no hand-off doubles the voxel, and
the first of them — nearest the eye, and the one a player standing in a wood is
most likely to be watching — is the gentlest. The close and mid bands are
reduced crowns still wearing the cutout, so sun and sky keep coming through a
canopy well past the first hand-off; only the far band trades the holes away.
`canopy.rs:Band::slot` is what merges the far band into one mesh: one mesh on
one material is one entity and one draw call for a chunk's whole crown, and at
that distance there is no bark grain or leaf hole left to tell apart.
`canopy.rs:Band::coats` says what each mesh wears.

## Matching the look across a hand-off

A voxel cannot be thinner than itself. Past the trunk a limb is a thread the
rasteriser draws half a voxel across (`raster.rs:THREAD`), so a coarse cut draws
every twig as wide as its own cell and the crown fills with bark the fine cut
never showed. Measured over one oak crown in the tree lab, the bark showing
through the canopy ran 1.4% of the crown's pixels at the near band, 2.1% at
three voxels a tile, 3.8% at two and 13.0% at one — and with it the crown's mean
luminance fell 22% between the near band and the far one. That is the jump a
hand-off shows.

The correction is one parameter: `trees::Cut::wood_like`, the resolution whose
limb widths the cut should show. A limb the cut would fatten is kept only as
often as its true width asks for, drawn from its place in the world so a twig
one band drops every coarser band drops as well. Every band cuts its wood like
`DETAIL` (`canopy.rs:Band::cut`), which brings the bark to 1.9%, 3.1% and 7.9%
and the far band's crown to within 16% of the near band's luminance, and takes
about a fifth of the far band's triangles with it.

Foliage needs no such correction, and the same measurement is what says so: a
crown is cells deep, so the layer behind closes whatever hole a thinner fill
opens. Thinning the leaves to 75% moved a crown's mean colour under 1% and its
see-through fraction by a point, on a dense oak and on an open pine alike. What
is left after the wood is corrected is lighting, not geometry: the mid band
reads 6% brighter than the near band under a high front sun and 0.4% brighter
under a low back one, because its larger faces catch a flatter light. A fixed
per-band tint would fix one sun and break the other, so there is none.

The figures come from `target/release/tree-lab` with `TREE_LAB_PRESET=oak
TREE_LAB_SEED=7 TREE_LAB_CAM=32,2,45,0,25` and `DWARF_EYE_SHOT`, one shot per
`TREE_LAB_VPT`, compared over a rectangle inside the crown. `TREE_LAB_WOOD` sets
the cut's reference resolution, which is how the uncorrected column above was
measured.

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

with `MIN_LEAF_PIXELS` 3, `DWARF_EYE_LEAF_PIXELS` overriding (`canopy.rs:near_band`,
`leaf_pixels`). At Bevy's default 45-degree lens that is 73 tiles (4.6 blocks)
into a 720-tall window and 120 tiles (7.5
blocks) into a 1190-tall one: a taller window or a longer lens pushes the band
out, which is the point of measuring in pixels. `DWARF_EYE_LOD_NEAR` overrides
it, in blocks.

Every later hand-off is the same rule on that band's own leaf voxel, which is
`DETAIL / detail` times as wide and so stays at the threshold that many times further
out (`canopy.rs:Band::edge`): the close band reaches 4N/3, the mid band's
half-tile voxel 2N, 145 tiles at 720, and the far band runs from there to the
camera's far plane.

The bands are a list, not a pair, and the list is the factory's:
`canopy.rs:BANDS` names the four entries of `factory::Chain::window` nearest
first, `main.rs:band_edges` is `factory::edges` over that run — one handover
distance per gap — and `main.rs:stage_ranges` turns those into one
`VisibilityRange` per band. A coarser stage is one more entry in the chain, one
more mesh per chunk from the worker, and nothing else: the edges follow from the
detail. `main.rs:horizon_ranges` is the same two calls over
`factory::Chain::instanced`, so the window and the horizon hand over by the same
numbers.

`main.rs:size_bands` recomputes N from the window and the camera's own
projection, and rewrites the ranges already on the GPU when either changes.
Bevy does the swapping: each canopy entity carries a `VisibilityRange` — near
`0..N`, close `N..4N/3`, mid `4N/3..2N`, far `2N..far` — and each hand-off
shares one margin, so one band dithers into the next rather than popping. The
ranges measure from the mesh's bounds (`use_aabb: true`), since chunk meshes
hold world-space vertices at an identity transform and would otherwise all sit
at the world origin.

That margin straddles its edge rather than starting at it, and reaches
`main.rs:CROSSFADE`, 0.45, of the way to whichever neighbouring edge is nearer,
measured as a ratio (`main.rs:band_fades`). The blend is a screen-space dither,
so it only reads as a fade if the camera spends real distance inside it: the
first hand-off now dithers over 28 tiles rather than the 16 a flat `E..E*1.15`
gave, and the last over 80. Under a half of the gap either way, two margins can
never run into each other however close together the edges fall, which is what
Bevy needs — a range's start margin has to be over before its end margin begins
— and is also the difference between a band fading in and a band flickering.

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
| [instancing.md](instancing.md) | true GPU instancing: one storage buffer of trees, a compute cull per view, indirect draws through the main pass, the prepass and the cascades |
| [horizon.md](horizon.md) | the terraced far band: region and world maps, the ground sprites it wears, the window's own tree chain drawn as instances, rivers, sites, the block mask and the stitched seam |

## Invariants and gotchas

- The block mask is the whole of the arbitration between tiers. Fine geometry
  never yields; the coarse mesh discards over any block whose fine chunks reach
  the ground, and at the rim of it a coarse cell is drawn as a fan whose
  fine-facing edge follows the fine tiles' own z-levels tile by tile
  (`horizon/stitch.rs`), so the two surfaces are one plane.
- `worker.rs:grounded_blocks` decides that: the lowest loaded chunk of a column
  must be at least half non-empty. A sparse lowest chunk is canopy with no
  ground under it.
- Retention is horizontal only, so a mid tier has to keep the same rule.
- A tree is cached per origin **and** per stage (`canopy.rs:Forest.trees`, a
  `factory::StageCache`), and every cut comes off one growth: the skeleton is
  the expensive half, so rasterising four times costs a fraction of growing four
  times. Retiring a tree takes all of its cuts.
- The worker builds every band for every chunk it meshes and ships them in one
  `Event::Chunks` entry; a chunk arrives whole or not at all.
- The close and mid bands drop plants and tufts but keep the strands, which are
  quads the growth crate has already meshed.
- A band's cut is `canopy.rs:Band::cut`, and every one of them asks for the near
  band's limb widths. A coarse cut left to itself draws each twig a cell wide,
  which is what makes a hand-off jump; the foliage fill is not the lever it
  looks like.
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
#6 (closed, the smoothed fine tier that turned the terracing off and kept the
boundary snap), #31 (the far band, landed).
