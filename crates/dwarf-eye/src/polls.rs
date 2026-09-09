//! The clock and the weather, read on a light connection of its own.
//!
//! Both are one-line Lua probes answered in milliseconds, and both used to ride
//! the worker's connection, where they queued behind a slab of blocks. A heavy
//! first pass took sixteen to twenty-one seconds and the sun stayed at midnight
//! for all of it (issue #4). Here they cost nothing but a socket: the clock
//! every second, the weather every ten, on a thread that never touches the map.
//!
//! It is a fourth connection rather than a lodger in `units.rs` because the
//! census runs at 4 Hz and must keep running at 4 Hz; hanging a Lua probe off
//! that loop would put a creature's position behind the weather, which is the
//! shape of the bug this fixes. One job, one connection, as walk mode and the
//! unit feed already are.
//!
//! Travel mode and loading screens answer `CR_LINK_FAILURE`. The schedule below
//! holds both probes off for a doubling back-off, one second to thirty, and says
//! so once rather than once a second.

use crate::clouds::Weather;
use crate::worker::{Event, WeatherReport};
use dfhack_remote::{Client, methods, rfr};
use dwarf_eye_world::clock;
use dwarf_eye_world::weather;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// How often the calendar is read. The sun moves a pixel a second at this
/// cadence, which is as smooth as DF's own day.
pub const CLOCK_PERIOD: Duration = Duration::from_secs(1);

/// How often the sky is read. Clouds drift over minutes; ten seconds is already
/// generous, and the probe is cheap enough that it costs nothing to be prompt.
pub const WEATHER_PERIOD: Duration = Duration::from_secs(10);

/// The first pause after a refused probe, and the ceiling it doubles to.
pub const BACKOFF_FIRST: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// When the viewer started, shared so the worker's pass timings and these poll
/// timings are measured from the same instant.
pub fn started() -> Instant {
    static STARTED: OnceLock<Instant> = OnceLock::new();
    *STARTED.get_or_init(Instant::now)
}

/// Whether the sky is open over the camera, so precipitation can reach it.
///
/// The voxels that answer this live on the worker's thread, so the worker sets
/// the flag at the end of each pass and the weather poll reads it. A stale
/// answer costs one reading of rain under a roof.
pub type SkyOpen = Arc<AtomicBool>;

/// A fresh flag, open until the worker says otherwise.
pub fn sky_open() -> SkyOpen {
    Arc::new(AtomicBool::new(true))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Probe {
    Clock,
    Weather,
}

/// When each probe is next due, and how long the connection is being left alone.
///
/// Time is a `Duration` since some start rather than an `Instant` so the whole
/// thing runs on synthetic time in the tests.
pub struct Schedule {
    clock_due: Duration,
    weather_due: Duration,
    /// What the next refusal will wait, doubling to `BACKOFF_MAX`.
    backoff: Duration,
    /// Nothing is asked before this. A refusal holds both probes, because a
    /// link failure is the connection's answer and not one probe's.
    quiet_until: Duration,
    failing: bool,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            clock_due: Duration::ZERO,
            weather_due: Duration::ZERO,
            backoff: BACKOFF_FIRST,
            quiet_until: Duration::ZERO,
            failing: false,
        }
    }
}

impl Schedule {
    /// The probe to run at `now`, if any. The clock goes first when both are
    /// due, which is every tenth wake-up and at startup.
    pub fn due(&self, now: Duration) -> Option<Probe> {
        if now < self.quiet_until {
            return None;
        }
        if self.clock_due <= now {
            Some(Probe::Clock)
        } else if self.weather_due <= now {
            Some(Probe::Weather)
        } else {
            None
        }
    }

    /// How long to sleep before anything is due again.
    pub fn wait(&self, now: Duration) -> Duration {
        let soonest = self.clock_due.min(self.weather_due).max(self.quiet_until);
        soonest.saturating_sub(now)
    }

    /// An answer arrived: that probe waits out its period and the back-off is
    /// forgotten.
    pub fn succeeded(&mut self, probe: Probe, now: Duration) {
        self.backoff = BACKOFF_FIRST;
        self.failing = false;
        match probe {
            Probe::Clock => self.clock_due = now + CLOCK_PERIOD,
            Probe::Weather => self.weather_due = now + WEATHER_PERIOD,
        }
    }

    /// No answer: hold everything for the back-off and double it. Returns
    /// whether this is the first refusal of a run, which is the only one worth
    /// a log line.
    pub fn failed(&mut self, now: Duration) -> bool {
        self.quiet_until = now + self.backoff;
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        let first = !self.failing;
        self.failing = true;
        first
    }

    /// Whether the last probe was refused.
    pub fn is_failing(&self) -> bool {
        self.failing
    }
}

/// Spawns the poll thread. It sends the same `Event::Clock` and
/// `Event::Weather` the worker used to send, down the same channel.
pub fn spawn(events: Sender<Event>, sky: SkyOpen) {
    thread::Builder::new()
        .name("dfhack-polls".into())
        .spawn(move || run(&events, &sky))
        .expect("spawning the poll thread");
}

fn run(events: &Sender<Event>, sky: &SkyOpen) {
    let started = started();
    let mut client = match Client::connect_local() {
        Ok(client) => client,
        Err(err) => {
            bevy::log::warn!("polls: no light connection ({err:#}); clock and sky stand still");
            return;
        }
    };

    let mut schedule = Schedule::default();
    let (mut first_clock, mut first_weather) = (true, true);
    loop {
        let now = started.elapsed();
        let Some(probe) = schedule.due(now) else {
            // A floor keeps a zero-length wait from spinning the thread.
            thread::sleep(schedule.wait(now).max(Duration::from_millis(5)));
            continue;
        };

        let answer = match probe {
            Probe::Clock => read_clock(&mut client).map(|(year, tick)| Event::Clock { year, tick }),
            Probe::Weather => read_weather(&mut client, sky).map(Event::Weather),
        };
        let now = started.elapsed();

        match answer {
            Some(event) => {
                let recovered = schedule.is_failing();
                match probe {
                    Probe::Clock if std::mem::take(&mut first_clock) => bevy::log::info!(
                        "startup: first clock reading at {:.1}s",
                        now.as_secs_f32()
                    ),
                    Probe::Weather if std::mem::take(&mut first_weather) => bevy::log::info!(
                        "startup: first weather reading at {:.1}s",
                        now.as_secs_f32()
                    ),
                    _ => {}
                }
                if recovered {
                    bevy::log::info!("polls: the map is back");
                }
                if events.send(event).is_err() {
                    return;
                }
                schedule.succeeded(probe, now);
            }
            None => {
                if schedule.failed(now) {
                    bevy::log::info!(
                        "polls: no answer from DFHack (travel mode or a loading screen); \
                         retrying with a back-off"
                    );
                }
            }
        }
    }
}

/// The calendar, from the Lua probe or the world map centre.
///
/// Adventure mode abandons `cur_year_tick`, so the probe reads every clock
/// global and the reading picks the one its mode keeps. A world with no year is
/// a world that is not loaded, which is a refusal rather than midnight.
fn read_clock(client: &mut Client) -> Option<(i32, i32)> {
    if client.run_command("lua", &[clock::PROBE]).is_ok()
        && let Some(reading) = clock::parse(&client.last_notices.concat())
        && reading.year > 0
    {
        return Some((reading.year, reading.year_tick()));
    }
    // The Lua path needs a script interpreter; the map centre is always there.
    let map: rfr::WorldMap = client.call_empty(methods::GET_WORLD_MAP_CENTER).ok()?;
    (map.cur_year() > 0).then(|| (map.cur_year(), map.cur_year_tick()))
}

/// The sky, from the Lua probe or the world map.
///
/// The protocol carries five cloud bits and nothing else, so the precipitation
/// grid, the stratus countdown and the moon come back through Lua the way the
/// clock does.
fn read_weather(client: &mut Client, sky: &SkyOpen) -> Option<WeatherReport> {
    let probed = client.run_command("lua", &[weather::PROBE]).is_ok();
    let reading = probed.then(|| weather::parse(&client.last_notices.concat())).flatten();
    match reading {
        Some(r) => {
            let outdoors = sky.load(Ordering::Relaxed);
            bevy::log::info!(
                "weather: {}{}",
                r.describe(),
                if outdoors { "" } else { ", under a ceiling" }
            );
            Some(WeatherReport {
                sky: Weather {
                    cumulus: r.cumulus_cover(),
                    stratus: r.stratus_cover(),
                    cirrus: r.cirrus_cover(),
                    fog: r.fog_cover(),
                    countdown: r.stratus_countdown(),
                },
                precip: r.at_character(),
                intensity: r.intensity(),
                snow: r.snow_cover(),
                moon: r.moon(),
                outdoors,
            })
        }
        // No script interpreter, or no world data: the world map still carries
        // the cloud kinds.
        None => {
            let map: rfr::WorldMap = client.call_empty(methods::GET_WORLD_MAP).ok()?;
            Some(WeatherReport { sky: from_world_map(&map), ..Default::default() })
        }
    }
}

/// Reads cloud cover over the embark out of the world map.
///
/// DF reports a cloud kind per world tile on four-step scales, so each becomes a
/// coverage fraction. The neighbourhood is averaged because a single world tile
/// flips between states more abruptly than a sky should.
fn from_world_map(map: &rfr::WorldMap) -> Weather {
    use dfhack_remote::rfr::{CumulusType, FogType, StratusType};

    let width = map.world_width.max(1);
    let height = map.world_height.max(1);
    let (cx, cy) = (map.map_x(), map.map_y());

    let mut totals = [0.0f32; 4];
    let mut samples = 0.0f32;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x >= width || y >= height {
                continue;
            }
            let Some(cloud) = map.clouds.get((y * width + x) as usize) else { continue };
            // The same scales `weather::Reading` puts the Lua kinds on, so the
            // two paths draw the same sky.
            use weather::{CIRRUS_COVER, CUMULUS_COVER, FOG_COVER, STRATUS_COVER};
            totals[0] += match cloud.cumulus() {
                CumulusType::CumulusNone => CUMULUS_COVER[0],
                CumulusType::CumulusMedium => CUMULUS_COVER[1],
                CumulusType::CumulusMulti => CUMULUS_COVER[2],
                CumulusType::CumulusNimbus => CUMULUS_COVER[3],
            };
            totals[1] += match cloud.stratus() {
                StratusType::StratusNone => STRATUS_COVER[0],
                StratusType::StratusAlto => STRATUS_COVER[1],
                StratusType::StratusProper => STRATUS_COVER[2],
                StratusType::StratusNimbus => STRATUS_COVER[3],
            };
            totals[2] += if cloud.cirrus() { CIRRUS_COVER } else { 0.0 };
            totals[3] += match cloud.fog() {
                FogType::FogNone => FOG_COVER[0],
                FogType::FogMist => FOG_COVER[1],
                FogType::FogNormal => FOG_COVER[2],
                // DFHack's proto spells this one `F0G_THICK`, with a zero.
                FogType::F0gThick => FOG_COVER[3],
            };
            samples += 1.0;
        }
    }

    if samples == 0.0 {
        return Weather::default();
    }
    Weather {
        cumulus: totals[0] / samples,
        stratus: totals[1] / samples,
        cirrus: totals[2] / samples,
        fog: totals[3] / samples,
        // The plugin drops the countdown bits; only the Lua probe has them.
        countdown: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: f32) -> Duration {
        Duration::from_secs_f32(s)
    }

    #[test]
    fn both_probes_are_due_at_the_start_and_the_clock_goes_first() {
        let mut schedule = Schedule::default();
        assert_eq!(schedule.due(Duration::ZERO), Some(Probe::Clock));
        schedule.succeeded(Probe::Clock, Duration::ZERO);
        assert_eq!(schedule.due(Duration::ZERO), Some(Probe::Weather));
        schedule.succeeded(Probe::Weather, Duration::ZERO);
        assert_eq!(schedule.due(Duration::ZERO), None);
    }

    #[test]
    fn the_clock_runs_once_a_second_and_the_weather_once_in_ten() {
        let mut schedule = Schedule::default();
        let (mut clocks, mut weathers) = (0, 0);
        let mut now = Duration::ZERO;
        while now < secs(60.0) {
            match schedule.due(now) {
                Some(probe) => {
                    match probe {
                        Probe::Clock => clocks += 1,
                        Probe::Weather => weathers += 1,
                    }
                    schedule.succeeded(probe, now);
                }
                None => now += schedule.wait(now).max(secs(0.05)),
            }
        }
        // A minute from zero: a clock a second, a sky every tenth of those.
        assert_eq!(clocks, 60);
        assert_eq!(weathers, 6);
    }

    #[test]
    fn a_refusal_holds_both_probes_and_the_wait_doubles_to_the_ceiling() {
        let mut schedule = Schedule::default();
        let mut now = Duration::ZERO;
        assert!(schedule.failed(now), "the first refusal is worth saying");
        // Held for one second, not asked again in the meantime.
        assert_eq!(schedule.due(secs(0.5)), None);
        assert_eq!(schedule.wait(now), secs(1.0));

        let mut waits = Vec::new();
        for _ in 0..8 {
            now += schedule.wait(now);
            assert_eq!(schedule.due(now), Some(Probe::Clock), "a held probe is still due");
            assert!(!schedule.failed(now), "only the first refusal is logged");
            waits.push(schedule.wait(now));
        }
        assert_eq!(waits[0], secs(2.0));
        assert_eq!(waits[1], secs(4.0));
        assert_eq!(waits[2], secs(8.0));
        assert_eq!(*waits.last().unwrap(), BACKOFF_MAX, "and it stops doubling");
    }

    #[test]
    fn an_answer_clears_the_back_off() {
        let mut schedule = Schedule::default();
        schedule.failed(Duration::ZERO);
        schedule.failed(secs(1.0));
        assert!(schedule.is_failing());

        schedule.succeeded(Probe::Clock, secs(3.0));
        assert!(!schedule.is_failing());
        assert_eq!(schedule.due(secs(3.0)), Some(Probe::Weather));
        // The next refusal starts from one second again.
        schedule.failed(secs(3.0));
        assert_eq!(schedule.wait(secs(3.0)), secs(1.0));
    }

    #[test]
    fn a_held_connection_is_never_asked_more_than_once_a_back_off() {
        let mut schedule = Schedule::default();
        let mut asked = 0;
        let mut now = Duration::ZERO;
        while now < secs(120.0) {
            match schedule.due(now) {
                Some(_) => {
                    asked += 1;
                    schedule.failed(now);
                }
                None => now += schedule.wait(now).max(secs(0.05)),
            }
        }
        // 1+2+4+8+16+30... : eight tries in two minutes, not a hundred and twenty.
        assert!((6..=9).contains(&asked), "asked {asked} times in two minutes");
    }
}
