//! Walk sync, end to end against a running game.
//!
//! Launches the viewer in walk mode on a scripted route — stand still while
//! the map paints, walk due east, walk back west — and watches Dwarf Fortress
//! from the outside to see whether the character actually went and came back.
//!
//! This needs a live game and it *moves the adventurer*, so it only runs when
//! asked:
//!
//! ```sh
//! make walk-test          # or: DWARF_EYE_LIVE=1 cargo test --release -p dwarf-eye --test walk_sync -- --nocapture
//! ```
//!
//! Start it with an adventurer standing on open ground with a clear run east;
//! a wall or a tree in the way is a legitimate failure of the route, not of
//! walk sync.

use dwarf_eye_world::Session;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Seconds the character stands still first, while the map streams in. The
/// viewer needs to be up before the walk means anything.
const SETTLE: f32 = 40.0;

/// Seconds spent walking each way.
const LEG: f32 = 4.0;

/// Tiles the round trip has to cover to count. The pace is set by the game,
/// so this is deliberately short of what a clear run manages.
const MIN_TILES: usize = 16;

/// Kills the viewer whatever happens, so a failed assertion never leaves a
/// process walking the character around.
struct Viewer(Child);

impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn position(df: &mut Session) -> Option<(i32, i32, i32)> {
    df.client
        .run_command("lua", &["local u=dfhack.world.getAdventurer(); print(u.pos.x,u.pos.y,u.pos.z)"])
        .ok()?;
    let printed = df.client.last_notices.join("");
    let mut numbers = printed.split_whitespace().filter_map(|t| t.parse().ok());
    Some((numbers.next()?, numbers.next()?, numbers.next()?))
}

#[test]
fn the_viewer_walks_the_character_east_and_back() {
    if std::env::var("DWARF_EYE_LIVE").is_err() {
        eprintln!("skipping: set DWARF_EYE_LIVE=1 and load an adventurer to run this");
        return;
    }
    let settle: f32 = std::env::var("DWARF_EYE_SETTLE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(SETTLE);

    let mut df = Session::connect_local().expect("connecting to DFHack");
    let start = position(&mut df).expect("reading where the adventurer stands");
    eprintln!("start {start:?}");

    let log = std::env::temp_dir().join("dwarf-eye-walk-sync.log");
    let sink = std::fs::File::create(&log).expect("opening the viewer's log");
    let viewer = Viewer(
        Command::new(env!("CARGO_BIN_EXE_dwarf-eye"))
            .env("DWARF_EYE_WALK", "1")
            .env("DWARF_EYE_WALK_DRIVE", format!(",{settle};90,{LEG};270,{LEG}"))
            .env("RUST_LOG", "warn")
            .stdout(Stdio::from(sink.try_clone().expect("sharing the log")))
            .stderr(Stdio::from(sink))
            .spawn()
            .expect("starting the viewer"),
    );

    // Watch from outside, at a finer rate than the viewer steps, so no tile is
    // missed and the pace can be reported.
    let watch = Duration::from_secs_f32(settle + 2.0 * LEG + 8.0);
    let began = Instant::now();
    let mut trail = vec![(0.0f32, start)];
    while began.elapsed() < watch {
        if let Some(now) = position(&mut df)
            && now != trail.last().unwrap().1
        {
            trail.push((began.elapsed().as_secs_f32(), now));
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    drop(viewer);

    let walked = trail.len() - 1;
    let end = trail.last().unwrap().1;
    eprintln!("\n{:>7}  {:>18}  {:>8}", "at", "tile", "since");
    for pair in trail.windows(2) {
        let ((_, from), (at, to)) = (pair[0], pair[1]);
        let gap = at - pair[0].0;
        let slope = if from.2 != to.2 { "  slope" } else { "" };
        eprintln!("{at:>6.1}s  {to:>18}  {gap:>7.2}s{slope}", to = format!("{to:?}"));
    }
    let pace = trail.last().map(|(at, _)| at).copied().unwrap_or(0.0);
    eprintln!("\n{walked} tiles walked, {:.2}s a tile while moving", {
        let moving: f32 = trail.windows(2).map(|p| p[1].0 - p[0].0).filter(|g| *g < 1.0).sum();
        let steps = trail.windows(2).filter(|p| p[1].0 - p[0].0 < 1.0).count().max(1);
        moving / steps as f32
    });
    let _ = pace;
    let _ = std::io::stderr().flush();

    let complaints = std::fs::read_to_string(&log).unwrap_or_default();
    let noted = |what: &str| {
        complaints
            .lines()
            .filter(|line| line.contains(what))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    // Walk mode calls a scripted route off rather than leaning on a wall or
    // carrying on from a tile the character has been moved away from. Its
    // reason is the most useful thing the run can tell us, so it comes first.
    assert!(
        noted("scripted walk stopped").is_empty(),
        "the route was abandoned: {:?}",
        noted("scripted walk stopped")
    );
    assert!(
        noted("stale").is_empty(),
        "steps reached the game late, which the one-slot order is meant to make impossible: {:?}",
        noted("stale")
    );
    assert!(
        noted("unconfirmed").is_empty(),
        "steps went unconfirmed, so the game was not keeping up: {:?}",
        noted("unconfirmed")
    );
    assert!(
        walked >= MIN_TILES,
        "only {walked} tiles walked in two {LEG}s legs; expected at least {MIN_TILES}"
    );
    assert_eq!(end, start, "the round trip did not come home, after {walked} tiles");
}
