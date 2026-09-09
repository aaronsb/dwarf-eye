//! Runs the weather probe against the live game and prints what came back.
//!
//! `cargo run -p dwarf-eye-world --example weather`, or with `--raw` for the
//! probe line itself.

use anyhow::{Result, bail};
use dwarf_eye_world::{Session, weather};

fn main() -> Result<()> {
    let raw = std::env::args().any(|a| a == "--raw");
    if std::env::args().any(|a| a == "--print") {
        println!("{}", weather::PROBE);
        return Ok(());
    }

    let mut df = Session::connect_local()?;
    df.client.run_command("lua", &[weather::PROBE])?;
    let text = df.client.last_notices.concat();
    if raw {
        print!("{text}");
        return Ok(());
    }
    let Some(reading) = weather::parse(&text) else {
        bail!("the probe printed nothing this parses:\n{text}");
    };
    println!("{}", reading.describe());
    for row in reading.grid {
        println!("  {row:?}");
    }
    Ok(())
}
