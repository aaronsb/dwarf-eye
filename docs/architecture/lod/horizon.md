# The far band

Status: landed (`crates/dwarf-eye-world/src/horizon/`, issue #31); the tree
chain is the factory's, shared with the window's canopy bands (issue #5); the
smooth world grid past the region details still joins by a hidden step rather
than a skirt (issue #9).

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

## What it wears

The coarse ground is drawn with **the fine map's own Dwarf Fortress sprites**,
not with flat vertex colour. Before, the fine window read as a darker, greener
patch in the middle of a paler band and the seam showed as a change of colour
rather than of detail.

`skin.rs` picks the sheet by biome:

| surface | sheet | chosen by |
|---|---|---|
| turf | `GRASS_5` | vegetation at or above 20 |
| bare ground | `DIRT_FLOOR_5` | below that |
| bare rock | `STONE_FLOOR_5` | elevation at or above 150 |
| snow | `ROUGH_ICE_FLOOR` | snow over 60 |
| riser, wet and low ground | `SOIL_WALL` side strip | drainage under 60 |
| riser, drained or mountain | `STONE_WALL` side strip | drainage 60 up, or elevation 150 up |

Those are exactly the families `library.rs:pack_under` and `pack_walls` already
fill for the fine mesher, reached through the read-only `ground_cell` and
`wall_side_cell`; the horizon packs nothing of its own.

**The tint is measured, not assumed.** `skin.rs:cell_mean` reads each packed
cell's mean back off the atlas in linear light, and the vertex colour is the
colour the survey asks for divided by that mean, so the surface averages to
exactly that colour whatever the sheet is. Multiplying a near-grey sheet by the
colour outright, the way the fine mesher does, turns every snowy cell white:
DF's rough ice floor is near-white to begin with.

**The repeat is the shader's.** A terrace slab is one quad six to forty-eight
tiles across and the atlas has no room around a cell to tile into, so a vertex
carries only the cell's own centre UV. `cloud_shadow.wgsl:horizon_texel` floors
that UV into the atlas grid to recover the cell, then wraps the sprite across
the surface by world position — one sprite to a world tile, Dwarf Fortress's own
density and the fine map's, so the two meet without a change of scale. Gradients
come from the unwrapped coordinate, so the mip level is continuous across a tile
boundary. Untextured horizon geometry — the world grid, rivers, buildings, the
blob shadows — points at the white cell and comes through as its own vertex
colour, unchanged.

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
keeps two things per 16-tile block column whose lowest chunk is at least half
solid: the **30th percentile of the column's top solid z**, which is the
arbitration and matches the set `worker.rs:grounded_blocks` ships to the GPU as
the mask texture; and the **top solid z of every tile in it**, skipping tiles
inside a tree, which is what the stitch is built from. A percentile is a
statistic: snapped to it, a coarse slab stood two or three levels off the floor
it was meant to meet wherever the block was not flat, with a pale riser between
them and the fine window's own strata showing under its rim.

A cell that touches the fine map is therefore **not a slab**. `stitch.rs` draws
it as a triangle fan over its own perimeter:

- along a side that faces fine ground the perimeter carries one vertex per
  **tile**, at that fine tile's own top level, so the two surfaces are one plane
  tile by tile;
- along every other side it carries the cell's quantised level;
- a vertex between two fine tiles takes the lower of them, so the outline is one
  polyline rather than a staircase with gaps.

The level difference is then spread across the width of the cell instead of
standing up as a riser. Consecutive perimeter vertices are shared and every one
is a corner of a triangle, so the fan has no gaps and no T-junctions inside
itself; `stitch.rs:fan_area` pins that in a test by checking the fan covers the
cell's own area exactly.

Terrace quantisation still applies **beyond** the strip: a stitched cell is the
only place a coarse surface holds a level that is not a whole one, and
`terrace.rs:cell_level` returns the stitched level there so a neighbouring
slab's riser hangs from what the stitch actually holds.

`stitch.rs:border` searches outward up to the cell's own width for the fine
edge, because the two grids do not line up — block columns are sixteen tiles and
the near band's cells are six — and `emit_cell` drops a cell only when all four
of its corners are covered, so the tiles between the last full cell and the
block boundary still get ground from one tier or the other.

A skirt in the riser's own wall side texture hangs `SKIRT` 1.5 below every
stretch of outline the fine map set, which covers both the crack where two bands
of different pitch disagree and the strata the fine map shows where its edge
falls on a slope. Elsewhere risers hang `RISER_SKIRT` 2.0 below the cell they
drop to.

The quantisation is a mode switch, and issue #6 threw it. Under
`DWARF_EYE_GROUND=smooth` — the default now that the fine tier is a smoothed
heightfield ([../pipeline/meshing.md](../pipeline/meshing.md)) — an unstitched
cell is a quad over four **corner** heights taken from the un-quantised
relieved survey (`terrace.rs:corner_top`, the mean of the four cell centres
meeting at that corner) rather than a flat slab at a whole level. Two cells of
one pitch average the same four centres, so they share the corner and the band
is one sheet with no risers in it.

The boundary snap is unchanged: a cell that touches the fine map is still the
triangle fan of `stitch.rs`, its body still at `stitched_level`, its fine-side
vertices still on the fine tiles' own levels. What changes around it is the
skirting. A smooth cell hangs one only where it cannot share a corner — a
change of pitch, a stitched neighbour, the fine rim, the world grid — and a
stitched cell hangs one along its whole outline rather than only the stretches
the fine map set, because in smooth mode its non-fine sides face a sheet at a
height it does not hold. `DWARF_EYE_GROUND=stepped` puts both tiers back to
terraces together.

## Forests

`scatter.rs` places one canonical crown per tree. A region tile's own
`tree_materials` name the species — `preset_for` maps the material name onto a
`dwarf-eye-trees` preset — and its `vegetation` sets the count:

```
count    = round(wanted * fade * treeline)
wanted   = mix(seen, PER_TILE * clamp01((vegetation - FLOOR) / (100 - FLOOR)),
               clamp01(distance_to_fine_ground / BLEND))
fade     = 1 inside NEAR, falling linearly to 0 at REACH
treeline = clamp01((TREELINE - elevation) / TREELINE_TAPER)
```

with `PER_TILE` 20, `FLOOR` 15, `NEAR` 720 tiles, `REACH` 1920, `BLEND` 200,
`TREELINE` 150 and `TREELINE_TAPER` 10.

Four rules keep that honest against what the player can actually see:

- **The fine map wins at its own edge.** `vegetation` is a 48-tile average and
  says nothing about the clearing the character is standing in.
  `fine::FineSurface::nearby_density` counts distinct tree origins per block
  column while it is surveying the ground anyway, and `crown_count_near` blends
  from that observed density at the window's edge to the survey's over `BLEND`
  tiles. A treeless window edge gives a treeless surround; a dense one continues
  the forest.
- **A tile that names no species grows nothing**, whatever its vegetation says:
  `vegetation` counts grass and shrubs too, and `tree_materials` is DF saying
  outright what wood is there.
- **Nothing grows at the tree line.** Elevation 150 and up is DF's mountain
  band, and the density tapers to nothing over the ten levels below it, so the
  line is soft rather than a ring drawn round a peak.
- **A site stands in a clearing**: no crown falls on a building footprint
  (`RegionTile.buildings`) or within `CLEARING` 6 tiles of one.

Only the count changes in any of these, and the count is a prefix of the seeded
list, so nothing ever moves. Position,
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

## The tree chain

**The far band draws the window's own chain.** `factory::chain(Class::Tree, ..)`
is one list of six stages and the horizon takes all of them
(`scatter.rs:stages`), so detail out here is the camera's distance to a tree and
nothing else. It used to be the survey the tree came from: the horizon opened at
one voxel to a tile while the window had four, three and two voxels first, and a
tree ten tiles outside the live window was visibly cruder than one ten tiles
inside it. The window's boundary showed in the canopy.

Which stage an instance draws at is decided by **the camera's distance to that
tree**, not by its distance from the window's centre. A region-sourced tree can
stand a few tiles from the eye — the live window is 144 tiles square and the
camera walks to its edge — and the old rule drew those as boxes the size of
houses.

Every placed tree is one entity per stage, each carrying its own
`VisibilityRange` (`main.rs:horizon_ranges`, over `factory::edges` and
`main.rs:band_fades` — the same two calls the canopy bands use), and Bevy does
the swapping the way it does for the canopy bands:

| stage | mesh | ends at | 720-tall window | shadow |
|---|---|---|---|---|
| 4 voxels | `horizon::grown` | `N` | 109 | casts |
| 3 voxels | `horizon::grown` | `4N/3` | 145 | casts |
| 2 voxels | `horizon::grown` | `2N` | 217 | casts |
| 1 voxel | `horizon::grown` | `4N` | 436 | a blob |
| crown | `dwarf_eye_trees::crown`, a trunk under one to three boxes | `max(8 N, 150)` | 872 | a blob |
| box | `dwarf_eye_trees::crown_box`, 12 triangles | the far plane | — | a blob |

`N` is the canopy's own near band (`canopy::near_band`, 109 tiles into a
720-tall window), so a longer lens or a taller window pushes the whole chain
out. The first four edges are the window's, to the bit
(`main.rs:the_horizon_hands_over_where_the_window_does`), and every one of them
is the projected-size rule on that cut's own leaf voxel — including the
one-voxel cut, which now runs to `4 N` rather than the `1.5 N` a separate
`GROWN_REACH` used to cap it at. The crown holds twice as far as the cut it
replaces, which is the ratio the band shipped with (`3 N` behind `1.5 N`), and
never gives way inside the shadow cascades. Hand-offs dither across
`main.rs:CROSSFADE` 0.45 of the log gap either side of the edge — the canopy's
own rule, in place of a separate `HORIZON_CROSSFADE`.

Past 1920 tiles nothing is placed at all — `terrace.rs:canopy_at` hands the
foliage back to the ground as colour over the same interval, so the two never
double-count.

Growth is the expensive half and there is no per-tree data out here to preserve,
so `grown.rs` grows `VARIANTS` 6 canonical skeletons per species at
`GROWN_HEIGHT` 7 tiles and rasterises **every cut off the one growth**, so a
tree keeps its shape as it hands over. They are held in a
`factory::StageCache` keyed by (preset and variant, stage) for the process, and
the instance transform gives each tree its height and its yaw. A batch is one
species, one variant and one stage; a batch's mesh has a height of its own and
an instance is scaled by its own height over that, so a tree is the same size
whichever stage draws it. Each cut asks the rasteriser for the near band's limb
widths (`trees::Cut::wood_like`), the same correction the canopy bands make.

Every stage wears the canopy's leaf surface on an opaque copy of the canopy
material with the horizon's block mask over it
(`main.rs:HorizonCanopyMaterial`), in two casts: the rasterised cuts take the
canopy's sky term, because their faces carry no shading of their own, and the
crown and box stages do not, because `crown.rs` already bakes a lit top and
darker sides into their vertex colours and the sky term over that takes a
crown's sides to nearly black. The cuts stay **opaque** where the window's would
wear the leaf cutout: an instanced mesh's vertex UVs are scaled by its transform
and the main pass recovers them from the world position, which the depth prepass
cannot do, so a masked instance would discard a different set of fragments in
each pass. That is the one way a horizon tree still differs from a window tree
at the same distance, and closing it means deriving the same UVs in
`cloud_shadow_prepass.wgsl`. An instanced crown cannot carry world-space
UVs in its vertex buffer the way a chunk's mesh does — the instance's scale
would take them with it and every tree would wear a different texel size — so
`cloud_shadow.wgsl:horizon_leaf` derives them from the world position instead,
which puts the near canopy's own density on a grown tree and a box crown alike.
The texture is a mipped, linearly minified copy of the leaf cutout
(`texture.rs:tiled_image`): the near band's nearest sampling with no mip chain
is pure sparkle at this range.

## Shadows

Everything inside the sun's cascades casts one. Bevy's default
`CascadeShadowConfig` reaches 150 tiles (`factory::SHADOW_DISTANCE`), and the
three finest cuts are inside or across that.

Past the cascades a shadow map has nothing left to resolve a tree with, and a
far band with no shadows at all reads as flat, so **every stage whose near edge
is at or beyond `SHADOW_DISTANCE` casts a blob instead** — today the one-voxel
cut outward, from 217 tiles (`main.rs:horizon_blob_from`). A blob is one dark
translucent quad lying on the ground, two triangles, turned to the sun's azimuth
and stretched along it by the tree's height over the tangent of the sun's
elevation, capped at `BLOB_MAX_STRETCH` 4 crown widths.
`main.rs:aim_blob_shadows` re-aims them when the sun has moved more than two
degrees and fades them out as it sets. There is **one blob a tree**, not one a
stage: it carries a range that starts where the first non-casting stage does and
runs to the far plane (`main.rs:horizon_blob_range`), so the ground under a far
wood is shaded once however many stages stand over it in turn. Before the chain
landed the crown stage ran from 164 to 327 tiles casting into cascades that
stopped at 150, which left a shadowless ring the blobs did not cover.

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

Since the chain landed, over a 22.7k-tree scatter (the viewer reports this line
itself at every horizon build):

| | triangles |
|---|---|
| ground: terraces, stitch fans, world grid, rivers, sites | 110k |
| 22.7k trees, if every one drew at 4 voxels a tile | 363M |
| 22.7k trees, if every one drew at 3 voxels | 171M |
| 22.7k trees, if every one drew at 2 voxels | 66M |
| 22.7k trees, if every one drew at 1 voxel | 17.7M |
| 22.7k trees, if every one drew its canonical crown | 679k |
| 22.7k trees, if every one drew its box | 272k |

The tree rows are alternatives, not a sum: a tree draws at one stage, and which
one is its own distance to the camera. Almost every tree is at the crown or the
box; only the handful inside 217 tiles pay for a cut. What actually reaches the
GPU is the viewer's own counter, and that is the number to read: **2.27M
triangles drawn of 4.29M held, at eye level with the camera standing in a
crown** — against 434k drawn before the chain, when everything past the window
was a box. Making the finer cuts cheaper is the edge rule's job, not a cap by
source: `MIN_LEAF_PIXELS` pulls every stage in together, for the window and the
horizon alike.

Entities are one per tree per stage plus one blob, so a six-stage chain over
22.7k trees is about 136k entities; only the stage in range draws.

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
- The coarse material is the terrain material with `horizon` set non-zero, which
  turns on the block-mask discard in both `cloud_shadow.wgsl` and
  `cloud_shadow_prepass.wgsl`. It has to discard in the prepass too, or its
  depth hides the fine ground behind it. The value says which horizon surface it
  is: **1** the coarse ground, whose UV names an atlas cell rather than a point,
  and **2** the far crowns, whose UVs the shader derives from the world
  position. The prepass reads the same struct and only tests `> 0.5`, so it
  needs no change when a kind is added — but the layout is shared and duplicated
  in `cloud_shadow_prepass.wgsl`, so a new **field** means editing three files.
- The prepass samples the base colour texture at the vertex's own UV, which for
  the coarse ground is a cell's centre and always opaque, so the alpha mask
  never discards the band. Wrapping in the prepass too would be wasted work.
- The horizon ground still casts no shadow — the same prepass shader serves the
  shadow pass and discards there. The far **trees** do cast, on their own
  material, which does not take that path.
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
under the same mask), #6 (closed, the smoothed fine tier that turned the
quantisation off and kept the boundary snap), #12 (elevation offset and other
protocol semantics).
