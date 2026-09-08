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
`Session::open_box` narrows the request box to the columns still open;
`Session::read_floors` records the highest wholly hidden block per column and
removes a floor when that level comes back visible. `UNDER_FLOOR` is 1, so the
block below a floor is still fetched and the floor block has a neighbour to cull
against.

## Invariants and gotchas

- A chunk whose 256 tiles are all hidden is never stored and is deleted on
  restore. That rock is the same under every world and it grew the cache to
  thirty times the walked map.
- A column whose lowest cached chunk is sparse holds canopy with no ground under
  it. Those files are removed on restore and refetched.
- The floor test on restore reads the chunk's own tiles, not the sidecar, so a
  stale `floors` file cannot delete land somebody has seen.
- A `floors` file from another version reads as nothing known, not as an error.
- Floors are dropped only within one block of the character and only when the
  character is at or below the floor (`Session::fetch`). A forced pass re-reads
  floors rather than discarding them.
- Ground revealed under a floor block that itself stays hidden is never asked
  for again. A tunnel from a neighbour or a cavern opened by an event is
  invisible until the character descends. That is issue #23.
- The cache is shared with the user's own running instance. Bump `MAGIC` or
  `FLOORS_VERSION` rather than deleting files; `make clean-cache` is the
  deliberate reset.

## Related issues

#23 (periodic full-depth re-probe and lateral propagation), #22 (closed, skip
blocks below the revealed surface), #17 (closed, restore-and-mesh cost).
