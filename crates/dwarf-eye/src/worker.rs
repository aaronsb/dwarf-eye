//! Runs the DFHack connection on its own thread so the render loop never blocks
//! on a socket.
//!
//! The worker owns the [`World`], because face culling needs neighbouring chunks
//! and it is cheaper to mesh next to the data than to ship the data across.

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
    Shutdown,
}

pub enum Event {
    /// The packed ground texture, sent once before any geometry.
    Atlas { width: u32, height: u32, pixels: Vec<u8> },
    Connected { world_name: String, save: String, center: (i32, i32, i32), size: (i32, i32, i32) },
    /// Geometry for chunks that changed; an empty `data` means "despawn this one".
    Chunks(Vec<(ChunkKey, MeshData)>),
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
            Command::Fetch { center, radius, depth, opts, force } => {
                let bounds =
                    BlockBounds::under_ceiling(center.0, center.1, center.2, radius, depth);
                let fetched = df.fetch(bounds, force)?;

                // Retire chunks the camera has left behind before remeshing, so
                // the loaded set stays proportional to the view and not to how
                // far the camera has travelled.
                let dropped = df.world.retain_within(bounds);
                if !dropped.is_empty() {
                    events.send(Event::Chunks(
                        dropped.into_iter().map(|k| (k, MeshData::default())).collect(),
                    ))?;
                }

                if fetched > 0 {
                    events.send(Event::Status(format!(
                        "{fetched} blocks fetched, {} chunks in view",
                        df.world.chunk_count()
                    )))?;
                    // A new block changes its neighbours' culling, so remesh the
                    // whole loaded set rather than only what just arrived.
                    remesh_all(&df, library.as_mut(), opts, events)?;
                }
            }
        }
    }
}

/// Meshes every loaded chunk and ships the results in batches.
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
