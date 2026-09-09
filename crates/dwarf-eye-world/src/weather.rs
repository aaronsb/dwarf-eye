//! Dwarf Fortress's weather, read from the globals the protocol never sends.
//!
//! RemoteFortressReader carries five cloud bits per world tile and nothing
//! else: no precipitation, no wind, no stratus counter, no moon. All of that
//! sits in DF's own memory, so it comes back the same way the clock does — one
//! line of DFHack Lua through `RunCommand`, parsed out of `last_notices`.
//!
//! Three globals carry it. `current_weather` is a 5x5 grid of `weather_type`
//! (0 none, 1 rain, 2 snow) over the embark. `world_data.region_map[x][y]` at
//! the embark's own world tile carries the cloud bitfield — the same five kinds
//! the plugin copies, plus the four-bit stratus countdown it drops — along with
//! the daily wind bits, the air-mass velocity, snowfall and temperature.
//! `world_data` itself carries `moon_phase` and `moon_angle`.
//!
//! `region_map` is a pointer to an array of pointers, and DFHack's Lua
//! dereferences the inner one on the first index, so the second axis needs
//! `_displace` rather than a second `[]`. `world.map.region_x` counts mid-level
//! tiles, sixteen to a world tile.

/// One line of DFHack Lua that prints every weather global, tagged for parsing.
///
/// Everything outside `current_weather` is read under `pcall`, so a save that
/// has no world data still yields the grid rather than nothing at all. Enum
/// fields come back as numbers in this build and as names in others, so `n`
/// maps a name back through its own enum table.
pub const PROBE: &str = concat!(
    "local g = df.global ",
    "local function n(t, v) if type(v) == 'number' then return v end return t[v] or 0 end ",
    "local w = {} ",
    "for y = 0, 4 do for x = 0, 4 do ",
    "w[#w + 1] = n(df.weather_type, g.current_weather[x][y]) end end ",
    "local cx, cy = 2, 2 ",
    "local cum, str, cir, fog, fr, cd = 0, 0, 0, 0, 0, 0 ",
    "local wx, wy, ax, ay, sn, tp, mp, ma = 0, 0, 0, 0, 0, 0, -1, 0 ",
    "pcall(function() ",
    "  local m = g.world.map ",
    "  local u = dfhack.world.getAdventurer and dfhack.world.getAdventurer() ",
    "  local px, py = g.window_x + 40, g.window_y + 20 ",
    "  if u then px, py = u.pos.x, u.pos.y end ",
    "  cx = math.min(4, math.max(0, px * 5 // math.max(1, m.x_count))) ",
    "  cy = math.min(4, math.max(0, py * 5 // math.max(1, m.y_count))) ",
    "end) ",
    "pcall(function() ",
    "  local m = g.world.map ",
    "  local wd = g.world.world_data ",
    "  mp, ma = wd.moon_phase, wd.moon_angle ",
    "  local e = wd.region_map[m.region_x // 16]:_displace(m.region_y // 16) ",
    "  local b = e.clouds ",
    "  cum, str, fog = n(df.cumulus_type, b.cumulus), n(df.stratus_type, b.stratus), \
       n(df.fog_type, b.fog) ",
    "  cir, fr, cd = b.cirrus and 1 or 0, n(df.front_type, b.front), b.countdown ",
    "  local d = e.wind ",
    "  wx = (d.east_1 and 1 or 0) + (d.east_2 and 1 or 0) \
        - (d.west_1 and 1 or 0) - (d.west_2 and 1 or 0) ",
    "  wy = (d.south_1 and 1 or 0) + (d.south_2 and 1 or 0) \
        - (d.north_1 and 1 or 0) - (d.north_2 and 1 or 0) ",
    "  ax, ay, sn, tp = e.air_x, e.air_y, e.snowfall, e.temperature ",
    "end) ",
    "print('dwarfeye-weather ' .. table.concat(w, ' ') .. string.format(",
    "' %d %d %d %d %d %d %d %d %d %d %d %d %d %d %d %d %d', ",
    "cx, cy, cum, str, cir, fog, fr, cd, wx, wy, ax, ay, sn, tp, mp, ma, g.weathertimer))",
);

const TAG: &str = "dwarfeye-weather";

/// The side of `current_weather`.
pub const GRID: usize = 5;
/// DF's lunar month, which is also its calendar month.
pub const MOON_DAYS: i32 = 28;
/// `region_map_entry.snowfall` runs 0 to 5000, but the coarse horizon band
/// already reads a cell as fully snowed at 100 (`horizon::shade`). The fine
/// window matches it, or the seam between them would be a line of white.
pub const SNOW_FULL: f32 = 100.0;

/// What is falling on one cell of the embark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Precip {
    #[default]
    None,
    Rain,
    Snow,
}

impl Precip {
    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Precip::Rain,
            2 => Precip::Snow,
            _ => Precip::None,
        }
    }

    pub fn is_wet(self) -> bool {
        self != Precip::None
    }

    pub fn name(self) -> &'static str {
        match self {
            Precip::None => "none",
            Precip::Rain => "rain",
            Precip::Snow => "snow",
        }
    }
}

/// Coverage fractions for DF's four-step cloud scales. The same numbers the
/// world-map path uses, so the Lua reading and the protocol reading draw the
/// same sky.
pub const CUMULUS_COVER: [f32; 4] = [0.0, 0.35, 0.62, 0.88];
pub const STRATUS_COVER: [f32; 4] = [0.0, 0.40, 0.75, 0.95];
pub const FOG_COVER: [f32; 4] = [0.0, 0.25, 0.55, 0.85];
pub const CIRRUS_COVER: f32 = 0.5;

fn step(table: [f32; 4], kind: i32) -> f32 {
    table[kind.clamp(0, 3) as usize]
}

/// Every weather global DF keeps, as one reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// `current_weather`, row-major: `grid[y][x]`.
    pub grid: [[i32; GRID]; GRID],
    /// The cell the character stands in.
    pub cell: (i32, i32),
    /// The centre world tile's cloud bitfield, on DF's own four-step scales.
    pub cumulus: i32,
    pub stratus: i32,
    pub cirrus: bool,
    pub fog: i32,
    pub front: i32,
    /// The four-bit stratus countdown the plugin drops, 0 to 15.
    pub countdown: i32,
    /// Daily wind, -2 to 2 per axis, positive east and south.
    pub wind: (i32, i32),
    /// Air-mass velocity on the same tile.
    pub air: (i32, i32),
    /// 0 to 5000, the same field `RegionTile.snow` is filled from.
    pub snowfall: i32,
    /// The region tile's own temperature field. Reads on a 0-to-100 scale in
    /// this build rather than the Urists the structures comment claims.
    pub temperature: i32,
    /// Day of the lunar month, or negative where world data was unreadable.
    pub moon_phase: i32,
    pub moon_angle: i32,
    /// Ticks until the weather is rerolled.
    pub timer: i32,
}

impl Reading {
    /// What is falling where the character stands.
    pub fn at_character(self) -> Precip {
        let (x, y) = (
            self.cell.0.clamp(0, 4) as usize,
            self.cell.1.clamp(0, 4) as usize,
        );
        Precip::from_code(self.grid[y][x])
    }

    /// How hard it is falling: the fraction of the embark that is wet. A cell
    /// or two is a passing shower, the whole grid is a storm.
    pub fn intensity(self) -> f32 {
        let wet = self
            .grid
            .iter()
            .flatten()
            .filter(|&&v| Precip::from_code(v).is_wet())
            .count();
        wet as f32 / (GRID * GRID) as f32
    }

    /// Snow lying on the ground, 0 to 1, on the coarse band's own scale.
    pub fn snow_cover(self) -> f32 {
        (self.snowfall as f32 / SNOW_FULL).clamp(0.0, 1.0)
    }

    /// How far round the lunar month DF's own moon stands, 0 at new and 0.5 at
    /// full. `None` where the world data could not be read, which leaves the
    /// viewer's 28-day derivation in charge.
    pub fn moon(self) -> Option<f32> {
        (self.moon_phase >= 0)
            .then(|| self.moon_phase.rem_euclid(MOON_DAYS) as f32 / MOON_DAYS as f32)
    }

    /// The stratus countdown as a fraction, which is how close the sheet is to
    /// its next transition.
    pub fn stratus_countdown(self) -> f32 {
        (self.countdown.clamp(0, 15) as f32) / 15.0
    }

    pub fn cumulus_cover(self) -> f32 {
        step(CUMULUS_COVER, self.cumulus)
    }

    pub fn stratus_cover(self) -> f32 {
        step(STRATUS_COVER, self.stratus)
    }

    pub fn cirrus_cover(self) -> f32 {
        if self.cirrus { CIRRUS_COVER } else { 0.0 }
    }

    pub fn fog_cover(self) -> f32 {
        step(FOG_COVER, self.fog)
    }

    /// One line for a log or a probe.
    pub fn describe(self) -> String {
        format!(
            "{} at cell {},{} over {:.0}% of the embark; cumulus {} stratus {} cirrus {} fog {} \
             front {} countdown {} wind {},{} snow {} temp {} moon {} timer {}",
            self.at_character().name(),
            self.cell.0,
            self.cell.1,
            self.intensity() * 100.0,
            self.cumulus,
            self.stratus,
            self.cirrus as i32,
            self.fog,
            self.front,
            self.countdown,
            self.wind.0,
            self.wind.1,
            self.snowfall,
            self.temperature,
            self.moon_phase,
            self.timer,
        )
    }
}

/// Picks the tagged line out of whatever DFHack echoed back.
pub fn parse(text: &str) -> Option<Reading> {
    let tail = text.split(TAG).nth(1)?;
    let mut fields = tail.split_whitespace().map(|f| f.parse::<i32>());
    let mut next = || fields.next()?.ok();

    let mut grid = [[0i32; GRID]; GRID];
    for row in grid.iter_mut() {
        for cell in row.iter_mut() {
            *cell = next()?;
        }
    }
    let cell = (next()?, next()?);
    let cumulus = next()?;
    let stratus = next()?;
    let cirrus = next()? != 0;
    let fog = next()?;
    let front = next()?;
    let countdown = next()?;
    let wind = (next()?, next()?);
    let air = (next()?, next()?);
    let snowfall = next()?;
    let temperature = next()?;
    let moon_phase = next()?;
    let moon_angle = next()?;
    let timer = next()?;
    Some(Reading {
        grid,
        cell,
        cumulus,
        stratus,
        cirrus,
        fog,
        front,
        countdown,
        wind,
        air,
        snowfall,
        temperature,
        moon_phase,
        moon_angle,
        timer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clear day over the embark this was first read on: no cloud, no rain,
    /// moon at 15 of 28, weather timer part way down.
    const CLEAR: &str = "dwarfeye-weather 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 \
                         2 2 0 0 0 0 0 0 0 0 35 99 0 48 15 19572 583";

    /// Rain over the western half of the embark, the character standing in it,
    /// under nimbostratus with the countdown part way down.
    const RAIN: &str = "prefix noise\ndwarfeye-weather \
                        1 1 0 0 0 1 1 1 0 0 1 1 1 0 0 1 1 0 0 0 1 0 0 0 0 \
                        1 2 2 3 0 2 1 9 -1 1 -12 4 250 9800 3 40000 120\ntrailing\n";

    #[test]
    fn parses_a_probe_line() {
        let r = parse(CLEAR).unwrap();
        assert_eq!(r.cell, (2, 2));
        assert_eq!(r.at_character(), Precip::None);
        assert_eq!(r.intensity(), 0.0);
        assert_eq!(r.moon_phase, 15);
        assert_eq!(r.timer, 583);
        assert_eq!(r.air, (35, 99));
    }

    #[test]
    fn parses_a_line_out_of_surrounding_output() {
        let r = parse(RAIN).unwrap();
        assert_eq!(r.cell, (1, 2));
        assert_eq!(r.at_character(), Precip::Rain);
        assert_eq!(r.cumulus, 2);
        assert_eq!(r.stratus, 3);
        assert!(!r.cirrus);
        assert_eq!(r.fog, 2);
        assert_eq!(r.countdown, 9);
        assert_eq!(r.wind, (-1, 1));
        assert_eq!(r.snowfall, 250);
        assert_eq!(r.moon_phase, 3);
    }

    #[test]
    fn a_truncated_line_is_no_reading() {
        assert!(parse("dwarfeye-weather 0 0 0").is_none());
        assert!(parse("nothing tagged here").is_none());
    }

    #[test]
    fn intensity_is_the_wet_fraction_of_the_grid() {
        let r = parse(RAIN).unwrap();
        // Eleven of twenty-five cells carry rain.
        assert!(
            (r.intensity() - 11.0 / 25.0).abs() < 1e-6,
            "{}",
            r.intensity()
        );
    }

    #[test]
    fn snow_reads_as_snow_and_carries_its_own_intensity() {
        let mut r = parse(CLEAR).unwrap();
        r.grid[2][2] = 2;
        r.grid[0][0] = 2;
        assert_eq!(r.at_character(), Precip::Snow);
        assert!((r.intensity() - 2.0 / 25.0).abs() < 1e-6);
    }

    #[test]
    fn cover_matches_the_world_map_scales() {
        let mut r = parse(CLEAR).unwrap();
        r.cumulus = 3;
        r.stratus = 2;
        r.cirrus = true;
        r.fog = 1;
        assert_eq!(r.cumulus_cover(), 0.88);
        assert_eq!(r.stratus_cover(), 0.75);
        assert_eq!(r.cirrus_cover(), 0.5);
        assert_eq!(r.fog_cover(), 0.25);
    }

    #[test]
    fn snow_cover_saturates_where_the_coarse_band_does() {
        let mut r = parse(CLEAR).unwrap();
        assert_eq!(r.snow_cover(), 0.0);
        r.snowfall = 50;
        assert_eq!(r.snow_cover(), 0.5);
        r.snowfall = 4000;
        assert_eq!(r.snow_cover(), 1.0);
    }

    #[test]
    fn the_moon_is_dfs_where_it_reads_and_the_guess_otherwise() {
        let mut r = parse(CLEAR).unwrap();
        // 15 Granite of year 100 read moon_phase 15, which the viewer's own
        // 28-day derivation puts at 0.514: the same full moon within a day.
        let phase = r.moon().unwrap();
        assert!((phase - 15.0 / 28.0).abs() < 1e-6, "{phase}");
        assert!(
            (phase - 0.514).abs() < 0.03,
            "{phase} is not the derived phase"
        );
        r.moon_phase = -1;
        assert_eq!(r.moon(), None);
    }

    #[test]
    fn the_probe_names_its_tag_and_every_global_it_reads() {
        assert!(PROBE.contains(TAG));
        for field in [
            "current_weather",
            "region_map",
            "moon_phase",
            "moon_angle",
            "countdown",
            "weathertimer",
            "snowfall",
        ] {
            assert!(PROBE.contains(field), "the probe never reads {field}");
        }
    }
}
