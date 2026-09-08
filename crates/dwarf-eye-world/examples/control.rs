//! Checks which DFHack commands are reachable for driving time and weather.

use anyhow::Result;
use dwarf_eye_world::Session;

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;

    for (command, args) in [
        ("weather", vec![]),
        ("lua", vec!["print('lua reachable, tick=' .. df.global.cur_year_tick)"]),
    ] {
        match df.client.run_command(command, &args) {
            Ok(()) => {
                println!("`{command}` ok");
                for note in &df.client.last_notices {
                    print!("    {note}");
                }
                println!();
            }
            Err(e) => println!("`{command}` failed: {e:#}"),
        }
    }
    Ok(())
}
