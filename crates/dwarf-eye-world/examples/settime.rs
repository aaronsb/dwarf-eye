//! Sets Dwarf Fortress's time of day, for testing lighting.
//!
//! `cargo run -p dwarf-eye-world --example settime -- 600`

use anyhow::Result;
use dwarf_eye_world::Session;

fn main() -> Result<()> {
    let tick_of_day: i32 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(600);

    let mut df = Session::connect_local()?;
    // Keep the date, move only the time within the day.
    df.client.run_command(
        "lua",
        &[&format!(
            "df.global.cur_year_tick = df.global.cur_year_tick - (df.global.cur_year_tick % 1200) + {tick_of_day}"
        )],
    )?;
    println!("set tick of day to {tick_of_day}");
    Ok(())
}
