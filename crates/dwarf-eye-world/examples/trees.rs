//! Checks that a tile's material index resolves to a plant raw id, and lists the
//! tree tiletype names DFHack reports.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::Session;

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;

    let plants: rfr::PlantRawList = df.client.call_empty(methods::GET_PLANT_RAWS)?;
    println!("plant raws: {}", plants.plant_raws.len());
    for i in [92, 94, 95, 105, 161, 172, 203] {
        let p = plants.plant_raws.get(i as usize);
        println!(
            "  mat_index {i:>4} -> id {:<24} name {:?}",
            p.and_then(|p| p.id.clone()).unwrap_or_else(|| "<none>".into()),
            p.and_then(|p| p.name.clone()).unwrap_or_default(),
        );
    }

    let tiletypes: rfr::TiletypeList = df.client.call_empty(methods::GET_TILETYPE_LIST)?;
    let mut names: Vec<_> = tiletypes
        .tiletype_list
        .iter()
        .filter(|t| {
            let n = t.name();
            n.starts_with("Tree") || n.contains("Sapling") || n.contains("Shrub")
        })
        .map(|t| (t.name().to_string(), format!("{:?}", t.shape()), t.direction().to_string()))
        .collect();
    names.sort();
    names.dedup();
    println!("\ntree-ish tiletypes: {}", names.len());
    for (name, shape, dir) in names.iter().take(48) {
        println!("  {name:<28} {shape:<14} direction {dir:?}");
    }
    Ok(())
}
