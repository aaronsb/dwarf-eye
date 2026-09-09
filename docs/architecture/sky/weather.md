# Weather: the probe, precipitation, snow and haze

Status: landed (`crates/dwarf-eye-world/src/weather.rs`,
`crates/dwarf-eye/src/precipitation.rs`, `crates/dwarf-eye/src/god_rays.rs`,
`crates/dwarf-eye/src/god_rays.wgsl`, the `wet` and `snow` terms in
`cloud_shadow.wgsl`); the rest of the inventory in flight (issue #32),
haze calibration planned (issue #18).

## What it does

Reads every weather global Dwarf Fortress keeps, draws the rain and snow the
protocol never mentions, lays snow on the ground, wets it when it rains, and
turns the fog kind into a haze density for the light shafts.

## Where the data comes from

RemoteFortressReader sends five cloud bits per world tile and stops there. The
precipitation grid, the stratus countdown, the wind and the moon are all in DF's
own memory, so they come back the way the clock does: one line of DFHack Lua
through `RunCommand`, echoed into `Client::last_notices` and parsed out of it.

`weather.rs:PROBE` prints one tagged line of integers:

```
dwarfeye-weather <25 grid cells> <cell_x> <cell_y>
  <cumulus> <stratus> <cirrus> <fog> <front> <countdown>
  <wind_x> <wind_y> <air_x> <air_y> <snowfall> <temperature>
  <moon_phase> <moon_angle> <weathertimer>
```

| Field | Global | Meaning |
|---|---|---|
| 25 grid cells | `df.global.current_weather[x][y]` | 0 none, 1 rain, 2 snow, row-major by y |
| cell | `world.map.x_count`, the adventurer's position | which of the 25 the character stands in |
| cumulus, stratus, cirrus, fog, front | `world_data.region_map[x][y].clouds` | the same five kinds the plugin copies, raw |
| countdown | the same bitfield | the four-bit stratus counter the plugin **drops** |
| wind | `.wind` (`region_daily_winds`) | -2..2 per axis, positive east and south |
| air | `.air_x`, `.air_y` | the air-mass velocity on that tile |
| snowfall | `.snowfall` | 0 to 5000, the field `RegionTile.snow` is filled from |
| temperature | `.temperature` | the region tile's own scale |
| moon_phase, moon_angle | `world_data` | day of the lunar month, and DF's moon angle |
| weathertimer | `df.global.weathertimer` | ticks until the weather is rerolled |

`weather.rs:parse` turns that into a `Reading`, and `Reading` does the
arithmetic: `at_character` is the grid cell the character stands in,
`intensity` the fraction of the 25 that is wet, `snow_cover` the snowfall on the
coarse band's scale, `moon` DF's phase as a fraction, `stratus_countdown` the
counter as a fraction, and `cumulus_cover` and its siblings the kinds on the
same four-step scales the world-map path uses.

`worker.rs`, `Command::Weather`, runs the probe and sends a `WeatherReport`. If
the Lua path fails — no script interpreter, no world data — it falls back to
`GetWorldMap` and `worker.rs:read_weather`, which averages the cloud kinds over
the 3x3 of world tiles around the embark and leaves precipitation, snow and the
moon at their defaults. The viewer's own derivations then stand in.

`crates/dwarf-eye-world/examples/weather.rs` runs the same probe without the GPU
and prints the reading; `--raw` prints the line itself.

## What each one does on screen

**Precipitation** (`precipitation.rs`). One mesh of a few thousand quads,
rebuilt on the CPU each frame: rain is a streak drawn along its own fall line,
snow a camera-facing flake at a tenth of the speed with more drift. Seeds are
fixed per particle and the volume wraps around the camera per axis, so the box
travels with the view without anything sliding through it; it is pushed half a
radius along the view so most of it falls where the camera is looking. Density
is how many quads are not collapsed to a point, `intensity * fade`; the fade is
three seconds each way, and a change of kind fades the old one out before the
new one starts. The kind at the character's cell decides what falls. Nothing
falls under a roof: `worker.rs:open_sky` looks 24 levels up the column above the
last fetch centre in the already-loaded map.

**Snow cover** (`SnowCover`, and the `snow` term in `cloud_shadow.wgsl`).
DF's snowfall whitens upward faces — the ground and the tops of crowns, never
the side of a wall — and roughens them, crossing to a new level over a minute.
The coarse horizon band shades its own snow out of `RegionTile.snow`
(`horizon::shade`), so `precipitation.rs:drive_surfaces` skips every material
with `horizon > 0.5`; whitening those too would count it twice and draw a line
at the seam. Both read a cell as fully snowed at snowfall 100, which is what
keeps the seam invisible.

**The wet look** (the `wet` and `polish` terms in the same shader). While rain
falls the albedo darkens, more on what looks up than on what faces sideways, and
the roughness drops, which is the whole of why a wet surface shines.

**Haze** (`god_rays.rs:drive`). The fog kind now arrives as DF's own rather than
a 3x3 average — 0.25 mist, 0.55 fog, 0.85 thick — with the stratus countdown and
the falling rain alongside it:

```
low_sun  = 1 - min(elevation * 4, 1)
density  = 0.006 + 0.014*low_sun + 0.033*fog + 0.005*stratus + 0.002*cumulus
                 + 0.004*countdown + 0.010*falling
falloff  = 1 / (60 - 44*fog)
dim      = 0.15 + 0.55*fog
strength = (0.45 + 0.35*low_sun) * clamp(sun.y * 6, 0, 1)
```

`god_rays.rs:god_rays` is a fullscreen pass added straight into the `Core3d`
schedule after the main pass and before early post-processing, so bloom picks
the shafts up and tonemapping brings them into range. Bind group 0 is Bevy's own
mesh view layout, which is what gives the pass the lights, the shadow cascades
and the atmosphere; group 1 carries the uniform, scene depth, and the same cloud
shadow map the ground is shaded with, read out of the terrain material.
`god_rays.wgsl` samples the cascades for sun visibility, applies the cloud
transmittance with the same slant lookup as `cloud_shadow.wgsl`, and dithers the
first step per frame.

**The moon** (`sky.rs:moon_phase`). DF's `moon_phase` replaces the viewer's
28-day derivation when the probe reads it, and the derivation stays as the
fallback. See [clock.md](clock.md).

## Knobs

| Knob | |
|---|---|
| `DWARF_EYE_WEATHER` | `clear`, `rain`, `snow`: forces both the sky and what falls, ignoring the game's grid and any roof |
| `DWARF_EYE_SNOW` | snow on the ground, 0..1; `DWARF_EYE_WEATHER=snow` lays 0.85 of its own |
| `DWARF_EYE_CLOUDS` | the cloud kinds by name, `cumulus=0.8,cirrus=0.4` |
| `DWARF_EYE_GODRAYS` | density, g, strength, falloff, height, dim, distance, steps; the bare token `off` starts them disabled |
| `G` | toggles the shafts |
| `1` `2` `3` | drive **DF's own** weather through `RunCommand` (`main.rs:handle_input`) |

## Invariants and gotchas

- `region_map` is a pointer to an array of pointers and DFHack's Lua
  dereferences the inner one on the first index, so the second axis needs
  `_displace(y)`, not a second `[]`. `world.map.region_x` counts mid-level
  tiles, sixteen to a world tile.
- Enum fields come back as numbers in this build and as names in others, so the
  probe maps a name back through its own enum table before printing.
- Everything past `current_weather` is read under `pcall`. A save with no world
  data still yields the grid, and the fields it could not read print as zero,
  with `moon_phase` as -1 so the viewer knows to keep its own.
- `DWARF_EYE_WEATHER` beats both the game's grid and the ceiling check, so a
  forced shot shows the weather it asked for wherever it is framed. It is the
  override for screenshots; the `1` `2` `3` keys change the player's game.
- A forced sky is seeded at startup rather than waiting on the first poll, which
  is a dozen seconds and a map pass behind.
- The weather poll shares the worker thread with map fetches, so the first
  reading lands well after the window does. Issue #4.
- `precipitation.rs` writes the whole position buffer every frame and the index
  buffer is built once, so the buffer stays `TOTAL` quads long whatever is
  falling; the surplus collapse to a point and draw nothing.
- Fog is not part of the cloud field at all. It reaches the haze and the surface
  terms, so a foggy sky still draws no extra cloud.
- The approved look is fog 0.4 to 0.8 at a low sun. Issue #18 carries the rest:
  region rainfall, a rain history, temperature, season, hour and nearby water, a
  forced-haze key, and calibration against the approved shots at 07:00.

## Claims to verify

The `moon_angle` field is read and carried but nothing consumes it: its units
are unverified, and a wrong guess would move the moon rather than leave it
where the phase puts it.

The rain streak width and alpha were reduced after the shot that verified the
fall, which showed them too wide and too bright. The snow shot is of the
current values; the rain has not been shot again since the change.

The repo [README](../../../README.md) says light shafts need no deferred
pipeline because a `VolumetricLight` on the sun and a `VolumetricFog` on the
camera are enough. The code is right and the README is stale: neither component
is used anywhere, and `god_rays.rs` is a pass of its own, written because Bevy's
volumetric fog shader knows nothing about the cloud shadow map.

## Related issues

#32 (the weather inventory: fronts, flows, evil rain, per-tile spatter snow,
wind into the cloud drift), #18 (haze inputs, haze key, calibration), #16 (water
and magma transparency would interact with the same medium), #14 (lightning
would need the night light rig).
