# What DFHack knows beyond the loaded map

Findings from reading DFHack's `remotefortressreader.cpp`, `Maps.cpp`, the
df-structures XML, and Armok Vision's distant-terrain scripts, on 2026-09-08.

## Full detail outside the window: not obtainable

`GetBlockList` resolves each block through `Maps::getBlock`, which indexes
`world.map.block_index`, an array allocated to exactly the loaded map.
`world.map.map_blocks` and `map_block_columns` hold only those blocks. DF
discards the local map on offload (`adventurest.offload_timer`,
`long_action_duration`) and regenerates it from `world_data.midmap_data.
region_details` plus site realizations. No RPC, Lua call or memory structure
carries tiletype and material for a tile outside the window.

The window size is whatever DF allocated: adventure mode gets 3x3 mid-level
tiles, 144x144. No init setting changes it. `region_midmap_datast` carries
`loadarea_sx/sy/ex/ey`, but block storage is allocated when the map is built,
so writing them would not enlarge the map. Unretiring a fortress is the only
route to a large loaded map.

The generator inputs are present (`world_region_details`: `elevation[17][17]`,
`biome[17][17]`, `seed[16][16]`, edge biomes, rivers, features), but the
generator is closed and exposed by no RPC, so offline regeneration is out.

## Units and orders

- `MapInfo.block_pos_x/y` are 48-tile mid-level tiles; `block_pos_z` is a
  z-level. Local z = elevation - block_pos_z.
- `RegionMap.tiles` is `y * 17 + x` (`CopyLocalMap` loops `yy` outer, `xx`
  inner). The 17th row and column overlap the next world tile.
- `WorldMap.elevation` and `RegionTile.elevation` share one z-level scale:
  0-99 ocean, 100-149 normal biomes, 150+ mountains. 99 is the sea-level
  sentinel that `water_elevation` reports. The world value is the worldgen
  input DF refines into the 17x17 grid, not a mean of it, so it sits a few
  levels off the region tiles.
- `SiteRealizationBuilding` coordinates are tiles relative to the containing
  mid-level tile; fields are `id`, `min_x/y`, `max_x/y`, `material`,
  `wall_info`, `tower_info`, `trench_info`, `type`.

## How Armok Vision draws the distance

`WorldMapMaker.cs` meshes the whole world at one quad per world tile from
`GetWorldMapNew`, with skirt quads down to lower neighbours, and skips world
tiles that have a region map. `RegionMaker.cs` meshes each region 17x17 at
48-tile pitch. Placement is arithmetic relative to the loaded map origin. The
region under the live window is not cut away; the fort draws over it.

dwarf-eye does the same two tiers, and additionally masks the coarse mesh per
block wherever fine chunks reach the ground (`shadow.rs`, `cloud_shadow*.wgsl`).

## Clock in adventure mode

`cur_year_tick` is a fortress-mode counter. Adventure mode leaves it behind and
keeps `cur_season_tick`, which counts one unit per 10 fortress ticks and wraps
at 10080 each season. `autofarm.cpp` carries a harvest date across seasons with
that constant:

```cpp
int harvest = (*df::global::cur_season_tick) + plant->growdur * 10;
while (can_plant && harvest >= 10080) { season = (season + 1) % 4; harvest -= 10080; }
```

So the year tick DF is really running on is

    cur_season * 100800 + cur_season_tick * 10

with 1200 ticks a day, 28 days a month, 12 months a year (403200 a year, 100800
a season). `World.cpp` and `scripts/position.lua` carry the day and month
divisors.

`cur_year_tick_advmode` (`precise_phase` in `df.game_v.xml`) is not a usable
time of day. Timestream advances it 144 per fortress tick:

```cpp
*cur_year_tick += timeskip;
*cur_year_tick_advmode += timeskip * 144;
```

but no divisor of a live reading lands on the hour the game shows, and its
value survived a write to `cur_year_tick` that the game's own clock did not.
`position.lua` reads the hour out of it as `advmode // 336`, which does not
agree with the 144 relation either. Treat it as a phase to carry along, not to
read from.

Writing `cur_year_tick` alone moves the renderer and nothing else, because DF
reads the season pair. `df.global.world.status.reports` records the discovery:
report timestamps ran 17250 to 17368, then dropped to 353, 100, 50, 4 as the
viewer's `.`/`,` keys wrote the fortress counter. The season pair kept the real
time throughout. Setting time now writes `cur_year_tick`, `cur_season`,
`cur_season_tick` together and moves `cur_year_tick_advmode` by the same delta
timestream would.

Resolution: the season counter steps 10 fortress ticks, so the derived time of
day is quantised to 12 game minutes. DF exposes nothing finer that stays
correct.
