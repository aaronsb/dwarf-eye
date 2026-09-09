//! A free-flying camera driven by WASD, QE and right-drag look.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;

/// How long the travel vector takes to swing onto a new direction. Long enough
/// that a tap of a key, or a sidestep around a tree, does not turn the preload
/// round; short enough that a real change of course is followed inside a pass.
const TRAVEL_EASE: f32 = 0.6;

/// How much of a full-speed run the eased vector must hold before it counts as
/// travel. Below this the camera is drifting or stopping, and there is no
/// direction worth fetching ahead on.
const TRAVELLING: f32 = 0.35;

#[derive(Component)]
pub struct FlyCamera {
    pub speed: f32,
    pub sensitivity: f32,
    pub yaw: f32,
    pub pitch: f32,
    /// Where the camera is going on the ground plan — render x east, render z
    /// south — eased over `TRAVEL_EASE`. Zero while it stands still.
    pub travel: Vec2,
}

impl Default for FlyCamera {
    fn default() -> Self {
        Self { speed: 24.0, sensitivity: 0.0025, yaw: 0.0, pitch: -0.6, travel: Vec2::ZERO }
    }
}

impl FlyCamera {
    /// The direction of travel as a unit vector, or `None` while the camera is
    /// not really going anywhere.
    pub fn travelling(&self) -> Option<Vec2> {
        (self.travel.length() >= TRAVELLING).then(|| self.travel.normalize())
    }
}

pub fn fly(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    camera: Option<Single<(&mut Transform, &mut FlyCamera)>>,
) {
    let Some(camera) = camera else { return };
    let (mut transform, mut fly) = camera.into_inner();

    if buttons.pressed(MouseButton::Right) {
        fly.yaw -= motion.delta.x * fly.sensitivity;
        fly.pitch = (fly.pitch - motion.delta.y * fly.sensitivity)
            .clamp(-std::f32::consts::FRAC_PI_2 + 0.01, std::f32::consts::FRAC_PI_2 - 0.01);
    }
    transform.rotation = Quat::from_euler(EulerRot::YXZ, fly.yaw, fly.pitch, 0.0);

    if scroll.delta.y != 0.0 {
        fly.speed = (fly.speed * (1.0 + scroll.delta.y * 0.12)).clamp(2.0, 400.0);
    }

    let mut direction = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        direction += *transform.forward();
    }
    if keys.pressed(KeyCode::KeyS) {
        direction += *transform.back();
    }
    if keys.pressed(KeyCode::KeyA) {
        direction += *transform.left();
    }
    if keys.pressed(KeyCode::KeyD) {
        direction += *transform.right();
    }
    if keys.pressed(KeyCode::KeyE) {
        direction += Vec3::Y;
    }
    if keys.pressed(KeyCode::KeyQ) {
        direction -= Vec3::Y;
    }

    let dt = time.delta_secs();
    if direction != Vec3::ZERO {
        let boost = if keys.pressed(KeyCode::ShiftLeft) { 4.0 } else { 1.0 };
        transform.translation += direction.normalize() * fly.speed * boost * dt;
    }

    // The ground plan of where the keys are pushing, eased. Standing still eases
    // it back to zero, which is how the preload stops leaning.
    let flat = Vec2::new(direction.x, direction.z).normalize_or_zero();
    let caught = 1.0 - (-dt / TRAVEL_EASE).exp();
    fly.travel = fly.travel.lerp(flat, caught);
}
