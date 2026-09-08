//! Runs the DFHack connection on its own thread so the render loop never blocks
//! on a socket.
//!
//! The worker owns the [`World`], because face culling needs neighbouring chunks
//! and it is cheaper to mesh next to the data than to ship the data across.

use crate::clouds::Weather;
use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::library::TileLibrary;
use dwarf_eye_world::{BlockBounds, MeshData, MeshOptions, Session, build_chunk};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;
use std::time::Duration;

/// Chunk key: block x, block y, z-level.
pub type ChunkKey = (i32, i32, i32);

pub enum Command {
    /// Pull blocks around a tile position and remesh what they touch.
    Fetch { center: (i32, i32, i32), radius: i32, depth: i32, opts: MeshOptions, force: bool },
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

pub enum Event {
    /// Cloud cover over the embark.
    Weather(Weather),
    /// Dwarf Fortress's calendar, polled while the world runs.
    Clock { year: i32, tick: i32 },
    /// The packed ground texture, sent once before any geometry.
    Atlas { width: u32, height: u32, pixels: Vec<u8> },
    Connected { world_name: String, save: String, center: (i32, i32, i32), size: (i32, i32, i32) },
    /// Geometry for chunks that changed; an empty `data` means "despawn this one".
    Chunks(Vec<(ChunkKey, MeshData)>),
    /// Coarse terrain beyond the loaded map, sent once the map's surface is known.
    Horizon(MeshData),
    Status(String),
    Failed(String),
}

pub struct Bridge {
    pub tx: Sender<Command>,
    pub rx: Receiver<Event>,
}

impl Bridge {
    /// Spawns the worker thread and returns the channel pair to talk to it.
    pub fn spawn() -> Self {
        let (tx, command_rx) = channel();
        let (event_tx, rx) = channel();
        thread::Builder::new()
            .name("dfhack".into())
            .spawn(move || {
                if let Err(err) = run(command_rx, &event_tx) {
                    let _ = event_tx.send(Event::Failed(format!("{err:#}")));
                }
            })
            .expect("spawning the DFHack worker thread");
        Self { tx, rx }
    }
}

fn run(commands: Receiver<Command>, events: &Sender<Event>) -> Result<()> {
    let mut df = Session::connect_local()?;
    // The window we last asked for. DFHack answers with only the blocks it
    // thinks changed, so any chunk we prune has to be re-requested outright or
    // it never comes back.
    let mut last_window: Option<(i32, i32, i32, i32, i32, i32, (i32, i32, i32))> = None;

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

    // Land from earlier sessions comes back before the first fetch.
    let restored = df.restore_cache();
    bevy::log::info!(
        "{} chunks restored from {}",
        restored.len(),
        df.cache_dir().map(|p| p.display().to_string()).unwrap_or_default()
    );
    if !restored.is_empty() {
        events.send(Event::Status(format!(
            "{} chunks restored from {}",
            restored.len(),
            df.cache_dir().map(|p| p.display().to_string()).unwrap_or_default()
        )))?;
        remesh_all(&df, library.as_mut(), MeshOptions { z_ceiling: i32::MAX, show_hidden: true }, events)?;
    }

    let mut horizon_sent = false;
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
            Command::Remesh { opts } => remesh_all(&df, library.as_mut(), opts, events)?,
            Command::Run { command, args } => {
                let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
                match df.client.run_command(&command, &borrowed) {
                    Ok(()) => events.send(Event::Status(format!("ran `{command}`")))?,
                    Err(e) => events.send(Event::Status(format!("`{command}` failed: {e}")))?,
                }
            }
            Command::Weather => {
                let map: rfr::WorldMap = df.client.call_empty(methods::GET_WORLD_MAP)?;
                events.send(Event::Weather(read_weather(&map)))?;
            }
            Command::Clock => {
                let map: rfr::WorldMap = df.client.call_empty(methods::GET_WORLD_MAP_CENTER)?;
                events.send(Event::Clock { year: map.cur_year(), tick: map.cur_year_tick() })?;
            }
            Command::Fetch { center, radius, depth, opts, force } => {
                // The game's window follows the character; a move changes what
                // every local coordinate means, so the next request must be
                // a full one.
                let window_moved = df.refresh_window()?;
                let bounds =
                    BlockBounds::under_ceiling(center.0, center.1, center.2, radius, depth);
                let window = (
                    bounds.min_x, bounds.max_x, bounds.min_y,
                    bounds.max_y, bounds.min_z, bounds.max_z, df.shift(),
                );
                let moved = last_window != Some(window);
                last_window = Some(window);

                let arrived = df.fetch(bounds, force || moved)?;

                // Chunks stay as the character travels, so the map paints in.
                // Only what is far behind the camera is retired.
                let keep = BlockBounds::under_ceiling(
                    center.0, center.1, center.2, RETAIN_RADIUS, RETAIN_DEPTH,
                );
                let dropped = df.world.retain_within(keep);
                if !dropped.is_empty() {
                    events.send(Event::Chunks(
                        dropped.into_iter().map(|k| (k, MeshData::default())).collect(),
                    ))?;
                }

                if !arrived.is_empty() {
                    df.persist(&arrived);
                    events.send(Event::Status(format!(
                        "{} blocks fetched, {} chunks held",
                        arrived.len(),
                        df.world.chunk_count()
                    )))?;
                    // A new block changes its neighbours' culling, so those
                    // remesh along with it.
                    remesh_touched(&df, library.as_mut(), opts, events, &arrived)?;
                }

                // The outer terrain needs the map's own surface to meet it, so
                // it waits for the first blocks, and follows the window after.
                if df.world.chunk_count() > 0 && (!horizon_sent || window_moved) {
                    horizon_sent = true;
                    match build_horizon(&mut df) {
                        Ok(mesh) => events.send(Event::Horizon(mesh))?,
                        Err(e) => events.send(Event::Status(format!("no horizon: {e:#}")))?,
                    }
                }
            }
        }
    }
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
            totals[0] += match cloud.cumulus() {
                CumulusType::CumulusNone => 0.0,
                CumulusType::CumulusMedium => 0.35,
                CumulusType::CumulusMulti => 0.62,
                CumulusType::CumulusNimbus => 0.88,
            };
            totals[1] += match cloud.stratus() {
                StratusType::StratusNone => 0.0,
                StratusType::StratusAlto => 0.40,
                StratusType::StratusProper => 0.75,
                StratusType::StratusNimbus => 0.95,
            };
            totals[2] += if cloud.cirrus() { 0.5 } else { 0.0 };
            totals[3] += match cloud.fog() {
                FogType::FogNone => 0.0,
                FogType::FogMist => 0.25,
                FogType::FogNormal => 0.55,
                // DFHack's proto spells this one `F0G_THICK`, with a zero.
                FogType::F0gThick => 0.85,
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
    }
}

/// Meshes every loaded chunk and ships the results in batches.
/// Pulls the region and world maps and stitches the land beyond the loaded map.
fn build_horizon(df: &mut Session) -> Result<MeshData> {
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
        transpose,
    ))
}

/// How far from the camera chunks are kept, in blocks and levels. Wide, so a
/// walk leaves the land behind it standing.
const RETAIN_RADIUS: i32 = 40;
const RETAIN_DEPTH: i32 = 60;

/// Remeshes the chunks that arrived and every loaded neighbour of theirs.
fn remesh_touched(
    df: &Session,
    mut library: Option<&mut TileLibrary>,
    opts: MeshOptions,
    events: &Sender<Event>,
    arrived: &[(i32, i32, i32)],
) -> Result<()> {
    const BATCH: usize = 48;
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
        batch.push((key, mesh));
        if batch.len() == BATCH {
            events.send(Event::Chunks(std::mem::take(&mut batch)))?;
            batch.reserve(BATCH);
        }
    }
    if !batch.is_empty() {
        events.send(Event::Chunks(batch))?;
    }
    Ok(())
}

fn remesh_all(
    df: &Session,
    mut library: Option<&mut TileLibrary>,
    opts: MeshOptions,
    events: &Sender<Event>,
) -> Result<()> {
    const BATCH: usize = 48;
    let mut batch = Vec::with_capacity(BATCH);

    for chunk in df.world.chunks() {
        let key = (chunk.block_x, chunk.block_y, chunk.z);
        let mesh = build_chunk(&df.world, chunk, opts, library.as_deref_mut());
        batch.push((key, mesh));
        if batch.len() == BATCH {
            events.send(Event::Chunks(std::mem::take(&mut batch)))?;
            batch.reserve(BATCH);
        }
    }
    if !batch.is_empty() {
        events.send(Event::Chunks(batch))?;
    }
    Ok(())
}
