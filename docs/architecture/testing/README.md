# Testing and validation

Status: landed (`Makefile`, `crates/*/src` unit tests,
`crates/dwarf-eye-world/examples/`, `crates/dwarf-eye/tests/walk_sync.rs`,
`tools/showcase/`).

## Four layers

### Unit tests, no game

`make test` runs `cargo test --release --workspace --lib`. It covers the pure
functions:

- clock arithmetic, both counters and `set_time` (`clock.rs`);
- ramp corner heights, the 47 sprite names and the skirt's texture walk
  (`ramp.rs`), including the cross-check that walk mode reads the same
  fractions the mesher draws;
- water corner heights and which faces survive, on synthetic 3x3 tile grids
  (`water.rs`);
- walk classification, bearings, the ground square and the heading's run,
  weighting, decay and turn (`walk.rs`);
- tree growth and rasterisation pinned by voxel counts and checksums, envelope
  bounds, streamers per preset (`dwarf-eye-trees`);
- cache round trips for the column floors, including a version that is not read
  (`cache.rs`);
- canopy palette interning and strand placement (`canopy.rs`), branch direction
  strings (`skeleton.rs`), growth tokens (`dwarf-eye-art:raws`).

### Probes, live game, no GPU

Examples under `crates/dwarf-eye-world/examples/`, run with
`cargo run --release -p dwarf-eye-world --example <name>`:

| Probe | |
|---|---|
| `sky` | DF's clock and cloud state |
| `budget` | where the triangles go over the live window (`make budget`) |
| `coverage` | which tiles get a sprite and which fall back to plain blocks |
| `horizon` | what DFHack knows beyond the loaded map (`make horizon`) |
| `luaq` | evaluates a Lua snippet and prints what it wrote |
| `settime` | sets the time of day for a lighting check |
| `slice` | z-level slices around the player as ASCII, to check decoding |
| `stale` | the disk cache against a forced fetch of the same blocks |

Others in the same directory cover single questions: `atlas`, `canopy`,
`floors`, `leafcut`, `links`, `materials`, `trees`, `roam`, `control`,
`adventure`.

### Live integration tests

`make walk-test` runs `crates/dwarf-eye/tests/walk_sync.rs` with
`DWARF_EYE_LIVE=1`. It **moves the adventurer**, so it is never part of
`make test`. It drives a scripted route east and back through walk sync from a
second session, and asserts the route was not abandoned, no step went stale or
unconfirmed, at least 16 tiles were walked, and the character came home.

### Visual verification

`DWARF_EYE_SHOT=path[:seconds]` saves one screenshot after the delay and exits,
which is how a build is checked without sitting at the window. `make shot` wraps
it. A look change gets a before and after pair at the same framing, so
`DWARF_EYE_VIEW=yaw,pitch` and `DWARF_EYE_CAM` are part of the evidence, not
conveniences. `make lab` and `make lab-shot` run the tree bench with no game;
`TREE_LAB_SUN` aims its light, and a viewer-side `DWARF_EYE_HOUR` joins them
once issue #14 lands.

### The showcase, and visual regression

`make showcase` shoots every scene in `tools/showcase/scenes.toml` and writes
[the gallery](../../gallery/README.md): the images, a caption each, the exact
env command that reproduces each one, and a header naming the world, the game
date and the commit. Adding a scene is one entry in that file and no code: id,
title, caption, group, an env map, and `baseline = true` if it should be
watched. Every shot runs its own viewer with a private `XDG_CACHE_HOME`, so the
cache the user's own instance shares is untouched, and with `DWARF_EYE_HUD=off`.
Nothing in a scene may touch the game: the hour is pinned with `DWARF_EYE_HOUR`
and the sky with `DWARF_EYE_CLOUDS` or `DWARF_EYE_WEATHER`. A shot that comes
back as bare sky means the game was between maps rather than the scene being
wrong, so the runner takes that frame again, up to three times.

`make showcase-check` re-shoots only the scenes marked `baseline`, into a
temporary directory, and prints the mean absolute pixel difference against what
is committed under `docs/gallery/`, failing over `DRIFT` (6 by default, in
levels out of 255). That is the visual regression layer: a shader or mesher
change that alters the look shows up as a number.

Its one condition is the game. Every framing is anchored on the character —
`DWARF_EYE_CAM` and `DWARF_EYE_VIEW` place the camera relative to where the
adventurer stands — and DF holds only 144 tiles around them, so a character who
has walked since the gallery was shot puts a different world in front of the
lens. Two shots taken half a minute apart while the player walks differ by 20
levels and more, which is the world moving, not the renderer. Run it with the
game standing where the gallery was shot; if the character has moved, re-shoot
the gallery with `make showcase` instead of reading a regression into the
numbers.

## What a change must include

- Name which layers it touched.
- New pure logic gets a unit test.
- A look change gets a screenshot pair in the report, and, if it touches
  anything a baseline scene shows, a `make showcase-check` line.
- Never `pkill dwarf-eye`: the user is usually in their own viewer. Verify with
  your own instance and a shot that exits.
- The game clock belongs to the player. Use a viewer-side override where one
  exists; if the clock must move, restore it.
- The chunk cache is shared with the user's instance. Bump a format version
  rather than deleting files.

## Related issues

#14 (the hour override the lab and the viewer will share), #9 (a fly-out visual
confirmation is the whole first half of it).
