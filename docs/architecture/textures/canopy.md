# World-space canopy surfaces

Status: landed (`crates/dwarf-eye-trees/src/texture.rs`,
`crates/dwarf-eye-world/src/canopy.rs`, `crates/dwarf-eye/src/main.rs`).

## What it does

Gives bark, leaves and weeping strands a texture at Dwarf Fortress's own texel
density, independent of how finely a tile is cut into voxels.

## How

`dwarf-eye-trees::texture` generates three surfaces at `texels` square:
`leaf_cutout(needle, openness, texels)`, `bark(texels)` and
`streamer_strip(texels)`. `main.rs:setup` bakes exactly two cutouts for the
whole world, broadleaf at openness 0.34 and needle at 0.5, plus one bark and one
strip, and wraps them with `main.rs:tree_texture`: `Repeat` addressing, nearest
filtering, no mip chain.

UVs are world coordinates in tiles, one repeat per tile
(`canopy.rs:push_face`). A vertical face takes v from world height divided by
`Z_SCALE`, so bark fissures run up a trunk; a horizontal face takes both from
x and z. A merged run of many voxels therefore shows the same texel size as a
single one, and as the ground beside it.

Which cutout a face gets is decided by the tree's habit:
`canopy.rs:voxelise` picks `Surface::Needle` for `Habit::Conifer` and
`Surface::Broadleaf` otherwise, and `canopy.rs:Volume::intern` keys the palette
on colour **and** surface so one shade on bark and on leaves stays two slots.

Cutout materials are `AlphaMode::Mask(0.5)`, double sided with no culling,
roughness 0.97, and `diffuse_transmission` from `shadow.rs:leaf_transmission`
(default 0.1, `DWARF_EYE_LEAF_LIGHT`) so a backlit leaf glows.

A fifth material, `CanopyMaterials::leaf`, carries the same leaf shading with no
texture and `AlphaMode::Opaque`, back faces culled. It is what the far canopy
band wears (`docs/architecture/lod/README.md`). The three cut bands before it —
near, close and mid — all keep the cutouts, since a half-resolution crown at 2N
still shows holes about a pixel wide and they are what lets light through it,
and only past the far edge does
the mask buy nothing while still costing a masked pass, a discard in the depth
prepass and the overdraw behind every hole. `canopy.rs:Band::coats` is where a
band's four meshes are matched to materials, and it is the one place that says
the far band wears no cutout.

## Invariants and gotchas

- The holes are the point: sun and sky come through the crown at texel scale,
  and the shadow under it is dappled at that scale.
- Per-species `cutout_openness` exists in `dwarf-eye-trees::params` but only the
  tree lab uses it. The renderer shares two cutouts.
- `mesh.rs:mesh_of_texels` addresses streamer quads against
  `streamer_uv(cell, texels)` at a caller-given density; `mesh`/`mesh_of` stay
  thin wrappers at `DEFAULT_TEXELS` (32) for callers that predate this. The
  tree lab threads its own `TexelDensity` resource through, so `-`/`=` and
  `DWARF_EYE_TEXELS` now rebuild streamers at the strip's own density (issue
  #25; a shot at 16 texels/tile shows the leaflets landing on their cells
  rather than sampling a neighbour's). The viewer meshes strands through
  `texture::live_texels()`, the one reader of `DWARF_EYE_TEXELS`, so the strip
  image and its UVs cannot disagree.
- Tree surfaces carry no mipmaps and minify nearest, so they alias at distance
  where the ground atlas does not.
- Leaf tones come from the species' own twig sprite, darkest first
  (`library.rs:leaf_tones`); bark tones fall back to shades of plain bark when
  the trunk sprite is a near-grey pattern (`library.rs:bark_tones`).

## Related issues

#1 (shrubs, saplings and dead trees through the same surfaces), #7 (ground
cover), #14 (extra materials per class is the pattern the canopy already
shows), #25 (open: streamer UVs at a live texel density, threaded as far as
the crate and the tree lab; `canopy.rs`'s own meshing still wants it).
