//! Ties a DFHack connection to a [`World`], handling the one-time raw fetches.
//!
//! Dwarf Fortress loads a window of the world and DFHack reports blocks
//! relative to it. In adventure mode that window follows the character, so
//! the same ground comes back at new local coordinates after a walk. The
//! session pins a render origin at the window's position on connect and
//! converts every request and reply, so chunks keep their places and the map
//! paints in as the character travels.

use crate::cache::Cache;
use crate::palette::Palette;
use crate::world::{BLOCK, BlockBounds, World};
use anyhow::Result;
use dfhack_remote::{Client, methods, rfr};

/// Tiles per unit of `MapInfo::block_pos`: DF positions the window in
/// 48-tile region tiles.
pub const REGION_TILE: i32 = 48;

pub struct Session {
    pub client: Client,
    pub world: World,
    pub map_info: rfr::MapInfo,
    pub version: rfr::VersionInfo,
    /// Absolute position of the render origin: tiles in x/y, z-level in z.
    origin: (i32, i32, i32),
    /// Chunks from earlier sessions, and where new ones are written.
    cache: Option<Cache>,
}

/// Absolute position of a window's corner: tiles in x/y, z-level in z.
fn window_origin(info: &rfr::MapInfo) -> (i32, i32, i32) {
    (info.block_pos_x() * REGION_TILE, info.block_pos_y() * REGION_TILE, info.block_pos_z())
}

impl Session {
    /// Connects and pulls the raws that stay fixed for the life of the world.
    pub fn connect_local() -> Result<Self> {
        let mut client = Client::connect_local()?;
        let version = client.call_empty(methods::GET_VERSION_INFO)?;
        let map_info: rfr::MapInfo = client.call_empty(methods::GET_MAP_INFO)?;
        let tiletypes = client.call_empty(methods::GET_TILETYPE_LIST)?;
        let materials = client.call_empty(methods::GET_MATERIAL_LIST)?;
        let world = World::new(Palette::new(tiletypes, materials));
        let origin = window_origin(&map_info);
        let cache = Cache::open(map_info.world_name_english(), map_info.save_name()).ok();
        Ok(Self { client, world, map_info, version, origin, cache })
    }

    /// Absolute key of a render-space chunk key.
    fn absolute(&self, key: (i32, i32, i32)) -> (i32, i32, i32) {
        (key.0 + self.origin.0 / BLOCK, key.1 + self.origin.1 / BLOCK, key.2 + self.origin.2)
    }

    fn relative(&self, key: (i32, i32, i32)) -> (i32, i32, i32) {
        (key.0 - self.origin.0 / BLOCK, key.1 - self.origin.1 / BLOCK, key.2 - self.origin.2)
    }

    pub fn cache_dir(&self) -> Option<&std::path::Path> {
        self.cache.as_ref().map(Cache::dir)
    }

    /// Brings every cached chunk of this world into the render space. Returns
    /// their keys.
    pub fn restore_cache(&mut self) -> Vec<(i32, i32, i32)> {
        let Some(cache) = &self.cache else { return Vec::new() };
        let Ok(entries) = cache.load_all() else { return Vec::new() };
        let mut keys = Vec::new();
        for (absolute, voxels) in entries {
            let key = self.relative(absolute);
            if self.world.restore(key, voxels) {
                keys.push(key);
            }
        }
        keys
    }

    /// Writes chunks to the cache under their absolute keys.
    pub fn persist(&self, keys: &[(i32, i32, i32)]) {
        let Some(cache) = &self.cache else { return };
        for &key in keys {
            if let Some(chunk) = self.world.chunk(key.0, key.1, key.2) {
                let _ = cache.store(self.absolute(key), chunk);
            }
        }
    }

    /// Re-reads where the window sits. True when it has moved since the last
    /// read.
    pub fn refresh_window(&mut self) -> Result<bool> {
        let info: rfr::MapInfo = self.client.call_empty(methods::GET_MAP_INFO)?;
        let moved = window_origin(&info) != window_origin(&self.map_info);
        self.map_info = info;
        Ok(moved)
    }

    /// The render origin, in absolute tiles and z-level.
    pub fn origin(&self) -> (i32, i32, i32) {
        self.origin
    }

    /// What to add to window-local coordinates to reach render coordinates:
    /// tiles in x/y, levels in z.
    pub fn shift(&self) -> (i32, i32, i32) {
        let w = window_origin(&self.map_info);
        (w.0 - self.origin.0, w.1 - self.origin.1, w.2 - self.origin.2)
    }

    /// Where the player is looking, in render coordinates.
    pub fn view_center(&mut self) -> Result<(i32, i32, i32)> {
        let view: rfr::ViewInfo = self.client.call_empty(methods::GET_VIEW_INFO)?;
        let (sx, sy, sz) = self.shift();
        Ok((
            view.view_pos_x() + view.view_size_x() / 2 + sx,
            view.view_pos_y() + view.view_size_y() / 2 + sy,
            view.view_pos_z() + sz,
        ))
    }

    /// Fetches the blocks of `bounds` (render-space blocks and levels) that
    /// fall inside the window, and folds them into the world. Returns the keys
    /// of the chunks that arrived.
    ///
    /// With `force` unset the server sends only blocks that changed since the
    /// last request on this connection.
    pub fn fetch(&mut self, bounds: BlockBounds, force: bool) -> Result<Vec<(i32, i32, i32)>> {
        let (sx, sy, sz) = self.shift();
        let shift_blocks = (sx.div_euclid(BLOCK), sy.div_euclid(BLOCK), sz);
        let local = BlockBounds {
            min_x: (bounds.min_x - shift_blocks.0).max(0),
            max_x: (bounds.max_x - shift_blocks.0).min(self.map_info.block_size_x()),
            min_y: (bounds.min_y - shift_blocks.1).max(0),
            max_y: (bounds.max_y - shift_blocks.1).min(self.map_info.block_size_y()),
            min_z: (bounds.min_z - shift_blocks.2).max(0),
            max_z: (bounds.max_z - shift_blocks.2).min(self.map_info.block_size_z()),
        };
        if local.min_x >= local.max_x || local.min_y >= local.max_y || local.min_z >= local.max_z {
            return Ok(Vec::new());
        }
        let blocks_needed = ((local.max_x - local.min_x)
            * (local.max_y - local.min_y)
            * (local.max_z - local.min_z))
            .max(1);
        let request = rfr::BlockRequest {
            blocks_needed: Some(blocks_needed),
            min_x: Some(local.min_x),
            max_x: Some(local.max_x),
            min_y: Some(local.min_y),
            max_y: Some(local.max_y),
            min_z: Some(local.min_z),
            max_z: Some(local.max_z),
            force_reload: Some(force),
        };
        let list: rfr::BlockList = self.client.call(methods::GET_BLOCK_LIST, &request)?;
        Ok(self.world.absorb(list, shift_blocks))
    }
}
