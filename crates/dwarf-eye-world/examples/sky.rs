//! Reports Dwarf Fortress's clock and cloud state.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::Session;

/// Dwarf Fortress runs 1200 ticks to a day, 28 days to a month, 12 months to a year.
const TICKS_PER_DAY: i32 = 1200;
const DAYS_PER_MONTH: i32 = 28;

const MONTHS: [&str; 12] = [
    "Granite", "Slate", "Felsite", "Hematite", "Malachite", "Galena",
    "Limestone", "Sandstone", "Timber", "Moonstone", "Opal", "Obsidian",
];

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let map: rfr::WorldMap = df.client.call_empty(methods::GET_WORLD_MAP_CENTER)?;

    let year = map.cur_year();
    let tick = map.cur_year_tick();
    let day_of_year = tick / TICKS_PER_DAY;
    let tick_of_day = tick % TICKS_PER_DAY;
    let month = (day_of_year / DAYS_PER_MONTH).clamp(0, 11) as usize;
    let day = day_of_year % DAYS_PER_MONTH + 1;

    println!("year {year}, tick {tick}");
    println!("  {} {} of {}", day, MONTHS[month], year);
    println!("  tick of day {tick_of_day} / {TICKS_PER_DAY}");
    println!("  fraction of day {:.3}", tick_of_day as f32 / TICKS_PER_DAY as f32);
    println!("  clouds reported: {}", map.clouds.len());
    for (i, c) in map.clouds.iter().take(4).enumerate() {
        println!(
            "    [{i}] front {:?} cumulus {:?} cirrus {:?} stratus {:?} fog {:?}",
            c.front(), c.cumulus(), c.cirrus(), c.stratus(), c.fog()
        );
    }
    // GetWorldMapCenter returns a trimmed map; try the full one for clouds.
    let full: rfr::WorldMap = df.client.call_empty(methods::GET_WORLD_MAP)?;
    println!(
        "\nfull world map {}x{}, clouds {}, region tiles {}",
        full.world_width, full.world_height, full.clouds.len(), full.region_tiles.len()
    );
    for (i, c) in full.clouds.iter().enumerate().take(6) {
        println!(
            "    [{i}] front {:?} cumulus {:?} cirrus {:?} stratus {:?} fog {:?}",
            c.front(), c.cumulus(), c.cirrus(), c.stratus(), c.fog()
        );
    }
    Ok(())
}
