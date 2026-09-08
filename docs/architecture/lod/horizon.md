# The far band

Status: landed (`crates/dwarf-eye-world/src/horizon/`, issue #31); the smooth
world grid past the region details still joins by a hidden step rather than a
skirt (issue #9).

## What it does

Builds the land beyond the live window out of the only two surveys DFHack
exposes — the region maps around the player and the world map — as terraced
ground with forests, rivers and sites on it, out to the horizon.

## The sources

`worker.rs:build_horizon` calls `GetRegionMapsNew` and `GetWorldMap`, then
`horizon::build` returns a [`Horizon`](../../../crates/dwarf-eye-world/src/horizon/mod.rs):
one ground mesh plus crown instances, rebuilt whenever the window moves.

- **Region maps**, 48 tiles per sample, `REGION_MAP_SIDE` 17 per side with the
  17th row and column overlapping the next world tile and skipped. In practice
  DFHack returns five world tiles square, so the region details reach 1920
  tiles from the window. Each tile carries elevation, water, rainfall,
  vegetation, drainage, snow, surface material, **tree materials** (the species
  mix), **stone materials** (a site's masonry), river edges and site building
  footprints.
- **World map**, 768 tiles per sample, over the whole 129x129 world: elevation,
  water, rainfall, vegetation, drainage. It is on its own elevation scale, so
  `mod.rs:world_bias` shifts it by the mean gap between the two surveys where
  they overlap.

Both land in one `field::Field`, keyed by absolute region tile: region samples
first, then the rest filled bilinearly from the world map.

## One surface model

Fine tiles step by whole z-levels. A smooth coarse surface can therefore only
cut through them or float over them, so inside the region details the coarse
band is built the way a fine floor is: **one flat slab per cell at a whole
z-level, plus a vertical riser wherever the neighbour sits lower**
(`terrace.rs`). Refining the survey buys detail in plan — terrace edges that
wind like contour lines — not a smoothness the fine map cannot match. The
relief noise is quantised along with everything else, so there are no sub-level
ripples near the window.

Three pitches, coarsening outward from the live window's centre, each chosen by
the region tile a cell falls in so a band boundary never splits a cell:

| Band | Pitch | Out to | Surface |
|---|---|---|---|
| near | 6 tiles | 384 | terraced |
| middle | 24 tiles | 1152 | terraced |
| region | 48 tiles | 1920 | terraced |
| world | 768 tiles | the whole world map | smooth |

Past 1920 tiles a z-level is under a pixel, so `mod.rs:emit_world_grid` goes
back to a smooth vertex grid with central-difference normals.

Between samples, `field::Field::at` runs elevation through **Catmull-Rom**,
which passes exactly through every sample DFHack gave; the scalars that only
drive colour are bilinear, since colour has no business overshooting.
`field::relief` then adds multi-octave value noise seeded from **absolute** tile
coordinates, at wavelengths 192, 64 and 24 tiles, with an amplitude of
`RELIEF_BASE 1.6 + RELIEF_SLOPE 4.0 * slope` z-levels scaled by drainage and
capped at 6: a flat wet plain stays flat, a well-drained mountainside breaks up
into spurs. Water is left alone.

## The stitch

The whole arbitration between tiers is the block mask, and `fine::FineSurface`
is the horizon's half of it. It surveys the loaded chunks once per build and
keeps, per 16-tile block column whose lowest chunk is at least half solid, the
**30th percentile of the column's top solid z** — a z-level, not a distance, so
a coarse slab and a fine floor at the same level are the same plane. It is the
same grounded-column rule as `worker.rs:grounded_blocks`, which ships the same
set to the GPU as the mask texture.

The rule, `terrace.rs:cell_level`:

1. over a grounded column, the coarse band draws nothing (and the mask would
   discard it anyway);
2. a cell with a grounded column as one of its four neighbours takes **that
   column's own level**, the lowest where several are adjacent, so the coarse
   never rides over fine ground;
3. every other cell takes the quantised survey.

At the rim a one-tile skirt hangs from the cell's edge whatever the levels say,
because the fine floor's own rim faces cover the rest and a gap there is a hole
in the world. Risers hang `RISER_SKIRT` 2.0 below the cell they drop to, which
covers the case where two bands of different pitch disagree by a level.

Only the quantisation is a mode switch: were the fine tier ever smoothed
(issue #6), the boundary snap to fine column heights is unchanged and the
terracing is what would be swapped out.

## Forests

`scatter.rs` places one canonical crown per tree. A region tile's own
`tree_materials` name the species — `preset_for` maps the material name onto a
`dwarf-eye-trees` preset — and its `vegetation` sets the count:

```
count = round(PER_TILE * clamp01((vegetation - FLOOR) / (100 - FLOOR)) * fade)
fade  = 1 inside NEAR, falling linearly to 0 at REACH
```

with `PER_TILE` 20, `FLOOR` 15, `NEAR` 720 tiles and `REACH` 1920. Position,
species, size (0.72 to 1.35 of the preset's height) and yaw all come from
`hash(rx, ry, k)` on the region tile's **absolute** coordinates, so a tree keeps
its spot as the window moves and thinning a patch drops the tail of the list
rather than reshuffling it. Crowns over a grounded column are skipped.

Woodland is not one habitat, so the mix is deliberately impure. `pick` gives
`STRAY` 7% of a patch to a species from a neighbouring region tile that this
tile does not carry, so a biome boundary interleaves rather than switching on a
48-tile line; past `CONTRAST_FROM` 1200 tiles another `CONTRAST` 3% goes to a
spruce or a standing dead tree, so the far band is not one green. `EMERGENT` 5%
of trees are `EMERGENT_SCALE` 1.8 times their jittered size, which breaks the
single-storey look. All of it off the same `hash(rx, ry, k)`.

Which stage of the tree chain an instance draws at is projected size:
`dwarf_eye_trees::crown` while `height * 30` exceeds the distance to it,
`dwarf_eye_trees::crown_box` (12 triangles) beyond that, and past 1920 tiles
nothing — `terrace.rs:canopy_at` hands the foliage back to the ground as colour
over the same interval, so the two never double-count.

The canonical crown is **axis-aligned boxes**, not a faceted ellipsoid: at this
range a rounded solid is only ever a dozen flat facets, and their angled edges
read as crystal rather than as foliage. A box shares the voxel map's own
vocabulary and shades the way the near canopy does, top face lit and sides
darker. Faces that can never be seen are left out — a trunk is four sides, its
top being inside the crown and its foot in the ground, and a crown box drops its
underside where the box below is at least as wide — so a preset costs:

| | triangles |
|---|---|
| oak, birch, willow, bush (trunk + two crown boxes) | 30 |
| pine, spruce (trunk + three narrowing boxes) | 40 |
| sapling, dead tree, mushroom tree | 20 |
| shrub, tall grass (one box) | 10 |
| any preset's `crown_box` | 12 |

Each species and stage is one mesh with one transform per instance, spawned in
`main.rs` under the `Horizon` marker; Bevy batches entities sharing a mesh and
material, so a whole forest is a handful of draw calls. They ride the horizon
material, so the block mask discards any that stand where fine chunks have
since arrived, and they are `NotShadowCaster`: a tree a thousand tiles off
contributes nothing to the shadow map but its own cost, four cascades over.

## Rivers and sites

`features.rs`, off the same region tiles. A river gets one flat strip per edge,
its width the edge's own `min_pos..max_pos` span, converging on the tile centre
so two edges through one tile connect and a spring or a mouth tapers. A site
building gets a bottomless box on the lowest terrace under its footprint,
coloured from the tile's first `stone_materials` entry pulled toward a neutral,
3 levels tall for a wall, its own `roof_z` for a tower, 0.4 for a trench.
Rivers skip wherever fine chunks stand in; buildings skip only the live window
itself, since a walked-and-cached tile still needs its coarse building drawn.

## Cost

Measured against the shipped world (`The Dimension of Griffons`, five world
tiles of region details):

| | triangles |
|---|---|
| ground: terraces, world grid, rivers, sites | 130k |
| 8.7k crown instances (1.3k crowns, 7.4k boxes) | 129k |
| **total** | **259k** |

Built in 0.08 s. The band it replaces was 359k triangles of smooth 48-pitch
grid running twelve world tiles out, most of it interpolated from the world map
and carrying no more information than the 768-pitch grid does.

## Invariants and gotchas

- Elevation banding is DF's: 0 to 99 ocean, 100 to 149 normal biomes, 150 and
  up mountains. The alignment to z-levels is the computed bias, not a fixed
  offset.
- `RegionMap.tiles` is indexed `y * 17 + x`. `DWARF_EYE_HORIZON_TRANSPOSE` flips
  the decode for testing; the default is what ships, and the
  `dwarf-eye-world --example horizon` probe is what checks it. No unit test pins
  the order.
- The render origin is `block_pos * 48`, so the render-local 48- and 16-tile
  grids align with region tiles and blocks. Seeds nevertheless use absolute
  coordinates (render plus origin), because the origin moves with the window.
- The coarse material is the terrain material with `horizon` set to 1, which
  turns on the block-mask discard in both `cloud_shadow.wgsl` and
  `cloud_shadow_prepass.wgsl`. It has to discard in the prepass too, or its
  depth hides the fine ground behind it; the same shader serves the shadow pass,
  so the horizon casts no shadow.
- The smooth world grid runs a world tile under the outermost terrace and four
  units lower there, so the join is a step hidden under the terrace's own outer
  skirt. There is still no true skirt geometry across that seam, and a low
  grazing camera far from the window can see it. That is the remaining half of
  issue #9.
- A one-level riser is drawn as a vertical face, not as a ramp-like slope. The
  fine map's ramps are a tile wide and a coarse cell is six or more, so a slope
  there would read as a chamfer rather than as a ramp.
- A crown seen from directly underneath shows the inside of its lowest box,
  because that underside is dropped. Two triangles a tree, and the band this
  stage serves is normally at or below the eye; `crown.rs:slabs` sets the flag
  if that ever stops being true.

## Claims to verify

[docs/dfhack-horizon-notes.md](../../dfhack-horizon-notes.md) describes how
Armok Vision draws the same two tiers and why full detail beyond the window does
not exist.

## Related issues

#31 (this band), #9 (the seam skirt past the region details), #10 (mid detail
under the same mask), #6 (a smoothed fine tier would turn the quantisation off
and keep the boundary snap), #12 (elevation offset and other protocol
semantics).
