//! Rivers and site buildings: the two things the region survey draws that are
//! not ground.

use crate::mesh::{MeshData, Z_SCALE};
use crate::palette::Rgb;
use dfhack_remote::rfr::{RiverEdge, RiverTile, SiteRealizationBuilding};

use super::shade::{BUILDING, RIVER, mix, to_linear};
use super::terrace::Terrain;
use super::{REGION_TILE, Window};

/// River edges with no data read back as this sentinel rather than absent.
const RIVER_SENTINEL: i32 = -30000;

/// River edges with no data read back as a `-30000` sentinel rather than being
/// absent, so a live edge is one that differs from it and has real width.
fn river_edge_valid(edge: &RiverEdge) -> bool {
    edge.min_pos() != RIVER_SENTINEL && edge.max_pos() != RIVER_SENTINEL && edge.max_pos() > edge.min_pos()
}

enum Border {
    North,
    South,
    East,
    West,
}

/// The two endpoints of an edge's span, in local tile coordinates (0..48).
fn border_points(border: Border, lo: f32, hi: f32) -> ([f32; 2], [f32; 2]) {
    let side = REGION_TILE as f32;
    match border {
        Border::North => ([lo, 0.0], [hi, 0.0]),
        Border::South => ([lo, side], [hi, side]),
        Border::West => ([0.0, lo], [0.0, hi]),
        Border::East => ([side, lo], [side, hi]),
    }
}

/// Orders three points so they wind the way this module's ground quads do, so
/// a river strip isn't culled from above.
fn ccw_order(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> ([f32; 2], [f32; 2], [f32; 2]) {
    let cross = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    if cross > 0.0 { (a, c, b) } else { (a, b, c) }
}

/// A flat strip for each river edge a region tile carries, its width the
/// edge's own `min_pos`..`max_pos` span, converging on the tile centre: this
/// both connects two edges that pass through the same tile and gives a
/// one-edge tile (a spring or a river mouth) a sensible taper.
pub fn emit_river(mesh: &mut MeshData, window: &Window, rx: i32, ry: i32, river: &RiverTile) {
    let ox = (rx * REGION_TILE - window.origin.0) as f32;
    let oz = (ry * REGION_TILE - window.origin.1) as f32;
    let side = REGION_TILE as f32;
    let center = [ox + side * 0.5, oz + side * 0.5];
    let color = to_linear(RIVER);
    for (edge, border) in [
        (river.north.as_ref(), Border::North),
        (river.south.as_ref(), Border::South),
        (river.east.as_ref(), Border::East),
        (river.west.as_ref(), Border::West),
    ] {
        let Some(edge) = edge else { continue };
        if !river_edge_valid(edge) {
            continue;
        }
        let (lo, hi) = border_points(border, edge.min_pos() as f32, edge.max_pos() as f32);
        let a = [ox + lo[0], oz + lo[1]];
        let b = [ox + hi[0], oz + hi[1]];
        // A river runs in the valley floor, so it sits at its own elevation's
        // slab top rather than on the terrace beside it.
        let y = window.height(edge.elevation()) + 0.15;
        let (p0, p1, p2) = ccw_order(a, b, center);
        mesh.push_quad(
            [[p0[0], y, p0[1]], [p1[0], y, p1[1]], [p2[0], y, p2[1]], [p2[0], y, p2[1]]],
            [0.0, 1.0, 0.0],
            color,
        );
    }
}

/// A building's footprint in render-local tile coordinates: the region tile's
/// origin plus its bounds, which are tiles relative to that tile.
pub fn building_bounds(window: &Window, rx: i32, ry: i32, building: &SiteRealizationBuilding) -> (i32, i32, i32, i32) {
    let ox = rx * REGION_TILE - window.origin.0;
    let oy = ry * REGION_TILE - window.origin.1;
    let (x0, x1) = (building.min_x().min(building.max_x()), building.min_x().max(building.max_x()));
    let (y0, y1) = (building.min_y().min(building.max_y()), building.min_y().max(building.max_y()));
    (ox + x0, oy + y0, ox + x1, oy + y1)
}

/// A ground-sitting box for a site building: walls at 3 levels, a tower up to
/// its own `roof_z` where DF gives one, a trench as a shallow slab. It stands
/// on the terrace under its own footprint, so a wall on a hillside starts at
/// the ground beside it.
pub fn emit_building(
    mesh: &mut MeshData,
    terrain: &Terrain,
    window: &Window,
    rx: i32,
    ry: i32,
    building: &SiteRealizationBuilding,
    stone: Rgb,
) {
    let (x0, z0, x1, z1) = building_bounds(window, rx, ry, building);
    if x1 - x0 < 1 || z1 - z0 < 1 {
        return;
    }
    // The lowest ground the footprint stands on, so the box never floats.
    let mut ground = f32::MAX;
    for (tx, tz) in [(x0, z0), (x1, z0), (x0, z1), (x1, z1), ((x0 + x1) / 2, (z0 + z1) / 2)] {
        if let Some(level) = terrain.level_at(tx, tz) {
            ground = ground.min(terrain.slab_top(level));
        }
    }
    if ground == f32::MAX {
        return;
    }
    let height = match (&building.tower_info, &building.trench_info) {
        (Some(tower), _) => tower.roof_z.map(|z| (z as f32).clamp(4.0, 40.0)).unwrap_or(8.0),
        (_, Some(_)) => 0.4,
        _ => 3.0,
    } * Z_SCALE;
    push_box(mesh, [x0 as f32, x1 as f32], [ground, ground + height], [z0 as f32, z1 as f32], stone);
}

/// A site's masonry colour: its own stone, pulled toward a neutral so a site
/// of bright ore does not glow, and toward `BUILDING` where DF names none.
pub fn stone_color(stone: Option<Rgb>) -> Rgb {
    match stone {
        Some(c) => mix(c, BUILDING, 0.45),
        None => BUILDING,
    }
}

/// A box open on the bottom, since it always sits on the ground: a top cap and
/// four walls, wound the same way as the tile mesher's caps so back-face
/// culling doesn't eat one.
fn push_box(mesh: &mut MeshData, [x0, x1]: [f32; 2], [y0, y1]: [f32; 2], [z0, z1]: [f32; 2], color: Rgb) {
    let c = to_linear(color);
    mesh.push_quad([[x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]], [0.0, 1.0, 0.0], c);
    mesh.push_quad([[x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0]], [0.0, 0.0, -1.0], c);
    mesh.push_quad([[x1, y0, z1], [x1, y1, z1], [x0, y1, z1], [x0, y0, z1]], [0.0, 0.0, 1.0], c);
    mesh.push_quad([[x0, y0, z1], [x0, y1, z1], [x0, y1, z0], [x0, y0, z0]], [-1.0, 0.0, 0.0], c);
    mesh.push_quad([[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]], [1.0, 0.0, 0.0], c);
}
