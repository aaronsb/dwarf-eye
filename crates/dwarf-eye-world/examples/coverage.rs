//! Reports which tiles near the player get a sprite model and which fall back
//! to a plain block.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::library::TileLibrary;
use dwarf_eye_world::model::Caps;
use dwarf_eye_world::{BlockBounds, Session};
use std::collections::HashMap;

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let tiletypes: rfr::TiletypeList = df.client.call_empty(methods::GET_TILETYPE_LIST)?;
    let plants: rfr::PlantRawList = df.client.call_empty(methods::GET_PLANT_RAWS)?;
    let mut lib = TileLibrary::load(&tiletypes, &plants)?;

    let names: HashMap<i32, (String, String)> = tiletypes
        .tiletype_list
        .iter()
        .map(|t| (t.id, (t.name().to_string(), format!("{:?}", t.shape()))))
        .collect();

    let (cx, cy, cz) = df.view_center()?;
    df.fetch(BlockBounds::around_tile(cx, cy, cz, 4, 4), true)?;

    // Count how each distinct tile in view is treated.
    let mut tally: HashMap<(i32, i32), usize> = HashMap::new();
    for chunk in df.world.chunks() {
        for v in &chunk.voxels {
            if v.solid.is_empty() {
                continue;
            }
            *tally.entry((v.tile_id, v.mat_index)).or_default() += 1;
        }
    }

    let mut rows: Vec<_> = tally
        .into_iter()
        .map(|((tile, mat), n)| {
            let handled = lib.handles(tile);
            let modelled = handled && lib.model(tile, mat, Caps::BOTH).is_some();
            (n, tile, mat, handled, modelled)
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));

    println!("{:>7}  {:<26} {:<14} {}", "count", "tiletype", "shape", "treatment");
    let mut plain_tree = 0;
    for (n, tile, _mat, handled, modelled) in rows.iter().take(400) {
        let (name, shape) = names.get(tile).cloned().unwrap_or_default();
        let treatment = match (handled, modelled) {
            (true, true) => "sprite model",
            (true, false) => "SPRITE MISSING -> plain block",
            _ => "plain block",
        };
        let tree_ish = name.starts_with("Tree") || shape == "Sapling" || shape == "Shrub";
        if tree_ish && !modelled {
            plain_tree += n;
            println!("{n:>7}  {name:<26} {shape:<14} {treatment}");
        }
    }
    println!("\ntree-ish tiles drawn as plain blocks: {plain_tree}");
    println!("models built: {}, families with no sprite: {}", lib.model_count(), lib.missing());
    Ok(())
}
