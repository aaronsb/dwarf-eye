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
