# Testing and validation

Status: landed (`Makefile`, `crates/*/src` unit tests,
`crates/dwarf-eye-world/examples/`, `crates/dwarf-eye/tests/walk_sync.rs`).

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

## What a change must include

- Name which layers it touched.
- New pure logic gets a unit test.
- A look change gets a screenshot pair in the report.
- Never `pkill dwarf-eye`: the user is usually in their own viewer. Verify with
  your own instance and a shot that exits.
- The game clock belongs to the player. Use a viewer-side override where one
  exists; if the clock must move, restore it.
- The chunk cache is shared with the user's instance. Bump a format version
  rather than deleting files.

## Related issues

#14 (the hour override the lab and the viewer will share), #9 (a fly-out visual
confirmation is the whole first half of it).
