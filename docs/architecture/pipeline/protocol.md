# Protocol and session

Status: landed (`crates/dfhack-remote/src/{client.rs,protocol.rs,methods.rs}`,
`crates/dwarf-eye-world/src/session.rs`).

## What it does

Speaks DFHack's remote RPC over TCP, binds the RemoteFortressReader methods by
name, and pulls the raws that stay fixed for the life of a world.

## How

`Client::connect_local` opens `127.0.0.1:5000` and performs the handshake.
`Client::call` and `Client::call_empty` send a protobuf request under an
8-byte header and decode the reply. `Client::run_command` reaches DFHack's own
console, which is how the viewer sets the clock and the weather.
`Session::connect_local` calls `GetVersionInfo`, `GetMapInfo`,
`GetTiletypeList` and `GetMaterialList` once, builds the `Palette`, and opens
the disk cache.

`.proto` files under `crates/dfhack-remote/proto/` are vendored from DFHack at
tag `53.16-r1.1` and compiled by `crates/dfhack-remote/build.rs`.

## Which connection carries what

Four sockets to DFHack, each with one job, because DFHack answers one request at
a time per connection and a small poll behind a large fetch is a poll wasted.

| Connection | Thread | Carries | Cadence |
| --- | --- | --- | --- |
| worker | `dfhack` | `GetMapInfo`, `GetViewInfo`, `GetBlockList`, `GetRegionMapsNew`, `GetWorldMap`, the raws and the atlas, `RunCommand` for the test keys | per fetch pass |
| walk | `dfhack-walk` | the adventurer's position and the movement commands (`walk.rs`) | per step |
| units | `dfhack-units` | `GetMapInfo`, `GetViewInfo`, `GetUnitList` (`units.rs`) | 250 ms |
| polls | `dfhack-polls` | the clock and weather Lua probes (`polls.rs`) | 1 s and 10 s |

Only the worker's connection asks for map blocks, and the change-set rule below
is why that matters: a second connection asking for blocks would keep its own
set of hashes.

## The clock and weather polls

Status: landed (`crates/dwarf-eye/src/polls.rs`, issue #4).

Both are `RunCommand("lua", …)` one-liners read back out of `last_notices`
(`clock.rs:PROBE`, `weather.rs:PROBE`), answered in milliseconds. They were
worker commands until issue #4: a heavy first pass held them for the whole 16 to
21 s it ran, and the sun stayed at midnight. `polls.rs:Schedule` is the cadence
and the back-off, on `Duration`s rather than `Instant`s so it is tested on
synthetic time; `polls.rs:run` is the loop that drives it.

A refused probe holds *both*, because `CR_LINK_FAILURE` is the connection's
answer and not one probe's: the wait doubles from one second to thirty, and the
refusal is logged once rather than once a second. When the map comes back, the
back-off is forgotten and one line says so.

The fallbacks are the same ones the worker had. No script interpreter, or no
world data, and the clock comes from `GetWorldMapCenter` and the sky from
`GetWorldMap`'s cloud kinds (`polls.rs:from_world_map`). A world with no year is
a world that is not loaded, so it counts as a refusal rather than midnight.

The one thing the poll cannot answer for itself is whether there is a roof over
the camera, because that is a voxel lookup in the worker's `World`. The worker
writes it to a shared `AtomicBool` at the end of each pass (`polls::SkyOpen`) and
the weather poll reads it.

## The unit poll

Status: landed (`crates/dwarf-eye/src/units.rs`, issue #15).

Creatures move several times a second and the map does not, so the census gets
a connection of its own — the third, after the worker's and walk mode's. A poll
queued behind a slab of blocks is a poll wasted, and a fetch pass waiting on a
poll is worse.

`UnitFeed::spawn` opens the connection and loops at 250 ms: `GetMapInfo` for
where the window sits, `GetViewInfo` for `follow_unit_id` (which is the
adventurer), and `GetUnitList` for everyone. `units.rs:parse_units` turns the
reply into absolute tiles, which is the only form worth sending: DF reports a
unit in window-local tiles, the same as a map block, so the same creature is at
two different positions either side of a re-centring. `main.rs:poll_units` and
`main.rs:draw_units` place the census against the render origin and ease each
unit from the last one to this one over a poll period.

Three RPCs at 4 Hz, none of them large: `GetUnitList` for 176 units is tens of
kilobytes. Nothing about the map pass changes, because nothing about it is
shared.

## Invariants and gotchas

- Both headers are raw C structs. The handshake is 12 bytes, `DFHack?\n` plus a
  little-endian `i32` version of 1; the message header is 8 bytes, an `i16` id,
  **two bytes of struct padding**, then an `i32` size.
- Method id 0 is `BindMethod` and id 1 is `RunCommand`; neither needs binding.
- An `RPC_REPLY_FAIL` header carries the error code in the size field and has
  no payload.
- DFHack refuses any reply over **64 MiB**, so block requests go in slabs of
  500 blocks (`session.rs:Session::fetch`).
- DFHack answers `CR_LINK_FAILURE` when no map is loaded, which is what travel
  mode and loading screens look like from here. The worker reports it as
  "waiting for the map" and retries (`worker.rs:run`); the poll connection backs
  off, doubling to thirty seconds, rather than asking once a second
  (`polls.rs:Schedule::failed`).
- An unforced `GetBlockList` returns only blocks whose hash changed since the
  last request **on this connection**. Walk mode's second connection therefore
  has its own change set.
- `BlockRequest` bounds are in 16-tile blocks for x/y and z-levels for z, but
  `MapBlock.map_x`/`map_y` come back in **tiles** (`world.rs:World::decode`).
- An incremental reply can carry a block with no tile array at all, when a unit
  moved or a liquid shifted. `World::absorb` skips those rather than replacing a
  decoded chunk with an empty one.
- Console output from `run_command` lands in `Client::last_notices`, which is
  how the Lua clock and weather probes are read back (`polls.rs`,
  `clock.rs:PROBE`, `weather.rs:PROBE`). `last_notices` is cleared at the start
  of every call, so it must be read before the next request on that connection.
- **`MapBlock.buildings` is not the block's buildings.** Every reply carries
  every building whose footprint falls inside the box that was *asked for*,
  hung off one arbitrary block of the reply — 106 instances on one block, and
  none on the other 199 — in the same window-local tiles the blocks are placed
  by. `world.rs:read_buildings` reads the list off the whole reply and
  `world.rs:stamp_buildings` writes it onto that reply's chunks, whose box is
  the same box. A building repeats across blocks, so `BuildingInstance::index`
  is what makes it one building, and `building_flags & 1` (`EXISTS`) separates
  a building from a plan.
- **`UnitDefinition::isValid` is declared and never filled.** Filtering on it
  drops every unit on the map. Only an explicit `false` means anything.
- **`GetCreatureRaws` does not decode.** DFHack writes creature descriptions
  straight out of the raws and some of them are not UTF-8, which fails the
  whole reply in prost. Race colours fall back to a stable hash per creature
  index (`units.rs:hashed_color`).

## Claims to verify

The repo [README](../../../README.md) documents the same header layout and the
block-versus-tile mismatch; both agree with the code.
[docs/vox-uristi-notes.md](../../vox-uristi-notes.md) lists further protocol
semantics that dwarf-eye has not all adopted, tracked as issue #12. Its note
that `reset_map_hashes()` governs resends is about the same change-set
behaviour; dwarf-eye forces a reload instead (`force_reload` in
`session.rs:Session::fetch`).

## Related issues

#12 (protocol gotchas from the vox-uristi report), #4 (landed for the clock and
the weather; the view centre is still read inside the fetch pass, where the pass
needs it anyway).
