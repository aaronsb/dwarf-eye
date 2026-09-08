//! Walk sync: a first-person camera that shares a tile with the adventurer.
//!
//! Free flight can go anywhere, which is exactly what the game cannot follow.
//! Walk mode gives the camera back to the character: it stands in the
//! character's tile at eye height, moves freely inside that one cell, and the
//! moment it crosses a cell edge the game is asked to take a step that way.
//!
//! Movement is optimistic. The camera does not wait at the edge; it walks on
//! into the next cell while the request is in flight, and the position poll
//! settles up afterwards. A confirmed step needs no correction. A refused one
//! springs the camera back and shuts that edge for a moment. The camera is
//! never allowed more than one cell ahead of what the game has confirmed, so a
//! second edge waits for the first step to land.
//!
//! The sync runs both ways. When the character moves without being asked —
//! the player clicking a distant tile in Dwarf Fortress, a series of automatic
//! steps, travel, being shoved — the poll sees a tile we did not ask for and
//! the camera simply follows it, keeping where the player stood inside the
//! cell and where they were looking.

use crate::camera::FlyCamera;
use crate::worker::{Bridge, Command};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use dwarf_eye_world::mesh::Z_SCALE;
use dwarf_eye_world::{Solid, World};

/// Tiles per second inside a cell. Close to the pace the game confirms a step
/// at, so a held key flows rather than stuttering against the cell edge.
const SPEED: f32 = 3.2;

/// Eye height above the tile's standing surface, in levels.
const EYE: f32 = 0.8;

/// How high a ramp or a staircase carries its footing inside its own cell.
const RAISED: f32 = 0.5 * Z_SCALE;

/// How close to a shut edge the camera may press.
const MARGIN: f32 = 0.12;

/// How long a step may stay unconfirmed before it is treated as refused.
const CONFIRM: f32 = 0.6;

/// How long a refused edge stays shut, so a wall is not hammered every frame.
const BLOCKED: f32 = 0.5;

/// Time constant of the glide that absorbs every jump: a step onto a ramp, a
/// spring back, the game moving the character a tile of its own accord.
/// Roughly a 0.15 s settle.
const GLIDE: f32 = 0.05;

/// A jump further than this is not eased but taken outright — travel, or a
/// teleport, where a glide would only be a long slide through the ground.
const SNAP: f32 = 5.0;

/// How far around the character walkability is shipped to the render thread.
/// The camera is never more than one cell from the confirmed tile, so this is
/// generous.
pub const GROUND_RADIUS: i32 = 4;

/// The shape of the ground near the character, so the camera can judge a step
/// and find its own eye height without a round trip to the game.
#[derive(Clone)]
pub struct Ground {
    /// Render tile the square is centred on.
    center: (i32, i32, i32),
    /// Solids for `z-1..=z+1` over a `(2r+1)` square, level-major then row-major.
    /// `None` where that chunk is not loaded.
    solids: Vec<Option<Solid>>,
}

impl Ground {
    /// Reads the square out of the voxel world. Cheap: a few hundred lookups.
    pub fn sample(world: &World, center: (i32, i32, i32)) -> Self {
        let radius = GROUND_RADIUS;
        let span = 2 * radius + 1;
        let mut solids = Vec::with_capacity((3 * span * span) as usize);
        for dz in -1..=1 {
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    solids.push(
                        world
                            .voxel(center.0 + dx, center.1 + dy, center.2 + dz)
                            .map(|v| v.solid),
                    );
                }
            }
        }
        Self { center, solids }
    }

    fn get(&self, tile: IVec3) -> Option<Solid> {
        let (dx, dy, dz) = (
            tile.x - self.center.0,
            tile.y - self.center.1,
            tile.z - self.center.2,
        );
        let r = GROUND_RADIUS;
        if dx.abs() > r || dy.abs() > r || dz.abs() > 1 {
            return None;
        }
        let span = 2 * r + 1;
        let i = (dz + 1) * span * span + (dy + r) * span + (dx + r);
        *self.solids.get(i as usize)?
    }
}

/// Whether a tile offers footing. Shrubs and saplings do: Dwarf Fortress lets
/// the character walk through them, and refusing would fence the player in.
fn stands_on(solid: Solid) -> bool {
    matches!(solid, Solid::Floor | Solid::Ramp | Solid::Stair | Solid::Foliage)
}

fn obstructs(solid: Solid) -> bool {
    matches!(solid, Solid::Cube | Solid::Fortification)
}

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
    /// Where the game last said the character stands, in render tiles.
    confirmed: Option<IVec3>,
    /// The cell the camera occupies: the confirmed tile, or one step ahead.
    cell: IVec3,
    /// Where inside that cell the camera stands, east and south, 0 to 1.
    offset: Vec2,
    pending: Option<Pending>,
    /// Directions that just refused a step, and how long they stay shut,
    /// indexed by `(dy + 1) * 3 + (dx + 1)`.
    blocked: [f32; 9],
    /// The gap between where the camera is drawn and where it stands, decayed
    /// away so every jump reads as a glide.
    glide: Vec3,
    ground: Option<Ground>,
    /// A position report waiting to be reconciled.
    fix: Option<(IVec3, Ground)>,
    /// Set while a poll is in flight, so requests never pile up behind a slow
    /// collection pass.
    awaiting: bool,
    since_poll: f32,
    /// What the HUD says about the mode.
    pub line: String,
}

impl WalkMode {
    fn solid(&self, tile: IVec3) -> Option<Solid> {
        self.ground.as_ref().and_then(|g| g.get(tile))
    }

    /// How high the footing in a cell stands inside it.
    fn footing(&self, cell: IVec3) -> f32 {
        match self.solid(cell) {
            Some(Solid::Ramp | Solid::Stair) => RAISED,
            _ => 0.0,
        }
    }

    /// Where the camera logically stands, before the glide is added.
    fn anchor(&self) -> Vec3 {
        Vec3::new(
            self.cell.x as f32 + self.offset.x,
            self.cell.z as f32 * Z_SCALE + self.footing(self.cell) + EYE,
            self.cell.y as f32 + self.offset.y,
        )
    }

    /// Applies a change that moves the camera without the player walking, and
    /// turns the discontinuity into a glide. A long jump is taken outright.
    fn jump(&mut self, change: impl FnOnce(&mut Self)) {
        let before = self.anchor();
        change(self);
        self.glide += before - self.anchor();
        if self.glide.length() > SNAP {
            self.glide = Vec3::ZERO;
        }
    }

    /// The level a step in `dir` would land on, or `None` where it cannot be
    /// taken. Unknown ground never refuses: terrain that has not streamed in
    /// yet is not a wall.
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

    fn shut(&self, dir: IVec2) -> bool {
        self.blocked[((dir.y + 1) * 3 + (dir.x + 1)) as usize] > 0.0
    }

    fn may_step(&self, dir: IVec2) -> bool {
        self.pending.is_none() && !self.shut(dir) && self.step_target(self.cell, dir).is_some()
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

/// Whether free flight should run this frame.
pub fn flying(walk: Res<WalkMode>) -> bool {
    !walk.active
}

pub fn walking(walk: Res<WalkMode>) -> bool {
    walk.active
}

/// Tab swaps between free flight and walking with the character.
pub fn toggle(keys: Res<ButtonInput<KeyCode>>, mut walk: ResMut<WalkMode>) {
    if !keys.just_pressed(KeyCode::Tab) {
        return;
    }
    walk.active = !walk.active;
    // Re-fix on every entry: the character may be anywhere by now.
    walk.confirmed = None;
    walk.pending = None;
    walk.blocked = [0.0; 9];
    walk.glide = Vec3::ZERO;
    walk.since_poll = 0.0;
    walk.line = if walk.active {
        "walk    finding the character…".into()
    } else {
        String::new()
    };
}

/// Takes whatever position reports the worker has sent. Only the newest
/// matters: an older one has already been overtaken.
pub fn receive(bridge: NonSend<Bridge>, mut walk: ResMut<WalkMode>) {
    for report in bridge.reports.try_iter() {
        walk.awaiting = false;
        walk.fix = Some((
            IVec3::new(report.tile.0, report.tile.1, report.tile.2),
            report.ground,
        ));
    }
}

/// Asks where the character stands, eight times a second while walking, and
/// never with a request already outstanding.
pub fn poll(time: Res<Time>, bridge: NonSend<Bridge>, mut walk: ResMut<WalkMode>) {
    if !walk.active {
        walk.awaiting = false;
        return;
    }
    walk.since_poll += time.delta_secs();
    // A dropped reply would otherwise wedge the poll for good.
    if walk.awaiting && walk.since_poll < 1.0 {
        return;
    }
    if walk.since_poll < 0.125 {
        return;
    }
    walk.since_poll = 0.0;
    walk.awaiting = true;
    let _ = bridge.tx.send(Command::Adventurer);
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

    reconcile(&mut walk);

    let Some(confirmed) = walk.confirmed else {
        walk.line = "walk    finding the character…".into();
        return;
    };

    for shut in &mut walk.blocked {
        *shut = (*shut - dt).max(0.0);
    }

    // A step that never arrives is a refusal: return to the tile it left and
    // shut that edge for a moment.
    let expired = walk.pending.as_mut().is_some_and(|p| {
        p.age += dt;
        p.age > CONFIRM
    });
    if expired && let Some(p) = walk.pending.take() {
        let (from, dir) = (p.from, p.dir);
        walk.jump(|w| {
            w.cell = from;
            if dir != IVec2::ZERO {
                let back = w.offset + dir.as_vec2();
                w.offset = back.clamp(Vec2::splat(MARGIN), Vec2::splat(1.0 - MARGIN));
            }
            w.blocked[((dir.y + 1) * 3 + (dir.x + 1)) as usize] = BLOCKED;
        });
    }

    if buttons.pressed(MouseButton::Right) {
        fly.yaw -= motion.delta.x * fly.sensitivity;
        fly.pitch = (fly.pitch - motion.delta.y * fly.sensitivity)
            .clamp(-std::f32::consts::FRAC_PI_2 + 0.01, std::f32::consts::FRAC_PI_2 - 0.01);
    }
    transform.rotation = Quat::from_euler(EulerRot::YXZ, fly.yaw, fly.pitch, 0.0);

    // Render x is east and render z is south, which is how the game lays out a
    // tile, so heading and cell offset share one plane.
    let forward = transform.forward();
    let right = transform.right();
    let mut heading = Vec2::ZERO;
    for (key, axis) in [
        (KeyCode::KeyW, Vec2::new(forward.x, forward.z)),
        (KeyCode::KeyS, -Vec2::new(forward.x, forward.z)),
        (KeyCode::KeyD, Vec2::new(right.x, right.z)),
        (KeyCode::KeyA, -Vec2::new(right.x, right.z)),
    ] {
        if keys.pressed(key) {
            heading += axis.normalize_or_zero();
        }
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
            let _ = bridge.tx.send(Command::Step { key: move_key(dir).into() });
        }
        // Whatever was not stepped is held inside the cell, a margin short of
        // the edge so the camera never sits in the wall.
        let held = taken.unwrap_or(IVec2::ZERO);
        if over.x != 0 && held.x == 0 {
            want.x = want.x.clamp(MARGIN, 1.0 - MARGIN);
        }
        if over.y != 0 && held.y == 0 {
            want.y = want.y.clamp(MARGIN, 1.0 - MARGIN);
        }
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
            let _ = bridge.tx.send(Command::Step { key: key.into() });
        }
    }

    // Every jump the camera did not walk decays away over a few frames.
    walk.glide *= (-dt / GLIDE).exp();
    if walk.glide.length() < 0.001 {
        walk.glide = Vec3::ZERO;
    }
    transform.translation = walk.anchor() + walk.glide;

    let state = match (&walk.pending, walk.blocked.iter().any(|&b| b > 0.0)) {
        (Some(_), _) => "stepping",
        (None, true) => "blocked",
        _ => "walking",
    };
    walk.line = format!(
        "walk    character tile ({}, {}, {})   {state}",
        confirmed.x, confirmed.y, confirmed.z
    );
}

/// Settles the camera against what the game says, once a position report is in.
fn reconcile(walk: &mut WalkMode) {
    let Some((tile, ground)) = walk.fix.take() else { return };

    // The first fix after entering the mode drops the camera into the tile.
    if walk.confirmed.is_none() {
        walk.ground = Some(ground);
        walk.cell = tile;
        walk.offset = Vec2::splat(0.5);
        walk.confirmed = Some(tile);
        walk.glide = Vec3::ZERO;
        return;
    }

    match walk.pending.take() {
        Some(pending) if tile == pending.from => {
            // Nothing has landed yet; keep waiting.
            walk.ground = Some(ground);
            walk.pending = Some(pending);
        }
        Some(pending)
            if pending.dir != IVec2::ZERO
                && tile.x == pending.from.x + pending.dir.x
                && tile.y == pending.from.y + pending.dir.y =>
        {
            // Our step, confirmed. The camera is already in the cell; only the
            // level is news, and it comes from a ramp.
            walk.confirmed = Some(tile);
            walk.jump(|w| {
                w.ground = Some(ground);
                w.cell = tile;
            });
        }
        Some(pending)
            if pending.dir == IVec2::ZERO && tile.x == pending.from.x && tile.y == pending.from.y =>
        {
            // A staircase, confirmed.
            walk.confirmed = Some(tile);
            walk.jump(|w| {
                w.ground = Some(ground);
                w.cell = tile;
            });
        }
        _ if walk.confirmed == Some(tile) => {
            walk.ground = Some(ground);
        }
        _ => {
            // The game moved the character somewhere of its own accord, so any
            // step of ours is moot and the camera just goes along, keeping the
            // player's place in the cell and their heading. A long jump —
            // travel — is taken outright rather than slid through.
            walk.blocked = [0.0; 9];
            walk.confirmed = Some(tile);
            walk.jump(|w| {
                w.ground = Some(ground);
                w.cell = tile;
            });
        }
    }
}
