//! Evaluates a Lua snippet in DFHack and prints what it wrote.
//!
//! `cargo run -p dwarf-eye-world --example luaq -- "print(df.global.cur_year_tick)"`

use anyhow::Result;
use dwarf_eye_world::Session;

fn main() -> Result<()> {
    let code = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let mut df = Session::connect_local()?;
    df.client.run_command("lua", &[&code])?;
    for line in &df.client.last_notices {
        print!("{line}");
    }
    println!();
    Ok(())
}
