# Level of detail

Status: fine chunks landed (`crates/dwarf-eye-world/src/mesh.rs`); the far band
from region data landed (`crates/dwarf-eye-world/src/horizon/`, issue #31); mid
detail in flight (issue #10); seam skirt past the region details planned
(issue #9).

## What it does

Draws three tiers of ground: full voxel detail where DF has tiles, nothing yet
in between, and, out to the horizon, a terraced heightfield from the region and
world maps carrying forests, rivers and sites.

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

Fine chunks stay at full detail however far away, out to
`worker.rs:RETAIN_RADIUS` 40 blocks horizontally. Issue #10 is the mid tier: a
surface-only 4-tile heightfield coloured from the top voxels beyond about 12
blocks, keeping the block mask over it, after which retention can grow.

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
- `cargo run --release -p dwarf-eye-world --example budget` reports where the
  triangles go, and `--example horizon` reports what DFHack knows beyond the
  window.

## Related issues

#10 (mid detail), #9 (the seam skirt where the terraces meet the world grid),
#6 (a smoothed fine tier would turn the terracing off and keep the boundary
snap), #31 (the far band, landed).
