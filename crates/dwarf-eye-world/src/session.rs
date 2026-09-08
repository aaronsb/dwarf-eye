//! Ties a DFHack connection to a [`World`], handling the one-time raw fetches.

use crate::palette::Palette;
use crate::world::{BlockBounds, World};
use anyhow::Result;
use dfhack_remote::{Client, methods, rfr};

pub struct Session {
    pub client: Client,
    pub world: World,
    pub map_info: rfr::MapInfo,
    pub version: rfr::VersionInfo,
}

impl Session {
    /// Connects and pulls the raws that stay fixed for the life of the world.
    pub fn connect_local() -> Result<Self> {
        let mut client = Client::connect_local()?;
        let version = client.call_empty(methods::GET_VERSION_INFO)?;
        let map_info = client.call_empty(methods::GET_MAP_INFO)?;
        let tiletypes = client.call_empty(methods::GET_TILETYPE_LIST)?;
        let materials = client.call_empty(methods::GET_MATERIAL_LIST)?;
        let world = World::new(Palette::new(tiletypes, materials));
        Ok(Self { client, world, map_info, version })
    }

    /// Where the player is looking, in tile coordinates.
    pub fn view_center(&mut self) -> Result<(i32, i32, i32)> {
        let view: rfr::ViewInfo = self.client.call_empty(methods::GET_VIEW_INFO)?;
        Ok((
            view.view_pos_x() + view.view_size_x() / 2,
            view.view_pos_y() + view.view_size_y() / 2,
            view.view_pos_z(),
        ))
    }

    /// Fetches blocks in `bounds` and folds them into the world.
    ///
    /// With `force` unset the server sends only blocks that changed since the
    /// last request on this connection.
    pub fn fetch(&mut self, bounds: BlockBounds, force: bool) -> Result<usize> {
        let blocks_needed = ((bounds.max_x - bounds.min_x)
            * (bounds.max_y - bounds.min_y)
            * (bounds.max_z - bounds.min_z))
            .max(1);
        let request = rfr::BlockRequest {
            blocks_needed: Some(blocks_needed),
            min_x: Some(bounds.min_x),
            max_x: Some(bounds.max_x),
            min_y: Some(bounds.min_y),
            max_y: Some(bounds.max_y),
            min_z: Some(bounds.min_z),
            max_z: Some(bounds.max_z),
            force_reload: Some(force),
        };
        let list: rfr::BlockList = self.client.call(methods::GET_BLOCK_LIST, &request)?;
        Ok(self.world.absorb(list))
    }
}
