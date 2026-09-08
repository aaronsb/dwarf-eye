# Disk cache and column floors

Status: landed (`crates/dwarf-eye-world/src/cache.rs`,
`crates/dwarf-eye-world/src/session.rs`); periodic re-probe in flight (issue #23).

## What it does

Keeps the land the character has walked past, which the game itself no longer
holds, and remembers how deep each block column is worth asking about.

## How

`Cache::open` places one directory per world under
`~/.cache/dwarf-eye/<world>-<save>/`, with `DWARF_EYE_CACHE` overriding the
root. One file per chunk, `<bx>_<by>_<z>.chunk`: a 4-byte magic `DEC2` then 256
voxels of 19 bytes (`cache.rs:Cache::store`, `cache.rs:read_chunk`). Keys are
absolute. `Session::persist` writes every chunk that arrived;
`Session::restore_cache` reads them back on connect and places them through
`World::restore`, which never overwrites a chunk the game has already supplied.

Alongside the chunks sits `floors`, one line per block column giving the level
at which that column turned to unrevealed rock, versioned by its first line
`def1` (`cache.rs:load_floors`, `cache.rs:store_floors`).

Column floors are the fetch depth. `Session::fetch` descends in slabs and
`Session::open_box` narrows the request box to the columns still open. The
bookkeeping lives in `session.rs:Floors`, one stop and one deepest-seen level
per column, with these rules:

- Close: a column with no floor closes at the highest wholly hidden level
  strictly below the deepest level it has ever seen. A standing floor never
  moves on its own.
- Overhang: a hidden block over ground already seen is not a floor.
- Reopen: ground seen at or under the floor drops it, and the same descent
  chases that column down until rock resumes.
- Retract: the deepest block seen coming back wholly hidden erases that memory,
  so the column closes where it was.
- Probe: every 30 s (`PROBE_EVERY`) an unforced pass asks under every floor to
  the bottom of the window. Hidden blocks are weighed and dropped, never cached
  or meshed. When nothing changed the probe returns zero blocks.
- Lateral: a sighting deeper than a four-neighbour's floor probes that neighbour
  to the same level in the same pass.
- The character's own block column and its eight neighbours drop their floors
  when the character's z reaches them. Forced and first passes take no probe.

## Invariants and gotchas

- A chunk whose 256 tiles are all hidden is never stored and is deleted on
  restore. That rock is the same under every world and it grew the cache to
  thirty times the walked map.
- A column whose lowest cached chunk is sparse holds canopy with no ground under
  it. Those files are removed on restore and refetched.
- The floor test on restore reads the chunk's own tiles, not the sidecar, so a
  stale `floors` file cannot delete land somebody has seen.
- A `floors` file from another version reads as nothing known, not as an error.
- DFHack gates tiles and designations by separate hashes, so a block can arrive
  with tiles and no `hidden` array. `session.rs:seen_into` treats a missing
  array as unknown; reading it as revealed reopened columns over untouched rock.
- Ground revealed without being cut arrives with seen bits and no tiles. Those
  blocks are re-asked forced, one small request per level.
- The change hashes belong to the plugin, not the connection: a second viewer on
  the same game consumes changes the first would have been sent.
- The cache is shared with the user's own running instance. Bump `MAGIC` or
  `FLOORS_VERSION` rather than deleting files; `make clean-cache` is the
  deliberate reset.

## Related issues

#23 (closed, periodic probe and lateral chase), #22 (closed, skip
blocks below the revealed surface), #17 (closed, restore-and-mesh cost).
