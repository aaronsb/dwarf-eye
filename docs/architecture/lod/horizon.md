# The coarse horizon

Status: landed (`crates/dwarf-eye-world/src/horizon.rs`, merged as 197d4ea);
visual confirmation and the seam skirt planned (issue #9).

## What it does

Builds the land beyond the live window out of the only two surveys DFHack
exposes: the region maps around the player and the world map.

## How

`worker.rs:build_horizon` calls `GetRegionMapsNew` and `GetWorldMap`, then
`horizon.rs:build` stitches one mesh, rebuilt whenever the window moves.

1. Region tiles become samples on a 48-tile grid (`horizon.rs:region_sample`),
   `REGION_MAP_SIDE` 17 per side with the 17th row and column overlapping the
   next world tile and skipped.
2. Where fine chunks exist, `horizon.rs:measured_surface` takes the 30th
   percentile of the loaded surface over that 48x48 square, needs at least 64
   samples, and sinks the coarse sample 0.6 below it.
3. The world map is shifted by a bias computed where the two surveys overlap,
   then interpolated bilinearly onto the same 48-tile grid out to `REACH` 12
   world tiles.
4. `horizon.rs:emit_region_grid` triangulates it with central-difference
   normals and a per-vertex brightness jitter, colouring through
   `horizon.rs:shade` from surface material, vegetation, rainfall, snow and
   elevation.
5. Rivers get four edge strips per region tile (`horizon.rs:emit_river`), site
   buildings get bottomless boxes 3 levels tall, 8 for a tower and 0.4 for a
   trench (`horizon.rs:emit_building`).
6. Everything past `REACH` is one world-tile grid at 768-tile spacing.

## Invariants and gotchas

- Elevation banding is DF's: 0 to 99 ocean, 100 to 149 normal biomes, 150 and up
  mountains. The alignment to z-levels is the computed bias, not a fixed offset.
- `RegionMap.tiles` is indexed `y * 17 + x`. `DWARF_EYE_HORIZON_TRANSPOSE` flips
  the decode for testing; the default is what ships, and the
  `dwarf-eye-world --example horizon` probe is what checks it. No unit test pins
  the order.
- Buildings are skipped only inside the live window itself. Rivers and ground
  quads are skipped wherever fine chunks stand in, cached or live.
- A ground quad is emitted only where all four corners have a sample, so a gap
  leaves a hole rather than a stretched triangle.
- The far world grid runs a world tile under the fine ring and two levels lower
  there, so the join is a step hidden under the finer surface. There is no skirt
  geometry anywhere in `horizon.rs`, and a low grazing camera can see the step.
  That is the second half of issue #9.
- The coarse material is the terrain material with `horizon` set to 1, which
  turns on the block-mask discard in both `cloud_shadow.wgsl` and
  `cloud_shadow_prepass.wgsl`. It has to discard in the prepass too, or its
  depth hides the fine ground behind it; the same shader serves the shadow pass,
  so the horizon casts no shadow.

## Claims to verify

The repo [README](../../../README.md) still lists under "Not done yet" that the
coarse horizon has no rivers or sites. The code is right and the README is
stale: `horizon.rs:emit_river` and `horizon.rs:emit_building` are on main. The
same line's claim about the bare step is still true.

[docs/dfhack-horizon-notes.md](../../dfhack-horizon-notes.md) describes how
Armok Vision draws the same two tiers and why full detail beyond the window does
not exist.

## Related issues

#9 (confirm visually, then the seam skirt), #10 (mid detail under the same
mask), #12 (elevation offset and other protocol semantics).
