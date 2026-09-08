//! Saves the packed ground atlas, to check what actually got into it.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::library::TileLibrary;
use dwarf_eye_world::Session;

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let tiletypes: rfr::TiletypeList = df.client.call_empty(methods::GET_TILETYPE_LIST)?;
    let plants: rfr::PlantRawList = df.client.call_empty(methods::GET_PLANT_RAWS)?;
    let lib = TileLibrary::load(&tiletypes, &plants)?;
    let atlas = lib.atlas();

    println!("atlas {}x{}, {} cells used", atlas.width, atlas.height, atlas.capacity_used());
    let path = std::env::args().nth(1).unwrap_or_else(|| "atlas.png".into());
    image::save_buffer(
        &path,
        &atlas.pixels,
        atlas.width,
        atlas.height,
        image::ColorType::Rgba8,
    )?;
    println!("wrote {path}\n");

    let names: std::collections::HashMap<i32, String> = tiletypes
        .tiletype_list
        .iter()
        .map(|t| (t.id, t.name().to_string()))
        .collect();
    let report = lib.ground_report();
    let (ok, bad): (Vec<_>, Vec<_>) = report.iter().partition(|r| r.2);
    println!("ground tiletypes packed: {} / {}", ok.len(), report.len());
    for (id, family, _) in bad.iter().take(20) {
        println!("  MISSING  {:<24} -> {family}", names.get(id).cloned().unwrap_or_default());
    }
    println!("\nsample of packed:");
    for (id, family, _) in ok.iter().filter(|(id, _, _)| {
        names.get(id).is_some_and(|n| !n.starts_with("Tree"))
    }).take(14) {
        println!("  {:<24} -> {family}", names.get(id).cloned().unwrap_or_default());
    }
    Ok(())
}
