//! Screenshots, and a way to aim the camera from the environment.
//!
//! `DWARF_EYE_VIEW=yaw,pitch` (degrees) sets where the camera looks once the
//! map arrives. `DWARF_EYE_SHOT=path[:seconds]` saves a screenshot after the
//! delay and exits, for checking a build without sitting at the window. F12
//! saves one at any time.

use crate::camera::FlyCamera;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};

/// The camera aim requested from the environment, applied once.
pub fn requested_view() -> Option<(f32, f32)> {
    let spec = std::env::var("DWARF_EYE_VIEW").ok()?;
    let (yaw, pitch) = spec.split_once(',')?;
    Some((yaw.trim().parse::<f32>().ok()?.to_radians(), pitch.trim().parse::<f32>().ok()?.to_radians()))
}

pub fn apply_view(fly: &mut FlyCamera) {
    if let Some((yaw, pitch)) = requested_view() {
        fly.yaw = yaw;
        fly.pitch = pitch;
    }
}

#[derive(Resource)]
struct Scheduled {
    path: String,
    remaining: f32,
    taken: bool,
}

fn schedule() -> Option<Scheduled> {
    let spec = std::env::var("DWARF_EYE_SHOT").ok()?;
    let (path, delay) = match spec.rsplit_once(':') {
        Some((p, d)) if d.parse::<f32>().is_ok() => (p.to_string(), d.parse().unwrap_or(8.0)),
        _ => (spec, 8.0),
    };
    Some(Scheduled { path, remaining: delay, taken: false })
}

fn run_schedule(
    time: Res<Time>,
    mut commands: Commands,
    mut scheduled: ResMut<Scheduled>,
    mut exit: MessageWriter<AppExit>,
) {
    scheduled.remaining -= time.delta_secs();
    if scheduled.remaining > 0.0 {
        return;
    }
    if !scheduled.taken {
        scheduled.taken = true;
        info!("saving screenshot to {}", scheduled.path);
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(scheduled.path.clone()));
        // Give the capture a moment to land on disk.
        scheduled.remaining = 1.5;
        return;
    }
    exit.write(AppExit::Success);
}

fn hotkey(keys: Res<ButtonInput<KeyCode>>, mut commands: Commands) {
    if keys.just_pressed(KeyCode::F12) {
        let path = format!(
            "dwarf-eye-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        info!("saving screenshot to {path}");
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
}

pub struct CapturePlugin;

impl Plugin for CapturePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, hotkey);
        if let Some(scheduled) = schedule() {
            app.insert_resource(scheduled).add_systems(Update, run_schedule);
        }
    }
}
