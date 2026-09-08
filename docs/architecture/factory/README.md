# The entity factory

Status: the registry is landed for vegetation
(`crates/dwarf-eye-world/src/factory.rs`); trees, shrubs, saplings, dead plants
and tufts route through it (issue #1). Items, units and buildings are still
unclassified (issues #15, #5).

## What it does

Take DF's own entity stream, classify every tile, plant, item and building, and
resolve each class to a treatment. The default treatment is the faithful
sprite-derived geometry that already exists; an override replaces the look and
nothing else.

```mermaid
flowchart LR
  E[DF entity: tile shape, material, species, item or building id, neighbourhood]
  E --> C[classify -> Class]
  C --> R[resolve -> Treatment]
  R --> D[default: sprite-derived model]
  R --> P[plant: dwarf-eye-trees grammar]
  R --> V[prefab: vox-uristi .vox]
  D --> M[chunk mesh]
  P --> M
  V --> M
```

## The overlay idea

A stock DF entity keeps its DF extent. The game says where the thing is, how
tall it is and how far it reaches; the treatment decides what it looks like
inside that. Nothing an override draws leaves the space the fortress map gave
it, so the map still reads as the same map.

Trees are the worked example. DF contributes height, a radius cap, the species
and a seed; the grammar in `dwarf-eye-trees` supplies the shape
([plants.md](plants.md)).

## Pages

| Page | |
|---|---|
| [plants.md](plants.md) | the L-system replacement for a stock plant, and the seed rules |
| [prefabs.md](prefabs.md) | voxel prefab instances such as vox-uristi's `.vox` buildings |
| [registering.md](registering.md) | how a new override is registered |

## The registry

`factory.rs` is five calls and no state:

```
classify(Tile, Near) -> Class      // Tree, Shrub, Sapling, DeadTree, TallGrass, Boulder, Built, Other
extent(Tile) -> Extent             // Tile or Tree: how much room DF gives it
resolve(Class, Style) -> Treatment // Sprite, Grown(VegetationKind, TreeParams), Billboard
plan(Tile, Near) -> Plan           // the pair, decided once per tiletype
seed(x, y, z, species) -> u64      // absolute tile and species, nothing else
```

`Tile` is what DF says about a tiletype: shape, material, special, DFHack's
name, and the species standing there. `Near` carries only what the tile cannot
say for itself — today, whether DF links it to a tree, which is what separates
a cap tile from masonry.

`TileLibrary::load` calls `plan` once per tiletype and keeps the answer, so a
mesher asking per tile pays one hash lookup.
`mesh.rs:build_chunk_budgeted` asks `Plan::grown`: what the factory grows is
skipped by the sprite path, and a one-tile plant keeps the ground slab the
billboard used to stand on. `Style::current` reads `DWARF_EYE_PLANTS=billboard`
once, which puts standing plants back on crossed sprites for comparison.

Sprite geometry is unchanged underneath: `library.rs:mode_for` still maps a
`TiletypeShape` to one of five `RenderMode` values for everything the factory
leaves alone.

| `RenderMode` | Tiles | Geometry |
|---|---|---|
| `Extrude` | trunks, cap walls | full-height mask |
| `ThinExtrude` | branches, twigs | a slab through the middle |
| `Billboard` | saplings, shrubs, boulders | two crossed vertical planes |
| `FlatTile` | floors, pebbles | a textured slab from the atlas |
| `Ramp` | ramps | a wedge from the ramp sheet |

## Claims to verify

The repo [README](../../../README.md) lists the flat tile treatment as "not
wired yet". The code is right and the README is stale: `library.rs:model` has
resolved `FlatTile` to an atlas cell since the ground atlas landed.

## Invariants

- Every override receives the entity's DF extent and stays inside it.
- Seeds come from absolute coordinates and species, never from render
  coordinates and never from the tile configuration around the entity.
- Classification also feeds the heightfield work, so a class has to say what a
  tile is, not only how to draw it.
- `Class::Built` is what someone raised: nothing vegetal is grown into it, and
  a crown's envelope is clipped by it (`tree.rs:Envelope::blocked`).

## Related issues

#5 (the registry), #1 (shrubs, saplings, dead trees, streamers), #7 (ground
cover), #15 (units, items and buildings), #6 (heightfield needs the same
classification).
