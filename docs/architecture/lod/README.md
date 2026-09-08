# Level of detail

Status: fine chunks landed (`crates/dwarf-eye-world/src/mesh.rs`); coarse
horizon landed (`crates/dwarf-eye-world/src/horizon.rs`); mid detail in flight
(issue #10); far band from region data planned (issue #31); seam skirt planned
(issue #9).

## What it does

Draws three tiers of ground: full voxel detail where DF has tiles, nothing yet
in between, and a coarse heightfield from the region and world maps out to the
horizon.

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
resolution the band needs, from full voxels down to one canonical crown per
species; building prefabs from site footprints or from construction tiles;
water and rivers as surfaces. Approximation lives in the placement rule, seeded
from absolute coordinates, so a horizon tree keeps its place as the camera
approaches and is replaced in place when fine data arrives. The band is chosen
by projected tile size on screen, not by which source fed it.

## The tiers

| Tier | Source | Spacing | Status |
|---|---|---|---|
| fine | `GetBlockList` and the disk cache | 1 tile | landed |
| mid | simplified cached chunks | 4 tiles | planned, issue #10 |
| coarse ring | `GetRegionMapsNew` | 48 tiles | landed |
| far world | `GetWorldMap`, interpolated | 768 tiles | landed |

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
| [horizon.md](horizon.md) | region and world maps, rivers, sites, the block mask and the seam |

## Invariants and gotchas

- The block mask is the whole of the arbitration between tiers. Fine geometry
  never yields; the coarse mesh discards over any block whose fine chunks reach
  the ground.
- `worker.rs:grounded_blocks` decides that: the lowest loaded chunk of a column
  must be at least half non-empty. A sparse lowest chunk is canopy with no
  ground under it.
- Retention is horizontal only, so a mid tier has to keep the same rule.
- `cargo run --release -p dwarf-eye-world --example budget` reports where the
  triangles go, and `--example horizon` reports what DFHack knows beyond the
  window.

## Related issues

#10 (mid detail), #9 (confirm rivers and sites, then the seam skirt), #6
(heightfield ground would change what the fine tier is).
