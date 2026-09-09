# Walls from the environment sheets

Status: landed (`crates/dwarf-eye-world/src/wall.rs`,
`crates/dwarf-eye-world/src/library.rs`, `crates/dwarf-eye-world/src/mesh.rs`).

## What it does

Draws a wall cube with DF's own wall sheet on its top face and a face derived
from the same sprite on its four sides, tinted by the tile's material the way DF
tints its grey sheets.

## How

DF only ever sees a wall from above. Each sheet under
`data/vanilla/vanilla_environment/graphics/images/` holds fifteen sprites named
by the neighbouring walls the tile joins — `STONE_WALL_N` through
`STONE_WALL_N_S_W_E`, indexed by `graphics_tiles.txt` — drawn as a four-level
dither over four greys, to be multiplied by the material's colour at draw time
(issue #26). Sheets whose tile has four random cuts number them `_1` to `_4`;
`wall.rs:sprite_names` asks for `_1` first and the plain name after, so both
spellings resolve without a table of which sheet is which.

`library.rs:pack_walls` fills sixteen atlas cells per sheet, once, at load:

- the fully connected sprite is flattened onto a darker cast of its own mean
  (`wall.rs:backdrop`, `wall.rs:opaque`) — DF composites over the black of an
  unlit map, which is right from above and too dark for a lit face;
- each of the fifteen variants is drawn over that (`wall.rs:over`), so the
  connection detail sits on solid rock rather than on a hole;
- the side face is the same fully connected sprite with DF's middle filled in
  (`wall.rs:fill_hole`, `wall.rs:side_strip`).

`mesh.rs:build_chunk_budgeted` reads the four-neighbour wall mask off the map,
asks `TileLibrary::wall_skin` for the pair of rects, and emits
`MeshData::textured_cuboid`: the variant on the lid, the side face on all four
walls, one cell of texture per z-level.

## The mapping

| Tiletype class | DFHack material, special | Sheet | Family |
|---|---|---|---|
| rough stone, lava stone, feature, glowing barrier | Stone / LavaStone / Feature / Hfs, Normal or Worn1-3 | `wall_stone.png` | `STONE_WALL` |
| smoothed stone, smoothed vein | Stone / LavaStone / Feature / Hfs / Mineral, Smooth | `wall_stone_smoothed.png` | `SMOOTHED_STONE_WALL` |
| soil, sand | Soil, any | `wall_soil.png` | `SOIL_WALL` |
| mineral vein | Mineral, Normal or Worn1-3 | `wall_ore_vein.png` | `ORE_VEIN_WALL` |
| constructed wall, pillar, fortification | Construction, any | `wall_rock_blocks.png` | `ROCK_BLOCKS_WALL` |
| natural ice | FrozenLiquid, Normal or Worn1-3 | `wall_ice.png` | `ICE_WALL` |
| smoothed ice | FrozenLiquid, Smooth | `wall_ice_smoothed.png` | `SMOOTHED_ICE_WALL` |
| semi-molten rock | Magma, any | `wall_magma.png` | `MAGMA_WALL` |
| constructed floor, shoddy floor, track floor | Construction, any, floor shape | `floor_stone_blocks.png` | `FLOOR_STONE_BLOCK` |

Cells: 8 families x (15 variants + 1 side) = 128, taking the atlas from 181 to
309 of its 1024 (`cargo run -p dwarf-eye-world --example atlas`).

A built floor is the same masonry seen from above, and DF ships it as a sheet of
its own. `library.rs:construction_floor` gives every constructed floor —
`ConstructedFloor`, the four shoddy cuts, the sixteen track variants —
`floor_stone_blocks.png`, one shared atlas cell (the 310th), tinted by the
construction's material like the wall. Where the slab faces a drop,
`mesh.rs:strip_foot` skirts it with the bottom sliver of `ROCK_BLOCKS_WALL`'s own
side strip, at the wall's texel density, so the edge of a roof is the course of
blocks the wall below it would have shown rather than a white skirt. The tiletype never
says what the thing was built from, so a wooden roof is blocks in wood colour;
see the built-floor note in [README.md](README.md).

The first five sheets are near-grey and get the material's colour damped, as the
ground does; ice and magma carry their own and take only the tile's brightness
(`Sprite::saturation` against `library.rs:PATTERN_SATURATION`).

## The side-face rule

DF ships no side, and nothing on a sheet can stand in for one: the north, south,
east and west bands of `wall_stone` differ by two luma levels out of 255, so no
band is a lit face. The face is therefore the fully connected sprite — the only
variant that covers the whole tile — with the hole DF leaves in its middle
filled from half a tile away, and its dither flattened onto rock.

It is deliberately not mirrored to hide the seam between tiles. Mirroring buys a
seamless cliff on the sheets that are pure noise and wrecks the ones that are
not: a smoothed or block wall is drawn as a bordered face, and folding it into
four turns a course of masonry into a kaleidoscope, while the border it already
has reads as the mortar between blocks.
`cargo run -p dwarf-eye-world --example walls` writes both the contact sheet and
a four-by-two cliff of each face; it needs a DF install, not a game.

## Invariants and gotchas

- The neighbour mask is the mesher's own four-neighbour scan, not DF's
  `Tiletype::direction`. Only smoothed and constructed walls carry a direction
  at all, and it links a wall to the masonry it was cut with rather than to the
  rock it stands in, so a smoothed wall against a rough one reports an open side
  where a voxel view shows solid stone.
- DF ships no sprite for a wall with nothing beside it — it draws that tile from
  four corner pieces, which one quad cannot carry — so a lone wall takes the
  fully connected variant (`wall.rs:variant`).
- `wall_stone_engraved.png` is unreachable. An engraving lives in the block's
  engraving list, not in the tiletype, so nothing here can ask for it.
- The three worn stone sheets differ from the plain one only in surface noise
  and are not worth 48 cells; `Worn1` to `Worn3` take `STONE_WALL`.
- The corner sprites on every sheet — `STONE_WALL_NE` and its three siblings,
  distinct from `STONE_WALL_N_E` — are overlays DF paints on top of a tile whose
  diagonal neighbour is a wall. A single textured quad cannot carry them.
- A tree's trunk, roots and mushroom cap all wear a wall shape; `wall.rs`
  refuses them by name and material so the sprite library keeps them.
- Fortifications take the wall skin of their material. DF's own fortification
  sheets are cut for slits seen from above and would need geometry, not a
  texture.

## Related issues

#13 (this), #26 (the grey-base-plus-tint library this follows), #16 (greedy
meshing would merge wall faces and change the UV rule).
