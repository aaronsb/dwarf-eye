# Atlas, mipmaps and texel density

Status: landed (`crates/dwarf-eye-art/src/atlas.rs`,
`crates/dwarf-eye/src/texture.rs`, `crates/dwarf-eye-world/src/library.rs`).

## What it does

Packs every ground, ramp and leaf sprite into one 2048x2048 texture so the whole
terrain draws with one material.

## How

`atlas.rs` lays out a 32x32 grid of 32-pixel cells with 16 pixels of padding on
each side, giving a stride of 64 and a side of 2048 (`atlas.rs:SIDE`).
`Atlas::insert` copies the sprite, optionally flattening pixels under alpha 128
onto a backdrop colour, then bleeds the cell's edge outward through all 16 rings
of padding. `Atlas::rect` returns UVs with a half-texel inset. Slot 0 is white,
so vertex-coloured geometry points at `atlas::WHITE_UV` and shares the material
(`mesh.rs:push_quad`, `horizon.rs`).

`library.rs` fills the atlas once during `TileLibrary::load`, in the order
`pack_leaves`, `pack_ground`, `pack_under`, `pack_ramps`. The worker sends the
finished pixels as `Event::Atlas` and `main.rs:drain_worker` uploads them.

`texture.rs:atlas_image` builds `MIP_LEVELS` 4 levels below the base by 2x2 box
filtering in linear light with alpha-weighted colour (`texture.rs:downsample`),
so transparent texels do not darken a sprite's edge. The sampler clamps,
magnifies nearest and minifies linear with linear mip filtering.

## Texel density

A DF sheet tile is 32x32 and an atlas cell is 32 wide, so a ground quad covering
one world tile shows 32 texels across. `dwarf-eye-trees::texture::DEFAULT_TEXELS`
is 32 for the same reason, and `main.rs:tree_texels` reads `DWARF_EYE_TEXELS`
over it, clamped to 4..128. Voxel resolution is separate:
`library.rs:DEFAULT_GRID` is 12 sub-voxels per tile edge for sprite models
(`DWARF_EYE_GRID`, 4..32), and `tree.rs:DETAIL` is 4 for grown trees.

## Invariants and gotchas

- Capacity is 1024 cells and `Atlas::insert` returns `None` past that. Every
  caller skips silently, so sprites simply stop appearing. Ramps alone cost 47
  cells per family.
- `PAD` 16 in `dwarf-eye-art` and `MIP_LEVELS` 4 in `dwarf-eye` encode the same
  invariant in two crates. Four halvings leave a 4-pixel cell with a pixel of
  its own bleed each side; more levels, or less padding, bleeds neighbours in.
- `DWARF_EYE_NO_MIPS` triggers on presence, so `DWARF_EYE_NO_MIPS=0` still
  disables mipmaps.
- The atlas is uploaded once. Anything packed after `TileLibrary::load` never
  reaches the GPU.
- Tree surfaces are separate images with `Repeat` addressing, nearest
  minification and no mip chain (`main.rs:tree_texture`).
- `ModelKey.dirs` is always 0 in `TileLibrary::model`; the field is inert and
  the tiletype id carries the distinction today.

## Related issues

#13 (walls would add a large family of cells to the same atlas).
