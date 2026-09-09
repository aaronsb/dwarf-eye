# Meshing

Status: landed (`crates/dwarf-eye-world/src/mesh.rs`,
`crates/dwarf-eye-world/src/heightfield.rs`,
`crates/dwarf-eye-world/src/water.rs`, `crates/dwarf-eye-world/src/canopy.rs`,
`crates/dwarf-eye/src/main.rs`).

## What it does

Turns one decoded chunk into triangle arrays, split by the material each surface
wants.

## How

`mesh.rs:build_chunk` walks the chunk's 256 tiles, culls faces against
neighbouring chunks through `World::voxel`, and emits either a sprite-derived
model or a plain cuboid. `MeshData` holds positions, normals, colours, UVs and
indices; the renderer copies them straight into a Bevy mesh
(`main.rs:to_bevy_mesh`). `mesh.rs:build_chunk_budgeted` is the same pass with a
`Budget` tally, which `make budget` prints.

Water is meshed separately too, by `water.rs:build_chunk`, and rides back
inside `MeshData::water`; `main.rs:upload_chunks` lifts it out with
`MeshData::take_water` and gives it its own entity on `main.rs:WaterMaterial`.
It travels inside the terrain buffer only so a chunk stays one value on the
channel.

Crowns are meshed separately by `canopy.rs:Forest::build_chunk` into
`CanopyMeshes`, four buffers: bark, broadleaf, needle, streamers. Each becomes
its own entity with its own material (`main.rs:CanopyMaterials`,
`main.rs:upload_chunks`).

Shading is baked into vertex colour per face (`mesh.rs:shade`), material colour
is pulled toward its own brightness (`mesh.rs:damp`), and a per-tile hash gives
a brightness wobble so a hillside of one material is not a painted plane
(`mesh.rs:jitter`).

## The ground heightfield

`heightfield.rs` draws natural ground as one smoothed sheet instead of a slab
per tile. `mesh.rs:build_chunk_in` builds a `Surface` per chunk, the tile loop
skips every tile that surface covers, and `Surface::emit` draws them all
afterwards.

Heights live on tile **corners**. A corner is the mean of what the four tiles
touching it ask for, so both chunks either side of a border compute a shared
corner from the same four tiles and the sheet has no seam — the trick
`water.rs` already uses. What a tile asks for:

| Tile | Asks for |
|---|---|
| natural floor, and the tile a shrub or a boulder stands in | its own slab top, `z + FLOOR_HEIGHT` |
| natural ramp | `ramp::slopes` at each corner: the field `ramp.rs` cut its wedge from |
| natural wall with natural ground on top | that ground's height, which is what a slope's high edge climbs to |
| natural wall with nothing on top | nothing, and it **pins** the corners it touches |
| constructed floor, stair, building, tree, anything worked | nothing, and it pins |

The classification is the factory's (`factory.rs:footing`, cached in
`Plan::footing`): `Ground`, `Slope`, `Cliff`, `Tile`. `heightfield.rs:probe`
reads it straight off the palette, so walk mode can ask without a sprite
library.

Smoothing is `SWEEPS` 2 Jacobi sweeps, each pulling a corner `RELAX` 0.5 of the
way toward the mean of its four cardinal neighbours. Pinned corners never move,
so a cliff edge, a road and a workshop floor keep the height the game gave them
and the sheet meets them flush. Two sweeps reach two rings, so a chunk is built
over its own tiles plus a `BORDER` of two and every corner it keeps is the
corner its neighbour computes — same inputs, same stencil, same answer, no
crack. A wider border would be needed for a third sweep.

Each covered tile becomes one two-triangle patch, folded along whichever
diagonal the surface is flatter across, with per-corner normals from central
differences and the tile's own ground sprite projected straight down onto it.
The sprite comes back off the cached model (`heightfield.rs:ground_uv`), so the
sheet wears exactly what the flat slab wore. Colour stays per tile, jitter and
all, so the ground still reads as ground rather than a painted plane. The rim
shows on the same rule the slab used: only where the tile beside it is open and
nothing solid stands under it.

`DWARF_EYE_GROUND=stepped` puts the terraces back
(`heightfield.rs:Ground::current`, printed at startup). Every heightfield path
hangs off one `covered` flag that is false whenever no surface was built, so
stepped mode is the pass as it stood.

### Grounding

The heightfield publishes `h`, and everything standing on the ground drops to
it. `heightfield.rs:grounded` is the whole rule: an entity stood on the tile's
own slab before and may sink to meet the sheet, never rise off it.

- one tile — a plant, a boulder — takes `h` at the tile's centre
  (`canopy.rs:sow`);
- a footprint — a building massing, an item pile — puts its lid on `h` at the
  centre and stretches its bottom down to the tile's lowest corner, which is
  the skirt (`mesh.rs:build_furnishings`).

## Geometry per tile

| `Solid` | Geometry |
|---|---|
| `Cube`, `Fortification` | full cuboid, faces culled by `occluded`; a wall samples the atlas, the neighbour variant on its lid and a derived face on its sides (`MeshData::textured_cuboid`), anything else keeps its vertex colour |
| `Floor` | a patch of the ground sheet where natural; otherwise a slab `FLOOR_HEIGHT` 0.12 thick, rim faces only at a drop |
| `Ramp` | a patch of the ground sheet where natural; a constructed one keeps the wedge from the ramp sheet, or a half-height block with no sprite |
| `Stair` | two stacked boxes |
| `Foliage` | inset box, 0.85 tall |
| magma | box whose height is the fill level over 7, opaque material |
| water | a surface with corner heights, on its own translucent material |

## Invariants and gotchas

- Sideways, an unloaded neighbour counts as open, so a chunk border keeps its
  walls; below the lowest loaded level nothing is ever seen
  (`mesh.rs:build_chunk_budgeted`, the `occluded` closure).
- A floor above hides the top of whatever it rests on, which is the `lid` case
  in the same closure.
- Tiles that belong to a tree are skipped here outright: `TileLibrary::is_trunk`
  or `TileLibrary::canopy_part` returning something means the canopy path owns
  that tile. Those two early-outs are what the entity factory replaces
  ([../factory/README.md](../factory/README.md), issue #5).
- A trunk with more trunk above it drops its top cap (`model.rs:Caps`). This is
  what keeps a forest affordable.
- Terrain ramps carry no direction, so the high side comes from whichever of
  `ramp.rs:NEIGHBOURS` is a wall. A wall's own sprite is picked the same way,
  from `wall.rs:NEIGHBOURS`, rather than from DF's `Tiletype::direction`
  ([../textures/walls.md](../textures/walls.md)).
- Magma still draws as an opaque box with vertex alpha the material ignores.
  Greedy-merging terrain cubes is the rest of issue #16; the merge already
  exists for crowns in `canopy.rs:emit` and in `dwarf-eye-trees::mesh`.
- Natural ground is one smoothed sheet; the tile loop draws no top face and no
  side face for a tile the sheet covers, and the sheet draws its own rim.
  Constructed floors, buildings, stairs, water and anything a plant or a tree
  grows out of keep their tile geometry.

## Water

`water.rs` meshes every wet tile of a chunk, whatever else that tile is
drawing: a pool's rim tiles are ramps, and the sprite paths in `mesh.rs` have
already moved on from them by the time a liquid would be emitted.

Each corner of a tile takes the mean of the four tiles that touch it. A liquid
tile contributes its own fill level; a wall, an unloaded tile, or a column that
carries on above contributes the tile's own level, which leaves the corner
alone; anything else — floor, ramp, open air — contributes nothing, so the
surface sinks to the ground there. Both tiles either side of an edge average
the same four tiles, so they agree on the two heights they share and the sheet
has no seam. That is also why no face is needed between two liquid tiles.

Faces: the top of each column, and a side only where the tile beside it is open
air at the same level. Never between two liquids, never into a wall, never
underneath, and none at all where the tile above is liquid too, or where a full
surface meets a floor resting on it.

Colour is vertex data: `palette.rs:water_color` reads the surface height plus
the tiles of water stacked below and returns a teal that darkens and thickens
with depth, so a shore is nearly clear and open water is not.

## Related issues

#5 (registry lookups in place of the scattered checks), #16 (magma
transparency and greedy meshing), #10 (mid LOD), #6 (closed, this
heightfield), #29 (closed, this water), #13 (closed, textured walls).
