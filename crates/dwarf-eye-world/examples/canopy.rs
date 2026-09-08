//! Prints one tree's tiles by z-level, so the shape the canopy mesher is given
//! can be checked against what it draws.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::Session;
use dwarf_eye_world::library::TileLibrary;
use std::collections::HashMap;

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let tiletypes: rfr::TiletypeList = df.client.call_empty(methods::GET_TILETYPE_LIST)?;
    let plants: rfr::PlantRawList = df.client.call_empty(methods::GET_PLANT_RAWS)?;
    let library = TileLibrary::load(&tiletypes, &plants)?;
    let view = df.view_center()?;
    let bounds = df.window_bounds(view.2 - 8, view.2 + 20);
    df.fetch(bounds, true)?;

    // Tiles per tree, keyed by the origin DFHack reports.
    let mut trees: HashMap<(i32, i32, i32), Vec<(i32, i32, i32, String)>> = HashMap::new();
    for chunk in df.world.chunks() {
        let (ox, oy, oz) = chunk.origin();
        for ly in 0..dwarf_eye_world::BLOCK {
            for lx in 0..dwarf_eye_world::BLOCK {
                let v = chunk.get(lx, ly);
                let (x, y, z) = (ox + lx, oy + ly, oz);
                let canopy = library.canopy_part(v.tile_id).is_some();
                if !canopy && !library.is_trunk(v.tile_id) {
                    continue;
                }
                let name = df
                    .world
                    .palette
                    .tiletype(v.tile_id)
                    .map(|t| t.name().to_string())
                    .unwrap_or_default();
                trees.entry(v.tree_origin(x, y, z)).or_default().push((x, y, z, name));
            }
        }
    }

    let mut biggest: Vec<_> = trees.into_iter().collect();
    biggest.sort_by_key(|(_, tiles)| std::cmp::Reverse(tiles.len()));
    for (origin, tiles) in biggest.iter().take(1) {
        println!("\ntree at {origin:?}, {} tiles", tiles.len());
        let mut levels: HashMap<i32, HashMap<&str, usize>> = HashMap::new();
        for (_, _, z, name) in tiles {
            let kind = if name.contains("Cap") {
                "cap"
            } else if name.contains("Twig") {
                "twig"
            } else if name.contains("Branch") {
                "branch"
            } else {
                "trunk"
            };
            *levels.entry(*z).or_default().entry(kind).or_default() += 1;
        }
        let mut zs: Vec<_> = levels.keys().copied().collect();
        zs.sort();
        let x0 = tiles.iter().map(|t| t.0).min().unwrap();
        let x1 = tiles.iter().map(|t| t.0).max().unwrap();
        let y0 = tiles.iter().map(|t| t.1).min().unwrap();
        let y1 = tiles.iter().map(|t| t.1).max().unwrap();
        for z in zs {
            let counts = &levels[&z];
            println!(
                "  z {z:>4}  trunk {:>3}  branch {:>3}  twig {:>3}  cap {:>3}",
                counts.get("trunk").copied().unwrap_or(0),
                counts.get("branch").copied().unwrap_or(0),
                counts.get("twig").copied().unwrap_or(0),
                counts.get("cap").copied().unwrap_or(0),
            );
            for y in y0..=y1 {
                let row: String = (x0..=x1)
                    .map(|x| {
                        match tiles.iter().find(|t| t.0 == x && t.1 == y && t.2 == z) {
                            None => '.',
                            Some((_, _, _, n)) if n.contains("Cap") => 'C',
                            Some((_, _, _, n)) if n.contains("Twig") => 't',
                            Some((_, _, _, n)) if n.contains("Branch") => 'b',
                            Some(_) => 'T',
                        }
                    })
                    .collect();
                println!("        {row}");
            }
        }
    }
    Ok(())
}
