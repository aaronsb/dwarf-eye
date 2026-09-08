# Registering an override

Status: landed for vegetation (`factory.rs`); prefabs and items still to come
(issues #5, #15).

## The path a plant takes

1. `library.rs:TileLibrary::load` calls `factory::plan` once per tiletype and
   keeps the `Class` and `Extent` on the tiletype's entry.
2. `mesh.rs:build_chunk_budgeted` asks `Plan::grown`, and skips the sprite path
   for what the factory grows; a one-tile plant still gets its ground slab.
3. `canopy.rs:Forest::nearby` collects the tree origins reaching the chunk
   (`TileLibrary::of_tree`) and `Forest::tree` grows and caches one volume per
   origin; `Forest::sow` grows and caches one plant per tile of the chunk.
4. `canopy.rs:emit` sorts merged faces into the four canopy meshes, which
   `main.rs:upload_chunks` pairs with the four canopy materials; plants are
   stamped into the same meshes.

`Treatment::Sprite` is the sprite-derived geometry of `library.rs:model`, which
is what every unregistered class still gets. Still to come: boulders as
something other than a billboard, and buildings as `.vox` prefabs.

## What registering one involves

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
  ([../testing/README.md](../testing/README.md)). `factory.rs` is pure, so
  classification and the seed rule are unit-tested on synthetic tiles.

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
