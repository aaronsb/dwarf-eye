# Weather: the probe, precipitation, snow and haze

Status: landed (`crates/dwarf-eye-world/src/weather.rs`,
`crates/dwarf-eye/src/precipitation.rs`, `crates/dwarf-eye/src/god_rays.rs`,
`crates/dwarf-eye/src/god_rays.wgsl`, the `wet` and `snow` terms in
`cloud_shadow.wgsl`); the rest of the inventory in flight (issue #32), the
haze model landed and calibrated with its two region inputs still to be
carried by the worker (issue #18).

## What it does

Reads every weather global Dwarf Fortress keeps, draws the rain and snow the
protocol never mentions, lays snow on the ground, wets it when it rains, and
turns what the air is holding into a haze density for the light shafts.

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

**Haze** (`god_rays.rs:Humidity::density`). Shaft visibility is moisture and
aerosol along the ray, so the density is a humidity model rather than a cloud
reading. One function holds all of it, and it splits in two: the sky, which is
what DF's cloud bitfield says, and the damp, which is the water the region, the
last shower and the map underfoot are holding.

```
low_sun = 1 - min(elevation * 4, 1)
recent  = exp(-minutes_since_rain / 90)          (0 if it has not rained)
damp    = 0.45*rainfall + 0.30*recent + 0.25*water
warmth  = 1 - 0.65*temperature
peak    = 1 + 0.6*twilight(hour)                 (gaussians at 06:00 and 18:00,
                                                  widths 2 and 2.5 hours)
density = 0.002 + 0.014*low_sun
        + 0.033*fog + 0.005*stratus + 0.002*cumulus
        + 0.004*countdown + 0.010*falling
        + 0.019*damp*warmth*peak

falloff  = 1 / (60 - 44*fog)
dim      = 0.15 + 0.55*fog
strength = (0.45 + 0.35*low_sun) * max(clamp(sun.y * 6, 0, 1), 0.5*moon_light)
```

The temperature and the hour scale the damp rather than adding to it, which is
what keeps a dry noon faint whatever the hour term is doing: a hot afternoon
burns the damp off and a dawn brings it back, but neither invents any. Every
input is separately monotone — wetter, cloudier, colder or darker air holds more
— and the 0.002 floor is what a clear, dry, high sun is left with.

`fog` is DF's own kind rather than a 3x3 average — 0.25 mist, 0.55 fog, 0.85
thick — and the stratus countdown rides with it, so the sheet's own build-up
thickens the air ahead of the change instead of the haze stepping when the kind
flips.

**Calibration.** The approved shots are `godrays_1.png` (cumulus 50%, fog 40%)
and `godrays_fog.png` (fog 80%), both 15 Granite of 100 at 07:00. At that hour
and sky the model gives 0.0211 against the 0.0207 the shots' own constants
(27f5af3) put there: two and a third per cent, inside the tenth
`god_rays.rs:a_foggy_dawn_lands_where_the_approved_shots_did` holds it to. The
same embark at noon, dry and warm, reads 0.003 — faint, where the old formula's
flat 0.006 base was a permanent wash. Shoot it with `DWARF_EYE_HOUR=7` and
`DWARF_EYE_CLOUDS`, never the game clock.

**Where the inputs come from.** `fog`, `stratus`, `cumulus` and `countdown` are
the probe's, through `Weather`; `falling` is `Precipitation::drawn`;
`minutes_since_rain` is `weather.rs:RainHistory`, one field aged a frame at a
time off what is actually being drawn; `water` is measured by
`god_rays.rs:measure_water`, the share of loaded chunks that spawned a water
surface, which is proximity rather than a tile count. `rainfall` and
`temperature` have no route yet: the probe reads the temperature and
`RegionTile.rainfall` sits beside it, but `worker.rs:WeatherReport` carries
neither, so both stand at a temperate embark's own 0.5 and
`DWARF_EYE_HAZE=rainfall=0.9,temp=0.2` sets them by hand. Two fields on the
report and two lines in `worker.rs` would finish it (issue #18).

**The moon.** Once the sun is down the shafts follow the moon at half the sun's
strength curve, which after exposure is about a twentieth of a daylight shaft:
the shader already marches whichever directional light is the brightest, and at
night that is the moon. The moon carries no shadow cascades, so a moonlit night
gets the medium's own glow toward the moon rather than shafts cut by the trees.

**The key.** `F` cycles the haze: the derived level, then 0, 0.2, 0.5 and 0.8
forced, as fractions of `HAZE_FULL` (0.05, a thick fog at a low sun). The HUD
reads `haze: derived 0.021` or `haze: forced 0.5`. `F` beats
`DWARF_EYE_GODRAYS=density=`, which beats the model, so a shot that pins the
density still gets what it asked for and the key is still a hand on the dial.

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
| `DWARF_EYE_HAZE` | the humidity inputs the worker does not carry: `rainfall=0.9,temp=0.2,water=0.1` |
| `G` | toggles the shafts |
| `F` | cycles the haze: derived, then 0, 0.2, 0.5, 0.8 forced |
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
- The approved look is fog 0.4 to 0.8 at a low sun, and the humidity model is
  calibrated to land on it at 07:00. What is left of issue #18 is the plumbing:
  `RegionTile.rainfall` and the region temperature stop at `WeatherReport`.
- `RainHistory` counts wall seconds, not game minutes: DF's own minute runs at
  whatever speed the player has the world at, and the damp in the air is a thing
  the eye is watching rather than a thing the world is counting. It also starts
  empty, so a viewer that has just opened reads as never having seen rain, which
  is the dry end of the term rather than the wet one.
- `measure_water` counts water *chunks*, not water tiles: one chunk is sixteen
  tiles square, so a stream through the middle of the view reads a few per cent
  and a coast reads half. It runs twice a second, because the number moves at
  the speed the map is fetched.

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
wind into the cloud drift), #18 (region rainfall and temperature onto
`WeatherReport`), #16 (water
and magma transparency would interact with the same medium), #14 (lightning
would need the night light rig).
