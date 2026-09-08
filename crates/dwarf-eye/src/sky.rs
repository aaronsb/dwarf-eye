//! Drives the sun, the moon and the exposure from Dwarf Fortress's own clock.
//!
//! DF runs 1200 ticks to a day, 28 days to a month, 12 months to a year. The
//! tick is the only clock the game exposes, so everything here derives from it.
//!
//! Night is the sun's light faded out over the last few degrees, a moon on the
//! opposite arc, a starlight floor under the sky's own ambient, and an exposure
//! metered off whichever of them is up.

use bevy::camera::Exposure;
use bevy::light::{GlobalAmbientLight, light_consts::lux};
use bevy::prelude::*;
use std::sync::OnceLock;

pub const TICKS_PER_DAY: i32 = 1200;
pub const DAYS_PER_MONTH: i32 = 28;
pub const DAYS_PER_YEAR: i32 = DAYS_PER_MONTH * 12;

/// A full moon's light. Far above the real quarter-lux, because the exposure
/// only opens five stops at night rather than the twenty an eye manages: this
/// is the value that puts a moonlit field about a sixth of daylight.
pub const MOON_ILLUMINANCE: f32 = 350.0;
/// Moonlight is sunlight off grey rock, but the eye reads a dim scene as cool,
/// so the moon is tinted the way film and the mind both render it.
pub const MOON_COLOUR: Color = Color::srgb(0.60, 0.72, 1.0);
/// The moon's disk. Radiance divides by its solid angle, so the intensity that
/// reads as a moon rather than a second sun is a small fraction.
pub const MOON_DISK_INTENSITY: f32 = 0.0007;
/// Starlight and airglow: the floor that keeps midnight readable when the moon
/// is down. Faded to nothing by day, where the sky's own ambient takes over.
pub const NIGHT_AMBIENT: f32 = 340.0;
pub const NIGHT_AMBIENT_COLOUR: Color = Color::srgb(0.42, 0.55, 1.0);
/// Bevy's own default ambient, which the daylit scene was built against.
pub const DAY_AMBIENT: f32 = 80.0;

/// The stop the daylit scene is graded at, and the furthest night opens to.
pub const DAY_EV100: f32 = 13.0;
pub const NIGHT_EV100: f32 = 7.6;
/// The light the meter still reads with sun and moon both down: starlight, the
/// airglow, and whatever the sky holds. Keeps the curve off its own floor.
pub const KEY_FLOOR: f32 = 250.0;
/// Stops of exposure per stop of light. An eye crossing from noon to midnight
/// opens some twenty stops; the scene is graded across five, so the curve is
/// compressed rather than physical.
pub const EXPOSURE_COMPRESSION: f32 = 0.65;
/// How long the exposure takes to follow the light, in seconds. The game's
/// clock arrives in steps of several minutes, and an eye does not jump either.
pub const EXPOSURE_ADAPT: f32 = 0.7;

/// The sun, and the moon that follows it round.
#[derive(Component)]
pub struct Sun;

#[derive(Component)]
pub struct Moon;

/// A 0..1 ramp with zero slope at both ends, so nothing pops as it crosses.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `DWARF_EYE_HOUR=22` (or `16:48`, or `16.8`) pins the time of day the view is
/// lit at, so night can be looked at without touching the game's own clock.
///
/// Returns the tick of day it stands for.
pub fn hour_override() -> Option<i32> {
    static TICK: OnceLock<Option<i32>> = OnceLock::new();
    *TICK.get_or_init(|| {
        let raw = std::env::var("DWARF_EYE_HOUR").ok()?;
        let hours = match raw.split_once(':') {
            Some((h, m)) => h.trim().parse::<f32>().ok()? + m.trim().parse::<f32>().ok()? / 60.0,
            None => raw.trim().parse::<f32>().ok()?,
        };
        let tick = (hours / 24.0 * TICKS_PER_DAY as f32).round() as i32;
        Some(tick.rem_euclid(TICKS_PER_DAY))
    })
}

/// `DWARF_EYE_EV100` pins the exposure, for finding the right stop by hand.
pub fn ev100_override() -> Option<f32> {
    static EV: OnceLock<Option<f32>> = OnceLock::new();
    *EV.get_or_init(|| std::env::var("DWARF_EYE_EV100").ok()?.trim().parse().ok())
}

pub const MONTHS: [&str; 12] = [
    "Granite", "Slate", "Felsite", "Hematite", "Malachite", "Galena", "Limestone", "Sandstone",
    "Timber", "Moonstone", "Opal", "Obsidian",
];

/// Where Dwarf Fortress's calendar currently stands.
#[derive(Resource, Default, Clone, Copy)]
pub struct Clock {
    pub year: i32,
    pub tick: i32,
}

impl Clock {
    pub fn day_of_year(self) -> i32 {
        (self.tick / TICKS_PER_DAY).rem_euclid(DAYS_PER_YEAR)
    }

    pub fn tick_of_day(self) -> i32 {
        self.tick.rem_euclid(TICKS_PER_DAY)
    }

    /// 0.0 at midnight, 0.5 at noon.
    pub fn day_fraction(self) -> f32 {
        self.tick_of_day() as f32 / TICKS_PER_DAY as f32
    }

    pub fn month(self) -> &'static str {
        MONTHS[(self.day_of_year() / DAYS_PER_MONTH).clamp(0, 11) as usize]
    }

    pub fn day_of_month(self) -> i32 {
        self.day_of_year() % DAYS_PER_MONTH + 1
    }

    /// A clock reading, in the game's own vocabulary.
    pub fn describe(self) -> String {
        let minutes = (self.day_fraction() * 24.0 * 60.0) as i32;
        format!(
            "{} {} of {}   {:02}:{:02}",
            self.day_of_month(),
            self.month(),
            self.year,
            minutes / 60,
            minutes % 60
        )
    }

    /// Direction from the world to the sun.
    ///
    /// Dawn puts it due east, noon overhead, dusk due west. The day's north/south
    /// lean follows the season, so winter light stays low.
    pub fn sun_direction(self) -> Vec3 {
        use std::f32::consts::TAU;

        // Dawn at a quarter past midnight, so noon lands at the zenith.
        let angle = (self.day_fraction() - 0.25) * TAU;
        // Granite is the start of spring, so the year's swing is offset to match.
        let season = ((self.day_of_year() as f32 / DAYS_PER_YEAR as f32) - 0.25) * TAU;
        let declination = season.sin() * 0.41;

        Vec3::new(angle.cos(), angle.sin(), declination.sin()).normalize()
    }

    /// Whether the sun is above the horizon.
    pub fn is_daylight(self) -> bool {
        self.sun_direction().y > 0.0
    }

    /// How much of the sun's own light reaches the ground, 1 by day and 0 once
    /// it has set. Without the fade a sunken sun lights every west-facing wall
    /// at full strength, and the scene snaps as it crosses. Three degrees either
    /// side of the horizon is about half an hour of the game's clock: long
    /// enough to read as a sunset, short enough that the light belongs to a sun
    /// that is actually there.
    pub fn sun_light(self) -> f32 {
        smoothstep(-0.05, 0.05, self.sun_direction().y)
    }

    /// How far into the night the sky is: 0 in daylight, 1 once the last of the
    /// twilight has gone. The starlight floor rides this, so it lifts and drops
    /// with the sky rather than with the sun's own light.
    pub fn night(self) -> f32 {
        1.0 - smoothstep(-0.14, 0.09, self.sun_direction().y)
    }

    /// How far the moon has swung away from the sun: 0 at new, 0.5 at full.
    /// Dwarf Fortress's 28-day month is a lunar month, so the cycle is the
    /// calendar's own.
    pub fn moon_phase(self) -> f32 {
        let cycle = (TICKS_PER_DAY * DAYS_PER_MONTH) as f32;
        (self.tick as f32 / cycle).rem_euclid(1.0)
    }

    /// The lit fraction of the moon's disk, 0 at new and 1 at full.
    pub fn moon_illumination(self) -> f32 {
        (1.0 - (self.moon_phase() * std::f32::consts::TAU).cos()) * 0.5
    }

    /// Direction from the world to the moon: the sun's own arc, lagging by the
    /// phase, so a full moon rises as the sun sets. Its lean is the sun's
    /// mirrored, which is what keeps the winter moon high.
    pub fn moon_direction(self) -> Vec3 {
        use std::f32::consts::TAU;

        let angle = (self.day_fraction() - 0.25 + self.moon_phase()) * TAU;
        let season = ((self.day_of_year() as f32 / DAYS_PER_YEAR as f32) - 0.25) * TAU;
        let declination = -season.sin() * 0.41;

        Vec3::new(angle.cos(), angle.sin(), declination.sin()).normalize()
    }

    /// The moon's light, faded as it sets and dimmed by its phase. A crescent
    /// keeps a quarter of the full moon's light rather than none, so the ground
    /// still reads on the darkest nights.
    pub fn moon_light(self) -> f32 {
        smoothstep(-0.05, 0.05, self.moon_direction().y)
            * (0.25 + 0.75 * self.moon_illumination())
    }

    /// What the exposure meters off: the sun and the moon that are actually up,
    /// over a floor for the light the sky keeps when neither is.
    pub fn key_light(self) -> f32 {
        lux::RAW_SUNLIGHT * self.sun_light() + MOON_ILLUMINANCE * self.moon_light() + KEY_FLOOR
    }

    /// The stop the scene is graded at. Metering off the key light rather than
    /// off the clock is what keeps dawn smooth: the exposure closes exactly as
    /// the sun's own light arrives, so neither one can outrun the other. With
    /// the sun up the key is the sun, and the day is graded where it always was.
    pub fn ev100(self) -> f32 {
        if let Some(ev) = ev100_override() {
            return ev;
        }
        let stops = (self.key_light() / lux::RAW_SUNLIGHT).log2();
        (DAY_EV100 + EXPOSURE_COMPRESSION * stops).clamp(NIGHT_EV100, DAY_EV100)
    }

    /// The time of day `DWARF_EYE_HOUR` asks for, if it asks for one. The date
    /// is the game's own either way, so the season keeps its lean.
    pub fn with_hour_override(self) -> Self {
        match hour_override() {
            Some(tick_of_day) => {
                Self { year: self.year, tick: self.tick - self.tick_of_day() + tick_of_day }
            }
            None => self,
        }
    }
}

/// Points the sun and the moon where the clock says they should be, and fades
/// each one out as it sets.
pub fn drive_lights(
    clock: Res<Clock>,
    mut sun: Query<(&mut Transform, &mut DirectionalLight), (With<Sun>, Without<Moon>)>,
    mut moon: Query<(&mut Transform, &mut DirectionalLight), (With<Moon>, Without<Sun>)>,
) {
    // Far enough away that the light reads as parallel across the map.
    if let Ok((mut transform, mut light)) = sun.single_mut() {
        *transform = Transform::from_translation(clock.sun_direction() * 4000.0)
            .looking_at(Vec3::ZERO, Vec3::Y);
        light.illuminance = lux::RAW_SUNLIGHT * clock.sun_light();
    }
    if let Ok((mut transform, mut light)) = moon.single_mut() {
        *transform = Transform::from_translation(clock.moon_direction() * 4000.0)
            .looking_at(Vec3::ZERO, Vec3::Y);
        light.illuminance = MOON_ILLUMINANCE * clock.moon_light();
    }
}

/// Opens the exposure and lifts a starlight floor under the sky's own ambient
/// as the sun goes down.
pub fn drive_exposure(
    time: Res<Time>,
    clock: Res<Clock>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut exposure: Query<&mut Exposure>,
) {
    let night = clock.night();
    let target = clock.ev100();
    // Eased in stops, the way an eye adapts, so a clock that arrives a quarter
    // of an hour at a time never shows the step.
    let follow = 1.0 - (-time.delta_secs() / EXPOSURE_ADAPT).exp();
    for mut exposure in &mut exposure {
        exposure.ev100 += (target - exposure.ev100) * follow;
    }
    // By day this stays at Bevy's own default, so the daylit scene is lit
    // exactly as it was; night lifts a cool floor on top of it.
    //
    // The floor is held against the exposure rather than against the light, so
    // it reads the same through twilight as it does at midnight. Otherwise the
    // sky brightens before the ground does, the meter closes down to follow it,
    // and the half hour either side of sunrise goes black.
    ambient.color = Color::WHITE.mix(&NIGHT_AMBIENT_COLOUR, night);
    ambient.brightness = DAY_AMBIENT + NIGHT_AMBIENT * night * (target - NIGHT_EV100).exp2();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 15 Granite of year 100: the middle of a month, so a full moon.
    fn day(tick_of_day: i32) -> Clock {
        Clock { year: 100, tick: 14 * TICKS_PER_DAY + tick_of_day }
    }

    #[test]
    fn the_day_is_graded_where_it_always_was() {
        // Anywhere the sun is properly up, the exposure is the day's own stop.
        for tick in 350..1050 {
            let clock = day(tick);
            let elevation = clock.sun_direction().y;
            if elevation > 0.05 {
                assert_eq!(clock.ev100(), DAY_EV100, "tick {tick}");
                assert_eq!(clock.sun_light(), 1.0, "tick {tick}");
            }
            // The starlight floor lingers a little longer, and is gone once the
            // sun is five degrees up.
            if elevation > 0.09 {
                assert_eq!(clock.night(), 0.0, "tick {tick}");
            }
        }
    }

    #[test]
    fn the_exposure_follows_the_light_without_overrunning_it() {
        let noon = day(TICKS_PER_DAY / 2);
        let daylight = noon.key_light() * (-noon.ev100()).exp2();
        let mut previous = day(0);
        for tick in 1..TICKS_PER_DAY {
            let clock = day(tick);
            // A tick is a minute and change of the game's clock; the adaptation
            // in `drive_exposure` eases whatever is left of the step.
            let step = (clock.ev100() - previous.ev100()).abs();
            assert!(step < 1.5, "exposure jumped {step} stops at tick {tick}");
            let light = (clock.sun_light() - previous.sun_light()).abs();
            assert!(light < 0.1, "sunlight jumped {light} at tick {tick}");

            // What the meter is left holding: never brighter than noon, so no
            // hour blows out, and never black, so every hour reads.
            let level = clock.key_light() * (-clock.ev100()).exp2();
            assert!(level <= daylight * 1.01, "over-exposed at tick {tick}");
            assert!(level >= daylight * 0.05, "under-exposed at tick {tick}");
            previous = clock;
        }
    }

    #[test]
    fn night_is_lit_and_day_is_not_flooded() {
        let midnight = day(0);
        assert_eq!(midnight.sun_light(), 0.0);
        assert!(midnight.night() > 0.99);
        // A full moon at midnight stands opposite the sun, which puts it high.
        assert!(midnight.moon_illumination() > 0.99, "{}", midnight.moon_illumination());
        assert!(midnight.moon_direction().y > 0.9);
        assert!(midnight.moon_light() > 0.99);
        assert!(midnight.ev100() < DAY_EV100 - 4.0);

        let noon = day(TICKS_PER_DAY / 2);
        assert!(noon.is_daylight());
        assert_eq!(noon.moon_light(), 0.0, "the full moon is down at noon");
        assert_eq!(noon.night(), 0.0);
    }

    #[test]
    fn the_hour_override_keeps_the_date() {
        let clock = Clock { year: 100, tick: 14 * TICKS_PER_DAY + 844 };
        let pinned = Clock { year: clock.year, tick: clock.tick - clock.tick_of_day() + 600 };
        assert_eq!(pinned.day_of_month(), clock.day_of_month());
        assert_eq!(pinned.tick_of_day(), 600);
    }
}
