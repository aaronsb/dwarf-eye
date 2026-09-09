//! Walls, textured from DF's own environment sheets.
//!
//! DF only ever sees a wall from above, and its sheets say so. One sheet per
//! surface — `wall_stone.png`, `wall_soil.png`, `wall_rock_blocks.png` — holds
//! fifteen sprites named by the neighbouring walls the tile joins,
//! `STONE_WALL_N` through `STONE_WALL_N_S_W_E`, drawn as a near-grey dither to
//! be multiplied by the material's colour at draw time (issue #26). There is no
//! sprite for a wall with nothing beside it: DF fills that tile from the four
//! corner pieces instead.
//!
//! Two things follow for a voxel view. The top face is DF's own picture and
//! wants only the right variant. The sides do not exist on the sheet at all —
//! measured, the north, south, east and west bands of `wall_stone` differ by
//! two luma levels out of 255, so nothing on the sheet is a lit face — so a
//! side is cut from the same sprite here, for tiling rather than for meaning.

use dfhack_remote::rfr::{TiletypeMaterial, TiletypeSpecial};
use dwarf_eye_art::Sprite;
use dwarf_eye_art::raws::{EAST, NORTH, SOUTH, WEST};

/// The four neighbours a wall joins, as `(bit, dx, dy)` on DF's x-east,
/// y-south grid. The bits are the raws' own, so a mask reads straight into a
/// sprite name.
pub const NEIGHBOURS: [(u8, i32, i32); 4] =
    [(NORTH, 0, -1), (SOUTH, 0, 1), (WEST, -1, 0), (EAST, 1, 0)];

/// A wall joined on all four sides.
pub const ALL: u8 = NORTH | SOUTH | WEST | EAST;

/// The variant a neighbour set draws.
///
/// DF ships no sprite for a wall with nothing beside it — it draws that tile
/// from the four corner pieces, which a single quad cannot carry — so a lone
/// wall takes the fully connected sprite, the only one that covers the whole
/// tile.
pub fn variant(mask: u8) -> u8 {
    let joined = mask & ALL;
    if joined == 0 { ALL } else { joined }
}

/// The suffix DF gives the sprite for a neighbour set, in its own `N S W E`
/// order.
pub fn sprite_suffix(mask: u8) -> String {
    const ORDER: [(u8, &str); 4] = [(NORTH, "N"), (SOUTH, "S"), (WEST, "W"), (EAST, "E")];
    ORDER
        .iter()
        .filter(|(bit, _)| variant(mask) & bit != 0)
        .map(|(_, name)| *name)
        .collect::<Vec<_>>()
        .join("_")
}

/// The sprite names to try for a family and neighbour set, best first.
///
/// Where a sheet holds four random cuts of the same tile it numbers them, so
/// `STONE_WALL_N_1` and `SOIL_WALL_N_S_W_E_1` exist while
/// `SMOOTHED_STONE_WALL_N` stands alone. Asking for the numbered one first and
/// the plain one after covers both without a table of which sheet is which.
pub fn sprite_names(family: &str, mask: u8) -> [String; 2] {
    let plain = format!("{family}_{}", sprite_suffix(mask));
    [format!("{plain}_1"), plain]
}

/// Every neighbour set that has a sprite of its own, so the atlas can be
/// filled before the first frame.
pub fn masks() -> impl Iterator<Item = u8> {
    1..=ALL
}

/// Which of DF's wall sheets a tiletype draws from, or `None` for a wall the
/// sprite library already draws.
///
/// The material and the `SMOOTH` special are the whole story. DF has no
/// engraved tiletype — an engraving lives in the block's engraving list, not in
/// the tile — so `wall_stone_engraved.png` is unreachable from here, and the
/// three worn stone sheets differ from the plain one only in surface noise and
/// are not worth their cells.
pub fn family_for(
    material: TiletypeMaterial,
    special: TiletypeSpecial,
    name: &str,
) -> Option<&'static str> {
    use TiletypeMaterial as M;
    use TiletypeSpecial as S;

    // A trunk, a root and a mushroom cap all wear a wall shape; they are the
    // sprite library's, drawn from the species' own sheets.
    if name.starts_with("Tree")
        || matches!(material, M::TreeMaterial | M::Mushroom | M::Plant | M::Root)
    {
        return None;
    }

    let smooth = matches!(special, S::Smooth | S::SmoothDead);
    Some(match (material, smooth) {
        (M::Soil, _) => "SOIL_WALL",
        (M::Construction, _) => "ROCK_BLOCKS_WALL",
        // Semi-molten rock is DF's `MAGMA` tiletype material, and the rock a
        // magma sea has cooled at its edge is `LAVA_STONE`. Both are the wall
        // of the magma sea; the sheet DF cut for them is the same one, and
        // rough lava stone anywhere else is still rock that ran.
        (M::Magma, _) => "MAGMA_WALL",
        (M::LavaStone, false) => "MAGMA_WALL",
        (M::Mineral, false) => "ORE_VEIN_WALL",
        (M::FrozenLiquid, false) => "ICE_WALL",
        (M::FrozenLiquid, true) => "SMOOTHED_ICE_WALL",
        (M::Stone | M::LavaStone | M::Feature | M::Hfs | M::Mineral, true) => {
            "SMOOTHED_STONE_WALL"
        }
        (M::Stone | M::Feature | M::Hfs, false) => "STONE_WALL",
        _ => return None,
    })
}

/// How dark the rock behind a wall sprite is, as a fraction of the sprite's
/// own mean.
const BACKDROP: f32 = 0.55;

/// The rock a wall sprite is drawn against.
///
/// DF composites its sheets onto the black of an unlit map, which is right for
/// a map read from above and wrong for a face the renderer lights. A darker
/// cast of the sprite's own mean keeps the dither reading as depth without
/// painting half the tile black.
pub fn backdrop(sprite: &Sprite) -> [u8; 3] {
    let (mut sum, mut n) = ([0u32; 3], 0u32);
    for p in sprite.pixels.iter().filter(|p| p[3] > 0) {
        for i in 0..3 {
            sum[i] += p[i] as u32 * p[3] as u32;
        }
        n += p[3] as u32;
    }
    if n == 0 {
        return [0, 0, 0];
    }
    std::array::from_fn(|i| ((sum[i] / n) as f32 * BACKDROP).min(255.0) as u8)
}

/// Composites `front` over `back`, pixel for pixel.
pub fn over(front: &Sprite, back: &Sprite) -> Sprite {
    let pixels = front
        .pixels
        .iter()
        .zip(&back.pixels)
        .map(|(f, b)| {
            let a = f[3] as u32;
            let out: [u8; 3] =
                std::array::from_fn(|i| ((f[i] as u32 * a + b[i] as u32 * (255 - a)) / 255) as u8);
            [out[0], out[1], out[2], f[3].max(b[3])]
        })
        .collect();
    Sprite { width: front.width, height: front.height, pixels }
}

/// Flattens a wall sprite onto an opaque backdrop.
///
/// DF's wall art is a four-level dither — alpha 0, 71, 168 and 255 over four
/// greys — so the alpha channel carries as much of the rock as the colour
/// does. Blending it rather than cutting it at half opacity, the way ground
/// sprites are flattened, is what keeps that texture.
pub fn opaque(sprite: &Sprite, backdrop: [u8; 3]) -> Sprite {
    let pixels = sprite
        .pixels
        .iter()
        .map(|p| {
            let a = p[3] as u32;
            let out: [u8; 3] = std::array::from_fn(|i| {
                ((p[i] as u32 * a + backdrop[i] as u32 * (255 - a)) / 255) as u8
            });
            [out[0], out[1], out[2], 255]
        })
        .collect();
    Sprite { width: sprite.width, height: sprite.height, pixels }
}

/// Fills the hole DF leaves in the middle of a fully connected wall.
///
/// The hole is a single blob around the centre — a wall enclosed on all four
/// sides has no lit face to show, so DF draws none — and it is well inside one
/// half of the tile, so the rock half a tile away is always rock.
pub fn fill_hole(sprite: &Sprite) -> Sprite {
    let (w, h) = (sprite.width, sprite.height);
    let mut pixels = Vec::with_capacity(sprite.pixels.len());
    for y in 0..h {
        for x in 0..w {
            let p = sprite.pixel(x, y);
            pixels.push(if p[3] == 0 { sprite.pixel((x + w / 2) % w, (y + h / 2) % h) } else { p });
        }
    }
    Sprite { width: w, height: h, pixels }
}

/// A wall's side face, derived from its own top-down sprite.
///
/// `sprite` is the fully connected variant, the only one that covers the whole
/// tile: filled where DF left its middle open, and flattened onto rock. It is
/// deliberately not mirrored to hide the seam between tiles. Mirroring buys a
/// seamless cliff on the sheets that are pure noise and ruins the ones that are
/// not — a smoothed or block wall is drawn as a bordered face, and folding it
/// into four turns a course of masonry into a kaleidoscope, while the border it
/// already has reads as the mortar between blocks.
pub fn side_strip(sprite: &Sprite, backdrop: [u8; 3]) -> Sprite {
    opaque(&fill_hole(sprite), backdrop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dwarf_eye_art::raws::parse_part;
    use std::collections::HashSet;

    fn sprite(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Sprite {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.push(f(x, y));
            }
        }
        Sprite { width, height, pixels }
    }

    #[test]
    fn a_suffix_is_spelled_in_dfs_own_order() {
        assert_eq!(sprite_suffix(ALL), "N_S_W_E");
        assert_eq!(sprite_suffix(WEST | EAST), "W_E");
        assert_eq!(sprite_suffix(EAST | NORTH), "N_E");
        assert_eq!(sprite_suffix(SOUTH), "S");
    }

    #[test]
    fn a_wall_with_nothing_beside_it_takes_the_fully_connected_sprite() {
        assert_eq!(variant(0), ALL);
        assert_eq!(sprite_names("STONE_WALL", 0), sprite_names("STONE_WALL", ALL));
    }

    #[test]
    fn the_numbered_cut_is_asked_for_first() {
        let [first, second] = sprite_names("STONE_WALL", NORTH);
        assert_eq!(first, "STONE_WALL_N_1");
        assert_eq!(second, "STONE_WALL_N");
    }

    #[test]
    fn df_ships_one_sprite_per_neighbour_set() {
        assert_eq!(masks().count(), 15);
        // `parse_part` eats a trailing direction group, so two names could in
        // principle land on the same index key. Neither spelling does.
        for family in ["STONE_WALL", "SMOOTHED_STONE_WALL"] {
            let keys: HashSet<_> = masks()
                .flat_map(|mask| sprite_names(family, mask))
                .map(|name| parse_part(&name))
                .collect();
            assert_eq!(keys.len(), 30, "{family} lost a variant to the parser");
        }
    }

    #[test]
    fn a_wall_sheet_is_chosen_by_material_and_smoothness() {
        use TiletypeMaterial as M;
        use TiletypeSpecial as S;
        let f = |m, s| family_for(m, s, "");
        assert_eq!(f(M::Soil, S::Normal), Some("SOIL_WALL"));
        assert_eq!(f(M::Stone, S::Normal), Some("STONE_WALL"));
        assert_eq!(f(M::Stone, S::Worn1), Some("STONE_WALL"));
        assert_eq!(f(M::Stone, S::Smooth), Some("SMOOTHED_STONE_WALL"));
        assert_eq!(f(M::Mineral, S::Normal), Some("ORE_VEIN_WALL"));
        assert_eq!(f(M::Mineral, S::Smooth), Some("SMOOTHED_STONE_WALL"));
        assert_eq!(f(M::FrozenLiquid, S::Normal), Some("ICE_WALL"));
        assert_eq!(f(M::FrozenLiquid, S::Smooth), Some("SMOOTHED_ICE_WALL"));
        assert_eq!(f(M::Construction, S::Smooth), Some("ROCK_BLOCKS_WALL"));
        // Semi-molten rock and the lava stone beside it share the magma sheet;
        // worked lava stone is masonry and takes the smoothed sheet.
        assert_eq!(f(M::Magma, S::Normal), Some("MAGMA_WALL"));
        assert_eq!(f(M::LavaStone, S::Normal), Some("MAGMA_WALL"));
        assert_eq!(f(M::LavaStone, S::Smooth), Some("SMOOTHED_STONE_WALL"));
        // Air and water are not walls; a tree's parts are the library's.
        assert_eq!(f(M::Air, S::Normal), None);
        assert_eq!(f(M::TreeMaterial, S::Normal), None);
        assert_eq!(family_for(M::Mushroom, S::Smooth, "TreeCapWallNSWE"), None);
        assert_eq!(family_for(M::Stone, S::Normal, "TreeTrunkPillar"), None);
    }

    #[test]
    fn the_hole_in_a_wall_is_filled_from_half_a_tile_away() {
        // DF leaves the middle of a fully connected wall transparent.
        let base = sprite(32, 32, |x, y| {
            let hole = (12..22).contains(&x) && (12..22).contains(&y);
            [x as u8, y as u8, 0, if hole { 0 } else { 255 }]
        });
        let filled = fill_hole(&base);
        assert!(filled.pixels.iter().all(|p| p[3] == 255));
        assert_eq!(filled.pixel(14, 15), base.pixel(30, 31));
        assert_eq!(filled.pixel(0, 0), base.pixel(0, 0));
    }

    #[test]
    fn a_side_face_is_solid_and_keeps_the_sprites_own_border() {
        // A built wall's border is the mortar between its blocks, so the edge
        // pixels have to survive to the face; only the hole is invented.
        let base = sprite(32, 32, |x, y| {
            let edge = x == 0 || y == 0 || x == 31 || y == 31;
            let hole = (12..22).contains(&x) && (12..22).contains(&y);
            let v = if edge { 240 } else { 90 };
            [v, v, v, if hole { 0 } else { 255 }]
        });
        let side = side_strip(&base, [40, 40, 40]);
        assert_eq!((side.width, side.height), (32, 32));
        assert!(side.pixels.iter().all(|p| p[3] == 255));
        assert_eq!(side.pixel(0, 0), [240, 240, 240, 255]);
        assert_eq!(side.pixel(31, 16), [240, 240, 240, 255]);
        assert_eq!(side.pixel(5, 5), [90, 90, 90, 255]);
    }

    #[test]
    fn flattening_keeps_the_dither_and_loses_the_alpha() {
        let base = sprite(2, 2, |x, _| [200, 200, 200, if x == 0 { 255 } else { 0 }]);
        let flat = opaque(&base, [50, 50, 50]);
        assert!(flat.pixels.iter().all(|p| p[3] == 255));
        assert_eq!(flat.pixel(0, 0), [200, 200, 200, 255]);
        assert_eq!(flat.pixel(1, 0), [50, 50, 50, 255]);
    }

    #[test]
    fn the_backdrop_is_a_darker_cast_of_the_sprite() {
        let base = sprite(2, 1, |_, _| [100, 200, 40, 255]);
        assert_eq!(backdrop(&base), [55, 110, 22]);
    }

    #[test]
    fn a_variant_draws_over_the_fully_connected_sprite() {
        let front = sprite(2, 1, |x, _| [255, 0, 0, if x == 0 { 255 } else { 0 }]);
        let back = sprite(2, 1, |_, _| [0, 0, 255, 255]);
        let out = over(&front, &back);
        assert_eq!(out.pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(out.pixel(1, 0), [0, 0, 255, 255]);
    }
}
