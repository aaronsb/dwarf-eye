//! Walk sync: a first-person camera that shares a tile with the adventurer.
//!
//! Free flight can go anywhere, which is exactly what the game cannot follow.
//! Walk mode gives the camera back to the character: it stands in the
//! character's tile at eye height, moves freely inside that one cell, and the
//! moment it crosses a cell edge the game is asked to take a step that way.
//!
//! Movement is optimistic. The camera does not wait at the edge; it walks on
//! into the next cell while the request is in flight, and the position poll
//! settles up afterwards. A confirmed step needs no correction. A step the
//! game never takes springs the camera back and shuts that edge for a couple
//! of seconds. The camera is never allowed more than one cell ahead of what
//! the game has confirmed, so a second edge waits for the first step to land.
//!
//! The sync runs both ways. When the character moves without being asked —
//! the player clicking a distant tile in Dwarf Fortress, a series of automatic
//! steps, travel, being shoved — the poll sees a tile we did not ask for and
//! the camera simply follows it, keeping where the player stood inside the
//! cell and where they were looking.
//!
//! All of that rests on a step being confirmed within a fraction of a second,
//! so walk mode holds its own Dwarf Fortress connection on its own thread.
//! Sharing the map thread does not work: one collection pass can hold it for
//! many seconds, every step in that time is scored as a refusal, and the
//! backlog then replays itself into the game all at once.
//!
//! That failure is designed out rather than tuned away. There is no queue
//! between here and the game: one slot holds the single step waiting to go,
//! writing a new order overwrites the old, and an order the confirmation
//! window has already passed is dropped rather than sent late. At most one
//! step can be outstanding, and a late step cannot exist.

use crate::camera::FlyCamera;
use crate::worker::Bridge;
use anyhow::Result;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use dwarf_eye_world::mesh::{FLOOR_HEIGHT, Z_SCALE};
use dwarf_eye_world::{
    BLOCK, BlockBounds, Ground as GroundMode, MeshOptions, Session, Solid, Surface, World, ramp,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Tiles per second inside a cell. Close to the pace the game confirms a step
/// at, so a held key flows rather than stuttering against the cell edge.
const SPEED: f32 = 3.2;

/// Eye height above the surface the character stands on, in levels. A tile is
/// about two metres across and a level about three tall in the game's fiction,
/// and a cube here is one of each, so a person's eye sits near the top of the
/// cell they are standing in.
const EYE: f32 = 0.85 * Z_SCALE;

/// How high the lower step of a staircase carries its footing inside its cell,
/// matching what the mesh draws there. A ramp has no single height: its
/// surface is a slope, read corner by corner out of [`ramp::surface`].
const STAIR_FOOTING: f32 = 0.5 * Z_SCALE;

/// How close the drawn eye may ever come to the ground beneath it, whatever
/// else is going on.
const CLEARANCE: f32 = 0.25 * Z_SCALE;

/// How far in front of the eye the near plane reaches, in tiles. Walking down
/// a slope the ground just ahead stands higher than the ground underfoot, and
/// it is that ground the near plane cuts into first.
const NEAR: f32 = 0.2;

/// A rise steeper than this beside the eye is a wall, not the floor being
/// walked on, and standing next to a wall must not lift the camera.
const A_WALL: f32 = 0.75 * Z_SCALE;

/// How close to a shut edge the camera may press.
const MARGIN: f32 = 0.12;

/// How long a step may stay unconfirmed before it is treated as refused. The
/// same window bounds how long an unsent order stays worth sending.
const CONFIRM: f32 = 0.6;

/// How long a refused edge stays shut. Long enough that a wall is leaned on
/// rather than hammered, short enough that a door opening is noticed.
const BLOCKED: f32 = 2.0;

/// Consecutive unconfirmed steps that call a scripted walk off. A player can
/// lean on a wall as long as they like; a script cannot.
const STRIKES: u32 = 3;

/// How long a scripted walk may push at an edge it is not allowed to cross
/// before it gives up. A step the game refuses shows as an unconfirmed strike;
/// a step our own reading of the ground refuses is never sent at all, and
/// without this a script would lean on a wall in silence.
const STALL: f32 = 1.5;

/// Time constant of the glide that absorbs every jump: a step onto a ramp, a
/// spring back, the game moving the character a tile of its own accord.
/// Roughly a 0.15 s settle.
const GLIDE: f32 = 0.05;

/// A jump further than this is not eased but taken outright — travel, or a
/// teleport, where a glide would only be a long slide through the ground.
const SNAP: f32 = 5.0;

/// How far around the character walkability is carried. The camera is never
/// more than one cell from the confirmed tile, so this is generous.
const GROUND_RADIUS: i32 = 4;

/// The longest gap between two moves the game drove that still reads as one
/// run. A little over a poll apart is travel; further apart is two separate
/// nudges, and turning the camera for a nudge would only make it seasick.
const RUN_GAP: f32 = 1.2;

/// Moves in a run before the camera turns for it. One move is a shove; two in
/// quick succession is a direction of travel.
const RUN_STEPS: usize = 2;

/// How many recent moves the heading is drawn from.
const RUN_MEMORY: usize = 4;

/// How much less each older move counts than the one after it, so a corner is
/// followed rather than averaged away.
const RUN_DECAY: f32 = 0.5;

/// The time constant of the turn onto a new heading: most of the swing happens
/// inside it, and the tail of it dies away over about twice as long again.
const TURN: f32 = 0.3;

/// How often the walk thread reads the character's position.
const POLL: Duration = Duration::from_millis(125);

// ---------------------------------------------------------------- the pilot

/// The one step waiting to reach the game.
struct Order {
    /// The suffix of `df.interface_key.A_MOVE_*`: `N`, `SE`, `UP` and so on.
    key: String,
    issued: Instant,
}

/// Where the character stands, in absolute tiles, with the shape of the ground
/// around them.
pub struct Report {
    tile: (i32, i32, i32),
    ground: Ground,
}

/// Walk mode's own connection to Dwarf Fortress, on its own thread.
///
/// It drives itself: while walk mode is on it reads the character's position
/// at a steady rate, and between reads it delivers whatever single step is
/// waiting. Nothing queues, so nothing can arrive late.
pub struct Pilot {
    /// The one step waiting to go. Writing replaces whatever was there.
    order: Arc<Mutex<Option<Order>>>,
    active: Arc<AtomicBool>,
    pub rx: Receiver<Report>,
}

impl Pilot {
    pub fn spawn() -> Self {
        let order: Arc<Mutex<Option<Order>>> = Arc::default();
        let active = Arc::new(AtomicBool::new(false));
        let (reports, rx) = channel();
        let (theirs, alive) = (Arc::clone(&order), Arc::clone(&active));
        thread::Builder::new()
            .name("dfhack-walk".into())
            .spawn(move || {
                if let Err(err) = pilot(&theirs, &alive, &reports) {
                    bevy::log::warn!("walk mode lost its connection: {err:#}");
                }
            })
            .expect("spawning the walk thread");
        Self { order, active, rx }
    }

    /// Asks for one step. Any step still waiting is dropped: only the newest
    /// intention matters, and only one can ever be in flight.
    fn step(&self, key: &str) {
        if let Ok(mut slot) = self.order.lock() {
            *slot = Some(Order { key: key.to_string(), issued: Instant::now() });
        }
    }

    fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }
}

fn pilot(
    order: &Mutex<Option<Order>>,
    active: &AtomicBool,
    reports: &Sender<Report>,
) -> Result<()> {
    let mut df = Session::connect_local()?;
    let origin = df.origin();
    let mut next_poll = Instant::now();
    // The block the local map was last pulled for. Walkability only needs the
    // character's own neighbourhood, so this is refreshed on crossing a block
    // rather than on every poll.
    let mut mapped: Option<(i32, i32, i32)> = None;

    loop {
        if !active.load(Ordering::Relaxed) {
            // Idle costs the game nothing.
            thread::sleep(Duration::from_millis(50));
            next_poll = Instant::now();
            continue;
        }

        // Steps go first: a step delayed behind a read is a step the camera
        // has already walked past.
        let waiting = order.lock().ok().and_then(|mut slot| slot.take());
        if let Some(Order { key, issued }) = waiting {
            if issued.elapsed().as_secs_f32() > CONFIRM {
                // The camera has given up on this one, so the game must never
                // see it. A late step is a step the player did not ask for.
                bevy::log::warn!("walk: dropped a stale {key} step");
            } else {
                let expr = format!(
                    "local s=dfhack.gui.getCurViewscreen(); s:feed_key(df.interface_key.A_MOVE_{key})"
                );
                if let Err(err) = df.client.run_command("lua", &[&expr]) {
                    bevy::log::warn!("walk: step {key} was not delivered: {err:#}");
                }
            }
        }

        if Instant::now() < next_poll {
            thread::sleep(Duration::from_millis(3));
            continue;
        }
        next_poll = Instant::now() + POLL;

        // The window follows the character, so where it sits has to be re-read
        // alongside the position: a stale window would place the same tile 48
        // tiles away.
        if df.refresh_window().is_err() {
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        let Ok(here) = df.view_center() else {
            thread::sleep(Duration::from_millis(200));
            continue;
        };

        let block = (here.0.div_euclid(BLOCK), here.1.div_euclid(BLOCK), here.2);
        if mapped != Some(block) {
            mapped = Some(block);
            let around = |r: i32, d: i32| BlockBounds {
                min_x: block.0 - r,
                max_x: block.0 + r + 1,
                min_y: block.1 - r,
                max_y: block.1 + r + 1,
                min_z: here.2 - d,
                max_z: here.2 + d + 1,
            };
            let _ = df.fetch(around(1, 2), true);
            df.world.retain_within(around(2, 4));
        }

        let tile = (here.0 + origin.0, here.1 + origin.1, here.2 + origin.2);
        reports.send(Report { tile, ground: Ground::sample(&df.world, here, origin) })?;
    }
}

// ---------------------------------------------------------------- the ground

/// The shape of the ground near the character, so the camera can judge a step
/// and find its own eye height without a round trip to the game.
pub struct Ground {
    /// Absolute tile the square is centred on.
    center: IVec3,
    /// Solids for `z-1..=z+1` over a `2 * GROUND_RADIUS + 1` square, level-major
    /// then row-major. `None` where that chunk is not loaded.
    solids: Vec<Option<Solid>>,
    /// The smoothed ground over the same square, in tiles east and south of
    /// its lower corner. `None` in stepped mode.
    surface: Option<Surface>,
    /// The render-frame level the sheet was read at, so a height off it can be
    /// turned back into a height over the square's own level whatever the
    /// render origin has done since.
    level: i32,
}

impl Ground {
    /// Reads the square out of a voxel world whose render frame is pinned at
    /// `origin`. Cheap: a few hundred lookups.
    fn sample(world: &World, center: (i32, i32, i32), origin: (i32, i32, i32)) -> Self {
        let r = GROUND_RADIUS;
        let mut solids = Vec::with_capacity((3 * (2 * r + 1) * (2 * r + 1)) as usize);
        for dz in -1..=1 {
            for dy in -r..=r {
                for dx in -r..=r {
                    solids.push(
                        world
                            .voxel(center.0 + dx, center.1 + dy, center.2 + dz)
                            .map(|v| v.solid),
                    );
                }
            }
        }
        // The eye rides exactly what the mesher drew, so it is built by the
        // same code over the same square (`heightfield.rs:Surface::over`).
        let surface = GroundMode::current().smooth().then(|| {
            Surface::over(
                world,
                (center.0 - r, center.1 - r, center.2),
                2 * r + 1,
                MeshOptions { z_ceiling: i32::MAX, show_hidden: true },
            )
        });
        Self {
            center: IVec3::new(center.0 + origin.0, center.1 + origin.1, center.2 + origin.2),
            solids,
            surface,
            level: center.2,
        }
    }

    /// The smoothed ground under a tile, `u` east and `v` south inside it, as
    /// a height over that tile's own base. `None` where the sheet has nothing
    /// there, or where what it has belongs to a different level.
    ///
    /// The sheet is read at one level and folds the levels either side of it
    /// into that plane, so a height off it belongs to whichever cell it lands
    /// inside — which is the test.
    fn smooth(&self, tile: IVec3, u: f32, v: f32) -> Option<f32> {
        let d = tile - self.center;
        let r = GROUND_RADIUS;
        if d.x.abs() > r || d.y.abs() > r {
            return None;
        }
        // `Surface` works in the render frame it was read in; the square keeps
        // only offsets, so ask it in its own coordinates.
        let (x, y) = ((d.x + r) as f32 + u, (d.y + r) as f32 + v);
        let base = (self.level + d.z) as f32 * Z_SCALE;
        let height = self.surface.as_ref()?.at(x, y)? - base;
        (0.0..=Z_SCALE).contains(&height).then_some(height)
    }

    fn get(&self, tile: IVec3) -> Option<Solid> {
        let d = tile - self.center;
        let r = GROUND_RADIUS;
        if d.x.abs() > r || d.y.abs() > r || d.z.abs() > 1 {
            return None;
        }
        let span = 2 * r + 1;
        *self.solids.get(((d.z + 1) * span * span + (d.y + r) * span + (d.x + r)) as usize)?
    }
}

/// Reads a ramp's 3x3 corner field as a bilinear patch: corners sit at the
/// tile's edges and the middles half way, so a position inside the tile falls
/// in one quadrant of the four.
fn patch(field: [[f32; 3]; 3], u: f32, v: f32) -> f32 {
    let pair = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        if t < 0.5 { (0, 1, t * 2.0) } else { (1, 2, (t - 0.5) * 2.0) }
    };
    // Row 0 is north and column 0 is west, as `ramp::levels` lays them out.
    let (north, south, down) = pair(v);
    let (west, east, across) = pair(u);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    lerp(
        lerp(field[north][west], field[north][east], across),
        lerp(field[south][west], field[south][east], across),
        down,
    )
}

/// Whether a tile offers footing. Shrubs and saplings do: Dwarf Fortress lets
/// the character walk through them, and refusing would fence the player in.
fn stands_on(solid: Solid) -> bool {
    matches!(solid, Solid::Floor | Solid::Ramp | Solid::Stair | Solid::Foliage)
}

fn obstructs(solid: Solid) -> bool {
    matches!(solid, Solid::Cube | Solid::Fortification)
}

// --------------------------------------------------------------- the heading

/// What a position report means, set against the step the camera was waiting
/// for. Everything that is not a step of ours is the game moving the character.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fix {
    /// The first report after entering the mode: the camera drops into it.
    First,
    /// Our step has not landed yet.
    Waiting,
    /// Our step, taken.
    Ordered,
    /// The character has not moved.
    Still,
    /// The game moved the character without being asked: travel, a click on a
    /// distant tile, arrow keys in the game's own window, being shoved.
    Driven,
}

/// Reads a report against what the camera asked for. `pending` is the step
/// waiting to land, as the tile it left and its horizontal direction.
fn classify(confirmed: Option<IVec3>, pending: Option<(IVec3, IVec2)>, tile: IVec3) -> Fix {
    let Some(confirmed) = confirmed else { return Fix::First };
    if let Some((from, dir)) = pending {
        if tile == from {
            return Fix::Waiting;
        }
        // A staircase moves only the level, so it is matched on the column.
        let landed = if dir == IVec2::ZERO {
            tile.x == from.x && tile.y == from.y
        } else {
            tile.x == from.x + dir.x && tile.y == from.y + dir.y
        };
        if landed {
            return Fix::Ordered;
        }
    }
    if confirmed == tile { Fix::Still } else { Fix::Driven }
}

/// The direction of travel, read off a run of moves the game drove.
///
/// Levels are left out of it. A ramp or a staircase changes z without saying
/// anything about which way the character is facing, and a run that crosses
/// one has to survive the crossing, so only the ground plan counts.
#[derive(Default)]
struct Heading {
    /// The last few moves, oldest first, as unit vectors east and south.
    steps: Vec<Vec2>,
    /// Seconds since the last move the game drove.
    since: f32,
    /// Whether a run is long enough to steer by.
    live: bool,
    /// Yaw velocity the ease is carrying.
    turn: f32,
}

impl Heading {
    /// Ages the run. A gap longer than `RUN_GAP` ends it, and the camera is
    /// simply left facing wherever the turn had reached.
    fn tick(&mut self, dt: f32) {
        if self.steps.is_empty() {
            return;
        }
        self.since += dt;
        if self.since > RUN_GAP {
            *self = Self::default();
        }
    }

    /// A move the game drove, as a tile delta on the ground plan.
    fn drove(&mut self, delta: IVec2) {
        if self.since > RUN_GAP {
            *self = Self::default();
        }
        self.since = 0.0;
        let step = Vec2::new(delta.x as f32, delta.y as f32).normalize_or_zero();
        // Straight up or down a staircase: the run carries on, but there is no
        // direction in this one to add to it.
        if step == Vec2::ZERO {
            return;
        }
        self.steps.push(step);
        if self.steps.len() > RUN_MEMORY {
            self.steps.remove(0);
        }
        self.live = self.steps.len() >= RUN_STEPS;
    }

    /// The player took the wheel, so the run is over at once rather than
    /// fading: walking with the client's own keys is the client's heading.
    fn ordered(&mut self) {
        *self = Self::default();
    }

    /// The yaw the run points at, weighted toward the newest moves, or `None`
    /// while nothing is live.
    fn aim(&self) -> Option<f32> {
        if !self.live {
            return None;
        }
        let mut sum = Vec2::ZERO;
        let mut weight = 1.0;
        for step in self.steps.iter().rev() {
            sum += *step * weight;
            weight *= RUN_DECAY;
        }
        let sum = sum.normalize_or_zero();
        if sum == Vec2::ZERO {
            return None;
        }
        // Render x runs east and render z runs south, while yaw has north at
        // zero and swings west as it grows.
        Some((-sum.x).atan2(-sum.y))
    }
}

/// An angle folded onto the short way round, in `-PI..=PI`.
fn wrap(angle: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    (angle + PI).rem_euclid(TAU) - PI
}

/// Eases an angle toward another, critically damped: it comes round in about
/// `TURN` seconds, never overshoots, and the result does not depend on the
/// frame rate.
fn ease_angle(yaw: f32, aim: f32, speed: &mut f32, dt: f32) -> f32 {
    let omega = 2.0 / TURN;
    let x = omega * dt;
    let decay = 1.0 / (1.0 + x + 0.48 * x * x + 0.235 * x * x * x);
    let gap = wrap(yaw - aim);
    // Aim for the nearest turn of the same heading, so the camera never takes
    // the long way round.
    let aim = yaw - gap;
    let carried = (*speed + omega * gap) * dt;
    *speed = (*speed - omega * carried) * decay;
    aim + (gap + carried) * decay
}

// ------------------------------------------------------------------ the mode

/// A step asked for and not yet seen in the game.
struct Pending {
    /// The tile it started from, which a refusal returns to.
    from: IVec3,
    /// Its horizontal direction, zero for a move up or down a staircase.
    dir: IVec2,
    age: f32,
}

#[derive(Resource, Default)]
pub struct WalkMode {
    pub active: bool,
    /// The render origin the map thread pinned, once it has connected.
    /// Positions travel as absolute tiles and are placed against this.
    origin: Option<IVec3>,
    /// Where the game last said the character stands, in absolute tiles.
    confirmed: Option<IVec3>,
    /// The cell the camera occupies: the confirmed tile, or one step ahead.
    cell: IVec3,
    /// Where inside that cell the camera stands, east and south, 0 to 1.
    offset: Vec2,
    pending: Option<Pending>,
    /// Directions that refused a step, and how long they stay shut, indexed by
    /// `(dy + 1) * 3 + (dx + 1)`.
    blocked: [f32; 9],
    /// Steps in a row the game has not taken. Reset by any real movement.
    strikes: u32,
    /// How long the camera has been pushing at an edge it may not cross.
    stalled: f32,
    /// The gap between where the camera is drawn and where it stands on the
    /// ground plan, decayed away so every jump reads as a glide. Height is
    /// never part of it: the eye rides the surface, so it slides down a slope
    /// rather than through it.
    glide: Vec2,
    ground: Option<Ground>,
    /// A position report waiting to be reconciled.
    fix: Option<(IVec3, Ground)>,
    /// The direction of travel while the game is doing the driving.
    heading: Heading,
    /// Scripted legs still to walk, from the environment. Current first.
    drive: Vec<Leg>,
    /// Set once the environment has been read.
    started: bool,
    /// Why a scripted walk gave up, if it did.
    aborted: Option<String>,
    /// What the HUD says about the mode.
    pub line: String,
}

impl WalkMode {
    fn solid(&self, tile: IVec3) -> Option<Solid> {
        self.ground.as_ref().and_then(|g| g.get(tile))
    }

    /// Which of a ramp's eight neighbours are walls, the same set the mesh
    /// builds its slope from, so the ground underfoot is the ground on screen.
    fn ramp_mask(&self, tile: IVec3) -> u8 {
        let mut mask = 0;
        for (bit, dx, dy) in ramp::NEIGHBOURS {
            if self
                .solid(IVec3::new(tile.x + dx, tile.y + dy, tile.z))
                .is_some_and(obstructs)
            {
                mask |= bit;
            }
        }
        mask
    }

    /// How high the footing in a tile stands above the tile's own base, `u`
    /// east and `v` south inside it: the surface the feet are on, as the mesh
    /// draws it. Dwarf Fortress puts a unit on the level whose floor it stands
    /// on, and that floor is a thin slab resting in the bottom of the cell,
    /// not the cell's base itself.
    fn footing(&self, tile: IVec3, solid: Option<Solid>, u: f32, v: f32) -> f32 {
        // Natural ground is one smoothed sheet, and the eye rides the sheet
        // the mesher drew rather than the slab underneath it. A ramp inside it
        // is part of the sheet: its corner field is where the sheet's samples
        // came from, so the two cannot disagree. A staircase is not ground and
        // keeps its own footing.
        if solid != Some(Solid::Stair)
            && let Some(h) = self.ground.as_ref().and_then(|g| g.smooth(tile, u, v))
        {
            return h;
        }
        match solid {
            // A ramp is a slope, not a step, so the height comes from where on
            // the tile the eye actually is. Its low edge meets the floor slabs
            // beside it and its high edge meets the slab on the level above,
            // which is what carries the eye across both without a step.
            Some(Solid::Ramp) => {
                FLOOR_HEIGHT + patch(ramp::slopes(self.ramp_mask(tile)), u, v) * Z_SCALE
            }
            Some(Solid::Stair) => STAIR_FOOTING,
            // Ground we have not seen yet is taken for ordinary floor, which
            // is what almost all of it is.
            _ => FLOOR_HEIGHT,
        }
    }

    /// The height of the ground directly under a render-space position.
    ///
    /// This is what the eye rides, rather than the level of the cell we think
    /// we are in. Walking off a ramp onto the floor beside it changes the tile
    /// and the level underfoot at the same moment, and because a ramp's edge
    /// corners meet the slabs next to them the height does not jump — the eye
    /// tracks the slope down and off it.
    fn surface(&self, x: f32, y: f32) -> f32 {
        let origin = self.origin.unwrap_or(IVec3::ZERO);
        let (tx, ty) = (x.floor(), y.floor());
        let (u, v) = (x - tx, y - ty);
        let (tx, ty) = (tx as i32 + origin.x, ty as i32 + origin.y);
        // The level we believe we are on first, then the one below — where a
        // ramp we have walked off the top of lives — then the one above.
        for z in [self.cell.z, self.cell.z - 1, self.cell.z + 1] {
            let tile = IVec3::new(tx, ty, z);
            let solid = self.solid(tile);
            if solid.is_some_and(stands_on) {
                return (z - origin.z) as f32 * Z_SCALE + self.footing(tile, solid, u, v);
            }
        }
        (self.cell.z - origin.z) as f32 * Z_SCALE + FLOOR_HEIGHT
    }

    /// Where the camera logically stands on the ground plan, before the glide.
    /// Height is not part of it: the eye takes that from whatever it is
    /// standing over.
    fn anchor(&self) -> Vec2 {
        let cell = self.cell - self.origin.unwrap_or(IVec3::ZERO);
        Vec2::new(cell.x as f32 + self.offset.x, cell.y as f32 + self.offset.y)
    }

    /// Applies a change that moves the camera without the player walking, and
    /// turns the discontinuity into a glide. A long jump is taken outright.
    fn jump(&mut self, change: impl FnOnce(&mut Self)) {
        let before = self.anchor();
        change(self);
        self.glide += before - self.anchor();
        if self.glide.length() > SNAP {
            self.glide = Vec2::ZERO;
        }
    }

    /// The level a step in `dir` would land on, or `None` where it cannot be
    /// taken. Unknown ground never refuses: terrain that has not streamed in
    /// yet is not a wall, and a step the game will not take is caught anyway
    /// when the poll shows the character has not moved.
    fn step_target(&self, from: IVec3, dir: IVec2) -> Option<i32> {
        let (x, y) = (from.x + dir.x, from.y + dir.y);
        let on_ramp = self.solid(from) == Some(Solid::Ramp);
        for dz in [0, -1, 1] {
            // A level is only gained off a ramp; Dwarf Fortress takes that step
            // itself as the character walks into the rise.
            if dz == 1 && !on_ramp {
                continue;
            }
            match self.solid(IVec3::new(x, y, from.z + dz)) {
                Some(solid) if stands_on(solid) => {
                    // Dropping to a ramp below only works if the cell above it
                    // is open to walk through.
                    if dz < 0 && self.solid(IVec3::new(x, y, from.z)).is_some_and(obstructs) {
                        continue;
                    }
                    return Some(from.z + dz);
                }
                Some(_) => {}
                None => return Some(from.z),
            }
        }
        None
    }

    fn slot(dir: IVec2) -> usize {
        ((dir.y + 1) * 3 + (dir.x + 1)) as usize
    }

    fn may_step(&self, dir: IVec2) -> bool {
        self.pending.is_none()
            && self.blocked[Self::slot(dir)] <= 0.0
            && self.step_target(self.cell, dir).is_some()
    }

    /// The character moved, wherever the move came from, so nothing is stuck.
    fn moved(&mut self, tile: IVec3) {
        self.confirmed = Some(tile);
        self.strikes = 0;
        self.stalled = 0.0;
    }

    /// Calls a scripted walk off and says why, once, in a line the live test
    /// looks for. A walk driven by hand is left alone: a player leaning on a
    /// wall is not a fault.
    fn abandon(&mut self, why: &str) {
        if self.drive.is_empty() {
            return;
        }
        self.drive.clear();
        self.aborted = Some(why.to_string());
        warn!("walk: scripted walk stopped, {why}");
    }
}

/// The eight compass directions, as the suffix of `df.interface_key.A_MOVE_*`.
fn move_key(dir: IVec2) -> &'static str {
    match (dir.x, dir.y) {
        (0, -1) => "N",
        (0, 1) => "S",
        (1, 0) => "E",
        (-1, 0) => "W",
        (1, -1) => "NE",
        (-1, -1) => "NW",
        (1, 1) => "SE",
        _ => "SW",
    }
}

/// A scripted walk, for checking the mode with no hand on the keyboard.
///
/// `DWARF_EYE_WALK=1` starts in walk mode as soon as the character is found.
/// `DWARF_EYE_WALK_DRIVE=bearing,seconds;bearing,seconds;…` then holds the
/// character walking on each compass bearing in turn — degrees, 0 north,
/// 90 east — so a there-and-back leaves the character where it started. A leg
/// with no bearing, `,30`, just stands still for that long, which is how a
/// scripted walk waits for the map to paint before setting off.
fn scripted_drive() -> Vec<Leg> {
    let Ok(spec) = std::env::var("DWARF_EYE_WALK_DRIVE") else { return Vec::new() };
    spec.split(';')
        .filter_map(|leg| {
            let (bearing, seconds) = leg.split_once(',')?;
            let bearing = bearing.trim();
            let bearing = if bearing.is_empty() {
                None
            } else {
                // A compass bearing turns the opposite way to the camera's yaw,
                // which has north at zero and swings west as it grows.
                Some(-bearing.parse::<f32>().ok()?.to_radians())
            };
            Some(Leg { bearing, seconds: seconds.trim().parse().ok()? })
        })
        .collect()
}

/// One leg of a scripted walk: a heading to hold, or a pause.
struct Leg {
    bearing: Option<f32>,
    seconds: f32,
}

// --------------------------------------------------------------- the systems

/// Whether free flight should run this frame.
pub fn flying(walk: Res<WalkMode>) -> bool {
    !walk.active
}

pub fn walking(walk: Res<WalkMode>) -> bool {
    walk.active
}

/// Tab swaps between free flight and walking with the character.
pub fn toggle(keys: Res<ButtonInput<KeyCode>>, bridge: NonSend<Bridge>, mut walk: ResMut<WalkMode>) {
    let wanted = if !walk.started {
        walk.started = true;
        walk.drive = scripted_drive();
        std::env::var("DWARF_EYE_WALK").is_ok()
    } else if keys.just_pressed(KeyCode::Tab) {
        !walk.active
    } else {
        return;
    };

    walk.active = wanted;
    bridge.pilot.set_active(wanted);
    // Anything the walk thread said before now describes a tile the character
    // may have long since left.
    for _ in bridge.pilot.rx.try_iter() {}
    walk.fix = None;
    // Re-fix on every entry: the character may be anywhere by now.
    walk.confirmed = None;
    walk.pending = None;
    walk.blocked = [0.0; 9];
    walk.strikes = 0;
    walk.stalled = 0.0;
    walk.aborted = None;
    walk.glide = Vec2::ZERO;
    walk.line = if wanted { "walk    finding the character…".into() } else { String::new() };
}

/// Takes whatever position reports the walk thread has sent. Only the newest
/// matters: an older one has already been overtaken.
pub fn receive(bridge: NonSend<Bridge>, mut walk: ResMut<WalkMode>) {
    if walk.origin.is_none()
        && let Some(&(x, y, z)) = bridge.origin.get()
    {
        walk.origin = Some(IVec3::new(x, y, z));
    }
    for report in bridge.pilot.rx.try_iter() {
        let tile = IVec3::new(report.tile.0, report.tile.1, report.tile.2);
        walk.fix = Some((tile, report.ground));
    }
}

pub fn walk(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    bridge: NonSend<Bridge>,
    mut walk: ResMut<WalkMode>,
    camera: Option<Single<(&mut Transform, &mut FlyCamera)>>,
) {
    let Some(camera) = camera else { return };
    let (mut transform, mut fly) = camera.into_inner();
    let dt = time.delta_secs();

    // Ageing the run before the new report is read is what makes a gap end it:
    // a report that arrives in time resets the clock straight afterwards.
    walk.heading.tick(dt);
    reconcile(&mut walk);

    let (Some(confirmed), Some(origin)) = (walk.confirmed, walk.origin) else {
        walk.line = "walk    finding the character…".into();
        return;
    };

    for shut in &mut walk.blocked {
        *shut = (*shut - dt).max(0.0);
    }

    // A step the game never took is a refusal: return to the tile it left and
    // shut that edge, rather than leaning on it again next frame. The order
    // itself has expired by now, so it can no longer reach the game either.
    let expired = walk.pending.as_mut().is_some_and(|p| {
        p.age += dt;
        p.age > CONFIRM
    });
    if expired && let Some(p) = walk.pending.take() {
        let (from, dir) = (p.from, p.dir);
        walk.strikes += 1;
        walk.jump(|w| {
            w.cell = from;
            if dir != IVec2::ZERO {
                let back = w.offset + dir.as_vec2();
                w.offset = back.clamp(Vec2::splat(MARGIN), Vec2::splat(1.0 - MARGIN));
            }
            w.blocked[WalkMode::slot(dir)] = BLOCKED;
        });
        // A player can lean on a wall all day; a scripted walk is called off
        // rather than left bouncing. The count runs across directions, so
        // turning to face a different obstacle does not reset it.
        if walk.strikes >= STRIKES {
            let why = format!("{} steps in a row went unconfirmed", walk.strikes);
            walk.abandon(&why);
        }
    }

    // A scripted walk steers instead of the mouse and holds the forward key.
    let mut scripted = false;
    if let Some(leg) = walk.drive.first_mut() {
        leg.seconds -= dt;
        let bearing = leg.bearing;
        if leg.seconds <= 0.0 {
            walk.drive.remove(0);
        }
        if let Some(bearing) = bearing {
            fly.yaw = bearing;
            scripted = true;
        }
    }

    // While the game is driving a run of moves, the direction of travel owns
    // the yaw: the mouse can still nod, but it cannot turn. When the run stops
    // the yaw simply stays where the turn left it and the mouse has it back on
    // its next movement.
    let aim = if scripted { None } else { walk.heading.aim() };
    if buttons.pressed(MouseButton::Right) {
        if aim.is_none() {
            fly.yaw -= motion.delta.x * fly.sensitivity;
        }
        fly.pitch = (fly.pitch - motion.delta.y * fly.sensitivity)
            .clamp(-std::f32::consts::FRAC_PI_2 + 0.01, std::f32::consts::FRAC_PI_2 - 0.01);
    }
    if let Some(aim) = aim {
        fly.yaw = ease_angle(fly.yaw, aim, &mut walk.heading.turn, dt);
    }
    transform.rotation = Quat::from_euler(EulerRot::YXZ, fly.yaw, fly.pitch, 0.0);

    // Render x is east and render z is south, which is how the game lays out a
    // tile, so heading and cell offset share one plane.
    let forward = Vec2::new(transform.forward().x, transform.forward().z).normalize_or_zero();
    let right = Vec2::new(transform.right().x, transform.right().z).normalize_or_zero();
    let mut heading = Vec2::ZERO;
    for (key, axis) in [
        (KeyCode::KeyW, forward),
        (KeyCode::KeyS, -forward),
        (KeyCode::KeyD, right),
        (KeyCode::KeyA, -right),
    ] {
        if keys.pressed(key) {
            heading += axis;
        }
    }
    if scripted {
        heading += forward;
    }
    let mut want = walk.offset + heading.normalize_or_zero() * SPEED * dt;

    // Which edges the intended position has gone past, and so which way the
    // character is being asked to walk.
    let past = |v: f32| if v < 0.0 { -1 } else if v > 1.0 { 1 } else { 0 };
    let over = IVec2::new(past(want.x), past(want.y));
    if over != IVec2::ZERO {
        // A refused diagonal may still slide along one of its axes, the way a
        // corner lets you round it.
        let attempts = if over.x != 0 && over.y != 0 {
            vec![over, IVec2::new(over.x, 0), IVec2::new(0, over.y)]
        } else {
            vec![over]
        };
        let taken = attempts.into_iter().find(|&dir| walk.may_step(dir));

        if let Some(dir) = taken {
            let from = walk.cell;
            walk.cell.x += dir.x;
            walk.cell.y += dir.y;
            // The level is left alone: a ramp changes it, and the game says by
            // how much when the step lands.
            want -= dir.as_vec2();
            walk.pending = Some(Pending { from, dir, age: 0.0 });
            bridge.pilot.step(move_key(dir));
        }
        // Whatever was not stepped is held inside the cell, a margin short of
        // the edge so the camera never stands in the wall.
        let held = taken.unwrap_or(IVec2::ZERO);
        if over.x != 0 && held.x == 0 {
            want.x = want.x.clamp(MARGIN, 1.0 - MARGIN);
        }
        if over.y != 0 && held.y == 0 {
            want.y = want.y.clamp(MARGIN, 1.0 - MARGIN);
        }
        // Pushing at an edge with no step going out at all: the ground we can
        // see says there is nothing to walk onto, so the game is never asked
        // and no strike is ever scored. A script has to notice that itself.
        if taken.is_none() && walk.pending.is_none() {
            walk.stalled += dt;
            if walk.stalled > STALL {
                walk.abandon("the way ahead is not walkable");
            }
        }
    } else {
        walk.stalled = 0.0;
    }
    walk.offset = want.clamp(Vec2::ZERO, Vec2::ONE);

    // Ramps the game handles on its own; a staircase has to be asked. Ground
    // that has not arrived yet is given the benefit of the doubt.
    let on_stairs = !matches!(walk.solid(walk.cell), Some(other) if other != Solid::Stair);
    if walk.pending.is_none() && on_stairs {
        let vertical = if keys.pressed(KeyCode::KeyE) {
            Some("UP")
        } else if keys.pressed(KeyCode::KeyQ) {
            Some("DOWN")
        } else {
            None
        };
        if let Some(key) = vertical {
            walk.pending = Some(Pending { from: walk.cell, dir: IVec2::ZERO, age: 0.0 });
            bridge.pilot.step(key);
        }
    }

    // Every jump the camera did not walk decays away over a few frames.
    walk.glide *= (-dt / GLIDE).exp();
    if walk.glide.length() < 0.001 {
        walk.glide = Vec2::ZERO;
    }
    let ground = walk.anchor() + walk.glide;
    let surface = walk.surface(ground.x, ground.y);
    // The near plane reaches a little past the eye, so the ground immediately
    // around it has to clear too — going down a slope, the tile behind the eye
    // is still the high one. A rise of most of a level is a wall standing
    // beside us rather than ground underfoot, and is left out.
    let mut underfoot = surface;
    for probe in [Vec2::X, Vec2::NEG_X, Vec2::Y, Vec2::NEG_Y] {
        let near = ground + probe * NEAR;
        let height = walk.surface(near.x, near.y);
        if height > underfoot && height - surface < A_WALL {
            underfoot = height;
        }
    }
    let eye = (surface + EYE).max(underfoot + CLEARANCE);
    transform.translation = Vec3::new(ground.x, eye, ground.y);

    let shut = walk.blocked.iter().filter(|&&b| b > 0.0).count();
    let state = match &walk.aborted {
        Some(why) => why.clone(),
        None => match (walk.strikes, walk.pending.is_some(), shut) {
            (s, _, _) if s >= STRIKES => "stuck",
            (_, true, _) => "stepping",
            (_, _, n) if n > 0 => "blocked",
            _ => "walking",
        }
        .to_string(),
    };
    let tile = confirmed - origin;
    let facing = if aim.is_some() { "df" } else { "free" };
    // The eye's height against the ground it is riding, which is what says
    // whether a slope is being followed or cut through.
    walk.line = format!(
        "walk    character tile ({}, {}, {})   eye {eye:.2} over ground {surface:.2}   \
         heading: {facing}   {state}",
        tile.x, tile.y, tile.z
    );
}

/// Settles the camera against what the game says, once a position report is in.
fn reconcile(walk: &mut WalkMode) {
    let Some((tile, ground)) = walk.fix.take() else { return };
    let pending = walk.pending.as_ref().map(|p| (p.from, p.dir));

    match classify(walk.confirmed, pending, tile) {
        // The first fix after entering the mode drops the camera into the tile.
        Fix::First => {
            walk.ground = Some(ground);
            walk.cell = tile;
            walk.offset = Vec2::splat(0.5);
            walk.glide = Vec2::ZERO;
            walk.moved(tile);
        }
        // Nothing has landed yet; keep waiting for it.
        Fix::Waiting => {
            walk.ground = Some(ground);
        }
        // Our step, confirmed. The camera is already in the cell; only the
        // level is news, and it comes from a ramp or a staircase. The player is
        // steering, so any run of the game's own is over.
        Fix::Ordered => {
            walk.pending = None;
            walk.heading.ordered();
            walk.moved(tile);
            walk.jump(|w| {
                w.ground = Some(ground);
                w.cell = tile;
            });
        }
        Fix::Still => {
            walk.pending = None;
            walk.ground = Some(ground);
        }
        Fix::Driven => {
            // The game moved the character somewhere of its own accord, so any
            // step of ours is moot and the camera just goes along, keeping the
            // player's place in the cell. A long jump — travel — is taken
            // outright rather than slid through. Where such moves come one
            // after another the camera also turns to face the way they lead.
            //
            // A scripted route, though, no longer means what it meant: it was
            // aimed from a tile the character has left, so it stops here.
            let from = walk.confirmed.unwrap_or(tile);
            walk.pending = None;
            walk.heading.drove(IVec2::new(tile.x - from.x, tile.y - from.y));
            walk.abandon("the character was moved from outside");
            walk.blocked = [0.0; 9];
            walk.moved(tile);
            walk.jump(|w| {
                w.ground = Some(ground);
                w.cell = tile;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render x runs east and render z runs south, so a compass bearing has to
    /// come out as the tile step the game would take. The first live run walked
    /// the wrong way, so this pins the convention down.
    #[test]
    fn bearings_reach_the_compass() {
        for (bearing, expect, key) in [
            (0.0, IVec2::new(0, -1), "N"),
            (90.0, IVec2::new(1, 0), "E"),
            (180.0, IVec2::new(0, 1), "S"),
            (270.0, IVec2::new(-1, 0), "W"),
        ] {
            let yaw = -f32::to_radians(bearing);
            let forward = Quat::from_euler(EulerRot::YXZ, yaw, 0.0, 0.0) * Vec3::NEG_Z;
            let step = IVec2::new(forward.x.round() as i32, forward.z.round() as i32);
            assert_eq!(step, expect, "bearing {bearing} walks the wrong way");
            assert_eq!(move_key(step), key);
        }
    }

    /// A ramp has to hand the eye off to the floors on either side of it
    /// without a step, or walking a slope clips through the ground.
    #[test]
    fn a_ramp_joins_the_floors_at_both_of_its_edges() {
        // A wall to the east lifts that side, so the slope runs up eastward.
        let slopes = ramp::slopes(ramp::E);
        let footing = |u: f32| FLOOR_HEIGHT + patch(slopes, u, 0.5) * Z_SCALE;
        assert_eq!(footing(0.0), FLOOR_HEIGHT, "the low edge left the floor beside it");
        assert_eq!(
            footing(1.0),
            Z_SCALE + FLOOR_HEIGHT,
            "the high edge fell short of the floor a level above"
        );
        let mut last = f32::NEG_INFINITY;
        for step in 0..=20 {
            let height = footing(step as f32 / 20.0);
            assert!(height >= last, "the slope stepped back down on the way up");
            last = height;
        }
    }

    /// A step the camera ordered has to be told apart from the game moving the
    /// character, because only the second kind turns the camera.
    #[test]
    fn a_step_we_ordered_is_not_the_game_driving() {
        let here = IVec3::new(10, 20, 3);
        let east = IVec2::new(1, 0);
        // No confirmed tile yet: the very first report just places the camera.
        assert_eq!(classify(None, None, here), Fix::First);
        // Waiting on a step that has not landed.
        assert_eq!(classify(Some(here), Some((here, east)), here), Fix::Waiting);
        // The step, taken — the level may change under it, off a ramp.
        let landed = IVec3::new(11, 20, 3);
        assert_eq!(classify(Some(here), Some((here, east)), landed), Fix::Ordered);
        assert_eq!(
            classify(Some(here), Some((here, east)), IVec3::new(11, 20, 4)),
            Fix::Ordered
        );
        // A staircase: the column stays, the level moves.
        assert_eq!(
            classify(Some(here), Some((here, IVec2::ZERO)), IVec3::new(10, 20, 4)),
            Fix::Ordered
        );
        // Nothing moved at all.
        assert_eq!(classify(Some(here), None, here), Fix::Still);
        // Somewhere we never asked for, with a step out and without one.
        assert_eq!(classify(Some(here), None, IVec3::new(10, 19, 3)), Fix::Driven);
        assert_eq!(
            classify(Some(here), Some((here, east)), IVec3::new(10, 19, 3)),
            Fix::Driven
        );
        // Travel, a long way off in one report.
        assert_eq!(classify(Some(here), None, IVec3::new(60, 90, 3)), Fix::Driven);
    }

    /// Turns the yaw a heading aims at back into the tile step it faces, so the
    /// tests can talk in compass directions.
    fn faces(yaw: f32) -> IVec2 {
        let forward = Quat::from_euler(EulerRot::YXZ, yaw, 0.0, 0.0) * Vec3::NEG_Z;
        IVec2::new(forward.x.round() as i32, forward.z.round() as i32)
    }

    /// One shove is not a direction of travel; two in quick succession are.
    #[test]
    fn a_run_needs_more_than_one_move_to_steer_by() {
        let mut heading = Heading::default();
        heading.drove(IVec2::new(1, 0));
        assert_eq!(heading.aim(), None, "a single move turned the camera");
        heading.tick(0.125);
        heading.drove(IVec2::new(1, 0));
        assert_eq!(faces(heading.aim().expect("two moves make a run")), IVec2::new(1, 0));
    }

    /// The heading follows a corner rather than averaging it away, and old
    /// moves fall out of memory entirely.
    #[test]
    fn the_heading_leans_on_the_newest_moves() {
        let mut heading = Heading::default();
        for step in [IVec2::new(0, -1), IVec2::new(0, -1), IVec2::new(1, 0), IVec2::new(1, 0)] {
            heading.tick(0.125);
            heading.drove(step);
        }
        // Two north then two east: an even average would face north-east, the
        // weighting has to have swung it past that toward east.
        let yaw = heading.aim().expect("a run of four moves");
        let forward = Quat::from_euler(EulerRot::YXZ, yaw, 0.0, 0.0) * Vec3::NEG_Z;
        assert!(forward.x > -forward.z, "the corner was averaged away: {forward:?}");
        // Two more east and the north is out of memory altogether.
        for _ in 0..2 {
            heading.tick(0.125);
            heading.drove(IVec2::new(1, 0));
        }
        assert_eq!(faces(heading.aim().unwrap()), IVec2::new(1, 0));
    }

    /// A gap ends the run, and the next move starts a fresh one.
    #[test]
    fn a_gap_ends_the_run() {
        let mut heading = Heading::default();
        heading.drove(IVec2::new(1, 0));
        heading.tick(0.125);
        heading.drove(IVec2::new(1, 0));
        assert!(heading.aim().is_some());
        // Aged past the window a frame at a time, as the system does.
        for _ in 0..((RUN_GAP / 0.016) as i32 + 2) {
            heading.tick(0.016);
        }
        assert_eq!(heading.aim(), None, "the run outlived its gap");
        // One move on its own does not bring it back.
        heading.drove(IVec2::new(0, 1));
        assert_eq!(heading.aim(), None);
        heading.tick(0.125);
        heading.drove(IVec2::new(0, 1));
        assert_eq!(faces(heading.aim().unwrap()), IVec2::new(0, 1), "the fresh run kept the old moves");
    }

    /// A ramp or a staircase changes the level without saying anything about
    /// the way the character faces, and must not break a run that crosses it.
    #[test]
    fn levels_do_not_break_the_run() {
        let mut heading = Heading::default();
        // East up a ramp, then straight up a staircase, then east again.
        for step in [IVec2::new(1, 0), IVec2::new(1, 0), IVec2::ZERO, IVec2::new(1, 0)] {
            heading.tick(0.125);
            heading.drove(step);
        }
        assert_eq!(faces(heading.aim().expect("the run survived the level change")), IVec2::new(1, 0));
        // The staircase held the run open past what would otherwise be a gap.
        let mut stairs = Heading::default();
        stairs.drove(IVec2::new(1, 0));
        stairs.tick(0.125);
        stairs.drove(IVec2::new(1, 0));
        for _ in 0..8 {
            stairs.tick(0.125);
            stairs.drove(IVec2::ZERO);
        }
        assert!(stairs.aim().is_some(), "climbing in place dropped the heading");
    }

    /// The player's own keys are the player's heading, so a confirmed step of
    /// ours ends whatever run the game had going.
    #[test]
    fn our_own_step_hands_the_heading_back() {
        let mut heading = Heading::default();
        heading.drove(IVec2::new(1, 0));
        heading.tick(0.125);
        heading.drove(IVec2::new(1, 0));
        assert!(heading.aim().is_some());
        heading.ordered();
        assert_eq!(heading.aim(), None);
    }

    /// The turn eases: most of the swing inside `TURN`, all of it shortly
    /// after, and never a step past the heading it is aiming at. The result
    /// must not depend on the frame rate either, so both rates are walked.
    #[test]
    fn the_turn_comes_round_without_overshooting() {
        for aim in [1.2f32, -1.2, 3.0, -3.0] {
            for dt in [1.0 / 60.0, 1.0 / 15.0] {
                let (mut yaw, mut speed) = (0.0f32, 0.0f32);
                let whole = wrap(0.0 - aim);
                let (mut near, mut done) = (0.0, 0.0);
                let frames = (1.0 / dt) as i32;
                for frame in 1..=frames {
                    yaw = ease_angle(yaw, aim, &mut speed, dt);
                    let left = wrap(yaw - aim);
                    assert!(
                        left * whole >= -1e-3,
                        "the turn overshot at frame {frame}: yaw {yaw}, aim {aim}"
                    );
                    let at = frame as f32 * dt;
                    if at <= TURN {
                        near = 1.0 - left.abs() / whole.abs();
                    }
                    done = 1.0 - left.abs() / whole.abs();
                }
                assert!(near > 0.45, "only {near:.2} of the way round after {TURN}s at dt {dt}");
                assert!(done > 0.97, "still {:.2} short after a second at dt {dt}", 1.0 - done);
            }
        }
    }

    /// A turn across the compass seam behind the camera goes the short way, not
    /// the long way round.
    #[test]
    fn the_turn_takes_the_short_way_round() {
        use std::f32::consts::PI;
        // Both a little to one side of due south, on opposite sides of the cut.
        let (mut yaw, mut speed) = (PI - 0.2, 0.0f32);
        let aim = -PI + 0.2;
        for _ in 0..60 {
            let before = yaw;
            yaw = ease_angle(yaw, aim, &mut speed, 1.0 / 60.0);
            assert!(yaw >= before, "the turn set off the long way round: {before} to {yaw}");
        }
        assert!(wrap(yaw - aim).abs() < 0.02, "the turn did not arrive: {yaw}");
        assert_eq!(faces(yaw), IVec2::new(0, 1), "it did not end up facing south");
    }

    #[test]
    fn the_ground_square_addresses_its_own_tiles() {
        let solids = vec![None; (3 * (2 * GROUND_RADIUS + 1) * (2 * GROUND_RADIUS + 1)) as usize];
        let mut ground =
            Ground { center: IVec3::new(100, 200, 30), solids, surface: None, level: 30 };
        let span = 2 * GROUND_RADIUS + 1;
        // One tile east and one level down, by hand.
        // Level z-1 is the first of the three, so its plane starts at zero.
        let index = GROUND_RADIUS * span + (GROUND_RADIUS + 1);
        ground.solids[index as usize] = Some(Solid::Ramp);
        assert_eq!(ground.get(IVec3::new(101, 200, 29)), Some(Solid::Ramp));
        assert_eq!(ground.get(IVec3::new(100, 200, 30)), None);
        // Outside the square, and outside the three levels.
        assert_eq!(ground.get(IVec3::new(200, 200, 30)), None);
        assert_eq!(ground.get(IVec3::new(101, 200, 27)), None);
    }
}
