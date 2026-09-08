# dwarf-eye

A sidecar to a running Dwarf Fortress game: a real-time 3D voxel view of the
live map, read over DFHack's RemoteFortressReader. The game stays the source of
truth for the world, the clock, the weather and the adventurer; dwarf-eye
renders it and, in walk mode, steps the adventurer through DFHack. Rust,
Bevy 0.19, GPL-3.0-or-later.

## Architecture docs are part of every change

`docs/architecture/` is a tree: an overview with the data-flow diagram, then one
directory per subsystem (pipeline, factory, textures, preload, lod, sky, walk).
Each page carries a Status line: landed, in flight (issue #n), planned (issue #n).

When a change adds, revises or removes a component:

- read the subsystem's page first and treat it as a claim to verify against the code;
- update the page in the same commit: status line, file and function pointers,
  invariants, related issues; add a page for a new component, delete the page for
  a removed one and fix the links;
- where code and page disagree, say in the commit which one was right.

The pages hold the design intent, so keep docstrings to what a reader of that
function needs. Agents given a task receive the same rule and the page path.

## Working conventions

- Backlog is GitHub issues on `aaronsb/dwarf-eye`. New work items become
  issues; close them from the landing commit or merge with a short comment.
- Heavy or screenshot-iterating work goes to subagents in worktrees with
  disjoint file ownership. Merge their branches onto main yourself, grep for
  `^<<<<<<<` before committing, then remove the worktree and branch.
- Plain commit messages, no attribution lines. Terse prose everywhere: say a
  thing once.
- `make` prints the targets. `make test` needs no game; `make run`,
  `make budget` and `make walk-test` need DF running with an adventurer.
- Never `pkill` dwarf-eye: the user is usually in their own viewer. Verify with
  your own instance and `DWARF_EYE_SHOT=path:secs`, which exits after the shot.
- The game clock belongs to the player. For lighting checks use a viewer-side
  override where one exists; if the clock must move, restore it right after.
- The chunk cache under `~/.cache/dwarf-eye/` is shared with the user's
  instance. Bump a format version instead of deleting files.

## Testing and validation

Four layers, described in `docs/architecture/testing/`:

- unit tests per crate (`make test`, no game): pure functions such as clock
  arithmetic, ramp corner heights, walk classification and heading, tree
  rasterisation pinned by voxel counts and checksums;
- probes: the examples under `crates/dwarf-eye-world/examples/` (`sky`,
  `budget`, `coverage`, `luaq`, `settime`, `slice`) read or poke the live game
  without the GPU;
- live integration tests gated by `DWARF_EYE_LIVE=1` (`make walk-test`), which
  move the adventurer and are never part of `make test`;
- visual verification: a before and after screenshot at the same framing via
  `DWARF_EYE_SHOT`, and the tree lab for lighting and vegetation without a game.

A change to a component names which layers it touched. New pure logic gets a
unit test; a look change gets a screenshot pair in the report.

The showcase (`make showcase`, docs/gallery/) runs at a version tag, not per
change: `make release TAG=vX.Y.Z` tags, shoots the scene list against the
running game, commits the gallery with the tag in its header. Between tags,
verify a look change with one or two shots of your own; each viewer launch is a
forced pass on the player's game, so state a launch budget in every brief.

## Facts not recoverable from the code

- DFHack refuses replies over 64 MiB and also answers CR_LINK_FAILURE when no
  map is loaded. Unforced GetBlockList returns only blocks whose hash changed.
- Fine detail beyond the 144-tile live window does not exist anywhere DFHack
  can read; the horizon comes from region and world maps.
- In adventure mode the true time is in the season counters, not
  `cur_year_tick`. A black scene with the HUD at 00:00 is night.
- Trees and plants: DF gives maximum bounds, never a mould; the lab presets are
  the look; seeds come from absolute coordinates plus species.
