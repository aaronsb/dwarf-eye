# Registering an override

Status: planned (issue #5). The path a tree takes today is the pattern the
registry generalises.

## The path today

A tree becomes geometry through four steps, none of them registered:

1. `library.rs:mode_for` and `library.rs:canopy_part` classify the tiletype at
   load, from its `TiletypeShape` and its name.
2. `mesh.rs:build_chunk_budgeted` early-outs on `TileLibrary::is_trunk` and
   `TileLibrary::canopy_part`, so no tile of a tree is drawn from its sprite.
3. `canopy.rs:Forest::nearby` finds the origins whose tiles reach the chunk, and
   `Forest::tree` grows and caches one volume per origin.
4. `canopy.rs:emit` sorts merged faces into the four canopy meshes, which
   `main.rs:upload_chunks` pairs with the four canopy materials.

## The intended shape

Issue #5 puts a registry in `dwarf-eye-world`:

```
classify(tile shape, material, species, item or building id, neighbourhood) -> Class
resolve(Class) -> Treatment
```

`Treatment::Default` is the sprite-derived geometry of `library.rs:model`.
Overrides are registered per class: trees first, then shrubs and one-tile plants
such as rhubarb, boulders, dead trees, and buildings as `.vox` prefabs.

## What registering one will involve

- A `Class` the classifier can produce from what a tile already carries. Adding
  a class must not need a second pass over the map.
- A treatment that takes the entity's DF extent and returns geometry inside it,
  plus the seed rule for anything procedural: absolute coordinates and species.
- A material, if the surface is not the terrain material. `CanopyMaterials` is
  the worked example of one class owning several: bark, broadleaf, needle and
  streamers, each an entity of its own per chunk.
- The corresponding skip in the default path, which is what the registry lookup
  replaces.
- A unit test for any new pure logic, and a screenshot pair for the look
  ([../testing/README.md](../testing/README.md)).

## Invariants

- One entity, one treatment. A tile drawn by an override is not also drawn by
  the default path.
- A treatment is a pure function of the entity, its extent, its seed and the
  species data. It may not read the render origin or the camera.
- Caches are keyed by absolute position, so a treatment's output survives a
  reload (`canopy.rs:Forest` keys on the tree's origin).

## Related issues

#5 (the registry), #1 and #7 (the plant treatments it will carry), #15
(buildings and units), #6 (classification feeds the heightfield).
