# Ramp sheets

Status: landed (`crates/dwarf-eye-world/src/ramp.rs`,
`crates/dwarf-eye-world/src/library.rs`, `crates/dwarf-eye-world/src/palette.rs`).

## What it does

Puts DF's own ramp artwork on a slope whose shape came from the walls around it.

A **natural** slope no longer draws a wedge: the ground heightfield subsumes it
([../pipeline/meshing.md](../pipeline/meshing.md)). What survives here is the
corner rule, which the heightfield takes as its height samples at a ramp tile,
and the sheets themselves, which a constructed ramp still wears. In stepped
mode (`DWARF_EYE_GROUND=stepped`) every ramp is a wedge again, which is what
this page describes.

## How

A terrain ramp carries no direction: DFHack reports `dir=--------`. The mesher
builds an eight-neighbour wall mask from `ramp.rs:NEIGHBOURS`
(`mesh.rs:build_chunk_budgeted`) and asks `library.rs:ramp(tile, mask, sand)`
for geometry, where `sand` is the specific tile's own sand hue, read off
`world.rs:Voxel::sand` ([README.md](README.md#soil-families)). The mask keys
both the geometry cache and DF's own sprite name, so the picture on the slope
always agrees with the shape underneath it.

`ramp.rs:family_for` chooses the sheet by tiletype material: `STONE_RAMP` for
stone, mineral, lava stone, feature, construction, HFS, root and tree material;
nothing for soil, so it wears the flat ground beside it instead, taken from
`library.rs:ground_under` — unless the specific tile's material is one of the
five sands, in which case `library.rs:ramp` asks for `ramp.rs:sand_family(hue)`
ahead of `family_for`, since which hue (if any) a soil ramp is comes from the
material, not the tiletype. `ramp.rs:sprite_name` builds the sheet's own name
for a mask, `WITH_WALL_N_S_E_W` for all four cardinals, else the cardinals
present in N S W E order plus the diagonals no cardinal already covers, else
`OTHER`. `library.rs:pack_ramps` packs all 47 distinct sprites of each wanted
family at load, `STONE_RAMP` and, unconditionally alongside any soil ramp
tiletype, all five sand families. `ramp.rs:is_flat` sends a ramp with no wall
neighbour to a plain ground slab.

## Invariants and gotchas

- Grass and soil ramp sheets exist but are not used: DF bakes a deep shadow into
  them that reads as a pit. `ramp.rs:neutralise` greys a sheet that is used as a
  pattern so the tile's material supplies the colour. A soil ramp — sand
  included — therefore already wears exactly the ground beside it and the
  renderer's own light, the same rule and the same shading a soil floor gets;
  what looks darker on a slope than on the flat is the surface normal facing
  away from the sun, not a texture difference (issue #3).
- DF ships six `SAND_*_RAMP` sheets, not five: `TAN`, `YELLOW`, `WHITE`,
  `BLACK` and `RED` match a material each (`palette.rs:sand_hue_from_id`);
  `BEIGE` matches none and is never packed. Unlike `STONE_RAMP` the sand
  sheets are full colour, not a pattern, so `library.rs:pack_ramps` never
  neutralises them — `ramp.rs:sand_family` is a separate call from
  `family_for`, packed with `tinted: false` always.
- There is no `FROZEN_FLOOR_5` sprite at all. DF's natural ice floor is
  `ROUGH_ICE_FLOOR` on the `FLOOR_ICE` page; asking for a frozen name left every
  ice ramp standing on nothing.
- A constructed sprite name is run back through `raws::parse_part` before
  lookup, because the parser eats a trailing direction group off every part
  name.
- A ramp's corners rise to the surface of the floor slab a level up, not to the
  level boundary, so walking off one is flush. Walk mode reads the same
  fractions out of `ramp.rs:slopes` and adds the same `FLOOR_HEIGHT`
  ([../walk/README.md](../walk/README.md)).
- Adjacent ramps read as steps, because a ramp is not a wall and so does not
  raise its neighbour's corner. That is why the heightfield does not stop at
  the ramp tile: it takes `ramp.rs:slopes` as the samples for that tile and
  smooths the whole sheet, so two ramps in a row become one grade
  (`heightfield.rs:ground_of`). The wedge itself only appears now under a
  constructed ramp or in stepped mode.

## Claims to verify

[docs/vox-uristi-notes.md](../../vox-uristi-notes.md) describes the same
neighbour-derived height field and records the differences: vox-uristi
voxelises a whole z-level so its ramp spans the level exactly, while dwarf-eye
rides the 0.12 floor slab. Its note that the skirt has no counterpart there is
correct; the skirt is dwarf-eye's own.

## Related issues

#3, landed: the top lip and the striped skirt (623de8c, 083ce1f), the ice
floor and the soil shading (837c67a), and the sand sheets — for ramps, walls
and floors together, since a sand tile is the same material index problem in
all three ([README.md](README.md#soil-families)). #6 (closed, the heightfield
that took the natural slope over).
