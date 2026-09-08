# Notes from vox-uristi

[vox-uristi](https://github.com/plule/vox-uristi) reads the same
RemoteFortressReader API and exports MagicaVoxel files. It is GPL-3.0 with no
Cargo.toml license field and no separate asset licence, so its `.vox` prefabs
are covered by the repo licence and reusable here.

Its geometry unit is `3 x 3 x 5` voxels per DF tile (`BASE = 3`, `HEIGHT = 5`,
`src/coords.rs:13`). `Box3D` is indexed `[z][y][x]` and **index 0 is the top**.
Row 0 of each slice is north. dwarf-eye's grid is configurable, so these shapes
want translating rather than copying.

## Ramps, without a direction

vox-uristi never decides which way a ramp faces. `ramp_levels`
(`src/export/tile.rs:21`) builds a 3x3 height field from the eight neighbours
and lets the slope fall out of it:

```rust
// 6 for a wall, 1 for anything else. 6 exceeds HEIGHT so the column saturates.
let c = map.neighbouring_8flat(coords, |o| o.block_tile.as_ref()
    .map(|t| t.ramp_contact_height()).unwrap_or(1));
let nw = c.nw.max(c.n).max(c.w);
let ne = c.ne.max(c.n).max(c.e);
let sw = c.sw.max(c.s).max(c.w);
let se = c.se.max(c.s).max(c.e);
[[nw,        (nw + ne) / 2, ne       ],
 [(nw + sw) / 2, max / 2,   (ne + se) / 2],
 [sw,        (sw + se) / 2, se       ]]
```

Then `box_from_levels`: `levels[y][x] > z`. `is_wall()` counts `Wall` and
`Fortification`; a missing tile contributes 1.

Each corner takes the max of its diagonal and the two cardinals flanking it, so
an inside-corner ramp rises on two sides. The ramp meets the wall above it
seamlessly with no orientation table.

The limitation they accepted: ramps are not walls, so a run of adjacent ramps
stays flat and reads as steps. `RampTop` draws nothing and only feeds the
hidden-tile computation.

## Fortifications and stairs

Fortifications (`generic.rs:174`) use four-neighbour wall connectivity:

```rust
slice = [[true,   conn.n, true  ],
         [conn.w, false,  conn.e],
         [true,   conn.s, true  ]];
shape = [slice, slice, full, full, full];
```

Bottom three voxel layers solid, top two pierced. The centre is always open, the
corners always solid, and edges fill only where a wall continues — so a run
reads as a parapet with a continuous slot that opens at the ends.

Stairs are a hardcoded five-layer helix rotated by `(z % 4)`, so stacked stairs
spiral and treads on consecutive levels line up.

Floors, boulders and pebbles all collapse to the same single bottom slice.

## Building prefabs

44 hand-authored `.vox` files under `assets/buildings/`, plus `Workshop/`,
`Furnace/`, `Trap/` and `SiegeEngine/`. A building's `BuildingDefinition.id()`
string maps straight to the file path, so most need no configuration.
`assets/prefabs.yaml` is 37 lines of exceptions with four keys — `model`,
`orientation`, `content`, `connectivity`.

**The palette index is a slot number, not a colour** (`prefabs.rs:170`):

| index | meaning |
|---|---|
| 0-7 | build materials, cycled |
| 8-15 | the same, darkened 20% in HSV |
| 16-23 | content materials |
| 24, 25, 26 | fire, wood, light |
| anything else | **the voxel is deleted** |

Reusing the assets without reproducing this convention renders garbage. The
deletion rule is what lets a prefab authored with three content slots draw
correctly on a bookcase holding one book.

Orientation is either DF's own `direction()`, a "wallyness" argmax over the 3x3
(`map.rs:200`), or a search for an adjacent building whose id is literally
`"Chair"`. Connectivity trims voxels after placement: `SelfOrWall` deletes the
far side of unconnected directions, which is how a door frame appears only where
it meets a wall.

Multi-tile buildings tile the prefab: first and last 3x3 cells become the ends,
middle cells repeat modulo `prefab_sx - 2`. A test asserts every model's
`size.z % HEIGHT == 0`.

Buildings stream inside **every** block list, repeatedly. They guard with a
`buildings_added` flag or get duplicates. Filtering skips
`building.room.is_some()` and requires `BuildingFlags::EXISTS` (0x1).

Neither units nor free-standing items are rendered at all; items appear only as
building contents.

## Protocol semantics

- **The `direction` field is a string, and its two forms mean opposite things.**
  One letter is the direction a branch is *heading*, so connectivity is the
  opposite; two or more letters is connectivity directly
  (`tree.rs:407`). `"--------"` is matched literally as a sentinel.
- **The graphics raws and DFHack disagree on what a heavy limb is called.**
  DFHack names the tiletype `TreeTrunkBranch`, which `family_from_tiletype`
  turns into `TREE_TRUNK_BRANCH`; the raws only ship `TREE_HEAVY_BRANCH`. So
  those tiles resolve to no sprite at all and fall back to a plain block. The
  canopy mesher folds them into the crown instead, but a sprite path for them
  still wants the alias.
- **Tree origin flips sign on z.** `tree_origin()` is
  `(x - tree_x, y - tree_y, z + tree_z)` — subtract on x and y, add on z.
- **Flow coordinates are global** even though flows live inside a block;
  everything else in a block is local.
- **Elevation is offset by 100**: `map_info.block_pos_z() - 100`, and the
  elevation DF displays is `view_pos_z()` plus that.
- **Spatter `amount` has three scales** by `MatterState`: 0-10000 solid, 0-255
  liquid, 0-100 powder.
- **Fire is a tiletype material**, not a flow.
- Material flags such as `IS_METAL` need two calls, neither on
  RemoteFortressReader: `core().list_materials()` with
  `BasicMaterialInfoMask{flags: true, reaction: true}` yields flag indices, and
  `core().list_enums()` turns those into names.
- `reset_map_hashes()` governs what RFR will resend; without it an already-sent
  block comes back empty.
- Their noise is seeded from the tile's global coordinates so re-received blocks
  do not resample and shimmer.
- Their material lookup is a linear scan of `material_list` per MatPair, cached.
  dwarf-eye keys a HashMap instead.

Liquids are `box_from_levels(water.clamp(2, 7))` — a flat column with no surface
shaping or neighbour blending, so levels 5, 6 and 7 all render full.
