//! Drives the sun from Dwarf Fortress's own clock.
//!
//! DF runs 1200 ticks to a day, 28 days to a month, 12 months to a year. The
//! tick is the only clock the game exposes, so everything here derives from it.

use bevy::prelude::*;

pub const TICKS_PER_DAY: i32 = 1200;
pub const DAYS_PER_MONTH: i32 = 28;
pub const DAYS_PER_YEAR: i32 = DAYS_PER_MONTH * 12;

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
}

/// Points the sun where the clock says it should be.
pub fn drive_sun(clock: Res<Clock>, mut sun: Query<&mut Transform, With<DirectionalLight>>) {
    let Ok(mut transform) = sun.single_mut() else { return };
    let direction = clock.sun_direction();
    // Far enough away that the light reads as parallel across the map.
    *transform =
        Transform::from_translation(direction * 4000.0).looking_at(Vec3::ZERO, Vec3::Y);
}
