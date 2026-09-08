# Plants: an L-system in place of a stock entity

Status: trees landed (`crates/dwarf-eye-world/src/tree.rs`,
`crates/dwarf-eye-world/src/canopy.rs`, `crates/dwarf-eye-trees/`, 62a35e4);
shrubs, saplings, dead trees and in-game streamers in flight (issue #1); ground
cover planned (issue #7).

## What it does

Replaces DF's tile-by-tile plant geometry with a grown plant, inside the bounds
DF gives it.

## How

DF's branch tiles are a coarse connectivity graph, not a description of limbs,
so the tiles are used only to say where the plant is allowed to be.
`tree.rs:Envelope::read` collects every tile whose `tree_origin` is this tree,
anchored at the origin rather than at any chunk, and derives per-level
footprints, centres, radii and the crown's lowest level.

`tree.rs:params` picks a preset from `dwarf-eye-trees` and lets the game
override only size: the height is the envelope's levels plus
`tree.rs:headroom`, the limb fraction is capped by `tree.rs:cap_radius` over the
height, and leaf and bark tones come from the species' own sprites.
`tree.rs:envelope` hands the generator a cylinder of `tree.rs:extent`, a bound
on gross overshoot rather than a mould. `tree.rs:grow` then calls
`dwarf_eye_trees::grow` and `rasterise` at `tree.rs:DETAIL` 4 sub-voxels a tile.

`canopy.rs:Forest` caches one voxelised tree per origin.
`canopy.rs:Forest::build_chunk` copies the slice that lands in a chunk into a
`Volume` with a voxel of halo, and `canopy.rs:emit` merges coplanar runs of one
shade into quads, sorted into the material each shade wants.

## The seed rule

`tree.rs:seed` hashes the tree's absolute origin tile plus its species. Not
render coordinates, so a tree keeps its shape when the origin moves; not the
tile configuration, so a tree does not change shape as neighbouring tiles
arrive. Issue #7 applies the same rule per tile for ground cover, hashing the
absolute tile and species.

## Invariants and gotchas

- A tree is grown and voxelised once and sliced per chunk, so a chunk boundary
  cannot change its shape.
- `canopy.rs:OVERHEAD` 24 lets the top chunk of a column carry voxels above it,
  so a crown is not shorn off at the ceiling of what has been sent.
- A tree whose top level sits against the edge of the loaded map is not short,
  it is unfinished: `Envelope::read` marks it `truncated` and carries the last
  footprint up to the height the raws give the species.
- `Forest::retire_near` forgets trees near arrived blocks, so a tree that can
  now be seen further up is regrown.
- `Volume::intern` keys a palette slot on colour and surface together, or bark
  would be drawn with the leaf cutout.
- Crowns may overlap neighbouring non-tree tiles by half a tile, by design.
- The lab and the game grow the same tree from the same parameters and seed,
  which is what `make lab` is for.

## What remains

From issue #1: shrubs, saplings and dead trees still draw as crossed billboards
from `model.rs`, though `VegetationKind::Shrub`, `Sapling` and `DeadTree` work
in the lab. The game path needs a per-tile plant cache in `Forest` keyed by the
absolute tile and species, and a skip in `mesh.rs`. Streamers are matched by a
plant id containing `WILLOW` and are lab-verified only; a streamer quad through
a hidden level is cut. All of it under the factory.

## Related issues

#1 (finish the plant integration), #7 (ground cover as crossed cutout quads),
#5 (the registry these hang off), #8 (exempt whole trees from the cut plane),
#11 (single-letter branch directions).
