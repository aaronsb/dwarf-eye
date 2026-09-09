# World preload on the travel vector

Status: landed (issue #21). The heading it rests on is landed as well
(`crates/dwarf-eye/src/walk.rs:Heading`, issue #20 closed).

## What it does

Fetches, meshes and keeps the land ahead of a travelling character before the
land beside or behind them. Nothing here changes *which* blocks a pass asks
about — the window, the column floors and the retention count are what they
were. It changes the order, and it forces the one strip an unforced pass would
never send.

## The travel vector

One unit vector on the ground plan, render x east and render y south, or `None`
when nothing is travelling. `main.rs:request_blocks` picks it and sends it with
the fetch centre in `Command::Fetch`.

- Walking, either hand on the wheel. `walk.rs:WalkMode::travel` reads a second
  `Heading` fed by every confirmed tile crossing (`WalkMode::moved`), so a run
  the game drove and a run the player walked with the client's keys both count.
  It is the same smoothing the camera turns on: up to `RUN_MEMORY` 4 crossings,
  live after `RUN_STEPS` 2, forgotten after `RUN_GAP` 1.2 s of standing still,
  the newest crossing weighted 1 and each older one `RUN_DECAY` 0.5 as much
  (`walk.rs:Heading::vector`). The camera's own `heading` is untouched: it still
  drops the run the moment the player steers, because a player's yaw is theirs.
- Flying. `camera.rs:FlyCamera::travel` eases the ground plan of WASD toward the
  keys over `TRAVEL_EASE` 0.6 s and decays it back to zero when they let go;
  `FlyCamera::travelling` reports it once it holds `TRAVELLING` 0.35 of a full
  run. A tap, or a sidestep round a tree, never turns the preload round.

Walk mode's vector wins while walk mode is on; the flier's stands in otherwise.
`DWARF_EYE_TRAVEL=east,south` — `1,0` for east — pins it over both
(`main.rs:pinned_travel`), which is how the order is checked against a live game
without walking anybody: the fetch centre stays where it is and only the order
changes.

## The rules

**Slab order.** `session.rs:travel_bands` cuts the window into three bands and
`Session::fetch_leading` descends them in order: the blocks ahead of the
character, the strip they stand in, then the blocks behind. A request is a box,
so the cut is one plane across the axis the heading mostly runs on; a diagonal
therefore carries one flanking quarter along with the blocks ahead, which is the
price of not turning one request into four. Each band is a slice of the same
box, and each descends top down through its own slabs exactly as one descent
did, so the box never widens and no column is asked about twice.

**Remesh order.** `worker.rs:leading_first` sorts the arrived chunks and their
neighbours by how far along the heading they sit, furthest first, then by
distance from the camera. The leading edge is the land the character is walking
into, and meshes leave in the order they are built. With nothing travelling the
order is simply nearest first, which is what the renderer uploads in anyway
(`main.rs:UPLOAD_BUDGET`).

**The newly covered strip.** `worker.rs:newly_covered` takes the box this pass
covers less the box the last one covered — up to six slabs, cut so they never
overlap — and `worker.rs:ahead_first` puts the leading one at the front. Those
go to `Session::fetch_leading` as `leading`, which descends them forced before
the unforced sweep of the window.

That strip is the one place an unforced pass can lose land. DFHack answers an
unforced `GetBlockList` with the blocks whose hash has moved since it last sent
them, and the hash it holds for a block at the window's new edge belongs to the
land that used to sit at those window-local coordinates. Everywhere else a shift
is harmless: the land that has moved under a local coordinate is land this
viewer already holds under its own render key, so a reply saying "unchanged" is
telling the truth about a chunk we have. Only the new edge is land we have never
seen. A change of depth — the character stepping down a level, which pushes the
box's floor lower — uncovers levels the same way, and comes out of the same
subtraction.

What the code did before was force the *whole* window on any change to the box
or the frame, which is correct and pays for the window to keep the strip. The
strip is a few block columns of a nine-by-nine window.

**Retention bias.** `worker.rs:retention_box` is the same `RETAIN_RADIUS` 40
block square around the camera, horizontal only, pushed `RETAIN_BIAS` 8 blocks
along the heading. The box never grows, so the count of chunks held is what it
was; what changes is which of them go when it is full. A chunk behind the
character falls out eight blocks early so a chunk ahead can stay in eight blocks
longer.

## Invariants a change here must keep

- A column drops out of a request at its floor, and the box narrows as the slabs
  descend. Reordering must not widen the box back out
  ([../pipeline/cache.md](../pipeline/cache.md)). The bands are slices, so they
  cannot.
- A forced request is expensive and undoes the change-set saving, so the leading
  strip has to stay a strip. It is bounded by how far the window can shift
  between two passes.
- Only what a forced descent asked about is ever dropped from the world and the
  cache (`session.rs:unanswered` over `Descent::asked_keys`). With the strip
  forced and the sweep unforced, that dropping now happens inside the strip
  rather than across the window: land the game has silently stopped holding in
  the middle of the window waits for the next forced pass.
- A chunk retired from the `World` needs a forced request to come back, so
  biasing retention toward the heading trades a re-fetch for the drop. Paid
  behind the character, where they are least likely to turn round.
- Retention never clips vertically. The camera's height is not a reason to drop
  the crowns above it.
- Walking re-requests every 0.3 s against a flier's 1.0 s
  (`main.rs:request_blocks`), which is the budget the ordering has to fit. Three
  bands are not three times the requests: a band a third as wide fits three
  times as many levels into the same 500-block slab.

## Tests

Pure, on synthetic boxes, no game: `session.rs` tests that the bands come out
ahead, sides, behind for each compass heading, that a diagonal cuts across the
axis it mostly runs on, and that the bands cover the window exactly once and
never reach outside it; `worker.rs` tests the strip a shift and a change of
depth uncover, that the strips do not overlap, that the leading one is forced
first, that retention keeps the block ahead and drops the one behind at the same
distance while holding its column count, and the remesh order.

Live: one pass against the running game with the heading pinned east said
`preload: 3 bands, travelling east (+1.00, +0.00); window held, 0 forced leading
boxes of 0 blocks, then the unforced sweep of 3094`, and that first forced pass
asked for the same 3094 blocks of a 3888-block box in the same 5.5 s as before
the bands. Nobody walked, so nothing was newly covered; the strip is what the
unit tests are for.

## Related issues

#21 (this page, landed), #20 (closed, the heading), #10 (mid detail changes what
retention costs), #4 (the polls that share the same thread).
