//! Runs the DFHack connection on its own thread so the render loop never blocks
//! on a socket.
//!
//! The worker owns the [`World`], because face culling needs neighbouring chunks
//! and it is cheaper to mesh next to the data than to ship the data across.

use crate::clouds::Weather;
use crate::polls;
use crate::walk::Pilot;
use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::library::TileLibrary;
use dwarf_eye_world::canopy::{BANDS, CanopyMeshes, Forest};
use dwarf_eye_world::weather::Precip;
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
    /// render tiles and level, for retiring far chunks. `heading` is which way
    /// it is travelling on the ground plan, as a unit vector east and south,
    /// which decides what is fetched, meshed and kept first.
    Fetch {
        center: (i32, i32, i32),
        opts: MeshOptions,
        force: bool,
        heading: Option<(f32, f32)>,
    },
    /// Remesh what is already loaded, without going back to DFHack.
    Remesh { opts: MeshOptions },
    /// Run a DFHack console command, for driving the world while testing.
    Run { command: String, args: Vec<String> },
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
    /// The region tile's rainfall and temperature at the character, each
    /// 0..1, where the probe carried them.
    pub climate: Option<(f32, f32)>,
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
            climate: None,
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
        // Every startup timestamp is measured from here.
        polls::started();
        let (tx, command_rx) = channel();
        let (event_tx, rx) = channel();
        let origin: Arc<OnceLock<(i32, i32, i32)>> = Arc::default();
        let theirs = Arc::clone(&origin);
        // The clock and the sky ride their own connection, so neither waits on
        // a pass; the worker only tells it whether there is a roof overhead.
        let sky = polls::sky_open();
        polls::spawn(event_tx.clone(), Arc::clone(&sky));
        thread::Builder::new()
            .name("dfhack".into())
            .spawn(move || {
                if let Err(err) = run(command_rx, &event_tx, &theirs, &sky) {
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
    sky: &polls::SkyOpen,
) -> Result<()> {
    let started = polls::started();
    let mut df = Session::connect_local()?;
    // Walk mode places its own position reads against this.
    let _ = origin.set(df.origin());
    bevy::log::info!("startup: connected and raws fetched in {:.1}s", started.elapsed().as_secs_f32());
    // The window we last asked for. DFHack answers with only the blocks it
    // thinks changed, so any chunk we prune has to be re-requested outright or
    // it never comes back.
    let mut last_window: Option<Window> = None;

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
    // The last preload order said out loud, so a steady walk does not repeat it.
    let mut spoke = String::new();
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
            Command::Fetch { center, opts, force, heading } => {
                let pass_started = std::time::Instant::now();
                // Travel mode and loading screens leave no map behind, and
                // DFHack answers with a link failure. Wait it out.
                let cost = match collect(
                    &mut df, center, opts, force, heading, &mut last_window, &mut horizon_sent,
                    &mut spoke, library.as_mut(), &mut forest, events,
                ) {
                    Ok(cost) => cost,
                    Err(err) => {
                        events.send(Event::Status(format!("waiting for the map: {err:#}")))?;
                        Default::default()
                    }
                };
                // The voxels that answer this live here, and the weather poll,
                // on its own connection, only reads the answer.
                sky.store(open_sky(&df.world, center), std::sync::atomic::Ordering::Relaxed);
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

/// The box one pass covered, in render space, and the window frame it was read
/// in. Two passes in the same frame with the same box cover the same land.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Window {
    bounds: BlockBounds,
    shift: (i32, i32, i32),
}

/// One collection pass: pull what the live window holds, cache it, mesh what
/// changed, and keep the horizon in step.
#[allow(clippy::too_many_arguments)]
fn collect(
    df: &mut Session,
    center: (i32, i32, i32),
    opts: MeshOptions,
    force: bool,
    heading: Option<(f32, f32)>,
    last_window: &mut Option<Window>,
    horizon_sent: &mut bool,
    spoke: &mut String,
    mut library: Option<&mut TileLibrary>,
    forest: &mut Forest,
    events: &Sender<Event>,
) -> Result<std::collections::HashMap<ChunkKey, usize>> {
    // The game's window follows the character; a move changes what every
    // local coordinate means, and it uncovers a strip of land at the leading
    // edge that this viewer has never seen.
    df.refresh_window()?;
    let view = df.view_center()?;
    df.watch_from(view);
    df.travel_toward(heading);
    let bounds = df.window_bounds(view.2 - COLLECT_BELOW, view.2 + COLLECT_ABOVE);
    let window = Window { bounds, shift: df.shift() };
    let was = last_window.replace(window);
    let window_moved = was.is_some_and(|w| w.shift != window.shift);

    // What this box covers and the last one did not. DFHack answers an unforced
    // request with the blocks whose hash has moved since it last sent them, and
    // the hash it holds for a block at the window's new edge is the hash of the
    // land that used to be there, so the strip can read as unchanged and never
    // arrive. It is asked for forced, and the blocks ahead of the character
    // first.
    let here = (center.0.div_euclid(BLOCK), center.1.div_euclid(BLOCK));
    let mut leading = match was {
        Some(w) if !force => newly_covered(w.bounds, bounds),
        _ => Vec::new(),
    };
    ahead_first(&mut leading, here, heading);
    let strips = leading.len();

    let arrived = df.fetch_leading(bounds, force, &leading)?;

    // The order the pass chose, said once per change rather than three times a
    // second: which way the character is going, how the window was cut for it,
    // and what the shift made this pass force.
    let said = format!("{} bands{}", df.last_pass.bands, describe(heading));
    if said != *spoke || strips > 0 {
        *spoke = said.clone();
        bevy::log::info!(
            "preload: {said}; window {}, {strips} forced leading {} of {} blocks, \
             then the unforced sweep of {}",
            if window_moved { "shifted" } else { "held" },
            if strips == 1 { "box" } else { "boxes" },
            df.last_pass.leading,
            df.last_pass.asked - df.last_pass.leading,
        );
    }

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
    let dropped = df.world.retain_within(retention_box(here, heading));
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
        cost = remesh_touched(
            df, library.as_deref_mut(), forest, opts, events, &touched, here, heading,
        )?;
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

/// How far along the heading the retention box is pushed. The box is the
/// budget, and this is what spends it forwards: the land behind falls out of it
/// this many blocks early so the land ahead can stay in it that much longer.
const RETAIN_BIAS: i32 = 8;

/// What is kept in the world: `RETAIN_RADIUS` blocks around the camera,
/// horizontally only, pushed `RETAIN_BIAS` blocks along the heading.
///
/// The box never grows, so the count of chunks held is what it always was. What
/// changes is which of them go when it is full: a chunk behind the character
/// drops before one the same distance ahead, and a dropped chunk costs a forced
/// request to come back — which is the trade, paid where they are least likely
/// to turn round and look.
fn retention_box(here: (i32, i32), heading: Option<(f32, f32)>) -> BlockBounds {
    let (dx, dy) = match heading {
        Some((hx, hy)) => (
            (hx * RETAIN_BIAS as f32).round() as i32,
            (hy * RETAIN_BIAS as f32).round() as i32,
        ),
        None => (0, 0),
    };
    BlockBounds {
        min_x: here.0 + dx - RETAIN_RADIUS,
        max_x: here.0 + dx + RETAIN_RADIUS + 1,
        min_y: here.1 + dy - RETAIN_RADIUS,
        max_y: here.1 + dy + RETAIN_RADIUS + 1,
        min_z: i32::MIN / 2,
        max_z: i32::MAX / 2,
    }
}

/// How far along the heading something sits, in blocks, from the camera's own
/// block. Behind is negative; with no heading everything is level and only the
/// distance from the camera tells them apart.
fn ahead_of(here: (i32, i32), at: (i32, i32), heading: Option<(f32, f32)>) -> f32 {
    let (dx, dy) = ((at.0 - here.0) as f32, (at.1 - here.1) as f32);
    match heading {
        Some((hx, hy)) => dx * hx + dy * hy,
        None => 0.0,
    }
}

/// Puts boxes in the order a travelling character wants them: the one furthest
/// along the heading first, so the leading edge is asked for before the strip
/// beside or behind it. Ties, and no heading at all, go to the nearest.
fn ahead_first(boxes: &mut [BlockBounds], here: (i32, i32), heading: Option<(f32, f32)>) {
    let rank = |b: &BlockBounds| {
        let mid = ((b.min_x + b.max_x - 1) / 2, (b.min_y + b.max_y - 1) / 2);
        let near = ((mid.0 - here.0).pow(2) + (mid.1 - here.1).pow(2)) as f32;
        (-ahead_of(here, mid, heading), near)
    };
    boxes.sort_by(|a, b| {
        rank(a).partial_cmp(&rank(b)).unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// The parts of `now` that `was` did not cover: the strip a window shift
/// uncovers at its leading edge, or the levels a change of depth adds.
///
/// Up to six slabs, cut so they never overlap: the two x sides over the whole
/// of the new box, then the y sides over what the two boxes share in x, then
/// the z ends over what they share in both.
fn newly_covered(was: BlockBounds, now: BlockBounds) -> Vec<BlockBounds> {
    let mut out = Vec::new();
    if now.min_x >= now.max_x || now.min_y >= now.max_y || now.min_z >= now.max_z {
        return out;
    }
    if now.min_x < was.min_x {
        out.push(BlockBounds { max_x: was.min_x.min(now.max_x), ..now });
    }
    if now.max_x > was.max_x {
        out.push(BlockBounds { min_x: was.max_x.max(now.min_x), ..now });
    }
    let (x_lo, x_hi) = (now.min_x.max(was.min_x), now.max_x.min(was.max_x));
    if x_lo >= x_hi {
        return out;
    }
    let shared = BlockBounds { min_x: x_lo, max_x: x_hi, ..now };
    if now.min_y < was.min_y {
        out.push(BlockBounds { max_y: was.min_y.min(now.max_y), ..shared });
    }
    if now.max_y > was.max_y {
        out.push(BlockBounds { min_y: was.max_y.max(now.min_y), ..shared });
    }
    let (y_lo, y_hi) = (now.min_y.max(was.min_y), now.max_y.min(was.max_y));
    if y_lo >= y_hi {
        return out;
    }
    let shared = BlockBounds { min_y: y_lo, max_y: y_hi, ..shared };
    if now.min_z < was.min_z {
        out.push(BlockBounds { max_z: was.min_z.min(now.max_z), ..shared });
    }
    if now.max_z > was.max_z {
        out.push(BlockBounds { min_z: was.max_z.max(now.min_z), ..shared });
    }
    out
}

/// A heading in words, for the log.
fn describe(heading: Option<(f32, f32)>) -> String {
    match heading {
        None => ", nothing travelling".to_string(),
        Some((x, y)) => {
            let compass = match (x.abs() >= y.abs(), x >= 0.0, y >= 0.0) {
                (true, true, _) => "east",
                (true, false, _) => "west",
                (false, _, true) => "south",
                (false, _, false) => "north",
            };
            format!(", travelling {compass} ({x:+.2}, {y:+.2})")
        }
    }
}

/// Remeshes the chunks that arrived and every loaded neighbour of theirs,
/// leading edge first: the chunk furthest along the heading is the one the
/// character is walking into, and the meshes leave in the order they are built.
#[allow(clippy::too_many_arguments)]
fn remesh_touched(
    df: &Session,
    mut library: Option<&mut TileLibrary>,
    forest: &mut Forest,
    opts: MeshOptions,
    events: &Sender<Event>,
    arrived: &[(i32, i32, i32)],
    here: (i32, i32),
    heading: Option<(f32, f32)>,
) -> Result<std::collections::HashMap<ChunkKey, usize>> {
    const BATCH: usize = 48;
    let mut cost = std::collections::HashMap::new();
    let mut keys: std::collections::HashSet<(i32, i32, i32)> = std::collections::HashSet::new();
    for &(x, y, z) in arrived {
        for (dx, dy, dz) in [(0, 0, 0), (1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)] {
            keys.insert((x + dx, y + dy, z + dz));
        }
    }
    let keys = leading_first(keys, here, heading);
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

/// The chunks to remesh, furthest along the heading first, nearest the camera
/// after that. With nothing travelling it is simply nearest first, which is the
/// order the renderer uploads in anyway.
fn leading_first(
    keys: std::collections::HashSet<ChunkKey>,
    here: (i32, i32),
    heading: Option<(f32, f32)>,
) -> Vec<ChunkKey> {
    let mut keys: Vec<ChunkKey> = keys.into_iter().collect();
    let rank = |k: &ChunkKey| {
        let near = ((k.0 - here.0).pow(2) + (k.1 - here.1).pow(2)) as f32;
        (-ahead_of(here, (k.0, k.1), heading), near, k.2)
    };
    keys.sort_by(|a, b| {
        rank(a).partial_cmp(&rank(b)).unwrap_or(std::cmp::Ordering::Equal)
    });
    keys
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A window box: nine blocks square, eight levels deep.
    fn window(x: i32, y: i32, z: i32) -> BlockBounds {
        BlockBounds { min_x: x, max_x: x + 9, min_y: y, max_y: y + 9, min_z: z, max_z: z + 8 }
    }

    #[test]
    fn a_window_that_has_not_moved_uncovers_nothing() {
        assert!(newly_covered(window(0, 0, 100), window(0, 0, 100)).is_empty());
    }

    #[test]
    fn a_shift_uncovers_the_strip_at_its_leading_edge() {
        // The window followed the character three blocks east.
        let strip = newly_covered(window(0, 0, 100), window(3, 0, 100));
        assert_eq!(strip.len(), 1);
        let it = strip[0];
        assert_eq!((it.min_x, it.max_x), (9, 12), "the three columns the old box never held");
        assert_eq!((it.min_y, it.max_y, it.min_z, it.max_z), (0, 9, 100, 108));
    }

    #[test]
    fn a_diagonal_shift_uncovers_two_strips_that_do_not_overlap() {
        let strips = newly_covered(window(0, 0, 100), window(3, 2, 100));
        assert_eq!(strips.len(), 2);
        let blocks: usize = strips
            .iter()
            .map(|b| ((b.max_x - b.min_x) * (b.max_y - b.min_y)) as usize)
            .sum();
        let mut seen = std::collections::HashSet::new();
        for b in &strips {
            for bx in b.min_x..b.max_x {
                for by in b.min_y..b.max_y {
                    seen.insert((bx, by));
                }
            }
        }
        assert_eq!(seen.len(), blocks, "the strips overlap");
        // Nine columns wide: three columns the old box never held, and two rows
        // over the six columns it did.
        assert_eq!(blocks, 9 * 3 + 6 * 2);
    }

    #[test]
    fn dropping_a_level_uncovers_the_levels_under_the_old_box() {
        let deeper = newly_covered(window(0, 0, 100), window(0, 0, 98));
        assert_eq!(deeper.len(), 1);
        assert_eq!((deeper[0].min_z, deeper[0].max_z), (98, 100));
    }

    #[test]
    fn the_strip_ahead_is_forced_before_the_one_behind() {
        let mut strips = newly_covered(window(0, 0, 100), window(3, 2, 100));
        // Travelling east: the eastern columns before the southern rows.
        ahead_first(&mut strips, (7, 6), Some((1.0, 0.0)));
        assert_eq!((strips[0].min_x, strips[0].max_x), (9, 12));
        // Travelling south turns the order round.
        ahead_first(&mut strips, (7, 6), Some((0.0, 1.0)));
        assert_eq!((strips[0].min_y, strips[0].max_y), (9, 11));
    }

    #[test]
    fn retention_drops_the_land_behind_before_the_land_ahead() {
        let here = (0, 0);
        let level = retention_box(here, None);
        assert!(level.contains_block(RETAIN_RADIUS, 0, 0));
        assert!(level.contains_block(-RETAIN_RADIUS, 0, 0));

        // Walking east, at the same distance from the camera: the block ahead
        // stays and the one behind goes.
        let east = retention_box(here, Some((1.0, 0.0)));
        assert!(east.contains_block(RETAIN_RADIUS + RETAIN_BIAS, 0, 0), "the land ahead is kept");
        assert!(!east.contains_block(-RETAIN_RADIUS, 0, 0), "the land behind is dropped");
        assert!(east.contains_block(-RETAIN_RADIUS + RETAIN_BIAS, 0, 0));
        // The box never grows: the same count of columns, moved.
        let columns = |b: BlockBounds| (b.max_x - b.min_x) * (b.max_y - b.min_y);
        assert_eq!(columns(east), columns(level));
        // And it never clips vertically.
        assert!(east.contains_block(0, 0, -10_000));
        assert!(east.contains_block(0, 0, 10_000));
    }

    #[test]
    fn the_leading_edge_is_meshed_first() {
        let keys: std::collections::HashSet<ChunkKey> =
            [(2, 0, 5), (-2, 0, 5), (0, 0, 5), (0, 3, 5)].into_iter().collect();
        let order = leading_first(keys.clone(), (0, 0), Some((1.0, 0.0)));
        assert_eq!(order[0], (2, 0, 5), "the chunk furthest east goes first");
        assert_eq!(order[3], (-2, 0, 5), "the one behind goes last");
        // Nothing travelling: nearest the camera first, and the same order
        // every time for the same set.
        let still = leading_first(keys.clone(), (0, 0), None);
        assert_eq!(still[0], (0, 0, 5));
        assert_eq!(still, leading_first(keys, (0, 0), None));
    }
}
