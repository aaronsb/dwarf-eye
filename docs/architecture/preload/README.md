# World preload on the travel vector

Status: planned (issue #21). The heading it depends on is landed
(`crates/dwarf-eye/src/walk.rs:Heading`, issue #20 closed).

## What it will do

Fetch and mesh the land ahead of a travelling character before the land beside
or behind them.

## What exists today

- The travel heading. `walk.rs:Heading` keeps up to `RUN_MEMORY` 4 recent
  DF-driven moves, needs `RUN_STEPS` 2 to go live, forgets a run after
  `RUN_GAP` 1.2 s, and weights the newest move 1, then 0.5, 0.25, 0.125
  (`walk.rs:Heading::aim`). Client-key travel feeds it the same way, through
  `Fix::Driven` in `walk.rs:reconcile`.
- Slab order. `session.rs:Session::fetch` descends the window in slabs of at
  most 500 blocks, narrowing the box to the columns still open
  (`session.rs:Session::open_box`). The order is fixed: top down, whole window.
- Remesh order. `worker.rs:remesh_touched` walks a `HashSet` of arrived chunks
  and their six neighbours, so the order is arbitrary.
- Retention. `worker.rs:collect` keeps a square of `RETAIN_RADIUS` 40 blocks
  around the camera, horizontal only, with no bias in any direction.

## Intended design

From issue #21, with the heading available:

- order the slabs so blocks ahead of the character on the heading come first,
  then the sides, then behind;
- remesh the leading edge first;
- when the window has just shifted, request the newly exposed leading strip with
  `force` before the unforced sweep over the rest;
- bias horizontal retention so cached chunks behind the character drop before
  those ahead once the retention radius is full.

Files named in the issue: `worker.rs:collect` for slab order, `session.rs`
`fetch` for the request, and the retention box in `worker.rs:collect`.

## Invariants a change here must keep

- A column drops out of a request at its floor, and the box narrows as the slabs
  descend. Reordering must not widen the box back out
  ([../pipeline/cache.md](../pipeline/cache.md)).
- A forced request is expensive and undoes the change-set saving, so forcing the
  leading strip has to stay a strip.
- A chunk retired from the `World` needs a forced request to come back, so
  biasing retention toward the heading trades a re-fetch for the drop.
- Walking re-requests every 0.3 s against a flier's 1.0 s
  (`main.rs:request_blocks`), which is the budget the ordering has to fit.

## Related issues

#21 (this page), #20 (closed, the heading), #10 (mid detail changes what
retention costs), #4 (the polls that share the same thread).
