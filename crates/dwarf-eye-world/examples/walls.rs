//! Contact sheet of the derived wall textures. Needs a DF install, not a game.
//!
//! One row per sheet: the side strip first, then the top face for each of the
//! fifteen neighbour sets in `wall::masks()` order.

use anyhow::Result;
use dwarf_eye_art::{Art, Sprite, find_install};
use dwarf_eye_world::wall;

const FAMILIES: [&str; 8] = [
    "STONE_WALL",
    "SMOOTHED_STONE_WALL",
    "SOIL_WALL",
    "ORE_VEIN_WALL",
    "ROCK_BLOCKS_WALL",
    "ICE_WALL",
    "SMOOTHED_ICE_WALL",
    "MAGMA_WALL",
];

fn sprite_of(art: &mut Art, family: &str, mask: u8) -> Option<Sprite> {
    wall::sprite_names(family, mask).into_iter().find_map(|name| {
        let key = dwarf_eye_art::raws::parse_part(&name);
        art.tree_sprite("", &key.family, key.dirs).cloned()
    })
}

fn main() -> Result<()> {
    let mut art = Art::load(&find_install()?)?;
    let scale = 3;
    let cell = 32 * scale;
    let cols = 16;
    let mut sheet = image::RgbaImage::new(cell * cols, cell * FAMILIES.len() as u32);

    for (row, family) in FAMILIES.iter().enumerate() {
        let Some(base) = sprite_of(&mut art, family, wall::ALL) else {
            println!("{family:<22} MISSING");
            continue;
        };
        let backdrop = wall::backdrop(&base);
        println!(
            "{family:<22} saturation {:.3} {}  backdrop {:?}",
            base.saturation(),
            if base.saturation() < 0.22 { "pattern, tinted" } else { "own colour" },
            backdrop,
        );

        let solid = wall::fill_hole(&base);
        let mut cells = vec![wall::side_strip(&base, backdrop)];
        for mask in wall::masks() {
            let Some(variant) = sprite_of(&mut art, family, mask) else { continue };
            cells.push(wall::opaque(&wall::over(&variant, &solid), backdrop));
        }

        for (col, tile) in cells.iter().enumerate() {
            for y in 0..cell {
                for x in 0..cell {
                    let p = tile.pixel(x / scale, y / scale);
                    sheet.put_pixel(col as u32 * cell + x, row as u32 * cell + y, image::Rgba(p));
                }
            }
        }
    }

    let path = std::env::args().nth(1).unwrap_or_else(|| "walls.png".into());
    sheet.save(&path)?;
    println!("\nwrote {path}");

    // A cliff of each side face: four tiles across, two z-levels down.
    let mut cliff = image::RgbaImage::new(cell * 4, cell * 2 * FAMILIES.len() as u32);
    for (row, family) in FAMILIES.iter().enumerate() {
        let Some(base) = sprite_of(&mut art, family, wall::ALL) else { continue };
        let side = wall::side_strip(&base, wall::backdrop(&base));
        for y in 0..cell * 2 {
            for x in 0..cell * 4 {
                let p = side.pixel((x / scale) % 32, (y / scale) % 32);
                cliff.put_pixel(x, row as u32 * cell * 2 + y, image::Rgba(p));
            }
        }
    }
    cliff.save(path.replace(".png", "_cliff.png"))?;
    Ok(())
}
