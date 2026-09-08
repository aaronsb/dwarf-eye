//! Lists the floor-ish tiletypes actually present around the player.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::{BlockBounds, Session};
use std::collections::HashMap;

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let tiletypes: rfr::TiletypeList = df.client.call_empty(methods::GET_TILETYPE_LIST)?;
    let info: HashMap<i32, (String, String, String)> = tiletypes
        .tiletype_list
        .iter()
        .map(|t| {
            (
                t.id,
                (
                    t.name().to_string(),
                    format!("{:?}", t.shape()),
                    format!("{:?} dir={}", t.material(), t.direction()),
                ),
            )
        })
        .collect();

    let (cx, cy, cz) = df.view_center()?;
    df.fetch(BlockBounds::around_tile(cx, cy, cz, 4, 4), true)?;

    let mut tally: HashMap<i32, usize> = HashMap::new();
    for chunk in df.world.chunks() {
        for v in &chunk.voxels {
            if !v.solid.is_empty() {
                *tally.entry(v.tile_id).or_default() += 1;
            }
        }
    }
    let mut rows: Vec<_> = tally.into_iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));

    println!("{:>7}  {:<26} {:<14} {}", "count", "tiletype", "shape", "tile material");
    for (tile, n) in rows.iter().take(45) {
        let (name, shape, material) = info.get(tile).cloned().unwrap_or_default();
        println!("{n:>7}  {name:<26} {shape:<14} {material}");
    }
    Ok(())
}
