# Contributing

dwarf-eye is in heavy development. Feature requests are welcome: open an issue
and say what you want to see and why. Bug reports and pull requests are by
arrangement for now, because the rough edges are known and the design is still
moving; if you have found something worth fixing, say so in a request first.

## Building

`make` prints the targets. All builds are release: Bevy is unusable in debug.

    make build      every crate
    make run        the viewer, against a running game
    make test       unit tests, no game needed
    make check      format check, clippy, tests

## Tests

Four layers, described in `docs/architecture/testing/`: unit tests per crate
(`make test`), probes under `crates/dwarf-eye-world/examples/` that read the
live game without the GPU, live integration tests gated by `DWARF_EYE_LIVE=1`
(`make walk-test`, which moves the adventurer), and visual verification, a
before and after screenshot at the same framing via `DWARF_EYE_SHOT`. New pure
logic gets a unit test; a look change gets a screenshot pair. Say which layers
a change touched.

## Docs travel with the change

`docs/architecture/` is one directory per subsystem, each page carrying a status
line. A change that adds, revises or removes a component updates its page in the
same commit: status, file and function pointers, invariants, related issues.
Where code and page disagree, say in the commit which one was right.

## Backlog and commits

The backlog is GitHub issues on `aaronsb/dwarf-eye`. New work items become
issues; close them from the landing commit with a short comment. Plain commit
messages, no attribution lines. Terse prose everywhere: say a thing once.

## Testing against a live game

The game belongs to the player, not to the test.

- The clock is the player's. For lighting checks use a viewer-side override
  where one exists; if the clock must move, restore it right after.
- The chunk cache under `~/.cache/dwarf-eye/` is shared with whatever instance
  the player is sitting in. Bump a format version instead of deleting files.
- Never `pkill` dwarf-eye. Verify with your own instance and
  `DWARF_EYE_SHOT=path:secs`, which exits after the shot.
