//! Reports what DFHack knows about the land beyond the loaded map.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::Session;

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let info: rfr::MapInfo = df.client.call_empty(methods::GET_MAP_INFO)?;
    println!(
        "loaded map: {} x {} x {} blocks ({} x {} tiles), at block ({}, {}, {})",
        info.block_size_x(), info.block_size_y(), info.block_size_z(),
        info.block_size_x() * 16, info.block_size_y() * 16,
        info.block_pos_x(), info.block_pos_y(), info.block_pos_z(),
    );

    let world: rfr::WorldMap = df.client.call_empty(methods::GET_WORLD_MAP)?;
    println!(
        "world map: {} x {} world tiles, centre ({}, {}), {} elevations, {} region tiles/side",
        world.world_width, world.world_height, world.center_x(), world.center_y(),
        world.elevation.len(), 16
    );

    let started = std::time::Instant::now();
    let regions: rfr::RegionMaps = df.client.call_empty(methods::GET_REGION_MAPS_NEW)?;
    println!(
        "region maps: {} world maps, {} region maps, in {:.0} ms",
        regions.world_maps.len(), regions.region_maps.len(),
        started.elapsed().as_secs_f32() * 1000.0
    );
    for map in regions.region_maps.iter().take(3) {
        let elev: Vec<i32> = map.tiles.iter().map(|t| t.elevation()).collect();
        println!(
            "  region ({}, {}) '{}': {} tiles, elevation {}..{}, water {:?}, vegetation {:?}",
            map.map_x(), map.map_y(), map.name_english(), map.tiles.len(),
            elev.iter().min().unwrap_or(&0), elev.iter().max().unwrap_or(&0),
            map.tiles.first().map(|t| t.water_elevation()),
            map.tiles.first().map(|t| t.vegetation()),
        );
    }
    for map in regions.world_maps.iter().take(1) {
        println!(
            "  world map '{}': {} x {}, {} elevations, {} region details",
            map.name_english(), map.world_width, map.world_height,
            map.elevation.len(), map.region_tiles.len()
        );
    }

    // Orientation: print each region map's elevation grid under both orderings,
    // and the world map around the player, so the ocean's side tells them apart.
    let (pwx, pwy) = (info.block_pos_x().div_euclid(16), info.block_pos_y().div_euclid(16));
    println!("world map elevation / water around world tile ({pwx}, {pwy}), index = y * width + x:");
    for wy in pwy - 3..=pwy + 3 {
        let row: Vec<String> = (pwx - 4..=pwx + 4)
            .map(|wx| {
                let i = (wy * world.world_width + wx) as usize;
                format!("{:>3}/{:<3}", world.elevation.get(i).copied().unwrap_or(-1), world.water_elevation.get(i).copied().unwrap_or(-1))
            })
            .collect();
        println!("  wy {wy}: {}", row.join(" "));
    }
    for map in regions.region_maps.iter().filter(|m| (m.map_x() - pwx).abs() <= 1 && (m.map_y() - pwy).abs() <= 1) {
        println!("region ({}, {}) elevations, rows = index / 17 (y-major reading):", map.map_x(), map.map_y());
        for r in 0..17 {
            let row: Vec<String> = (0..17).map(|c| format!("{:>3}", map.tiles[r * 17 + c].elevation())).collect();
            println!("  {}", row.join(" "));
        }
    }

    // Calibrate: the loaded map's surface against the region tiles over it.
    use dwarf_eye_world::{BLOCK, Solid, world::BlockBounds};
    let (bx, by, bz) = (info.block_size_x(), info.block_size_y(), info.block_size_z());
    // Only the upper levels, in slabs: the whole map in one request breaks the link.
    let mut fetched = 0;
    let mut z = bz;
    while z > bz - 60 {
        let bounds = BlockBounds { min_x: 0, max_x: bx, min_y: 0, max_y: by, min_z: (z - 6).max(0), max_z: z };
        fetched += df.fetch(bounds, true)?.len();
        z -= 6;
    }
    println!("fetched {fetched} blocks of the loaded map's top 60 levels");
    // Region tile units: block_pos is in 48-tile region tiles, 16 per world tile.
    let (rx0, ry0) = (info.block_pos_x(), info.block_pos_y());
    let (wx, wy) = (rx0.div_euclid(16), ry0.div_euclid(16));
    println!("map origin region tile ({rx0}, {ry0}) = world tile ({wx}, {wy}) + ({}, {})", rx0.rem_euclid(16), ry0.rem_euclid(16));
    let tiles_per_region = 48;
    for ty in 0..(by * BLOCK / tiles_per_region) {
        for tx in 0..(bx * BLOCK / tiles_per_region) {
            // Surface: highest non-empty voxel at the region tile's centre column.
            let (cx, cy) = (tx * tiles_per_region + 24, ty * tiles_per_region + 24);
            let mut surface = None;
            for z in (0..bz).rev() {
                if let Some(v) = df.world.voxel(cx, cy, z) {
                    if v.solid != Solid::Empty {
                        surface = Some(z);
                        break;
                    }
                }
            }
            let (rx, ry) = (rx0 + tx, ry0 + ty);
            let (mwx, mwy) = (rx.div_euclid(16), ry.div_euclid(16));
            let region = regions.region_maps.iter().find(|m| m.map_x() == mwx && m.map_y() == mwy);
            let elev = region.and_then(|m| m.tiles.get((rx.rem_euclid(16) * 17 + ry.rem_euclid(16)) as usize)).map(|t| t.elevation());
            let elev_t = region.and_then(|m| m.tiles.get((ry.rem_euclid(16) * 17 + rx.rem_euclid(16)) as usize)).map(|t| t.elevation());
            println!("  region tile ({rx}, {ry}) surface z {:?}  region elevation x-major {:?} y-major {:?}", surface, elev, elev_t);
        }
    }

    // Rivers and buildings: how many region tiles carry each, across every
    // region map fetched. River edges with no data read back as a -30000
    // sentinel rather than being absent, so a live edge is one that isn't.
    const RIVER_SENTINEL: i32 = -30000;
    let river_valid = |e: &rfr::RiverEdge| e.min_pos() != RIVER_SENTINEL && e.max_pos() != RIVER_SENTINEL;
    let (mut river_tiles, mut river_edges, mut buildings, mut towers, mut trenches) = (0, 0, 0, 0, 0);
    let mut river_samples = Vec::new();
    let mut building_samples = Vec::new();
    for map in &regions.region_maps {
        for (i, tile) in map.tiles.iter().enumerate() {
            if let Some(river) = tile.river_tiles.as_ref() {
                let edges: [(&str, &Option<rfr::RiverEdge>); 4] =
                    [("north", &river.north), ("south", &river.south), ("east", &river.east), ("west", &river.west)];
                let valid: Vec<_> = edges.into_iter().filter_map(|(name, e)| e.as_ref().filter(|e| river_valid(e)).map(|e| (name, e))).collect();
                if !valid.is_empty() {
                    river_tiles += 1;
                    river_edges += valid.len();
                    if river_samples.len() < 8 {
                        for (name, e) in &valid {
                            river_samples.push(format!(
                                "  river region ({},{}) tile#{i}: {name} min {} max {} active {} elev {}",
                                map.map_x(), map.map_y(), e.min_pos(), e.max_pos(), e.active(), e.elevation()
                            ));
                        }
                    }
                }
            }
            for b in &tile.buildings {
                buildings += 1;
                towers += b.tower_info.is_some() as i32;
                trenches += b.trench_info.is_some() as i32;
                if building_samples.len() < 8 {
                    building_samples.push(format!(
                        "  building region ({},{}) tile#{i}: id {} bbox ({},{})-({},{}) type {} tower {} trench {}",
                        map.map_x(), map.map_y(), b.id(), b.min_x(), b.min_y(), b.max_x(), b.max_y(), b.r#type(),
                        b.tower_info.is_some(), b.trench_info.is_some()
                    ));
                }
            }
        }
    }
    println!(
        "rivers: {river_tiles} region tiles with a live river edge, {river_edges} edges total; buildings: {buildings} ({towers} towers, {trenches} trenches)"
    );
    for line in river_samples.iter().chain(building_samples.iter()) {
        println!("{line}");
    }
    Ok(())
}

#[allow(dead_code)]
fn unused() {}
