# Absolute coordinates and the render origin

Status: landed (`crates/dwarf-eye-world/src/session.rs`).

## What it does

Keeps chunks in one fixed frame while Dwarf Fortress's live window slides under
the character.

## How

DF holds a 144-tile window and reports blocks relative to it. `Session` pins a
render origin at the window's position on connect
(`session.rs:window_origin`, called from `Session::connect_local`) and converts
in both directions: `Session::shift` gives what to add to window-local
coordinates to reach render space, `Session::absolute` and `Session::relative`
convert chunk keys.

Three frames are in play.

| Frame | Unit | Where it appears |
|---|---|---|
| window-local | blocks in x/y, levels in z | `BlockRequest`, `BlockList` |
| render | blocks in x/y, levels in z | `World` keys, mesh positions, camera |
| absolute | world tiles in x/y, elevation in z | disk cache keys, tree seeds |

`REGION_TILE` is 48: `MapInfo::block_pos_x/y` count 48-tile mid-level tiles, so
the window origin in tiles is that times 48, and `block_pos_z` is already a
z-level.

Bevy is y-up and DF is z-up, so the mesher swaps y and z
(`mesh.rs:build_chunk_budgeted`) and scales the vertical by `mesh.rs:Z_SCALE`,
currently `1.0`.

## Invariants and gotchas

- Anything that must survive a reload is keyed absolutely: cache filenames
  (`cache.rs:Cache::path`), column floors (`cache.rs:store_floors`) and tree
  seeds (`tree.rs:seed`, which adds the render origin back on).
- Nothing seeds from render coordinates or from the tile configuration around
  a feature. A tree keeps its shape when the origin moves and when neighbouring
  blocks arrive later.
- Walk mode reads absolute tiles on its own connection and places them against
  the same origin, published through `worker.rs:Bridge::origin`, a `OnceLock`
  set as soon as the map thread connects.
- A window move invalidates every local coordinate, so the next request is
  forced (`worker.rs:collect`, `Session::refresh_window`).
- Elevation as DF displays it is `view_pos_z()` plus `block_pos_z() - 100`. The
  renderer works in raw z-levels and does not subtract the 100
  ([docs/dfhack-horizon-notes.md](../../dfhack-horizon-notes.md)); the horizon
  builder applies the offset where it compares against region elevations.

## Related issues

#21 (retention and slab order relative to the character), #23 (column floors
are absolute).
