//! Walks the load window across the map the way the camera does, reporting what
//! survives. Reproduces chunk loss without the renderer in the way.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::library::TileLibrary;
use dwarf_eye_world::{BlockBounds, MeshOptions, Session, build_chunk};

const RADIUS: i32 = 5;
const DEPTH: i32 = 22;

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let tiletypes: rfr::TiletypeList = df.client.call_empty(methods::GET_TILETYPE_LIST)?;
    let plants: rfr::PlantRawList = df.client.call_empty(methods::GET_PLANT_RAWS)?;
    let mut lib = TileLibrary::load(&tiletypes, &plants)?;

    let (cx, cy, cz) = df.view_center()?;
    let ceiling = cz + 16;
    let opts = MeshOptions { z_ceiling: ceiling, show_hidden: true };

    println!(
        "{:>4} {:>5} {:>6}  {:>8} {:>8} {:>8} {:>10} {:>9}",
        "step", "blkx", "forced", "fetched", "dropped", "chunks", "triangles", "solid"
    );
    let mut last_window = None;

    // Walk east across the map and back again, staying inside it. Returning to
    // the start is the real test: the chunks there were dropped once already.
    let path: Vec<i32> = (0..6)
        .map(|i| cx + i * 12)
        .chain((0..6).rev().map(|i| cx + i * 12))
        .collect();
    for (step, &x) in path.iter().enumerate() {
        let bounds = BlockBounds::under_ceiling(x, cy, ceiling, RADIUS, DEPTH);
        let window = (bounds.min_x, bounds.max_x, bounds.min_y, bounds.max_y, bounds.min_z, bounds.max_z);
        let moved = last_window != Some(window);
        last_window = Some(window);

        let fetched = df.fetch(bounds, moved)?;
        let dropped = df.world.retain_within(bounds).len();

        let mut triangles = 0usize;
        let mut solid = 0usize;
        let chunks: Vec<_> = df.world.chunks().map(|c| (c.block_x, c.block_y, c.z)).collect();
        for key in &chunks {
            let Some(chunk) = df.world.chunk(key.0, key.1, key.2) else { continue };
            let mesh = build_chunk(&df.world, chunk, opts, Some(&mut lib));
            triangles += mesh.triangle_count();
            if !mesh.is_empty() {
                solid += 1;
            }
        }
        println!(
            "{step:>4} {:>5} {:>6}  {fetched:>8} {dropped:>8} {:>8} {triangles:>10} {solid:>9}",
            bounds.min_x + RADIUS,
            moved,
            df.world.chunk_count()
        );
    }
    let _ = cz;
    Ok(())
}
