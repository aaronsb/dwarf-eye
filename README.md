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

### Colour

DF's `state_color` describes a material as a substance, not as terrain: loam is
grey, grass plants are brown, willow is orange. `palette.rs` therefore takes
ground cover, liquids and wood from the tiletype's material *class*, and lets
stone, ore and constructions keep their own colour so granite still reads
differently from marble.

The pink is rock salt. That one is DF's own colour, and it is correct.

## Not done yet

- Units, buildings and items are fetched but not drawn.
- Ramps are half-height blocks rather than wedges; the tile's facing direction
  is available in `Tiletype::direction` and unused.
- Fortifications draw as plain cubes.
- Water and magma get vertex alpha, but the material is opaque, so they render
  solid.
- No greedy meshing — every visible face is its own quad.
- Every fetch that returns anything remeshes the whole loaded set.
- No LOD, so the load radius is what keeps the triangle count down.
