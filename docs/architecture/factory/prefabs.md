# Voxel prefab instances

Status: planned (issues #5 and #15). Nothing on main draws a building.

## What it will do

Let a class resolve to a hand-authored voxel model stamped at the entity's DF
extent, the way trees resolve to a grown one.

## The source

[vox-uristi](https://github.com/plule/vox-uristi) reads the same
RemoteFortressReader API and ships 44 hand-authored `.vox` files under
`assets/buildings/`, plus `Workshop/`, `Furnace/`, `Trap/` and `SiegeEngine/`.
It is GPL-3.0 with no separate asset licence, so the prefabs are covered by the
repo licence and reusable here. A building's `BuildingDefinition.id()` maps
straight to the file path, and `assets/prefabs.yaml` holds 37 lines of
exceptions under four keys: `model`, `orientation`, `content`, `connectivity`.

## What the intended design has to carry over

From [docs/vox-uristi-notes.md](../../vox-uristi-notes.md), verified against
that repository:

- **A palette index is a slot number, not a colour.** 0 to 7 are build materials
  cycled, 8 to 15 the same darkened 20% in HSV, 16 to 23 content materials, 24,
  25 and 26 fire, wood and light, and any other index deletes the voxel.
  Reusing the assets without reproducing this convention renders garbage.
- Their geometry unit is 3x3x5 voxels per DF tile, `Box3D` is indexed `[z][y][x]`
  and index 0 is the top, row 0 north. dwarf-eye's grid is configurable
  (`library.rs:DEFAULT_GRID` 12, `tree.rs:DETAIL` 4), so the shapes want
  translating rather than copying.
- Orientation is DF's own `direction()`, or an argmax over the 3x3 of how walled
  each side is, or a search for an adjacent building of a given id.
- Connectivity trims voxels after placement, which is how a door frame appears
  only where it meets a wall.
- Multi-tile buildings tile the prefab: the first and last 3x3 cells are the
  ends and the middle cells repeat.
- Buildings stream inside **every** block list, repeatedly. A consumer guards
  with an added flag or gets duplicates, and skips `building.room.is_some()`
  while requiring `BuildingFlags::EXISTS`.

## Invariants a prefab treatment must keep

- The instance occupies the building's DF extent and no more, the same rule
  trees follow.
- Palette slots resolve against the building's own materials, so one prefab
  serves every material the game builds it from.
- A prefab is a treatment like any other: registered per class, resolved by the
  factory, meshed into the chunk that holds it
  ([registering.md](registering.md)).

## Related issues

#15 (draw units, items and buildings; units start as capsules), #5 (the registry
and its per-class overrides), #12 (the protocol semantics the same report
records).
