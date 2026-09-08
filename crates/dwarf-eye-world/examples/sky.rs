//! Reports Dwarf Fortress's clock and cloud state.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::{Session, clock};

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let map: rfr::WorldMap = df.client.call_empty(methods::GET_WORLD_MAP_CENTER)?;

    df.client.run_command("lua", &[clock::PROBE])?;
    let reading = clock::parse(&df.client.last_notices.concat());

    match reading {
        Some(r) => {
            println!("{} mode", if r.adventure { "adventure" } else { "fortress" });
            println!("  clock       {}", clock::describe(r.year, r.year_tick()));
            println!(
                "  fortress    tick {} -> {}",
                r.cur_year_tick,
                clock::describe(r.year, r.cur_year_tick)
            );
            println!(
                "  adventure   season {} tick {} -> {}",
                r.cur_season,
                r.cur_season_tick,
                clock::describe(
                    r.year,
                    r.cur_season * clock::TICKS_PER_SEASON
                        + r.cur_season_tick * clock::TICKS_PER_SEASON_TICK
                )
            );
            println!("  advmode     {}", r.cur_year_tick_advmode);
            println!("  drift       {} ticks", r.drift());
        }
        None => println!("no Lua clock; falling back to the map centre"),
    }
    println!(
        "  map centre  year {} tick {}",
        map.cur_year(),
        map.cur_year_tick()
    );

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
