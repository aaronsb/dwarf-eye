# GPU instancing

Status: landed (`crates/dwarf-eye/src/instancing.rs`,
`crates/dwarf-eye/src/instancing.wgsl`, issue #34). The far band's tree chain
draws through it; the window's own bands and the blob shadows still do not.

## What it does

Draws a forest with no entity per tree and no baked copy per tree: the whole
scatter is one storage buffer of instances, a compute pass per view decides
which of them draw, and each (mesh, material) group is one
`draw_indexed_indirect`.

## Why

The far band places one canonical crown per tree over a six-stage chain, and a
tree was an entity per stage. Measured on the chain merge, 88k of those entities
gave **5 fps at 4.5 M triangles and 16 fps at 240 k**: per-entity CPU work —
visibility ranges, transform propagation, extraction — was the bound, and the
triangles were not. Baking a cell's trees into one mesh
(`horizon/batch.rs`) cut the entities to a few thousand and got the frame back,
at the cost of one copy of the mesh per tree in memory and a rebuild of all of
it on every window move. Instancing removes both: the ECS holds nothing per
tree, and the GPU holds one copy of each mesh.

## The pipeline, in ten lines

1. `main.rs:instance_batches` turns the scatter's `CrownBatch`es into
   `instancing::Batch`es: a mesh, and a transform plus a band per tree.
2. `Instanced` is one main-world resource, replaced whole when the horizon is
   rebuilt and extracted only when its generation changes.
3. `prepare_store` concatenates every batch into one vertex buffer per attribute
   and one index buffer, one instance buffer, and one `DrawIndexedIndirectArgs`
   template per batch.
4. `prepare_view_instances` gives every view — the camera and each shadow
   cascade — a compacted index list, a copy of the draw arguments with the
   instance counts zeroed, and a uniform holding that view's clip matrix, eye
   and six frustum planes.
5. `cull` dispatches one thread per instance per view, before any pass runs.
6. Each thread tests the instance's bounding sphere against the frustum, then
   its distance to the eye against its own band.
7. A survivor takes a slot with one `atomicAdd` on its batch's instance count
   and writes its index into that batch's slice of the compacted list.
8. `queue_instances` puts one `BinnedRenderPhaseType::NonMesh` item per view into
   `Opaque3d`, `Opaque3dPrepass` and `Shadow`.
9. `DrawInstanced` binds the geometry once, then per batch sets the instance
   bind group with that batch's dynamic uniform offset and issues
   `draw_indexed_indirect`.
10. The vertex shader reads `visible[instance_index]`, looks the instance up and
    applies its yaw, scale and position.

## The bands, and why one tree draws once

A stage's `VisibilityRange` is written into every instance of it as four
distances — the fade-in's start and end, then the fade-out's — so the compute
shader runs `main.rs:band_fades`'s own rule rather than a second one.

Bevy dithers a crossfade **per pixel**, which is why a ranged mesh compiles with
`VISIBILITY_RANGE_DITHER` and discards on every fragment it ever draws. The
instanced path dithers **per tree** instead: each tree carries a threshold
hashed from its own position (`instancing::dither_of`), every stage of that tree
gets the same number, and a stage draws the tree when its fade-in has passed the
threshold and its fade-out has not. The ramps are monotonic and ordered outward,
so exactly one stage satisfies that at any distance — pinned by
`instancing.rs:exactly_one_stage_draws_a_tree_at_any_distance`. At this range a
tree is small enough that swapping the whole of it reads as the same soft
hand-off, and it costs no discard.

## The three passes

The main pass, the depth prepass and the sun's shadow cascades run **the same
vertex shader**. A custom vertex shader that only the main pass runs leaves the
depth buffer holding untransformed geometry, which breaks the shadow map and the
occlusion-culling depth pyramid together; that is exactly the trap behind the
old note that "a custom vertex shader breaks the prepass". `MAIN_PASS` is the
only thing that differs, and it only adds the shading.

Culling is per view, not per camera, so a cascade culls against its own frustum:
a tree behind the eye is dropped from the main pass and still casts into the
shadow map. A batch whose stage begins at or beyond the cascades
(`main.rs:horizon_blob_from`) is skipped in the shadow pass entirely, which is
where `NotShadowCaster` went.

## Bind groups

| group | holds |
|---|---|
| 0 | Bevy's mesh view bind group: lights, shadow maps, fog, the atmosphere |
| 1 | Bevy's view binding array: the environment map, irradiance volumes, decals |
| 2 | ours: instances, the compacted list, the per-batch uniform, the per-view uniform, the scene uniform, the leaf surface, the cloud shadow bake, the block mask |

Groups 0 and 1 are Bevy's own, taken whole from `MeshPipeline::get_view_layout`
for the view's own `MeshPipelineKey`. **Group 1 is not optional here**: the
canopy's sky term is the environment map's fill scaled down
(`shadow.rs:canopy_sky`), so dropping the binding array would light an instanced
tree differently from a window tree and put the window's boundary back into the
canopy. Group 2 is where Bevy would put the mesh bind group, which a pipeline
with its own transforms does not use.

The depth passes need no view bindings at all — the vertex shader takes its clip
matrix from the per-view uniform — so there the instance group is group 0.
`INSTANCE_BIND_GROUP` is a shader def, not a constant in the file.

## Cost

Measured against the shipped world (`The Dimension of Griffons`), 14.7k trees,
at the eye-level framing `DWARF_EYE_CAM=0.6 DWARF_EYE_VIEW=0,-12` and from 400
tiles up, at 1280x720:

| | entities | fps at eye level | fps from the air |
|---|---|---|---|
| per-cell baked (`DWARF_EYE_INSTANCING=0 DWARF_EYE_HORIZON_MERGE=1`) | 4424 | 64 | 83 |
| instanced | 680 | 75 | 120 |

Both draw the same picture. The far band's own counters at the air framing:
25254 instances, 2861 drawn, 22393 culled, 50k triangles.

The synthetic bench (`cargo run --release --bin instance-lab`, no game) holds
100k instances of one canonical crown over 900 tiles: **81.7k drawn at 2.45 M
triangles and 121 fps** at eye level looking into the wood, and 0 drawn, 100k
culled at the same rate with the camera turned round.

## Invariants and gotchas

- The GPU record and `instancing.rs:GpuInstance` are one layout in two places
  and a test pins its size. A `vec3<u32>` tail would align the record to 16 and
  round it up to 96 bytes; three scalars keep it at 80.
- **One shader file, three `#ifdef`s.** Splitting the record into a
  `#define_import_path` library makes naga_oil emit a copy of it per importing
  module, and a function then rejects a value of what looks like its own
  parameter type ("Argument 0 value doesn't match the type"). The compute pass
  lives in `instancing.wgsl` for that reason alone.
- **A view's `MeshPipelineKey` carries no primitive topology.**
  `MeshPipelineKey::primitive_topology` reads zero as `PointList`, so a crown
  specialised straight off `ViewKeyCache` draws as sixty scattered dots.
  `instancing.rs:with_topology` is the one line that fixes it.
- Pipeline errors are not logged. `PipelineCache::get_render_pipeline_state`
  returns them and nothing prints them, so a shader that fails to compile shows
  up only as geometry that never appears.
- Bevy's own render-system order is Queue, QueueMeshes, PhaseSort, Prepare,
  PrepareBindGroups, Render: anything a queue system needs has to be prepared in
  `RenderSystems::Queue`, not in `Prepare`.
- Nothing is read back. The HUD's drawn count is the same rule
  (`instancing::survives`) run on the CPU for the camera's own view, which the
  tests pin against the shader's arithmetic; a readback would either stall the
  frame or arrive late for a number only a human reads.
- The blob shadows are still merged per cell, because a blob is re-aimed in
  place when the sun moves rather than transformed. They are the one stage of
  the far band left outside this path.
- Bevy's depth pyramid is reachable
  (`bevy_core_pipeline::mip_generation::experimental::depth::ViewDepthPyramid`
  is a public component with public texture views), but the cull does not test
  against it yet: the frustum and band tests already drop nine tenths of the
  scatter, and the pyramid's size is the depth buffer rounded down to a power of
  two, which the UV mapping would have to account for.

## Related issues

#34 (this), #10 (the window's own bands, which could take the same path), #5
(the one chain the bands and the instances share).
