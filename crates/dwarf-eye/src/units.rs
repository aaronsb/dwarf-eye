//! Where every creature stands, read on a light connection of its own.
//!
//! Units move several times a second and the map does not. Asking for them on
//! the worker's connection would put a poll behind a slab of blocks and a
//! pass behind a poll, so this takes a third connection: three small RPCs
//! every quarter second, and nothing the map fetch ever waits on.
//!
//! What it sends is a census in absolute tiles. The renderer places it against
//! the render origin the same way walk mode places the character, and eases
//! each unit between two censuses so a creature walks rather than teleports.
//!
//! The look is the factory's: `Class::Unit` resolves to `Treatment::Capsule`,
//! and DF gives the size the capsule is cut to. A prefab replaces the capsule
//! later without any of this changing.

use anyhow::Result;
use dfhack_remote::{Client, methods, rfr};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

/// How often the census is taken. Four a second is under DF's own step rate
/// and cheap enough that the eased positions carry the rest.
pub const POLL: Duration = Duration::from_millis(250);

/// Tiles per unit of `MapInfo::block_pos`, the same 48-tile region tile the
/// session pins its origin to.
const REGION_TILE: i32 = 48;

/// DF's `unit_flags1.dead`.
const DEAD: u32 = 0x2;

/// The body size of an adult human, DF's own units. Everything else is drawn
/// as its cube root against this, so a peafowl is a third of a person and a
/// pig two thirds rather than each being guessed at.
const HUMAN_SIZE: f32 = 7775.0;

/// How tall an adult human stands, as a fraction of a z-level.
const HUMAN_HEIGHT: f32 = 0.85;

/// One creature, placed in absolute tiles.
#[derive(Clone, Copy, Debug)]
pub struct Unit {
    pub id: i32,
    /// Absolute tile, with DF's sub-tile offset folded in.
    pub at: (f32, f32, f32),
    /// Creature index, which is what the colour is drawn from.
    #[allow(dead_code)]
    pub race: i32,
    pub color: [u8; 3],
    /// Height in z-levels and radius in tiles, from DF's body size.
    pub height: f32,
    pub radius: f32,
    /// The character the player is inside.
    pub adventurer: bool,
}

/// Everything standing on the map at one instant.
pub struct Census {
    pub units: Vec<Unit>,
}

/// The unit poller's end of the channel.
pub struct UnitFeed {
    pub rx: Receiver<Census>,
}

impl UnitFeed {
    /// Spawns the poller on its own connection and thread.
    pub fn spawn() -> Self {
        let (tx, rx) = channel();
        thread::Builder::new()
            .name("dfhack-units".into())
            .spawn(move || {
                if let Err(err) = poll(&tx) {
                    bevy::log::warn!("units: the poll connection went away: {err:#}");
                }
            })
            .expect("spawning the unit thread");
        Self { rx }
    }
}

/// Creature colours out of the raws, by creature index.
///
/// DF gives every creature a colour and it is the one the game itself draws
/// them in, so it is worth a round trip at connect. A species the raws leave
/// black falls back to a hash of its index, which at least keeps two species
/// apart.
#[derive(Default)]
pub struct Races {
    colors: HashMap<i32, [u8; 3]>,
}

impl Races {
    pub fn from_raws(list: &rfr::CreatureRawList) -> Self {
        let mut colors = HashMap::new();
        for raw in &list.creature_raws {
            let Some(c) = raw.color.as_ref() else { continue };
            let rgb = [
                c.red.clamp(0, 255) as u8,
                c.green.clamp(0, 255) as u8,
                c.blue.clamp(0, 255) as u8,
            ];
            if rgb != [0, 0, 0] {
                colors.insert(raw.index(), rgb);
            }
        }
        Self { colors }
    }

    /// The colour a race is drawn in.
    pub fn color(&self, race: i32) -> [u8; 3] {
        if let Some(found) = self.colors.get(&race) {
            return *found;
        }
        hashed_color(race)
    }
}

/// A stable colour per race, for when the raws offer none.
///
/// Spread around the hue circle at one lightness, so two species never come
/// out the same and none of them comes out black.
fn hashed_color(race: i32) -> [u8; 3] {
    let mut h = (race as u32).wrapping_mul(0x9E3779B1);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545F491);
    h ^= h >> 13;
    let hue = (h & 0xFFFF) as f32 / 65535.0 * 6.0;
    let sector = hue as u32 % 6;
    let f = hue - hue.floor();
    let (lo, hi) = (0.35, 0.85);
    let up = lo + (hi - lo) * f;
    let down = hi - (hi - lo) * f;
    let rgb = match sector {
        0 => [hi, up, lo],
        1 => [down, hi, lo],
        2 => [lo, hi, up],
        3 => [lo, down, hi],
        4 => [up, lo, hi],
        _ => [hi, lo, down],
    };
    [
        (rgb[0] * 255.0) as u8,
        (rgb[1] * 255.0) as u8,
        (rgb[2] * 255.0) as u8,
    ]
}

/// Reads one reply into a census.
///
/// `window` is the absolute tile the live window's corner sits at: DF reports
/// a unit where it reports a map block, in tiles local to that window, so the
/// same creature is at two different local positions either side of a
/// re-centring and only the absolute position is worth sending.
pub fn parse_units(
    list: &rfr::UnitList,
    window: (i32, i32, i32),
    following: i32,
    races: &Races,
) -> Vec<Unit> {
    let mut out = Vec::with_capacity(list.creature_list.len());
    for unit in &list.creature_list {
        // `isValid` is a field DFHack declares and does not fill, so an absent
        // one means nothing; only an explicit false is a unit to skip.
        if unit.is_valid == Some(false) || unit.flags1() & DEAD != 0 {
            continue;
        }
        let race = unit.race.as_ref().map(|r| r.mat_type).unwrap_or(-1);
        let size = unit
            .size_info
            .as_ref()
            .map(|s| s.size_cur() as f32)
            .filter(|s| *s > 0.0)
            .unwrap_or(HUMAN_SIZE);
        let scale = (size / HUMAN_SIZE).cbrt();
        let height = (HUMAN_HEIGHT * scale).clamp(0.12, 1.8);
        out.push(Unit {
            id: unit.id,
            at: (
                (window.0 + unit.pos_x()) as f32 + 0.5 + unit.subpos_x(),
                (window.1 + unit.pos_y()) as f32 + 0.5 + unit.subpos_y(),
                (window.2 + unit.pos_z()) as f32 + unit.subpos_z(),
            ),
            race,
            color: races.color(race),
            height,
            // Half a tile is the whole tile: a creature never leans into its
            // neighbour, whatever DF says it weighs.
            radius: (height * 0.3).clamp(0.08, 0.42),
            adventurer: unit.id == following,
        });
    }
    out
}

fn poll(census: &Sender<Census>) -> Result<()> {
    let mut client = Client::connect_local()?;
    // The raws are a few hundred kilobytes and never change; a species with no
    // colour of its own falls back to a hash, so a failure here costs nothing
    // but the hues.
    let races = match client.call_empty::<rfr::CreatureRawList>(methods::GET_CREATURE_RAWS) {
        Ok(raws) => Races::from_raws(&raws),
        Err(err) => {
            // DFHack writes creature descriptions straight out of the raws and
            // some of them are not UTF-8, which fails the whole reply. Nothing
            // is lost but the hues.
            bevy::log::info!("units: no creature raws ({err:#}); colouring races by hash");
            Races::default()
        }
    };

    loop {
        let next = Instant::now() + POLL;
        let window: Option<rfr::MapInfo> = client.call_empty(methods::GET_MAP_INFO).ok();
        let view: Option<rfr::ViewInfo> = client.call_empty(methods::GET_VIEW_INFO).ok();
        let list: Option<rfr::UnitList> = client.call_empty(methods::GET_UNIT_LIST).ok();

        if let (Some(window), Some(list)) = (window, list) {
            let corner = (
                window.block_pos_x() * REGION_TILE,
                window.block_pos_y() * REGION_TILE,
                window.block_pos_z(),
            );
            let following = view.map(|v| v.follow_unit_id()).unwrap_or(-1);
            let units = parse_units(&list, corner, following, &races);
            if census.send(Census { units }).is_err() {
                return Ok(());
            }
        } else {
            // No map: travel, or a loading screen. Nothing to report and
            // nothing to be gained by hammering it.
            thread::sleep(Duration::from_millis(500));
        }

        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(id: i32, pos: (i32, i32, i32), race: i32) -> rfr::UnitDefinition {
        rfr::UnitDefinition {
            id,
            is_valid: Some(true),
            pos_x: Some(pos.0),
            pos_y: Some(pos.1),
            pos_z: Some(pos.2),
            race: Some(rfr::MatPair { mat_type: race, mat_index: 0 }),
            ..Default::default()
        }
    }

    #[test]
    fn a_unit_lands_where_the_window_puts_it() {
        let list = rfr::UnitList { creature_list: vec![unit(7, (3, 4, 5), 2)] };
        let units = parse_units(&list, (480, 96, 100), -1, &Races::default());
        assert_eq!(units.len(), 1);
        // Absolute tile, centred in it.
        assert_eq!(units[0].at, (483.5, 100.5, 105.0));
        assert_eq!(units[0].race, 2);
        assert!(!units[0].adventurer);
    }

    #[test]
    fn the_dead_and_the_invalid_are_not_drawn() {
        let mut dead = unit(1, (0, 0, 0), 0);
        dead.flags1 = Some(DEAD);
        let mut gone = unit(2, (0, 0, 0), 0);
        gone.is_valid = Some(false);
        let list = rfr::UnitList { creature_list: vec![dead, gone, unit(3, (0, 0, 0), 0)] };
        let units = parse_units(&list, (0, 0, 0), -1, &Races::default());
        assert_eq!(units.iter().map(|u| u.id).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn a_unit_dfhack_says_nothing_about_is_still_a_unit() {
        // DFHack declares `isValid` and never fills it, so an absent one is a
        // live creature rather than a reason to drop every unit on the map.
        let mut quiet = unit(9, (1, 2, 3), 0);
        quiet.is_valid = None;
        let list = rfr::UnitList { creature_list: vec![quiet] };
        assert_eq!(parse_units(&list, (0, 0, 0), -1, &Races::default()).len(), 1);
    }

    #[test]
    fn the_followed_unit_is_the_adventurer() {
        let list = rfr::UnitList { creature_list: vec![unit(11, (0, 0, 0), 0), unit(12, (1, 0, 0), 0)] };
        let units = parse_units(&list, (0, 0, 0), 12, &Races::default());
        assert!(!units[0].adventurer);
        assert!(units[1].adventurer);
    }

    #[test]
    fn size_sets_the_capsule_and_never_leaves_the_tile() {
        let mut big = unit(1, (0, 0, 0), 0);
        big.size_info = Some(rfr::BodySizeInfo { size_cur: Some(500_000), ..Default::default() });
        let mut small = unit(2, (0, 0, 0), 0);
        small.size_info = Some(rfr::BodySizeInfo { size_cur: Some(400), ..Default::default() });
        let list = rfr::UnitList { creature_list: vec![big, small] };
        let units = parse_units(&list, (0, 0, 0), -1, &Races::default());
        assert!(units[0].height > units[1].height, "a dragon is taller than a peafowl");
        for u in &units {
            assert!(u.radius <= 0.5, "a creature stays inside its own tile");
            assert!(u.radius > 0.0);
        }
    }

    #[test]
    fn a_race_without_a_colour_still_gets_one_of_its_own() {
        let races = Races::default();
        assert_ne!(races.color(4), races.color(5));
        assert_eq!(races.color(4), races.color(4));
        assert_ne!(races.color(4), [0, 0, 0]);
    }

    #[test]
    fn the_raws_colour_wins_where_there_is_one() {
        let raws = rfr::CreatureRawList {
            creature_raws: vec![rfr::CreatureRaw {
                index: Some(3),
                color: Some(rfr::ColorDefinition { red: 10, green: 200, blue: 30 }),
                ..Default::default()
            }],
        };
        let races = Races::from_raws(&raws);
        assert_eq!(races.color(3), [10, 200, 30]);
    }
}
