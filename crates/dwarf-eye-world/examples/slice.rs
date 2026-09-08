//! Dumps z-level slices around the player as ASCII, to sanity-check decoding.
//!
//! `cargo run -p dwarf-eye-world --example slice`

use anyhow::Result;
use dwarf_eye_world::{BlockBounds, Session, Solid};

const WIDTH: i32 = 64;
const HEIGHT: i32 = 24;

fn glyph(v: dwarf_eye_world::Voxel) -> char {
    if v.magma > 0 {
        return '~';
    }
    if v.water > 0 {
        return '≈';
    }
    match v.solid {
        Solid::Empty => ' ',
        Solid::Cube => '█',
        Solid::Floor => '·',
        Solid::Ramp => '▲',
        Solid::Stair => '≡',
        Solid::Fortification => '#',
        Solid::Foliage => '"',
    }
}

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    println!(
        "{} — rfr {}",
        df.map_info.world_name_english(),
        df.version.remote_fortress_reader_version()
    );

    let (cx, cy, cz) = df.view_center()?;
    println!("view centre: tile ({cx}, {cy}, {cz})\n");

    let bounds = BlockBounds::around_tile(cx, cy, cz, 3, 1);
    let fetched = df.fetch(bounds, true)?;
    println!("fetched {fetched} blocks into {} chunks\n", df.world.chunk_count());

    for z in (cz - 1..=cz + 1).rev() {
        println!("── z {z} ──");
        for y in cy - HEIGHT / 2..cy + HEIGHT / 2 {
            let row: String = (cx - WIDTH / 2..cx + WIDTH / 2)
                .map(|x| df.world.voxel(x, y, z).map(glyph).unwrap_or('?'))
                .collect();
            println!("{row}");
        }
        println!();
    }
    Ok(())
}
