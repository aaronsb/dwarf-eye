//! Spike: can we move the adventurer through DFHack, and read the tile back cheaply?
//!
//! Run with a live adventurer loaded.
//! `cargo run --release -p dwarf-eye-world --example adventure -- [DIR] [steps]`
//! where DIR is one of N S E W NE NW SE SW UP DOWN.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::Session;

fn lua(df: &mut Session, expr: &str) -> String {
    match df.client.run_command("lua", &[expr]) {
        Ok(()) => df.client.last_notices.join("").trim_end().to_string(),
        Err(e) => format!("<failed: {e:#}>"),
    }
}

const POS: &str = "local u=dfhack.world.getAdventurer(); print(u.pos.x,u.pos.y,u.pos.z)";

fn unit_pos(df: &mut Session) -> Option<(i32, i32, i32)> {
    let out = lua(df, POS);
    let mut n = out.split_whitespace().filter_map(|t| t.parse().ok());
    Some((n.next()?, n.next()?, n.next()?))
}

/// The view centre in window-local tiles, as `Session::view_center` reads it
/// before the render shift is applied.
fn view_pos(df: &mut Session) -> Result<(i32, i32, i32)> {
    let v: rfr::ViewInfo = df.client.call_empty(methods::GET_VIEW_INFO)?;
    Ok((
        v.view_pos_x() + v.view_size_x() / 2,
        v.view_pos_y() + v.view_size_y() / 2,
        v.view_pos_z(),
    ))
}

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    println!("connected\n");

    println!("== what exists ==");
    for probe in [
        "print(dfhack.world.getAdventurer ~= nil)",
        "print(dfhack.gui.getCurViewscreen())",
        "local k=df.interface_key; print(k.A_MOVE_N,k.A_MOVE_NE,k.A_MOVE_SW,k.A_MOVE_UP,k.A_MOVE_DOWN)",
        POS,
    ] {
        println!("  {probe}\n    -> {}", lua(&mut df, probe));
    }

    println!("\n== read costs ==");
    let t = std::time::Instant::now();
    for _ in 0..20 {
        let _ = unit_pos(&mut df);
    }
    println!("  lua getAdventurer: {:.1} ms/call", t.elapsed().as_secs_f64() * 1000.0 / 20.0);
    let t = std::time::Instant::now();
    for _ in 0..20 {
        let _ = view_pos(&mut df)?;
    }
    println!("  GetViewInfo:       {:.1} ms/call", t.elapsed().as_secs_f64() * 1000.0 / 20.0);
    let t = std::time::Instant::now();
    let units: rfr::UnitList = df.client.call_empty(methods::GET_UNIT_LIST)?;
    println!(
        "  GetUnitList:       {:.1} ms, {} units",
        t.elapsed().as_secs_f64() * 1000.0,
        units.creature_list.len()
    );

    // `--trail <seconds>` just watches, printing every tile the character
    // moves to. Run it beside the viewer to see walk mode drive the game.
    if let Some(i) = std::env::args().position(|a| a == "--trail") {
        let seconds: f32 = std::env::args()
            .nth(i + 1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(30.0);
        let started = std::time::Instant::now();
        let mut last = None;
        let mut steps = 0;
        println!("\n== watching for {seconds:.0} s ==");
        while started.elapsed().as_secs_f32() < seconds {
            let now = unit_pos(&mut df);
            if now != last {
                if let (Some(a), Some(b)) = (last, now) {
                    steps += 1;
                    println!(
                        "  {:>5.1}s  {a:?} -> {b:?}   {}",
                        started.elapsed().as_secs_f32(),
                        if a.2 != b.2 { "slope" } else { "" }
                    );
                } else if let Some(b) = now {
                    println!("  {:>5.1}s  at {b:?}", started.elapsed().as_secs_f32());
                }
                last = now;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        println!("  {steps} tiles walked, ending at {last:?}");
        return Ok(());
    }

    let dir = std::env::args().nth(1).unwrap_or_else(|| "E".into()).to_uppercase();
    let steps: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(6);

    println!("\n== {steps} moves {dir}: unit pos vs view centre, and time to confirm ==");
    for step in 0..steps {
        let before = unit_pos(&mut df);
        let feed = format!(
            "local s=dfhack.gui.getCurViewscreen(); s:feed_key(df.interface_key.A_MOVE_{dir})"
        );
        let sent = std::time::Instant::now();
        let err = lua(&mut df, &feed);

        // Poll at 8 Hz until it moves or we give up.
        let mut confirmed = None;
        for _ in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(125));
            let now = unit_pos(&mut df);
            if now != before {
                confirmed = now;
                break;
            }
        }
        let view = view_pos(&mut df)?;
        match confirmed {
            Some(p) => println!(
                "  {step}: {:?} -> {p:?} in {:>4.0} ms   view centre {view:?} {}",
                before.unwrap(),
                sent.elapsed().as_secs_f64() * 1000.0,
                if view == p { "== unit" } else { "!! DIFFERS" },
            ),
            None => println!("  {step}: {before:?} refused (no move in 1 s) {err}"),
        }
    }

    Ok(())
}
