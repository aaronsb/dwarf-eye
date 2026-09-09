# Clock and calendar

Status: landed (`crates/dwarf-eye-world/src/clock.rs`, `crates/dwarf-eye/src/sky.rs`);
issue #19 closed by it.

## What it does

Reads the time of day and the day of the year out of Dwarf Fortress in both
game modes, and writes it back when the viewer steps the clock.

## How

`clock.rs:PROBE` is one line of DFHack Lua that prints every clock global under
a tag. The worker runs it through `RunCommand`, reads the echo out of
`Client::last_notices` and parses it with `clock.rs:parse` into a
`clock.rs:Reading`. `Reading::year_tick` then picks the counter the current mode
keeps. If the Lua path fails, the worker falls back to `GetWorldMapCenter`,
which carries `cur_year` and `cur_year_tick` (`worker.rs:run`, `Command::Clock`).

`sky.rs:Clock` holds year and tick, and derives everything else:
`day_of_year`, `tick_of_day`, `day_fraction`, `month`, `day_of_month`,
`describe`.

The moon is the one part that is no longer derived. `Clock::moon` carries DF's
own `world_data.moon_phase`, read by the weather probe
(`weather.rs:PROBE`, see [weather.md](weather.md)) and set on the `Weather`
event rather than the `Clock` one, because that is the poll that reads it.
`Clock::moon_phase` returns it when it is there and falls back to the calendar's
28-day month otherwise. The two agree to within a day: 15 Granite of year 100
read `moon_phase` 15, a phase of 0.536 against the derived 0.514, both a full
moon.

| Constant | |
|---|---|
| `TICKS_PER_DAY` | 1200 |
| `DAYS_PER_MONTH` | 28 |
| `DAYS_PER_YEAR` | 336 |
| `TICKS_PER_YEAR` | 403200 |
| `TICKS_PER_SEASON` | 100800 |
| `TICKS_PER_SEASON_TICK` | 10 |
| `ADVMODE_PER_TICK` | 144 |

## Invariants and gotchas

- Fortress mode runs `cur_year_tick`. Adventure mode abandons it and keeps
  `cur_season` and `cur_season_tick`, so the true tick is
  `cur_season * 100800 + cur_season_tick * 10`. Reading the fortress counter in
  adventure mode put the sun at the wrong hour, which was issue #19.
- `cur_season_tick` steps 10 fortress ticks, so adventure-mode time of day is
  quantised to 12 game minutes. DF exposes nothing finer that stays correct.
- `cur_year_tick_advmode` is a phase to carry along, not a time to read. No
  divisor of a live reading lands on the hour the game shows.
- `clock.rs:set_time` writes `cur_year_tick`, `cur_season`, `cur_season_tick`
  together and moves the advmode counter by the delta DFHack's timestream would.
  Writing the fortress counter alone moves the renderer and nothing else.
- The clock belongs to the player. `,` and `.` step it an hour, six with shift
  (`main.rs:handle_input`); put it back after a lighting check.
- The clock poll shares the worker thread with map fetches, so a heavy pass
  leaves the sun at the wrong hour until it clears. Issue #4.
- The moon arrives on the twelve-second weather poll, not the half-second clock
  poll, so the first few seconds of a session run on the derived phase.
- `moon_angle` is read and carried but unused; its units are unverified.

## Claims to verify

[docs/dfhack-horizon-notes.md](../../dfhack-horizon-notes.md) derives the
season-counter arithmetic from DFHack's `autofarm.cpp` and records the
experiment that found it. The code agrees, and the unit tests in `clock.rs` pin
the same numbers.

## Related issues

#19 (closed), #4 (poll off the critical path), #14 (a viewer-side hour override
so the player's clock need not move), #32 (the moon phase, and the rest of the
weather globals it rides with).
