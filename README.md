# dwarf-eye

A Minecraft-style voxel view of a **live** Dwarf Fortress map, fed by DFHack's
RemoteFortressReader plugin.

![grassland](docs/surface.png)

## Requirements

Dwarf Fortress with DFHack (the Steam build ships it). Verified against DF
53.16 / DFHack 53.16-r1.1 / RemoteFortressReader 0.21.0.

DFHack's remote server listens on `127.0.0.1:5000` out of the box. The
`allow_remote` flag in `dfhack-config/remote-server.json` only chooses the bind
address, so a local client needs no configuration.

## Running

Start Dwarf Fortress, load a fort or an adventurer, then:

```sh
cargo run --release -p dwarf-eye
```

| Key | |
|---|---|
| `WASD` | move |
| `Q` / `E` | down / up |
| `Shift` | 4x speed |
| right-drag | look |
| wheel | move speed |
| `[` `]` | lower / raise the cut plane |
| `H` | show or hide undiscovered tiles |

`DWARF_EYE_Z_OFFSET=-8` starts the cut plane eight levels below the player, for
looking straight into the rock.

![cutaway](docs/cutaway.png)

## Layout

| Crate | |
|---|---|
| `dfhack-remote` | the DFHack RPC protocol: handshake, method binding, generated protobuf types |
| `dwarf-eye-art` | reads DF's own sprite sheets and the raws that index them |
| `dwarf-eye-world` | decodes map blocks into voxels and meshes them; no engine dependency |
| `dwarf-eye` | the Bevy renderer |

The DFHack connection runs on its own thread and owns the world, because face
culling needs neighbouring chunks and meshing next to the data beats shipping
the data across. The render thread only uploads finished vertex buffers.

### Protocol notes

Both headers are raw C structs, so byte layout matters:

- Handshake: 12 bytes — `DFHack?\n` then a little-endian `i32` version of 1.
  The server answers `DFHack!\n` and its version.
- Message header: 8 bytes — `i16` id, **two bytes of struct padding**, `i32` size.
- Method id 0 is `BindMethod`; every other id is negotiated by name.
- A `RPC_REPLY_FAIL` header carries the error code in the size field and has no
  payload.

`BlockRequest` bounds are in 16-tile **blocks** for x/y and z-levels for z, but
`MapBlock.map_x`/`map_y` come back in **tiles**.

`.proto` files under `crates/dfhack-remote/proto/` are vendored from DFHack at
tag `53.16-r1.1`.

### Sprites as voxels

![sprite-derived trees](docs/sprites.png)

A DF tile sprite is drawn looking straight down, so its opaque region is a
horizontal cross-section of whatever fills the tile. `TREE_TRUNK_PILLAR` is a
disc, so extruding its alpha mask gives a round trunk — the shape is already in
the art, and nothing has to be modelled.

Four treatments, chosen from `TiletypeShape`:

| Mode | Tiles | Geometry |
|---|---|---|
| Extrude | trunks, cap walls | full-height mask |
| Thin extrude | branches, twigs | a slab through the middle |
| Billboard | saplings, shrubs, boulders | two crossed vertical planes |
| Flat tile | floors, pebbles | a textured slab *(not wired yet)* |

Caps come from vertical continuity: a trunk with more trunk above it has no
visible top, so that face is skipped. Models are cached per tiletype, species
and cap pair, then stamped into the chunk mesh.

`DWARF_EYE_GRID` sets sub-voxels per tile edge (default 12, range 4–32).

Resolving a tile to a sprite closes a four-link chain:

```
DFHack tiletype  TreeTrunkPillar + direction "--------"
material index   419:203  ->  plant raw WILLOW
graphics raw     [PLANT_GRAPHICS:WILLOW]
                 [TREE_TILE:TREE_TRUNK_PILLAR:TREE_WILLOW:11:12]
tile page        [TILE_PAGE:TREE_WILLOW] images/tree_willow.png, 32x32
```

Only 20 of 72 tree species ship their own sheet; the rest fall back to the
generic `TILE_GRAPHICS` table, which spells absent connections in lowercase
(`TREE_TRUNK_S_nwe` is the same tile as `TREE_TRUNK_S`).

DFHack's tiletype names and the raws' family names drifted apart, so
`library.rs` carries an alias table: DFHack says `TreeBranches`, the raws say
`TREE_BRANCH`; roots resolve to the environment sheet's `ROOT_WALL`. Families
such as `ROOT_WALL` ship only directional variants, so a lookup that matches
neither the tile's connections nor an undirected entry falls through to the
closest variant by direction bits.

`cargo run --release -p dwarf-eye-world --example coverage` reports which tiles
in view get a sprite and which fall back to plain blocks.

### Colour

DF's `state_color` describes a material as a substance, not as terrain: loam is
grey, grass plants are brown, willow is orange. `palette.rs` therefore takes
ground cover, liquids and wood from the tiletype's material *class*, and lets
stone, ore and constructions keep their own colour so granite still reads
differently from marble.

The pink is rock salt. That one is DF's own colour, and it is correct.

## Not done yet

- Terrain still draws as flat-coloured blocks. The sprites are there —
  `PEBBLES_FLOOR_1..5`, `GRASS_1..5`, `BOULDER`, `ENGRAVED_STONE_WALL` — but
  they use a third naming convention that needs its own tiletype mapping.
- Units, buildings and items are fetched but not drawn.
- Ramps are half-height blocks rather than wedges; the tile's facing direction
  is available in `Tiletype::direction` and unused.
- Fortifications draw as plain cubes.
- Water and magma get vertex alpha, but the material is opaque, so they render
  solid.
- No greedy meshing — every visible face is its own quad.
- Every fetch that returns anything remeshes the whole loaded set.
- No LOD, so the load radius is what keeps the triangle count down.
