//! Reports where the triangles go, over the live window.

use anyhow::Result;
use dfhack_remote::{methods, rfr};
use dwarf_eye_world::canopy::{Band, CanopyBudget, Forest, MID_DETAIL};
use dwarf_eye_world::library::TileLibrary;
use dwarf_eye_world::mesh::{Budget, build_chunk_budgeted};
use dwarf_eye_world::{MeshOptions, Session};

fn main() -> Result<()> {
    let mut df = Session::connect_local()?;
    let tiletypes: rfr::TiletypeList = df.client.call_empty(methods::GET_TILETYPE_LIST)?;
    let plants: rfr::PlantRawList = df.client.call_empty(methods::GET_PLANT_RAWS)?;
    let mut library = TileLibrary::load(&tiletypes, &plants).ok();
    let view = df.view_center()?;
    let bounds = df.window_bounds(view.2 - 32, view.2 + 20);
    let fetched = df.fetch(bounds, true)?;
    println!("fetched {} blocks", fetched.len());

    let origin = df.origin();
    let opts = MeshOptions { z_ceiling: view.2 + 16, show_hidden: true };
    let mut total = Budget::default();
    let mut canopy = CanopyBudget::default();
    let mut mid = CanopyBudget::default();
    let mut forest = Forest::default();
    let mut chunks = 0;
    for chunk in df.world.chunks() {
        let mut budget = Budget::default();
        let _ = build_chunk_budgeted(&df.world, chunk, opts, library.as_mut(), &mut budget);
        total.models += budget.models;
        total.ramps += budget.ramps;
        total.ground_under += budget.ground_under;
        total.cubes += budget.cubes;
        total.floors += budget.floors;
        total.surface += budget.surface;
        total.foliage += budget.foliage;
        total.liquids += budget.liquids;
        total.other += budget.other;
        total.merged += budget.merged;
        if let Some(lib) = library.as_mut() {
            let _ = forest.build_budgeted(&df.world, chunk, opts, lib, origin, Band::Near, &mut canopy);
            let _ = forest.build_budgeted(&df.world, chunk, opts, lib, origin, Band::Mid, &mut mid);
        }
        chunks += 1;
    }
    // The classes are counted as the mesher pushes faces; the greedy merge
    // takes some back out afterwards (`mesh.rs:Plane`).
    let sum = canopy.triangles + total.models + total.ramps + total.ground_under + total.cubes + total.floors + total.surface + total.foliage + total.liquids + total.other;
    println!(
        "{chunks} chunks, {} triangles ({sum} pushed, {} merged away, {:.2}%)",
        sum - total.merged,
        total.merged,
        total.merged as f32 * 100.0 / sum.max(1) as f32,
    );
    for (name, n) in [
        ("trees", canopy.triangles),
        ("sprite models (trunks, shrubs, boulders)", total.models),
        ("ramps", total.ramps),
        ("ground under billboards", total.ground_under),
        ("cubes (walls, soil, trunks without sprites)", total.cubes),
        ("floors", total.floors),
        ("smoothed ground sheet", total.surface),
        ("foliage blocks", total.foliage),
        ("liquids", total.liquids),
        ("other", total.other),
    ] {
        println!("  {:>10}  {:>5.1}%  {name}", n, n as f32 * 100.0 / sum.max(1) as f32);
    }
    println!(
        "mid band at {MID_DETAIL} sub-voxels per tile: {} triangles, {:.2}x fewer than near",
        mid.triangles,
        canopy.triangles as f32 / mid.triangles.max(1) as f32,
    );
    println!(
        "trees at {} sub-voxels per tile: {} triangles merged, {} unmerged ({:.2}x)",
        dwarf_eye_world::tree::DETAIL,
        canopy.triangles,
        canopy.unmerged,
        canopy.unmerged as f32 / canopy.triangles.max(1) as f32,
    );
    if let Some(lib) = library.as_ref() {
        println!("  {} species carry a leaf cutout", lib.leaf_cells());
    }
    let (leaf, bark) = forest.voxel_counts();
    println!(
        "  {} trees grown, {} leaf voxels, {} bark voxels, bark is {:.1}% of leaves",
        forest.tree_count(),
        leaf,
        bark,
        bark as f32 * 100.0 / leaf.max(1) as f32,
    );
    Ok(())
}
