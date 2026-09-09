//! Reports which materials and colours the tiles around the player resolve to.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::{BlockBounds, Session};
use std::collections::HashMap;

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let (cx, cy, cz) = df.view_center()?;

    // Raw block data, so we can see what the arrays hold before decoding.
    let bounds = BlockBounds::around_tile(cx, cy, cz, 2, 2);
    let request = rfr::BlockRequest {
        blocks_needed: Some(4000),
        min_x: Some(bounds.min_x),
        max_x: Some(bounds.max_x),
        min_y: Some(bounds.min_y),
        max_y: Some(bounds.max_y),
        min_z: Some(bounds.min_z),
        max_z: Some(bounds.max_z),
        force_reload: Some(true),
    };
    let list: rfr::BlockList = df.client.call(methods::GET_BLOCK_LIST, &request)?;

    let mut tally: HashMap<(i32, i32, i32), usize> = HashMap::new();
    let mut arrays = [0usize; 4];
    for b in &list.map_blocks {
        arrays[0] += b.materials.len();
        arrays[1] += b.layer_materials.len();
        arrays[2] += b.vein_materials.len();
        arrays[3] += b.base_materials.len();
        for (i, &tile) in b.tiles.iter().enumerate() {
            if let Some(p) = b.materials.get(i) {
                *tally.entry((tile, p.mat_type, p.mat_index)).or_default() += 1;
            }
        }
    }
    println!(
        "blocks {}   array lengths: materials {} layer {} vein {} base {}\n",
        list.map_blocks.len(),
        arrays[0],
        arrays[1],
        arrays[2],
        arrays[3]
    );

    let mut rows: Vec<_> = tally.into_iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));

    println!(
        "{:>7}  {:<26} {:<10} {:<9} {:<28} {:<9} colour",
        "count", "tiletype", "shape", "matpair", "material", "sand"
    );
    for ((tile, mat_type, mat_index), n) in rows.into_iter().take(22) {
        let pair = rfr::MatPair { mat_type, mat_index };
        let tt = df.world.palette.tiletype(tile);
        println!(
            "{n:>7}  {:<26} {:<10} {:<9} {:<28} {:<9} {:?}",
            tt.and_then(|t| t.name.clone()).unwrap_or_else(|| format!("#{tile}")),
            format!("{:?}", df.world.palette.shape(tile)),
            format!("{mat_type}:{mat_index}"),
            df.world.palette.material_name(&pair).unwrap_or("<none>"),
            df.world
                .palette
                .sand_hue(&pair)
                .map(|h| format!("{h:?}"))
                .unwrap_or_else(|| "-".to_string()),
            df.world.palette.color(tile, &pair),
        );
    }
    Ok(())
}
