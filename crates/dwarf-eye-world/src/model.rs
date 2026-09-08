//! Turns a sprite's alpha channel into tile-sized geometry.
//!
//! A DF tile sprite is drawn looking straight down, so its opaque region is a
//! horizontal cross-section of whatever occupies the tile. Extruding that mask
//! gives real shape for free: the trunk sprite is a disc, so the trunk comes out
//! round rather than square.

use crate::mesh::{MeshData, Z_SCALE};
use dwarf_eye_art::Grid;
use dwarf_eye_art::atlas::Rect;

/// How a tile's mask becomes geometry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RenderMode {
    /// Full-height extrusion: trunks, cap walls, anything with a vertical run.
    Extrude,
    /// A thin horizontal slab: branches and twigs are limbs, not columns.
    ThinExtrude,
    /// Two crossed vertical planes: saplings, shrubs, boulders.
    Billboard,
    /// A textured slab on the floor: pebbles, floors, grass.
    FlatTile,
}

impl RenderMode {
    fn vertical_span(self) -> (f32, f32) {
        match self {
            RenderMode::Extrude => (0.0, 1.0),
            RenderMode::ThinExtrude => (0.34, 0.66),
            RenderMode::FlatTile => (0.0, 0.12),
            RenderMode::Billboard => (0.0, 1.0),
        }
    }
}

/// Which caps a tile needs, decided by what sits above and below it.
///
/// A trunk with more trunk above it has no visible top, so the cap is both wrong
/// and expensive. Dropping it is what keeps a forest affordable.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Caps {
    pub top: bool,
    pub bottom: bool,
}

impl Caps {
    pub const BOTH: Caps = Caps { top: true, bottom: true };
}

fn to_linear(rgb: [u8; 3]) -> [f32; 4] {
    let f = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    [f(rgb[0]), f(rgb[1]), f(rgb[2]), 1.0]
}

fn shade(color: [f32; 4], factor: f32) -> [f32; 4] {
    [color[0] * factor, color[1] * factor, color[2] * factor, color[3]]
}

/// A textured slab on the ground: one quad for the surface, four thin sides.
///
/// The sprite is a picture rather than a cross-section, so it is sampled from
/// the atlas instead of voxelised. Twelve triangles replace about three hundred,
/// and the texture keeps its full resolution.
pub fn build_flat_tile(uv: Rect, height: f32) -> MeshData {
    let mut mesh = MeshData::default();
    let y = height * Z_SCALE;
    let white = [1.0, 1.0, 1.0, 1.0];

    mesh.push_textured_quad(
        [[0.0, y, 0.0], [0.0, y, 1.0], [1.0, y, 1.0], [1.0, y, 0.0]],
        [0.0, 1.0, 0.0],
        white,
        [
            [uv.u0, uv.v0],
            [uv.u0, uv.v1],
            [uv.u1, uv.v1],
            [uv.u1, uv.v0],
        ],
    );

    // The rim is too shallow to be worth texturing; shade it off the surface.
    let side = shade(white, 0.72);
    mesh.push_quad([[0.0, 0.0, 0.0], [0.0, y, 0.0], [1.0, y, 0.0], [1.0, 0.0, 0.0]], [0.0, 0.0, -1.0], side);
    mesh.push_quad([[1.0, 0.0, 1.0], [1.0, y, 1.0], [0.0, y, 1.0], [0.0, 0.0, 1.0]], [0.0, 0.0, 1.0], side);
    mesh.push_quad([[0.0, 0.0, 1.0], [0.0, y, 1.0], [0.0, y, 0.0], [0.0, 0.0, 0.0]], [-1.0, 0.0, 0.0], side);
    mesh.push_quad([[1.0, 0.0, 0.0], [1.0, y, 0.0], [1.0, y, 1.0], [1.0, 0.0, 1.0]], [1.0, 0.0, 0.0], side);

    mesh
}

/// Builds a tile's geometry in local space: x and z span 0..1, y spans the
/// tile's height. The mesher translates it into place.
pub fn build_model(grid: &Grid, mode: RenderMode, caps: Caps) -> MeshData {
    let mut mesh = MeshData::default();
    if mode == RenderMode::Billboard {
        build_billboard(&mut mesh, grid);
        return mesh;
    }

    let n = grid.size as i32;
    let step = 1.0 / grid.size as f32;
    let (lo, hi) = mode.vertical_span();
    let (y0, y1) = (lo * Z_SCALE, hi * Z_SCALE);

    // Caps: merge runs along x so a solid disc costs rows, not cells.
    for (draw, y, normal, factor) in [
        (caps.top, y1, [0.0, 1.0, 0.0], 1.0),
        (caps.bottom, y0, [0.0, -1.0, 0.0], 0.55),
    ] {
        if !draw {
            continue;
        }
        for gy in 0..n {
            let mut x = 0;
            while x < n {
                let cell = grid.get(x as u32, gy as u32);
                if !cell.solid {
                    x += 1;
                    continue;
                }
                let mut run = 1;
                while x + run < n {
                    let next = grid.get((x + run) as u32, gy as u32);
                    if !next.solid || next.color != cell.color {
                        break;
                    }
                    run += 1;
                }
                let (x0, x1) = (x as f32 * step, (x + run) as f32 * step);
                let (z0, z1) = (gy as f32 * step, (gy + 1) as f32 * step);
                let color = shade(to_linear(cell.color), factor);
                if normal[1] > 0.0 {
                    mesh.push_quad([[x0, y, z0], [x0, y, z1], [x1, y, z1], [x1, y, z0]], normal, color);
                } else {
                    mesh.push_quad([[x0, y, z0], [x1, y, z0], [x1, y, z1], [x0, y, z1]], normal, color);
                }
                x += run;
            }
        }
    }

    // Sides: one quad per exposed cell edge, merged vertically by construction
    // because every column spans the same height.
    for gy in 0..n {
        for gx in 0..n {
            let cell = grid.get(gx as u32, gy as u32);
            if !cell.solid {
                continue;
            }
            let (x0, x1) = (gx as f32 * step, (gx + 1) as f32 * step);
            let (z0, z1) = (gy as f32 * step, (gy + 1) as f32 * step);

            if !grid.solid(gx, gy - 1) {
                mesh.push_quad(
                    [[x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0]],
                    [0.0, 0.0, -1.0],
                    shade(to_linear(cell.color), 0.8),
                );
            }
            if !grid.solid(gx, gy + 1) {
                mesh.push_quad(
                    [[x1, y0, z1], [x1, y1, z1], [x0, y1, z1], [x0, y0, z1]],
                    [0.0, 0.0, 1.0],
                    shade(to_linear(cell.color), 0.8),
                );
            }
            if !grid.solid(gx - 1, gy) {
                mesh.push_quad(
                    [[x0, y0, z1], [x0, y1, z1], [x0, y1, z0], [x0, y0, z0]],
                    [-1.0, 0.0, 0.0],
                    shade(to_linear(cell.color), 0.68),
                );
            }
            if !grid.solid(gx + 1, gy) {
                mesh.push_quad(
                    [[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]],
                    [1.0, 0.0, 0.0],
                    shade(to_linear(cell.color), 0.68),
                );
            }
        }
    }

    mesh
}

/// Two crossed vertical planes, for objects that stand alone in their tile.
///
/// The sprite is read as a picture rather than a cross-section here: its rows
/// become height. Runs of one colour merge along a row, which matters because a
/// meadow can hold thousands of these.
fn build_billboard(mesh: &mut MeshData, grid: &Grid) {
    let n = grid.size as i32;
    let step = 1.0 / grid.size as f32;
    let thickness = step * 1.5;
    let (near, far) = (0.5 - thickness * 0.5, 0.5 + thickness * 0.5);

    for gy in 0..n {
        // Sprite rows run top-down; world height runs the other way.
        let (y0, y1) = (
            (n - 1 - gy) as f32 * step * Z_SCALE,
            (n - gy) as f32 * step * Z_SCALE,
        );

        let mut x = 0;
        while x < n {
            let cell = grid.get(x as u32, gy as u32);
            if !cell.solid {
                x += 1;
                continue;
            }
            let mut run = 1;
            while x + run < n {
                let next = grid.get((x + run) as u32, gy as u32);
                if !next.solid || next.color != cell.color {
                    break;
                }
                run += 1;
            }
            let (a0, a1) = (x as f32 * step, (x + run) as f32 * step);
            let color = to_linear(cell.color);

            for face in 0..2 {
                let (front, back, normal) = if face == 0 {
                    (
                        [[a0, y0, near], [a0, y1, near], [a1, y1, near], [a1, y0, near]],
                        [[a1, y0, far], [a1, y1, far], [a0, y1, far], [a0, y0, far]],
                        [0.0, 0.0, -1.0],
                    )
                } else {
                    (
                        [[near, y0, a1], [near, y1, a1], [near, y1, a0], [near, y0, a0]],
                        [[far, y0, a0], [far, y1, a0], [far, y1, a1], [far, y0, a1]],
                        [-1.0, 0.0, 0.0],
                    )
                };
                mesh.push_quad(front, normal, shade(color, 0.92));
                mesh.push_quad(back, [-normal[0], 0.0, -normal[2]], shade(color, 0.78));
            }
            x += run;
        }
    }
}
