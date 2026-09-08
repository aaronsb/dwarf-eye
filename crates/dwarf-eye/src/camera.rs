//! A free-flying camera driven by WASD, QE and right-drag look.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;

#[derive(Component)]
pub struct FlyCamera {
    pub speed: f32,
    pub sensitivity: f32,
    pub yaw: f32,
    pub pitch: f32,
}

impl Default for FlyCamera {
    fn default() -> Self {
        Self { speed: 24.0, sensitivity: 0.0025, yaw: 0.0, pitch: -0.6 }
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

    if direction != Vec3::ZERO {
        let boost = if keys.pressed(KeyCode::ShiftLeft) { 4.0 } else { 1.0 };
        transform.translation += direction.normalize() * fly.speed * boost * time.delta_secs();
    }
}
