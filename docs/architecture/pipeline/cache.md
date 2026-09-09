# Disk cache and column floors

Status: landed (`crates/dwarf-eye-world/src/cache.rs`,
`crates/dwarf-eye-world/src/session.rs`); periodic re-probe in flight (issue #23).

## What it does

Keeps the land the character has walked past, which the game itself no longer
holds, and remembers how deep each block column is worth asking about.

## How

`Cache::open` places one directory per world under
`~/.cache/dwarf-eye/<world>-<save>/`, with `DWARF_EYE_CACHE` overriding the
root. One file per chunk, `<bx>_<by>_<z>.chunk`: a 4-byte magic `DEC3` then 256
voxels of 24 bytes (`cache.rs:Cache::store`, `cache.rs:read_chunk`). Keys are
absolute. `Session::persist` writes every chunk that arrived;
`Session::restore_cache` reads them back on connect and places them through
`World::restore`, which never overwrites a chunk the game has already supplied.

`DEC3` added the five bytes a building takes in a voxel: DF's building type and
subtype, and where the tile sits inside the footprint (issue #15). A `DEC2`
file fails to read and is deleted, so the first connect after the bump refetches
the land rather than losing it.

What a building is *made of* is not in the file. Colour and material sheet live
in `Chunk::built`, a sparse side table filled from the reply, and so do item
piles in `Chunk::piles`: both are what the game holds right now, and a chunk
read back from disk is land the game has moved on from. A restored hall keeps
its furniture and draws it in stone until the window returns to it.

Alongside the chunks sits `floors`, one line per block column giving the level
at which that column turned to unrevealed rock, versioned by its first line
`def1` (`cache.rs:load_floors`, `cache.rs:store_floors`).

## The game wins where the window reaches

The cache is only for land the game no longer holds. Inside the live window the
game is the authority, so a forced request is the whole truth about the box it
asked for: `Session::fetch` records every key a forced descent asked about, and
`session.rs:unanswered` names the ones nothing came back for. Those chunks are
dropped from the world and their files deleted, and `worker.rs:collect`
despawns their meshes and remeshes their neighbours. An unforced pass records
nothing, because there absence means unchanged.

DFHack never sends a block whose 256 tiles are all air or nothing, forced or
not, so "asked for and not answered" reads as "there is no land there". That is
what carried a felled tree, or a crown written under the wrong key, from one
session to the next: nothing in a hash-gated reply ever contradicts a chunk the
cache already holds.

Every reply also says which window it was built in (`BlockList::map_x/map_y`,
DFHack's own `Maps::getPosition`). A pass takes seconds and the window follows
the character, so a reply can arrive in a frame one region tile — 48 tiles, 3
blocks — from the one the pass began in. `session.rs:reply_frame` places each
reply by its own frame; a pass that saw the window move places its blocks but
drops nothing, since its box no longer describes where it looked.

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
- Only what a forced pass asked about is dropped. Land under a column floor is
  never asked and so is never dropped; the probe is what looks there.
- `crates/dwarf-eye-world/examples/stale.rs` weighs the cache against a forced
  fetch of the whole window. A wrong chunk shows as a perfect match a few blocks
  away; an old one as a handful of tiles.
- The cache is shared with the user's own running instance. Bump `MAGIC` or
  `FLOORS_VERSION` rather than deleting files; `make clean-cache` is the
  deliberate reset.

## Related issues

#23 (closed, periodic probe and lateral chase), #22 (closed, skip
blocks below the revealed surface), #17 (closed, restore-and-mesh cost).
