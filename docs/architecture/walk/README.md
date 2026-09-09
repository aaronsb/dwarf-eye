# Walk sync mode

Status: landed (`crates/dwarf-eye/src/walk.rs`, `crates/dwarf-eye/tests/walk_sync.rs`);
issue #20 closed by the heading work.

## What it does

Hands the camera to the adventurer: the eye stands in the character's tile,
`WASD` walks inside that cell, and crossing a cell edge asks Dwarf Fortress to
step the character that way.

## How

`walk.rs:Pilot::spawn` opens a second DFHack connection on its own thread
(`walk.rs:pilot`), because a step has to be confirmed in a fraction of a second
and one map collection pass can take many. It polls the character's position
every `POLL` 125 ms through `Session::refresh_window` and `Session::view_center`
and reports back; orders go the other way as one `A_MOVE_*` interface key fed
through DFHack Lua.

```mermaid
stateDiagram-v2
  [*] --> InCell
  InCell --> Ordered: edge crossed, may_step
  Ordered --> InCell: Fix::Ordered, level from the game
  Ordered --> Sprung: no confirmation in 0.6 s
  Sprung --> InCell: edge shut for 2 s, glide back
  InCell --> Followed: Fix::Driven, the game moved the character
  Followed --> InCell: cell taken, offset kept
```

## The parts

| Part | |
|---|---|
| optimistic step | `walk.rs:walk` crosses the edge first and asks after; `WalkMode::may_step` allows one pending step at a time |
| reconciliation | `walk.rs:classify` sorts a report into `First`, `Waiting`, `Ordered`, `Still` or `Driven`; `WalkMode::jump` glides the camera rather than teleporting it |
| refusal | a step unconfirmed after `CONFIRM` 0.6 s springs the camera back and shuts that edge for `BLOCKED` 2.0 s |
| surface | `WalkMode::surface` probes `z`, `z-1` then `z+1` for something to stand on and `WalkMode::footing` reads the ground sheet, falling back to the slab, the ramp fraction or the stair footing |
| heading | `walk.rs:Heading` averages the last four DF-driven moves, newest weighted 1, 0.5, 0.25, 0.125, and eases the yaw round in `TURN` 0.3 s |

## Invariants and gotchas

- The camera is never more than one cell ahead of the game. A second edge waits
  for the first step to land.
- The eye rides what the mesher drew, and over natural ground that is the
  smoothed sheet, not the slab. `Ground::sample` builds a `Surface` over the
  same square it reads solids over, with the same
  `heightfield.rs:Surface::over` the mesher uses, and `WalkMode::footing` takes
  its height wherever the sheet lands inside the cell being asked about
  ([../pipeline/meshing.md](../pipeline/meshing.md)). A natural ramp is part of
  that sheet, so nothing re-derives its wedge; a staircase is not ground and
  keeps `STAIR_FOOTING`.
- Where the sheet has nothing — a constructed floor, a stair, terrain that has
  not streamed in, or `DWARF_EYE_GROUND=stepped` — `WalkMode::footing` falls
  back to the old rule: `FLOOR_HEIGHT` plus the bilinear patch over
  `ramp.rs:slopes`, cross-checked by
  `ramp.rs:corner_heights_follow_the_fractions_walk_mode_reads`. That is also
  the rule the heightfield's ramp samples come from, so the two agree either
  way.
- Unknown ground never refuses a step. Unloaded terrain would otherwise stop the
  character at the edge of what has streamed in.
- A rise of `A_WALL` 0.75 of a level or more is a wall, not floor, so the eye
  does not climb it.
- Ramps are never asked for explicitly. DF takes the level itself as the
  character walks into the rise; `Q` and `E` are for stairs.
- `Fix::Driven` keeps the offset inside the cell and the look direction. The
  yaw is taken over only once a run of at least two moves is live.
- Three unconfirmed steps in a row, or 1.5 s pushing at ground the viewer's own
  reading refuses, abandons a scripted route.
- `DWARF_EYE_WALK=1` starts in walk sync. `DWARF_EYE_WALK_DRIVE=bearing,seconds;…`
  walks a scripted route, compass degrees, 0 north, 90 east, an empty bearing to
  stand still.
- The pilot fetches and retains a small box around the character on its own
  connection, so its change set is separate from the map thread's.

## Related issues

#20 (closed, face the direction of travel), #21 (the heading is what the preload
work orders slabs by), #4 (the same argument for a light connection, applied to
the polls).
