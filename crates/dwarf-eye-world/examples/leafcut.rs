//! Reports how much of each species' leaf cutout is opaque.

use anyhow::Result;
use dwarf_eye_art::{Art, find_install};

fn main() -> Result<()> {
    let mut art = Art::load(&find_install()?)?;
    let species: Vec<String> = art.index.plants.keys().cloned().collect();
    let mut rows: Vec<(String, String, f32)> = Vec::new();
    for id in species.iter().chain(std::iter::once(&String::new())) {
        for family in ["TREE_TWIGS", "TREE_BRANCH", "TREE_CAP_FLOOR_1"] {
            if let Some(s) = art.tree_sprite(id, family, 0) {
                rows.push((id.clone(), family.to_string(), s.coverage()));
                break;
            }
        }
    }
    rows.sort_by(|a, b| a.2.total_cmp(&b.2));
    for (id, family, cover) in rows.iter().take(8) {
        println!("{id:<24} {family:<18} {:.0}% opaque", cover * 100.0);
    }
    println!("...");
    for (id, family, cover) in rows.iter().rev().take(6) {
        println!("{id:<24} {family:<18} {:.0}% opaque", cover * 100.0);
    }
    Ok(())
}
