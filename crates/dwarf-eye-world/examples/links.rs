//! Checks DFHack's single-letter branch directions against the map.
//!
//! A one-letter direction is meant to name the way a branch heads, so what it
//! joins is the opposite tile. This counts how often each reading lands on
//! another tree tile.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::Session;
use dwarf_eye_world::library::TileLibrary;
use dwarf_eye_world::skeleton::{links_from_direction, step};
use dwarf_eye_art::raws;

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let tiletypes: rfr::TiletypeList = df.client.call_empty(methods::GET_TILETYPE_LIST)?;
    let plants: rfr::PlantRawList = df.client.call_empty(methods::GET_PLANT_RAWS)?;
    let library = TileLibrary::load(&tiletypes, &plants)?;
    let view = df.view_center()?;
    let bounds = df.window_bounds(view.2 - 8, view.2 + 20);
    df.fetch(bounds, true)?;

    let is_tree = |x: i32, y: i32, z: i32| -> bool {
        df.world.voxel(x, y, z).is_some_and(|v| {
            library.canopy_part(v.tile_id).is_some() || library.is_trunk(v.tile_id)
        })
    };

    let (mut heading, mut joining, mut total) = (0usize, 0usize, 0usize);
    for chunk in df.world.chunks() {
        let (ox, oy, oz) = chunk.origin();
        for ly in 0..dwarf_eye_world::BLOCK {
            for lx in 0..dwarf_eye_world::BLOCK {
                let v = chunk.get(lx, ly);
                let Some(t) = df.world.palette.tiletype(v.tile_id) else { continue };
                if library.canopy_part(v.tile_id).is_none() {
                    continue;
                }
                let raw = raws::direction_mask(t.direction());
                if raw.count_ones() != 1 {
                    continue;
                }
                let (x, y, z) = (ox + lx, oy + ly, oz);
                let (jx, jy) = step(links_from_direction(t.direction()));
                let (hx, hy) = step(raw);
                total += 1;
                joining += is_tree(x + jx, y + jy, z) as usize;
                heading += is_tree(x + hx, y + hy, z) as usize;
            }
        }
    }
    println!("single-letter branch tiles: {total}");
    println!("  opposite tile is a tree: {joining}");
    println!("  named tile is a tree:    {heading}");
    Ok(())
}
