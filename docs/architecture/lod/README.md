# Level of detail

Status: fine chunks landed (`crates/dwarf-eye-world/src/mesh.rs`); coarse
horizon landed (`crates/dwarf-eye-world/src/horizon.rs`); mid detail planned
(issue #10); seam skirt planned (issue #9).

## What it does

Draws three tiers of ground: full voxel detail where DF has tiles, nothing yet
in between, and a coarse heightfield from the region and world maps out to the
horizon.

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
