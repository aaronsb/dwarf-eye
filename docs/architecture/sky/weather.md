# Weather, haze and god rays

Status: landed (`crates/dwarf-eye/src/god_rays.rs`,
`crates/dwarf-eye/src/god_rays.wgsl`); follow-ups planned (issue #18).

## What it does

Turns DF's reported weather into a haze density, and marches light shafts
through it from the sun.

## How

`main.rs:poll_weather` asks for `GetWorldMap` every twelve seconds and
`worker.rs:read_weather` reduces it to four fractions. `god_rays.rs:drive` reads
those and the sun's elevation into the `GodRays` resource:

```
low_sun  = 1 - min(elevation * 4, 1)
density  = 0.006 + 0.014*low_sun + 0.033*fog + 0.005*stratus + 0.002*cumulus
falloff  = 1 / (60 - 44*fog)
dim      = 0.15 + 0.55*fog
strength = (0.45 + 0.35*low_sun) * clamp(sun.y * 6, 0, 1)
```

`god_rays.rs:god_rays` is a fullscreen pass added straight into the `Core3d`
schedule after the main pass and before early post-processing, so bloom picks
the shafts up and tonemapping brings them into range. Bind group 0 is Bevy's own
mesh view layout, which is what gives the pass the lights, the shadow cascades
and the atmosphere; group 1 carries the uniform, scene depth, and the same
cloud shadow map the ground is shaded with, read out of the terrain material.
`god_rays.wgsl` samples the cascades for sun visibility, applies the cloud
transmittance with the same slant lookup as `cloud_shadow.wgsl`, and dithers the
first step per frame.

`G` toggles the shafts. `DWARF_EYE_GODRAYS` overrides any of density, g,
strength, falloff, height, dim, distance and steps, and the bare token `off`
starts them disabled.

`1`, `2` and `3` drive DF's own weather through `RunCommand`
(`main.rs:handle_input`).

## Invariants and gotchas

- `god_rays.rs:prepare_depth_usage` adds `TEXTURE_BINDING` to the camera's depth
  usage before the core 3D depth textures are prepared. Without it the pass
  cannot bind depth and the march never stops at the scene.
- MSAA changes the depth binding type, so the pipeline keeps two layouts and
  picks by `Msaa`.
- The shafts silently do nothing while the cloud map is not yet resident, or
  while the sun is at or below the horizon.
- `sun_visibility` returns lit outside the shadow cascades, so distant ground
  does not read as a black disc.
- Fog is not part of the cloud field at all. It reaches only the haze, so a
  foggy sky draws no extra cloud.
- The approved look is fog 0.4 to 0.8 at a low sun. Issue #18 carries the
  remaining work: derive density from DF's own fog kind, region rainfall, recent
  rain, temperature, season, hour and nearby water, add a forced-haze key, and
  calibrate against the approved shots at 07:00.

## Claims to verify

The repo [README](../../../README.md) says light shafts need no deferred
pipeline because a `VolumetricLight` on the sun and a `VolumetricFog` on the
camera are enough. The code is right and the README is stale: neither component
is used anywhere, and `god_rays.rs` is a pass of its own, written because Bevy's
volumetric fog shader knows nothing about the cloud shadow map.

## Related issues

#18 (haze inputs, haze key, calibration), #16 (water and magma transparency
would interact with the same medium).
