//! Prints the occupancy masks DF's own sprites give us.

use anyhow::Result;
use dwarf_eye_art::{Art, find_install, raws};

fn show(art: &mut Art, plant: &str, tiletype: &str, direction: &str, n: u32) -> Result<()> {
    let family = raws::family_from_tiletype(tiletype);
    let dirs = raws::direction_mask(direction);
    let Some(sprite) = art.tree_sprite(plant, &family, dirs) else {
        println!("{tiletype:<24} -> {family} dirs {dirs:04b}   NO SPRITE\n");
        return Ok(());
    };
    let coverage = sprite.coverage();
    let grid = sprite.downsample(n);
    println!(
        "{tiletype} -> {plant} / {family} dirs {dirs:04b}   {:.0}% opaque, {} of {} cells solid",
        coverage * 100.0,
        grid.solid_count(),
        n * n
    );
    for y in 0..n {
        let row: String = (0..n)
            .map(|x| if grid.get(x, y).solid { '#' } else { '.' })
            .collect();
        println!("  {row}");
    }
    println!();
    Ok(())
}

fn main() -> Result<()> {
    let install = find_install()?;
    let mut art = Art::load(&install)?;
    println!(
        "{} tile pages, {} species indexed\n",
        art.page_count(),
        art.plant_count()
    );

    show(&mut art, "WILLOW", "TreeTrunkPillar", "--------", 16)?;
    show(&mut art, "WILLOW", "TreeTrunkNS", "N-S-----", 16)?;
    show(&mut art, "WILLOW", "TreeBranchNSEW", "N-S-W-E-", 16)?;
    show(&mut art, "WILLOW", "TreeTwigs", "--------", 16)?;

    // WALNUT ships no sheet of its own, so this must land on the generic one.
    show(&mut art, "WALNUT", "TreeTrunkPillar", "--------", 16)?;
    show(&mut art, "WALNUT", "TreeTrunkN", "N-------", 16)?;

    // The free-standing objects that want billboards.
    show(&mut art, "WALNUT", "Sapling", "--------", 16)?;
    show(&mut art, "WALNUT", "Shrub", "--------", 16)?;
    println!("generic tiles indexed: {}", art.index.generic.len());
    let mut families: Vec<&str> = art.index.generic.keys().map(|k| k.family.as_str()).collect();
    families.sort_unstable();
    families.dedup();
    println!("generic families: {}", families.len());
    for want in ["FLOOR", "PEBBLE", "BOULDER", "WALL", "GRASS", "RAMP", "SHRUB", "SAPLING"] {
        let hits: Vec<&&str> = families.iter().filter(|f| f.contains(want)).take(6).collect();
        println!("  {want:<9} {} matches: {hits:?}",
            families.iter().filter(|f| f.contains(want)).count());
    }
    Ok(())
}
