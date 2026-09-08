//! Sets Dwarf Fortress's time of day, for testing lighting.
//!
//! `cargo run -p dwarf-eye-world --example settime -- 600`

use anyhow::Result;
use dwarf_eye_world::{Session, clock};

fn main() -> Result<()> {
    let tick_of_day: i32 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(600);

    let mut df = Session::connect_local()?;
    df.client.run_command("lua", &[clock::PROBE])?;
    let before = clock::parse(&df.client.last_notices.concat())
        .ok_or_else(|| anyhow::anyhow!("could not read the clock"))?;

    // Keep the date, move only the time within the day.
    let day = before.year_tick() / clock::TICKS_PER_DAY;
    let target = day * clock::TICKS_PER_DAY + tick_of_day.rem_euclid(clock::TICKS_PER_DAY);
    df.client.run_command("lua", &[&clock::set_time(target)])?;

    df.client.run_command("lua", &[clock::PROBE])?;
    let after = clock::parse(&df.client.last_notices.concat())
        .ok_or_else(|| anyhow::anyhow!("could not read the clock back"))?;

    let mode = if after.adventure { "adventure" } else { "fortress" };
    println!("{mode} mode");
    println!("  was {}", clock::describe(before.year, before.year_tick()));
    println!("  now {}", clock::describe(after.year, after.year_tick()));
    println!(
        "  cur_year_tick {} cur_season {} cur_season_tick {} advmode {}",
        after.cur_year_tick, after.cur_season, after.cur_season_tick, after.cur_year_tick_advmode
    );
    Ok(())
}
