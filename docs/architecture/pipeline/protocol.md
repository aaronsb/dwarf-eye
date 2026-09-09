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
  "waiting for the map" and retries (`worker.rs:run`).
- An unforced `GetBlockList` returns only blocks whose hash changed since the
  last request **on this connection**. Walk mode's second connection therefore
  has its own change set.
- `BlockRequest` bounds are in 16-tile blocks for x/y and z-levels for z, but
  `MapBlock.map_x`/`map_y` come back in **tiles** (`world.rs:World::decode`).
- An incremental reply can carry a block with no tile array at all, when a unit
  moved or a liquid shifted. `World::absorb` skips those rather than replacing a
  decoded chunk with an empty one.
- Console output from `run_command` lands in `Client::last_notices`, which is
  how the Lua clock probe is read back (`worker.rs`, `clock.rs:PROBE`).
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

#12 (protocol gotchas from the vox-uristi report), #4 (a second light
connection for polls).
