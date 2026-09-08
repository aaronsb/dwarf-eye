# Sun, sky and stars

Status: landed (`crates/dwarf-eye/src/sky.rs`, `crates/dwarf-eye/src/stars.rs`,
`crates/dwarf-eye/src/main.rs`); night lighting in flight (issue #14).

## What it does

Puts one directional light where DF's calendar says the sun is, colours the sky
around it, and hangs a star field that turns with the same clock.

## How

`sky.rs:Clock::sun_direction` turns the day fraction into an angle and the day
of the year into a declination of 0.41 radians amplitude, Earth's tilt. Dawn is
due east, noon overhead, dusk due west, and winter light comes in low.
`sky.rs:drive_sun` places the light at that direction times 4000 looking at the
origin, so the rays read parallel.

`main.rs:setup` spawns the rest: `Atmosphere::earth` with a raymarched
`AtmosphereMode`, a `SunDisk::EARTH` of 32 arcminutes,
`AtmosphereEnvironmentMapLight` at intensity 2.6, `Exposure` ev100 13.0,
`Tonemapping::AcesFitted`, `DebandDither` and `Bloom::NATURAL`.

`stars.rs:build_star_mesh` makes one mesh of 1800 emissive quads on a sphere of
radius 20000, each billboarded toward the centre, brightness and warmth baked into
vertex colour, from an LCG seeded `0x5EED5741` so the sky is the same every run.
`stars.rs:drive` spins the field by the day fraction about an axis tilted
`AXIS_TILT` 0.62 radians, recentres it on the camera every frame, and fades it
with `(-sun.y * 6).clamp(0, 1)`.

## Invariants and gotchas

- Raymarching the atmosphere removes the seams the lookup textures leave and
  sharpens volumetric shadows.
- The star sphere is recentred on the camera every frame, so its radius is a
  parallax choice rather than a range limit. The camera's far plane is 40000.
- The environment light is raised above the physical default because there is no
  bounce lighting to fill the shadows.
- Night: a second directional light without shadow cascades lags the sun by the
  moon's phase from DF's 28-day month (`sky.rs:moon_phase`), 350 lux at full; a
  cool ambient floor weighted by `Clock::night` and held against the exposure;
  exposure metered off whichever light is up (day ev100 13, full-moon night
  8.2, no moon 7.6), eased with a 0.7 s time constant. The sun's illuminance
  fades over 3 degrees either side of the horizon. `DWARF_EYE_HOUR=hh[:mm]`
  pins the hour for lighting and disables the clock keys.
- Shaders that need "the sun" scan for the brightest directional light; Bevy
  sorts a shadowless moon to index 0.
- Emissive materials are still a follow-up (issue #14).
- Star fade completes once the sun is a sixth of a radian up, which is why the
  field is gone well before full daylight.

## Claims to verify

The repo [README](../../../README.md) says stars sit at 620 units because the
camera's default far plane is 1000. The code is right and the README is stale:
`main.rs:setup` sets `far: 40000.0`, and the field is recentred on the camera
each frame, so nothing clips it.

## Related issues

#14 (moon light, starlight ambient floor, emissive materials with falloff), #18
(the sun's elevation also drives haze).
