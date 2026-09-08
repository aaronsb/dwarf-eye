//! Reports where the triangles go, over the live window.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::library::TileLibrary;
use dwarf_eye_world::mesh::{Budget, build_chunk_budgeted};
use dwarf_eye_world::{MeshOptions, Session};

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let tiletypes: rfr::TiletypeList = df.client.call_empty(methods::GET_TILETYPE_LIST)?;
    let plants: rfr::PlantRawList = df.client.call_empty(methods::GET_PLANT_RAWS)?;
    let mut library = TileLibrary::load(&tiletypes, &plants).ok();
    let view = df.view_center()?;
    let bounds = df.window_bounds(view.2 - 32, view.2 + 20);
    let fetched = df.fetch(bounds, true)?;
    println!("fetched {} blocks", fetched.len());

    let opts = MeshOptions { z_ceiling: view.2 + 16, show_hidden: true };
    let mut total = Budget::default();
    let mut chunks = 0;
    for chunk in df.world.chunks() {
        let mut budget = Budget::default();
        let _ = build_chunk_budgeted(&df.world, chunk, opts, library.as_mut(), &mut budget);
        total.models += budget.models;
        total.ramps += budget.ramps;
        total.ground_under += budget.ground_under;
        total.cubes += budget.cubes;
        total.floors += budget.floors;
        total.foliage += budget.foliage;
        total.liquids += budget.liquids;
        total.other += budget.other;
        chunks += 1;
    }
    let sum = total.models + total.ramps + total.ground_under + total.cubes + total.floors + total.foliage + total.liquids + total.other;
    println!("{chunks} chunks, {sum} triangles");
    for (name, n) in [
        ("sprite models (trees, shrubs, boulders)", total.models),
        ("ramps", total.ramps),
        ("ground under billboards", total.ground_under),
        ("cubes (walls, soil, trunks without sprites)", total.cubes),
        ("floors", total.floors),
        ("foliage blocks", total.foliage),
        ("liquids", total.liquids),
        ("other", total.other),
    ] {
        println!("  {:>10}  {:>5.1}%  {name}", n, n as f32 * 100.0 / sum.max(1) as f32);
    }
    Ok(())
}
