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
texture and `AlphaMode::Opaque`, back faces culled. It is what the mid canopy
band wears (`docs/architecture/lod/README.md`): past the band edge a hole in the
cutout is under a pixel, so the mask buys nothing and costs a masked pass, a
discard in the depth prepass and the overdraw behind every hole.
`canopy.rs:Band::coats` is where a band's four meshes are matched to materials,
and it is the one place that says the mid band wears no cutout.

## Invariants and gotchas

- The holes are the point: sun and sky come through the crown at texel scale,
  and the shadow under it is dappled at that scale.
- Per-species `cutout_openness` exists in `dwarf-eye-trees::params` but only the
  tree lab uses it. The renderer shares two cutouts.
- `dwarf-eye-trees::mesh` calls `streamer_uv` with `DEFAULT_TEXELS` hardcoded
  while the strip image is generated at `tree_texels()`. Setting
  `DWARF_EYE_TEXELS` to anything but 32 misaligns streamer UVs from the strip.
- Tree surfaces carry no mipmaps and minify nearest, so they alias at distance
  where the ground atlas does not.
- Leaf tones come from the species' own twig sprite, darkest first
  (`library.rs:leaf_tones`); bark tones fall back to shades of plain bark when
  the trunk sprite is a near-grey pattern (`library.rs:bark_tones`).

## Related issues

#1 (shrubs, saplings and dead trees through the same surfaces), #7 (ground
cover), #14 (extra materials per class is the pattern the canopy already shows).
