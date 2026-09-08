# Plants: an L-system in place of a stock entity

Status: landed (`crates/dwarf-eye-world/src/{factory.rs,tree.rs,canopy.rs}`,
`crates/dwarf-eye-trees/`) — trees, shrubs, saplings, dead plants, tufts and
in-game streamers (issue #1); ground cover planned (issue #7).

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

A dead tree takes the same path with `VegetationKind::DeadTree` and the
`dead_tree` preset, so it grows bare rather than leafing out:
`Envelope::read` marks the tree dead when the factory classes any of its tiles
`Class::DeadTree`.

`canopy.rs:Forest` caches one voxelised tree per origin.
`canopy.rs:Forest::build_chunk` copies the slice that lands in a chunk into a
`Volume` with a voxel of halo, and `canopy.rs:emit` merges coplanar runs of one
shade into quads, sorted into the material each shade wants.

## Plants that stand in one tile

A shrub, a sapling, a tuft or a dead stem gets exactly one tile from DF, and
that is the whole of DF's contribution. `canopy.rs:sprout` resolves the class
through the factory, takes the preset, paints it in the colour DF gives the
tile (`factory.rs:recolour`; a sapling reads its species' own leaf and bark
tones instead, since a sapling's species is a tree's), grows it at the
preset's natural size and then fits the mesh into the tile:
`canopy.rs:fitted` scales geometry and texture coordinates together so the
surface keeps the map's texel density, and `canopy.rs:tile_local` stands it on
the floor and centres it. Growing a grammar at one tile tall gives a stub;
growing it whole and shrinking it keeps the lab's proportions.

Plants are meshed by the growth crate rather than sliced into the chunk's voxel
volume — a tile's worth of plant at `DETAIL` 4 is a blob — and stamped into the
bark and broadleaf meshes with the same six face shades a sliced tree gets.
`canopy.rs:DEFAULT_PLANT_DETAIL` 2 puts a shrub at about the four voxels a tile
a tree is cut into; `DWARF_EYE_PLANT_VOXELS` raises it.

`canopy.rs:Forest::plant` caches one plant per tile, cleared wholesale past
`PLANT_CACHE` since a plant is a pure function of its tile.
`canopy.rs:Forest::sow` walks a chunk's own 16x16 tiles: a plant never leaves
its tile, so there is no halo to scan and nothing to slice.

## The seed rule

`factory.rs:seed` hashes an absolute tile plus a species, and everything
procedural uses it: `tree.rs:seed` passes the tree's origin tile, a standing
plant passes the tile it stands on. Not render coordinates, so a plant keeps its
shape when the origin moves; not the tile configuration, so it does not change
shape as neighbouring tiles arrive. Issue #7 applies the same rule to ground
cover.

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
- Built work bounds a tree the way the ground does. `Envelope::read` records the
  constructed tiles inside the tree's reach and `tree.rs:envelope` cuts them out
  of the cylinder, so a tree beside a building leans over its roof instead of
  growing through it.
- A strand is cut where its level ends and where it meets built work or rock
  (`canopy.rs:hang`), so a curtain stops at a roof rather than hanging through
  it; its own tree is the exception, since a strand starts inside the crown.
- A standing plant is drawn entirely inside its tile, floor to ceiling, so
  nothing crosses a chunk boundary and nothing leans into a neighbour.
- The lab and the game grow the same tree from the same parameters and seed,
  which is what `make lab` is for.

## What remains

Ground cover (issue #7): grass is still a flat atlas tile, and `Class::TallGrass`
is reachable only from DF's own grassy shrub tiles. `DWARF_EYE_PLANTS=billboard`
keeps the old crossed-sprite path for comparison.

## Related issues

#1 (finish the plant integration), #7 (ground cover as crossed cutout quads),
#5 (the registry these hang off), #8 (exempt whole trees from the cut plane),
#11 (single-letter branch directions).
