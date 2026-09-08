//! Weighs the disk cache against the game: a forced fetch of the whole window,
//! block by block, against the chunks the cache holds there.
//!
//! Prints one line per cached chunk the game disagrees with — `DIFFER` where it
//! hands over something else, `MISSING` where it hands over nothing at all,
//! which for DFHack means the block holds nothing but air. Each line also names
//! the nearest live block that matches the cached one exactly, because land
//! written under the wrong key reads as a perfect match a few blocks away
//! rather than as plausible drift.
//!
//! `DWARF_EYE_CACHE=<dir> cargo run -p dwarf-eye-world --example stale`

use anyhow::{Result, bail};
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::cache::Cache;
use dwarf_eye_world::world::{BLOCK, TILES_PER_BLOCK};
use dwarf_eye_world::{Chunk, Session, World};

/// How far around a chunk to look for the block it actually holds.
const SEARCH: i32 = 6;

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let origin = df.origin();
    let cache = Cache::open(df.map_info.world_name_english(), df.map_info.save_name())?;
    let cached = cache.load_all()?;
    println!("cache holds {} chunks, origin {origin:?}", cached.len());

    // The window follows the character, so the frame is read before and after
    // and the run is thrown away if it moved.
    df.refresh_window()?;
    let before = df.shift();
    let shift = (before.0.div_euclid(BLOCK), before.1.div_euclid(BLOCK), before.2);
    let (bx, by, bz) =
        (df.map_info.block_size_x(), df.map_info.block_size_y(), df.map_info.block_size_z());
    let tiletypes: rfr::TiletypeList = df.client.call_empty(methods::GET_TILETYPE_LIST)?;
    let mut names: Vec<String> = Vec::new();
    for t in &tiletypes.tiletype_list {
        let id = t.id as usize;
        if names.len() <= id {
            names.resize(id + 1, String::new());
        }
        names[id] = t.name().to_string();
    }
    let materials = df.client.call_empty(methods::GET_MATERIAL_LIST)?;
    let mut live = World::new(dwarf_eye_world::Palette::new(tiletypes, materials));
    let mut arrived = 0;
    let mut z = 0;
    while z < bz {
        let top = (z + 6).min(bz);
        let request = rfr::BlockRequest {
            blocks_needed: Some(bx * by * (top - z)),
            min_x: Some(0),
            max_x: Some(bx),
            min_y: Some(0),
            max_y: Some(by),
            min_z: Some(z),
            max_z: Some(top),
            force_reload: Some(true),
        };
        let list: rfr::BlockList = df.client.call(methods::GET_BLOCK_LIST, &request)?;
        arrived += list.map_blocks.len();
        live.absorb(list, shift);
        z = top;
    }
    df.refresh_window()?;
    if df.shift() != before {
        bail!("the window moved while the fetch was in flight; run it again");
    }
    println!("live: {arrived} blocks came back, {} chunks", live.chunk_count());

    let (mut agree, mut differ, mut missing, mut outside) = (0, 0, 0, 0);
    for (absolute, voxels) in &cached {
        let key =
            (absolute.0 - origin.0 / BLOCK, absolute.1 - origin.1 / BLOCK, absolute.2 - origin.2);
        let local = (key.0 - shift.0, key.1 - shift.1, key.2 - shift.2);
        if local.0 < 0
            || local.0 >= bx
            || local.1 < 0
            || local.1 >= by
            || local.2 < 0
            || local.2 >= bz
        {
            outside += 1;
            continue;
        }
        let bad = |other: &Chunk| {
            (0..TILES_PER_BLOCK.min(voxels.len()))
                .filter(|&i| voxels[i].tile_id != other.voxels[i].tile_id)
                .count()
        };
        // Where else does this chunk's content live?
        let mut elsewhere = (usize::MAX, (0, 0, 0));
        for dx in -SEARCH..=SEARCH {
            for dy in -SEARCH..=SEARCH {
                for dz in -2..=2 {
                    let Some(other) = live.chunk(key.0 + dx, key.1 + dy, key.2 + dz) else {
                        continue;
                    };
                    let n = bad(other);
                    if n < elsewhere.0 {
                        elsewhere = (n, (dx, dy, dz));
                    }
                }
            }
        }
        match live.chunk(key.0, key.1, key.2) {
            None => {
                let filled = voxels.iter().filter(|v| !v.solid.is_empty()).count();
                missing += 1;
                println!(
                    "MISSING render {key:?} absolute {absolute:?}: the game has no block there, \
                     the cache has {filled} solid tiles; nearest live match {:?} with {}/256 bad",
                    elsewhere.1, elsewhere.0
                );
            }
            Some(chunk) => {
                let n = bad(chunk);
                if n == 0 {
                    agree += 1;
                    continue;
                }
                differ += 1;
                let at = (0..TILES_PER_BLOCK)
                    .find(|&i| voxels[i].tile_id != chunk.voxels[i].tile_id)
                    .unwrap_or(0);
                let name = |id: i32| names.get(id as usize).cloned().unwrap_or_default();
                println!(
                    "DIFFER render {key:?} absolute {absolute:?}: {n}/256 tiles, first at {},{}: \
                     cached {} vs live {}; nearest live match {:?} with {}/256 bad",
                    at as i32 % BLOCK,
                    at as i32 / BLOCK,
                    name(voxels[at].tile_id),
                    name(chunk.voxels[at].tile_id),
                    elsewhere.1,
                    elsewhere.0,
                );
            }
        }
    }
    println!(
        "{agree} agree, {differ} differ, {missing} the game has no block for, \
         {outside} outside the window"
    );
    Ok(())
}
