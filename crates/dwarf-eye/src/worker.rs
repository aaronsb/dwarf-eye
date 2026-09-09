//! Runs the DFHack connection on its own thread so the render loop never blocks
//! on a socket.
//!
//! The worker owns the [`World`], because face culling needs neighbouring chunks
//! and it is cheaper to mesh next to the data than to ship the data across.

use crate::clouds::Weather;
use crate::walk::Pilot;
use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::library::TileLibrary;
use dwarf_eye_world::canopy::{BANDS, CanopyMeshes, Forest};
use dwarf_eye_world::clock;
use dwarf_eye_world::weather::{self, Precip};
use dwarf_eye_world::{BLOCK, BlockBounds, MeshData, MeshOptions, Session, build_chunk};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

/// Chunk key: block x, block y, z-level.
pub type ChunkKey = (i32, i32, i32);

pub enum Command {
    /// Pull blocks around a tile position and remesh what they touch.
    /// Collect what the live window holds. `center` is where the camera is, in
    /// render tiles and level, for retiring far chunks.
    Fetch { center: (i32, i32, i32), opts: MeshOptions, force: bool },
    /// Remesh what is already loaded, without going back to DFHack.
    Remesh { opts: MeshOptions },
    /// Read the game's calendar.
    Clock,
    /// Run a DFHack console command, for driving the world while testing.
    Run { command: String, args: Vec<String> },
    /// Read the world's cloud cover.
    Weather,
    Shutdown,
}

/// Everything one weather poll learned.
///
/// The Lua probe fills all of it; the world-map fallback fills the sky and
/// leaves the rest at its default, which is a dry, snowless, moonless report the
/// viewer's own derivations stand in for.
pub struct WeatherReport {
    pub sky: Weather,
    /// What is falling where the character stands.
    pub precip: Precip,
    /// The fraction of the 5x5 grid that is wet.
    pub intensity: f32,
    /// Snow lying on the ground, 0..1.
    pub snow: f32,
    /// DF's own moon phase, where the probe could read it.
    pub moon: Option<f32>,
    /// False when the loaded map has something solid over the camera.
    pub outdoors: bool,
}

impl Default for WeatherReport {
    fn default() -> Self {
        Self {
            sky: Weather::default(),
            precip: Precip::None,
            intensity: 0.0,
            snow: 0.0,
            moon: None,
            outdoors: true,
        }
    }
}

pub enum Event {
    /// Cloud cover over the embark, and what is falling out of it.
    Weather(WeatherReport),
    /// Dwarf Fortress's calendar, polled while the world runs.
    Clock { year: i32, tick: i32 },
    /// The packed ground texture, sent once before any geometry.
    Atlas { width: u32, height: u32, pixels: Vec<u8> },
    Connected { world_name: String, save: String, center: (i32, i32, i32), size: (i32, i32, i32) },
    /// Geometry for chunks that changed: terrain, then one set of crown meshes
    /// per detail band, nearest first, as `canopy::BANDS` orders them. Crowns
    /// ride their own materials, so they arrive as their own meshes. Nothing
    /// but empty meshes means "despawn this one".
    Chunks(Vec<(ChunkKey, MeshData, Vec<CanopyMeshes>)>),
    /// Coarse terrain beyond the loaded map, sent once the map's surface is known.
    Horizon(dwarf_eye_world::horizon::Horizon),
    /// Blocks whose fine chunks reach the ground, where the horizon must yield.
    Coverage(Vec<(i32, i32)>),
    Status(String),
    Failed(String),
}

pub struct Bridge {
    pub tx: Sender<Command>,
    pub rx: Receiver<Event>,
    /// The render origin, once the map thread has connected. Walk mode reads
    /// absolute tiles off its own connection and places them against this.
    pub origin: Arc<OnceLock<(i32, i32, i32)>>,
    /// Walk mode's own connection, which must never wait behind a map pass.
    pub pilot: Pilot,
    /// The unit census, on a third connection. Creatures move several times a
    /// second and a poll queued behind a slab of blocks is a poll wasted.
    pub units: crate::units::UnitFeed,
}

impl Bridge {
    /// Spawns the worker thread and returns the channel pair to talk to it.
    pub fn spawn() -> Self {
        let (tx, command_rx) = channel();
        let (event_tx, rx) = channel();
        let origin: Arc<OnceLock<(i32, i32, i32)>> = Arc::default();
        let theirs = Arc::clone(&origin);
        thread::Builder::new()
            .name("dfhack".into())
            .spawn(move || {
                if let Err(err) = run(command_rx, &event_tx, &theirs) {
                    let _ = event_tx.send(Event::Failed(format!("{err:#}")));
                }
            })
            .expect("spawning the DFHack worker thread");
        Self { tx, rx, origin, pilot: Pilot::spawn(), units: crate::units::UnitFeed::spawn() }
    }
}

fn run(
    commands: Receiver<Command>,
    events: &Sender<Event>,
    origin: &OnceLock<(i32, i32, i32)>,
) -> Result<()> {
    let started = std::time::Instant::now();
    let mut df = Session::connect_local()?;
    // Walk mode places its own position reads against this.
    let _ = origin.set(df.origin());
    bevy::log::info!("startup: connected and raws fetched in {:.1}s", started.elapsed().as_secs_f32());
    // The window we last asked for. DFHack answers with only the blocks it
    // thinks changed, so any chunk we prune has to be re-requested outright or
    // it never comes back.
    let mut last_window: Option<(i32, i32, i32, i32, i32, i32, (i32, i32, i32))> = None;

    // Trees are grown once each and kept, so a chunk never regrows one.
    let mut forest = Forest::default();

    // Dwarf Fortress's own sprites, indexed by tiletype and species.
    let tiletypes: rfr::TiletypeList = df.client.call_empty(methods::GET_TILETYPE_LIST)?;
    let plants: rfr::PlantRawList = df.client.call_empty(methods::GET_PLANT_RAWS)?;
    let mut library = match TileLibrary::load(&tiletypes, &plants) {
        Ok(lib) => {
            let atlas = lib.atlas();
            events.send(Event::Atlas {
                width: atlas.width,
                height: atlas.height,
                pixels: atlas.pixels.clone(),
            })?;
            events.send(Event::Status(format!(
                "{} ground sprites packed, models at {1}x{1} sub-voxels",
                atlas.capacity_used(),
                lib.grid_size()
            )))?;
            Some(lib)
        }
        Err(err) => {
            events.send(Event::Status(format!("no DF sprites ({err:#}); drawing plain blocks")))?;
            None
        }
    };

    let center = df.view_center()?;
    events.send(Event::Connected {
        world_name: df.map_info.world_name_english().to_string(),
        save: df.map_info.save_name().to_string(),
        center,
        size: (
            df.map_info.block_size_x() * dwarf_eye_world::BLOCK,
            df.map_info.block_size_y() * dwarf_eye_world::BLOCK,
            df.map_info.block_size_z(),
        ),
    })?;

    bevy::log::info!("startup: sprite library ready at {:.1}s", started.elapsed().as_secs_f32());

    // Land from earlier sessions comes back before the first fetch.
    let (cached_files, cached_bytes) = df.cache_size();
    let restored = df.restore_cache();
    bevy::log::info!(
        "{} chunks restored from {} (at {:.1}s); cache held {cached_files} chunks, {} MB, \
         {} column floors known",
        restored.len(),
        df.cache_dir().map(|p| p.display().to_string()).unwrap_or_default(),
        started.elapsed().as_secs_f32(),
        cached_bytes / 1_000_000,
        df.floor_count(),
    );
    if !restored.is_empty() {
        events.send(Event::Coverage(grounded_blocks(&df.world)))?;
        events.send(Event::Status(format!(
            "{} chunks restored from {}",
            restored.len(),
            df.cache_dir().map(|p| p.display().to_string()).unwrap_or_default()
        )))?;
        let cost = remesh_all(
            &df,
            library.as_mut(),
            &mut forest,
            MeshOptions { z_ceiling: i32::MAX, show_hidden: true },
            events,
        )?;
        bevy::log::info!(
            "startup: restored chunks meshed at {:.1}s",
            started.elapsed().as_secs_f32()
        );
        log_depths("restored", &depth_histogram(&df.world, &cost));
    } else {
        bevy::log::info!(
            "startup: restored chunks meshed at {:.1}s",
            started.elapsed().as_secs_f32()
        );
    }
    bevy::log::info!("startup: {}", dwarf_eye_world::canopy::timing::report());
    let mut first_pass = true;

    let mut horizon_sent = false;
    // Where the camera last asked for map, which is where the weather poll looks
    // up for a roof.
    let mut last_center: Option<(i32, i32, i32)> = None;
    loop {
        let command = match commands.try_recv() {
            Ok(c) => c,
            Err(TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(8));
                continue;
            }
            Err(TryRecvError::Disconnected) => return Ok(()),
        };

        match command {
            Command::Shutdown => return Ok(()),
            Command::Remesh { opts } => {
                remesh_all(&df, library.as_mut(), &mut forest, opts, events)?;
            }
            Command::Run { command, args } => {
                let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
                match df.client.run_command(&command, &borrowed) {
                    Ok(()) => events.send(Event::Status(format!("ran `{command}`")))?,
                    Err(e) => events.send(Event::Status(format!("`{command}` failed: {e}")))?,
                }
            }
            Command::Weather => {
                // The protocol carries five cloud bits and nothing else, so the
                // precipitation grid, the stratus countdown and the moon come
                // back through Lua the way the clock does.
                let probed = df.client.run_command("lua", &[weather::PROBE]).is_ok();
                let reading = probed
                    .then(|| weather::parse(&df.client.last_notices.concat()))
                    .flatten();
                match reading {
                    Some(r) => {
                        let outdoors = last_center.is_none_or(|c| open_sky(&df.world, c));
                        bevy::log::info!(
                            "weather: {}{}",
                            r.describe(),
                            if outdoors { "" } else { ", under a ceiling" }
                        );
                        events.send(Event::Weather(WeatherReport {
                            sky: Weather {
                                cumulus: r.cumulus_cover(),
                                stratus: r.stratus_cover(),
                                cirrus: r.cirrus_cover(),
                                fog: r.fog_cover(),
                                countdown: r.stratus_countdown(),
                            },
                            precip: r.at_character(),
                            intensity: r.intensity(),
                            snow: r.snow_cover(),
                            moon: r.moon(),
                            outdoors,
                        }))?;
                    }
                    // No script interpreter, or no world data: the world map
                    // still carries the cloud kinds.
                    None => {
                        if let Ok(map) =
                            df.client.call_empty::<rfr::WorldMap>(methods::GET_WORLD_MAP)
                        {
                            events.send(Event::Weather(WeatherReport {
                                sky: read_weather(&map),
                                ..Default::default()
                            }))?;
                        }
                    }
                }
            }
            Command::Clock => {
                // Adventure mode abandons cur_year_tick, so read every clock
                // global and let the reading pick the one its mode keeps.
                let probed = df.client.run_command("lua", &[clock::PROBE]).is_ok();
                let reading = probed
                    .then(|| clock::parse(&df.client.last_notices.concat()))
                    .flatten();
                match reading {
                    Some(r) => events.send(Event::Clock { year: r.year, tick: r.year_tick() })?,
                    // The Lua path needs a script interpreter; the map center
                    // is always there.
                    None => {
                        if let Ok(map) =
                            df.client.call_empty::<rfr::WorldMap>(methods::GET_WORLD_MAP_CENTER)
                        {
                            events.send(Event::Clock {
                                year: map.cur_year(),
                                tick: map.cur_year_tick(),
                            })?;
                        }
                    }
                }
            }
            Command::Fetch { center, opts, force } => {
                last_center = Some(center);
                let pass_started = std::time::Instant::now();
                // Travel mode and loading screens leave no map behind, and
                // DFHack answers with a link failure. Wait it out.
                let cost = match collect(
                    &mut df, center, opts, force, &mut last_window, &mut horizon_sent,
                    library.as_mut(), &mut forest, events,
                ) {
                    Ok(cost) => cost,
                    Err(err) => {
                        events.send(Event::Status(format!("waiting for the map: {err:#}")))?;
                        Default::default()
                    }
                };
                if first_pass {
                    first_pass = false;
                    bevy::log::info!(
                        "startup: first window pass took {:.1}s, done at {:.1}s",
                        pass_started.elapsed().as_secs_f32(),
                        started.elapsed().as_secs_f32()
                    );
                    bevy::log::info!(
                        "startup: pass asked for {} blocks of a {}-block box, {} came back; \
                         {} column floors stood at the start of it, {} after, sitting {} to {} \
                         levels under the character",
                        df.last_pass.asked,
                        df.last_pass.whole_box,
                        df.last_pass.arrived,
                        df.last_pass.floors,
                        df.floor_count(),
                        df.floor_depths().0,
                        df.floor_depths().1,
                    );
                    bevy::log::info!(
                        "startup: {}",
                        dwarf_eye_world::canopy::timing::report()
                    );
                    log_depths("loaded", &depth_histogram(&df.world, &cost));
                }
            }
        }
    }
}

/// How far up the column above the camera is searched for a roof.
const CEILING_SEARCH: i32 = 24;

/// Whether the sky is open over a tile, so precipitation can reach it.
///
/// The loaded map is already here, so this is a lookup rather than a request. A
/// column that has not been fetched reads as open: rain outside is the commoner
/// mistake, and it corrects itself the moment the blocks land.
fn open_sky(world: &dwarf_eye_world::World, center: (i32, i32, i32)) -> bool {
    let (x, y, z) = center;
    !(1..=CEILING_SEARCH)
        .any(|up| world.voxel(x, y, z + up).is_some_and(|v| !v.solid.is_empty()))
}

/// Reads cloud cover over the embark out of the world map.
///
/// DF reports a cloud kind per world tile on four-step scales, so each becomes a
/// coverage fraction. The neighbourhood is averaged because a single world tile
/// flips between states more abruptly than a sky should.
fn read_weather(map: &rfr::WorldMap) -> Weather {
    use dfhack_remote::rfr::{CumulusType, FogType, StratusType};

    let width = map.world_width.max(1);
    let height = map.world_height.max(1);
    let (cx, cy) = (map.map_x(), map.map_y());

    let mut totals = [0.0f32; 4];
    let mut samples = 0.0f32;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x >= width || y >= height {
                continue;
            }
            let Some(cloud) = map.clouds.get((y * width + x) as usize) else { continue };
            // The same scales `weather::Reading` puts the Lua kinds on, so the
            // two paths draw the same sky.
            use weather::{CIRRUS_COVER, CUMULUS_COVER, FOG_COVER, STRATUS_COVER};
            totals[0] += match cloud.cumulus() {
                CumulusType::CumulusNone => CUMULUS_COVER[0],
                CumulusType::CumulusMedium => CUMULUS_COVER[1],
                CumulusType::CumulusMulti => CUMULUS_COVER[2],
                CumulusType::CumulusNimbus => CUMULUS_COVER[3],
            };
            totals[1] += match cloud.stratus() {
                StratusType::StratusNone => STRATUS_COVER[0],
                StratusType::StratusAlto => STRATUS_COVER[1],
                StratusType::StratusProper => STRATUS_COVER[2],
                StratusType::StratusNimbus => STRATUS_COVER[3],
            };
            totals[2] += if cloud.cirrus() { CIRRUS_COVER } else { 0.0 };
            totals[3] += match cloud.fog() {
                FogType::FogNone => FOG_COVER[0],
                FogType::FogMist => FOG_COVER[1],
                FogType::FogNormal => FOG_COVER[2],
                // DFHack's proto spells this one `F0G_THICK`, with a zero.
                FogType::F0gThick => FOG_COVER[3],
            };
            samples += 1.0;
        }
    }

    if samples == 0.0 {
        return Weather::default();
    }
    Weather {
        cumulus: totals[0] / samples,
        stratus: totals[1] / samples,
        cirrus: totals[2] / samples,
        fog: totals[3] / samples,
        // The plugin drops the countdown bits; only the Lua probe has them.
        countdown: 0.0,
    }
}

/// Meshes every loaded chunk and ships the results in batches.
/// Pulls the region and world maps and stitches the land beyond the loaded map.
fn build_horizon(
    df: &mut Session,
    skins: &dwarf_eye_world::horizon::skin::Skins,
) -> Result<dwarf_eye_world::horizon::Horizon> {
    let regions: rfr::RegionMaps = df.client.call_empty(methods::GET_REGION_MAPS_NEW)?;
    let world_map: rfr::WorldMap = df.client.call_empty(methods::GET_WORLD_MAP)?;
    let transpose = std::env::var("DWARF_EYE_HORIZON_TRANSPOSE").is_ok();
    Ok(dwarf_eye_world::horizon::build(
        &df.world.palette,
        &df.map_info,
        df.origin(),
        &regions,
        &world_map,
        &df.world,
        skins,
        transpose,
    ))
}

/// Levels collected around the character: the window is fetched whole in x/y,
/// and this deep in z, so everything Dwarf Fortress discloses is kept.
const COLLECT_ABOVE: i32 = 200;
const COLLECT_BELOW: i32 = 32;

/// One collection pass: pull what the live window holds, cache it, mesh what
/// changed, and keep the horizon in step.
#[allow(clippy::too_many_arguments)]
fn collect(
    df: &mut Session,
    center: (i32, i32, i32),
    opts: MeshOptions,
    force: bool,
    last_window: &mut Option<(i32, i32, i32, i32, i32, i32, (i32, i32, i32))>,
    horizon_sent: &mut bool,
    mut library: Option<&mut TileLibrary>,
    forest: &mut Forest,
    events: &Sender<Event>,
) -> Result<std::collections::HashMap<ChunkKey, usize>> {
    // The game's window follows the character; a move changes what every
    // local coordinate means, so the next request must be a full one.
    let window_moved = df.refresh_window()?;
    let view = df.view_center()?;
    df.watch_from(view);
    let bounds = df.window_bounds(view.2 - COLLECT_BELOW, view.2 + COLLECT_ABOVE);
    let window = (
        bounds.min_x, bounds.max_x, bounds.min_y,
        bounds.max_y, bounds.min_z, bounds.max_z, df.shift(),
    );
    let moved = *last_window != Some(window);
    *last_window = Some(window);

    let arrived = df.fetch(bounds, force || moved)?;

    // Land the forced pass proved is no longer there. The chunks are already
    // out of the world and the cache; the meshes and any tree grown off those
    // tiles go with them.
    if df.last_pass.window_moved {
        bevy::log::info!(
            "window moved while the pass was in flight; its replies were placed by their own frame"
        );
    }
    let stale = std::mem::take(&mut df.last_pass.stale);
    if !stale.is_empty() {
        bevy::log::info!(
            "cache: {} chunks dropped, the game answered nothing for them: {:?}",
            stale.len(),
            &stale[..stale.len().min(8)],
        );
        forest.retire_near(&stale);
        events.send(Event::Chunks(
            stale
                .iter()
                .map(|&k| (k, MeshData::default(), Vec::new()))
                .collect(),
        ))?;
    }

    // Chunks stay as the character travels, so the map paints in. Only what
    // is far behind the camera is retired.
    // Retire only what is far away horizontally. Never clip vertically: the
    // camera's height is not a reason to lose the crowns above it, and a
    // dropped chunk does not come back, since DFHack's unforced pass sends
    // only blocks that changed on its side.
    let (bx, by) = (center.0.div_euclid(BLOCK), center.1.div_euclid(BLOCK));
    let keep = BlockBounds {
        min_x: bx - RETAIN_RADIUS,
        max_x: bx + RETAIN_RADIUS + 1,
        min_y: by - RETAIN_RADIUS,
        max_y: by + RETAIN_RADIUS + 1,
        min_z: i32::MIN / 2,
        max_z: i32::MAX / 2,
    };
    let dropped = df.world.retain_within(keep);
    if !dropped.is_empty() {
        events.send(Event::Chunks(
            dropped
                .iter()
                .map(|&k| (k, MeshData::default(), Vec::new()))
                .collect(),
        ))?;
    }

    // A column reopening is rare and worth saying out loud: it is ground the
    // viewer had written off coming back.
    for &((bx, by), z) in &df.last_pass.reopened {
        bevy::log::info!("floors: column {bx},{by} reopened, ground seen at level {z}");
    }
    if df.last_pass.probe_requests > 0 {
        bevy::log::info!(
            "floors: probed {} blocks under {} floors in {} requests; kept {:?}",
            df.last_pass.probed,
            df.last_pass.floors,
            df.last_pass.probe_requests,
            df.last_pass.probe_kept,
        );
    }

    if !arrived.is_empty() || !dropped.is_empty() || !stale.is_empty() {
        events.send(Event::Coverage(grounded_blocks(&df.world)))?;
    }
    let mut cost = std::collections::HashMap::new();
    if !arrived.is_empty() || !stale.is_empty() {
        df.persist(&arrived);
        events.send(Event::Status(format!(
            "{} blocks fetched, {} chunks held",
            arrived.len(),
            df.world.chunk_count()
        )))?;
        // A new block changes its neighbours' culling, so those remesh along
        // with it, and so do the neighbours of one that has just gone away.
        // A block that has just arrived can lengthen a tree whose top we could
        // not see, so those trees are grown again rather than reused.
        forest.retire_near(&arrived);
        let touched: Vec<_> = arrived.iter().chain(stale.iter()).copied().collect();
        cost = remesh_touched(df, library.as_deref_mut(), forest, opts, events, &touched)?;
    }

    // The outer terrain needs the map's own surface to meet it, so it waits
    // for the first blocks, and follows the window after.
    if df.world.chunk_count() > 0 && (!*horizon_sent || window_moved) {
        *horizon_sent = true;
        let skins = library
            .as_deref()
            .map(dwarf_eye_world::horizon::skin::Skins::from_library)
            .unwrap_or_default();
        match build_horizon(df, &skins) {
            Ok(mesh) => events.send(Event::Horizon(mesh))?,
            Err(e) => events.send(Event::Status(format!("no horizon: {e:#}")))?,
        }
    }
    Ok(cost)
}

/// What the loaded map holds at one level under its column's surface.
#[derive(Default, Clone, Copy)]
struct Depth {
    blocks: usize,
    solid: usize,
    hidden: usize,
    triangles: usize,
}

/// The loaded map by depth under its own surface, deepest row last.
///
/// A column's surface is the highest level in it holding anything solid.
/// Everything under that is what a fixed fetch depth pays for — fetched,
/// cached, meshed — so the histogram is here to make the bill legible.
fn depth_histogram(
    world: &dwarf_eye_world::World,
    triangles: &std::collections::HashMap<ChunkKey, usize>,
) -> Vec<(i32, Depth)> {
    let mut surface: std::collections::HashMap<(i32, i32), i32> = std::collections::HashMap::new();
    for chunk in world.chunks() {
        if chunk.voxels.iter().any(|v| !v.solid.is_empty()) {
            let top = surface.entry((chunk.block_x, chunk.block_y)).or_insert(chunk.z);
            *top = (*top).max(chunk.z);
        }
    }

    let mut rows: std::collections::BTreeMap<i32, Depth> = std::collections::BTreeMap::new();
    for chunk in world.chunks() {
        let Some(&top) = surface.get(&(chunk.block_x, chunk.block_y)) else { continue };
        let row = rows.entry(chunk.z - top).or_default();
        row.blocks += 1;
        row.solid += chunk.voxels.iter().filter(|v| !v.solid.is_empty()).count();
        row.hidden += chunk.voxels.iter().filter(|v| v.hidden).count();
        row.triangles +=
            triangles.get(&(chunk.block_x, chunk.block_y, chunk.z)).copied().unwrap_or(0);
    }
    rows.into_iter().rev().collect()
}

/// Prints the histogram, shallowest first, with the deep tail summed.
fn log_depths(what: &str, rows: &[(i32, Depth)]) {
    const SHOWN: usize = 12;
    let line = |depth: &str, d: Depth| {
        bevy::log::info!(
            "startup: {what} {depth}: {} blocks, {} solid, {} hidden, {} triangles",
            d.blocks,
            d.solid,
            d.hidden,
            d.triangles
        );
    };
    for &(z, d) in rows.iter().take(SHOWN) {
        line(&format!("{z:+}"), d);
    }
    if rows.len() > SHOWN {
        let rest = rows[SHOWN..].iter().fold(Depth::default(), |a, (_, d)| Depth {
            blocks: a.blocks + d.blocks,
            solid: a.solid + d.solid,
            hidden: a.hidden + d.hidden,
            triangles: a.triangles + d.triangles,
        });
        line(&format!("{}..{}", rows[SHOWN].0, rows.last().map(|r| r.0).unwrap_or(0)), rest);
    }
}

/// Blocks whose lowest loaded chunk is mostly solid: the fine data there
/// reaches the ground, so the coarse horizon can give way. A column whose
/// lowest chunk is sparse holds only canopy, and the ground under it is not
/// loaded yet.
fn grounded_blocks(world: &dwarf_eye_world::World) -> Vec<(i32, i32)> {
    let mut lowest: std::collections::HashMap<(i32, i32), &dwarf_eye_world::Chunk> =
        std::collections::HashMap::new();
    for chunk in world.chunks() {
        let entry = lowest.entry((chunk.block_x, chunk.block_y)).or_insert(chunk);
        if chunk.z < entry.z {
            *entry = chunk;
        }
    }
    lowest
        .into_iter()
        .filter(|(_, chunk)| {
            let filled = chunk.voxels.iter().filter(|v| !v.solid.is_empty()).count();
            filled * 2 >= chunk.voxels.len()
        })
        .map(|(key, _)| key)
        .collect()
}

/// How far from the camera chunks are kept, in blocks and levels. Wide, so a
/// walk leaves the land behind it standing.
const RETAIN_RADIUS: i32 = 40;

/// Remeshes the chunks that arrived and every loaded neighbour of theirs.
fn remesh_touched(
    df: &Session,
    mut library: Option<&mut TileLibrary>,
    forest: &mut Forest,
    opts: MeshOptions,
    events: &Sender<Event>,
    arrived: &[(i32, i32, i32)],
) -> Result<std::collections::HashMap<ChunkKey, usize>> {
    const BATCH: usize = 48;
    let mut cost = std::collections::HashMap::new();
    let mut keys: std::collections::HashSet<(i32, i32, i32)> = std::collections::HashSet::new();
    for &(x, y, z) in arrived {
        for (dx, dy, dz) in [(0, 0, 0), (1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)] {
            keys.insert((x + dx, y + dy, z + dz));
        }
    }
    let mut batch = Vec::with_capacity(BATCH);
    for key in keys {
        let Some(chunk) = df.world.chunk(key.0, key.1, key.2) else { continue };
        let mesh = build_chunk(&df.world, chunk, opts, library.as_deref_mut());
        // Every band is built here: they share the growth the forest caches,
        // and a chunk that arrives has to arrive whole.
        let mut crowns: Vec<CanopyMeshes> = Vec::new();
        if let Some(lib) = library.as_deref_mut() {
            for band in BANDS {
                crowns.push(forest.build_chunk(&df.world, chunk, opts, lib, df.origin(), band));
            }
        }
        let near = crowns.first().map(CanopyMeshes::triangle_count).unwrap_or(0);
        cost.insert(key, mesh.triangle_count() + near);
        batch.push((key, mesh, crowns));
        if batch.len() == BATCH {
            events.send(Event::Chunks(std::mem::take(&mut batch)))?;
            batch.reserve(BATCH);
        }
    }
    if !batch.is_empty() {
        events.send(Event::Chunks(batch))?;
    }
    Ok(cost)
}

/// Meshes every loaded chunk, returning what each cost in triangles.
fn remesh_all(
    df: &Session,
    mut library: Option<&mut TileLibrary>,
    forest: &mut Forest,
    opts: MeshOptions,
    events: &Sender<Event>,
) -> Result<std::collections::HashMap<ChunkKey, usize>> {
    const BATCH: usize = 48;
    let mut batch = Vec::with_capacity(BATCH);
    let mut cost = std::collections::HashMap::new();

    for chunk in df.world.chunks() {
        let key = (chunk.block_x, chunk.block_y, chunk.z);
        let mesh = build_chunk(&df.world, chunk, opts, library.as_deref_mut());
        // Every band is built here: they share the growth the forest caches,
        // and a chunk that arrives has to arrive whole.
        let mut crowns: Vec<CanopyMeshes> = Vec::new();
        if let Some(lib) = library.as_deref_mut() {
            for band in BANDS {
                crowns.push(forest.build_chunk(&df.world, chunk, opts, lib, df.origin(), band));
            }
        }
        let near = crowns.first().map(CanopyMeshes::triangle_count).unwrap_or(0);
        cost.insert(key, mesh.triangle_count() + near);
        batch.push((key, mesh, crowns));
        if batch.len() == BATCH {
            events.send(Event::Chunks(std::mem::take(&mut batch)))?;
            batch.reserve(BATCH);
        }
    }
    if !batch.is_empty() {
        events.send(Event::Chunks(batch))?;
    }
    Ok(cost)
}
