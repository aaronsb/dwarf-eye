# Clouds and their shadows

Status: landed (`crates/dwarf-eye/src/clouds.rs`, `crates/dwarf-eye/src/noise.rs`,
`crates/dwarf-eye/src/shadow.rs`, `crates/dwarf-eye/src/cloud_volume.wgsl`,
`crates/dwarf-eye/src/cloud_shadow.wgsl`).

## What it does

Draws the sky DF reports and shadows the ground with the same sky.

## How

DF gives a cloud kind per world tile rather than a coverage number.
`worker.rs:read_weather` averages the 3x3 of world tiles around the embark and
turns each kind into a fraction, giving `clouds.rs:Weather`: cumulus, stratus,
cirrus, fog.

`noise.rs:weather_sheet` packs that into a 256x256 sheet: red is coverage, green
is the kind mix from stratus to cumulus, blue is cirrus. Two 3D volumes carry
the shape, `noise.rs:base_noise` at 128 cubed in the Perlin-Worley layout and
`noise.rs:detail_noise` at 32 cubed. Everything tiles at integer periods.

The visible sky is one cuboid slab 13200 tiles wide raymarched by
`cloud_volume.wgsl`, spawned by `clouds.rs:setup` and moved by `clouds.rs:drive`.
The vertical profile is where the kinds differ: stratus is a thin sheet near the
base, cumulus is tall with a flat bottom and a tapering crown, cirrus is a single
analytic sheet at `CIRRUS_HEIGHT` 290 streaked along the wind.

Shadows are baked on the CPU. `clouds.rs:bake_shadow` spawns a thread that
marches `SAMPLES` 24 steps toward the sun per texel over a 1024x1024 map
covering one `WEATHER_PERIOD` 2048 of ground, then hands it to every
`TerrainMaterial` asset. `cloud_shadow.wgsl:transmittance` does the slant lookup
per fragment and multiplies the result into the shaded colour.

| Knob | |
|---|---|
| `DWARF_EYE_CLOUDS` | force a sky, `cumulus=0.8,cirrus=0.4` |
| `DWARF_EYE_WIND` | wind in tiles per second, `x,z` |
| `DWARF_EYE_CLOUD_TUNE` | sigma, detail, gain, ambient, haze, cirrus, shadow, steps |

## Invariants and gotchas

- Clouds are geometry, not volumetric fog. Bevy's `volumetric_fog.wgsl`
  attenuates its own ambient by Beer's law, so a thick cloud gets less fill
  light and interiors render black.
- The cloud material is fragment only. A custom vertex shader breaks the depth
  prepass with `Location[7] ... is not provided by the previous stage outputs`.
- Material bindings live in bind group **3** in this version of Bevy, not 2.
  Group 2 is the mesh. Hardcoding 2 leaves the bindings out of the pipeline
  layout and the shader fails validation with no other clue. The shaders use
  `@group(#{MATERIAL_BIND_GROUP})`.
- Every texture in `shadow.rs:CloudShadow` is bound always, never `None`. An
  unbound handle drops its binding from the layout with the identical failure,
  which is why `main.rs:setup` binds `flat_shadow(255)` and `empty_mask()`.
- The shadow march wraps horizontally rather than clipping at the deck's bounds.
  Clipping draws a straight box edge across the ground.
- The deck holds a fixed altitude above the terrain: it follows the camera
  horizontally and hangs off `clouds.rs:GroundLevel`, set once at
  `Event::Connected`. Following the camera vertically puts the viewer inside it
  and everything greys out.
- `clouds.rs:Field::density` and the `density` function in `cloud_volume.wgsl`
  are the same function twice. Divergence desynchronises the drawn sky from the
  baked ground shadow with no error.
- `shadow.rs:ShadowUniform` is duplicated field for field in both WGSL files.
- The camera needs `DepthPrepass` or the volume march never stops at terrain.
- Rebaking is gated: nothing below `sun.y` 0.02, nothing while a bake is in
  flight, and a moved sun needs more than 1.5 degrees and a second.

## Related issues

#18 (fog is not in the cloud field; it only reaches the god rays), #14 (night
lighting).
