//! Connects to a running DFHack and reports what the remote API is offering.
//!
//! Run with Dwarf Fortress open: `cargo run -p dfhack-remote --example probe`

use anyhow::Result;
use dfhack_remote::{Client, methods, rfr};

fn main() -> Result<()> {
    let mut df = Client::connect_local()?;
    println!("connected to DFHack on 127.0.0.1:5000\n");

    let version: rfr::VersionInfo = df.call_empty(methods::GET_VERSION_INFO)?;
    println!("dfhack version    {}", version.dfhack_version());
    println!("df version        {}", version.dwarf_fortress_version());
    println!("rfr version       {}", version.remote_fortress_reader_version());

    let paused: rfr::SingleBool = df.call_empty(methods::GET_PAUSE_STATE)?;
    println!("paused            {}", paused.value());

    let map: rfr::MapInfo = df.call_empty(methods::GET_MAP_INFO)?;
    println!("\nworld             {:?}", map.world_name_english());
    println!("save              {:?}", map.save_name());
    println!(
        "map size          {} x {} x {} blocks  ({} x {} tiles wide)",
        map.block_size_x(),
        map.block_size_y(),
        map.block_size_z(),
        map.block_size_x() * 16,
        map.block_size_y() * 16,
    );
    println!(
        "map origin        block ({}, {}, {})",
        map.block_pos_x(),
        map.block_pos_y(),
        map.block_pos_z()
    );

    let tiletypes: rfr::TiletypeList = df.call_empty(methods::GET_TILETYPE_LIST)?;
    println!("\ntiletypes         {}", tiletypes.tiletype_list.len());

    let materials: rfr::MaterialList = df.call_empty(methods::GET_MATERIAL_LIST)?;
    println!("materials         {}", materials.material_list.len());

    let view: rfr::ViewInfo = df.call_empty(methods::GET_VIEW_INFO)?;
    println!(
        "\nview window       {} x {} at ({}, {}, {})",
        view.view_size_x(),
        view.view_size_y(),
        view.view_pos_x(),
        view.view_pos_y(),
        view.view_pos_z()
    );
    println!(
        "cursor            ({}, {}, {})",
        view.cursor_pos_x(),
        view.cursor_pos_y(),
        view.cursor_pos_z()
    );

    let units: rfr::UnitList = df.call_empty(methods::GET_UNIT_LIST)?;
    println!("units             {}", units.creature_list.len());

    // Pull a small slab of map around the view to prove block streaming works.
    let request = rfr::BlockRequest {
        blocks_needed: Some(200),
        min_x: Some(view.view_pos_x() / 16 - 2),
        max_x: Some(view.view_pos_x() / 16 + 3),
        min_y: Some(view.view_pos_y() / 16 - 2),
        max_y: Some(view.view_pos_y() / 16 + 3),
        min_z: Some(view.view_pos_z() - 2),
        max_z: Some(view.view_pos_z() + 3),
        force_reload: Some(true),
    };
    let blocks: rfr::BlockList = df.call(methods::GET_BLOCK_LIST, &request)?;
    println!("\nblocks returned   {}", blocks.map_blocks.len());
    if let Some(b) = blocks.map_blocks.first() {
        println!(
            "first block       ({}, {}, {}) with {} tiles, {} materials, {} buildings, {} items",
            b.map_x,
            b.map_y,
            b.map_z,
            b.tiles.len(),
            b.materials.len(),
            b.buildings.len(),
            b.items.len()
        );
    }

    Ok(())
}
