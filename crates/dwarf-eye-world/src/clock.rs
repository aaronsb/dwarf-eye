//! Dwarf Fortress's calendar, read from the globals DF actually keeps current.
//!
//! Fortress mode runs `cur_year_tick`: 1200 ticks to a day, 28 days to a month,
//! 12 months to a year. Adventure mode leaves that counter behind and keeps
//! `cur_season_tick`, which counts one unit per 10 fortress ticks and wraps at
//! 10080 each season (`DFHack/dfhack plugins/autofarm.cpp`, which carries a
//! harvest date forward with `while (harvest >= 10080) { season = (season + 1)
//! % 4; harvest -= 10080; }`). Four seasons of 84 days give 403200 ticks a year.

/// One line of DFHack Lua that prints every clock global, tagged for parsing.
pub const PROBE: &str = concat!(
    "local g = df.global ",
    "print(string.format('dwarfeye-clock %d %d %d %d %d %d', ",
    "g.cur_year, g.cur_year_tick, g.cur_season, g.cur_season_tick, ",
    "g.cur_year_tick_advmode, dfhack.world.isAdventureMode() and 1 or 0))",
);

const TAG: &str = "dwarfeye-clock";

pub const TICKS_PER_DAY: i32 = 1200;
pub const DAYS_PER_MONTH: i32 = 28;
pub const DAYS_PER_YEAR: i32 = DAYS_PER_MONTH * 12;
pub const TICKS_PER_YEAR: i32 = TICKS_PER_DAY * DAYS_PER_YEAR;
/// A season is three months.
pub const TICKS_PER_SEASON: i32 = TICKS_PER_YEAR / 4;
/// Fortress ticks per unit of `cur_season_tick`.
pub const TICKS_PER_SEASON_TICK: i32 = 10;
/// `cur_year_tick_advmode` per fortress tick, as DFHack's timestream plugin
/// advances the pair (`*cur_year_tick_advmode += timeskip * 144;`).
pub const ADVMODE_PER_TICK: i32 = 144;

/// Every clock global DF exposes, plus which mode is reading them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub year: i32,
    pub cur_year_tick: i32,
    pub cur_season: i32,
    pub cur_season_tick: i32,
    pub cur_year_tick_advmode: i32,
    pub adventure: bool,
}

impl Reading {
    /// The year tick DF is really running on.
    pub fn year_tick(self) -> i32 {
        if self.adventure {
            self.cur_season * TICKS_PER_SEASON + self.cur_season_tick * TICKS_PER_SEASON_TICK
        } else {
            self.cur_year_tick
        }
    }

    /// What the fortress-mode counter would read if the two agreed.
    pub fn drift(self) -> i32 {
        self.year_tick() - self.cur_year_tick
    }
}

/// Picks the tagged line out of whatever DFHack echoed back.
pub fn parse(text: &str) -> Option<Reading> {
    let tail = text.split(TAG).nth(1)?;
    let mut fields = tail.split_whitespace().map(|f| f.parse::<i32>());
    let mut next = || fields.next()?.ok();
    Some(Reading {
        year: next()?,
        cur_year_tick: next()?,
        cur_season: next()?,
        cur_season_tick: next()?,
        cur_year_tick_advmode: next()?,
        adventure: next()? != 0,
    })
}

/// DFHack Lua that moves the game to `tick`, keeping every clock in step.
///
/// The season pair is written outright, because adventure mode reads it. The
/// advmode counter moves by the same delta the fortress counter does, which is
/// how timestream keeps it consistent.
pub fn set_time(tick: i32) -> String {
    let tick = tick.rem_euclid(TICKS_PER_YEAR);
    format!(
        "local g = df.global \
         local now = dfhack.world.isAdventureMode() \
             and (g.cur_season * {season} + g.cur_season_tick * {scale}) \
             or g.cur_year_tick \
         local t = {tick} \
         g.cur_year_tick_advmode = g.cur_year_tick_advmode + (t - now) * {advmode} \
         g.cur_year_tick = t \
         g.cur_season = t // {season} \
         g.cur_season_tick = (t % {season}) // {scale}",
        season = TICKS_PER_SEASON,
        scale = TICKS_PER_SEASON_TICK,
        advmode = ADVMODE_PER_TICK,
    )
}

/// A clock reading in the game's own vocabulary, for logs and examples.
pub fn describe(year: i32, tick: i32) -> String {
    const MONTHS: [&str; 12] = [
        "Granite", "Slate", "Felsite", "Hematite", "Malachite", "Galena", "Limestone", "Sandstone",
        "Timber", "Moonstone", "Opal", "Obsidian",
    ];
    let day_of_year = (tick / TICKS_PER_DAY).rem_euclid(DAYS_PER_YEAR);
    let minutes = tick.rem_euclid(TICKS_PER_DAY) * 24 * 60 / TICKS_PER_DAY;
    format!(
        "{} {} of {}  {:02}:{:02}",
        day_of_year % DAYS_PER_MONTH + 1,
        MONTHS[(day_of_year / DAYS_PER_MONTH).clamp(0, 11) as usize],
        year,
        minutes / 60,
        minutes % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_probe_line() {
        let r = parse("dwarfeye-clock 100 5 0 1727 6091 1\n").unwrap();
        assert_eq!(r.year, 100);
        assert_eq!(r.cur_year_tick, 5);
        assert_eq!(r.cur_season_tick, 1727);
        assert!(r.adventure);
    }

    #[test]
    fn adventure_reads_the_season_counter() {
        let r = parse("dwarfeye-clock 100 5 0 1727 6091 1").unwrap();
        assert_eq!(r.year_tick(), 17270);
        assert_eq!(describe(r.year, r.year_tick()), "15 Granite of 100  09:24");
    }

    #[test]
    fn fortress_keeps_the_year_counter() {
        let r = parse("dwarfeye-clock 100 17270 0 1727 6091 0").unwrap();
        assert_eq!(r.year_tick(), 17270);
        assert_eq!(r.drift(), 0);
    }

    #[test]
    fn a_season_of_season_ticks_is_a_season_of_ticks() {
        assert_eq!(TICKS_PER_SEASON / TICKS_PER_SEASON_TICK, 10080);
        assert_eq!(TICKS_PER_SEASON, 100800);
    }

    #[test]
    fn set_time_wraps_into_the_year() {
        assert!(set_time(-1).contains(&format!("local t = {}", TICKS_PER_YEAR - 1)));
    }
}
