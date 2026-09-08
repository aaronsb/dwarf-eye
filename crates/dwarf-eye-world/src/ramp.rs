//! Ramps as a corner-height field, after vox-uristi, textured with DF's own
//! ramp sprite.
//!
//! A DF ramp carries no direction: `Tiletype::direction` is `--------` for
//! terrain ramps, so the slope has to come from the neighbours. vox-uristi never
//! decides on a facing at all. It gives each of the eight neighbours a height
//! contribution — 6 for a wall, 1 for anything else — takes each corner as the
//! max of its diagonal and the two cardinals flanking it, and lets the slope
//! fall out of the resulting 3x3 field. An inside corner rises on two sides, a
//! lone diagonal wall lifts one corner, and every one of the 256 neighbour sets
//! resolves without a table.
//!
//! DF's own art is built on the same idea: `STONE_RAMP_WITH_WALL_N_W` and its
//! forty-six siblings are named by exactly this wall set, with diagonals that a
//! cardinal already covers left out — the same reduction the max rule makes. So
//! the sprite that matches a mask always agrees with the geometry built from it.

use crate::mesh::{MeshData, Z_SCALE};
use dfhack_remote::rfr::TiletypeMaterial;
use dwarf_eye_art::Sprite;
use dwarf_eye_art::atlas::Rect;

/// Neighbour bits, clockwise from north. North is -y in DF, -z in world space.
pub const N: u8 = 1 << 0;
pub const NE: u8 = 1 << 1;
pub const E: u8 = 1 << 2;
pub const SE: u8 = 1 << 3;
pub const S: u8 = 1 << 4;
pub const SW: u8 = 1 << 5;
pub const W: u8 = 1 << 6;
pub const NW: u8 = 1 << 7;

/// The eight neighbours as `(bit, dx, dy)` in DF's x-east / y-south grid.
pub const NEIGHBOURS: [(u8, i32, i32); 8] = [
    (N, 0, -1),
    (NE, 1, -1),
    (E, 1, 0),
    (SE, 1, 1),
    (S, 0, 1),
    (SW, -1, 1),
    (W, -1, 0),
    (NW, -1, -1),
];

/// vox-uristi's voxel column height. Its integer arithmetic carries over, so
/// the half-steps land where its exported models put them.
const HEIGHT: u32 = 5;
/// A wall's contribution. Six exceeds `HEIGHT`, so the column saturates and a
/// second wall on the same corner changes nothing.
const WALL: u32 = 6;
/// Anything that is not a wall, including a missing tile.
const OPEN: u32 = 1;

/// The 3x3 height field for a neighbour set, indexed `[row][col]` with row 0
/// north and column 0 west.
pub fn levels(mask: u8) -> [[u32; 3]; 3] {
    let c = |bit: u8| if mask & bit != 0 { WALL } else { OPEN };
    let nw = c(NW).max(c(N)).max(c(W));
    let ne = c(NE).max(c(N)).max(c(E));
    let sw = c(SW).max(c(S)).max(c(W));
    let se = c(SE).max(c(S)).max(c(E));
    let peak = nw.max(ne).max(sw).max(se);
    [
        [nw, (nw + ne) / 2, ne],
        [(nw + sw) / 2, peak / 2, (ne + se) / 2],
        [sw, (sw + se) / 2, se],
    ]
}

/// A level as a height in world units, from the floor slab to a full z-level.
///
/// vox-uristi fills `level` of `HEIGHT` voxel layers; here the same range is
/// stretched so an unraised corner sits exactly on the floor slabs beside it
/// and a saturated one reaches the wall above.
fn height(level: u32, floor: f32) -> f32 {
    let t = level.min(HEIGHT).saturating_sub(1) as f32 / (HEIGHT - 1) as f32;
    (floor + t * (1.0 - floor)) * Z_SCALE
}

/// Whether a mask leans on nothing, and so has no slope to build.
pub fn is_flat(mask: u8) -> bool {
    mask == 0
}

/// The suffix of DF's ramp sprite for this neighbour set.
///
/// Cardinals come first in DF's `N S W E` order, then only those diagonals no
/// present cardinal already covers — DF ships no `..._N_NW`, because a north
/// wall has already raised that corner. All four cardinals is spelled
/// `N_S_E_W`, the one name that breaks the order.
pub fn sprite_suffix(mask: u8) -> String {
    const CARDINALS: [(u8, &str); 4] = [(N, "N"), (S, "S"), (W, "W"), (E, "E")];
    const DIAGONALS: [(u8, u8, &str); 4] = [
        (NW, N | W, "NW"),
        (NE, N | E, "NE"),
        (SW, S | W, "SW"),
        (SE, S | E, "SE"),
    ];

    if CARDINALS.iter().all(|(bit, _)| mask & bit != 0) {
        return "WITH_WALL_N_S_E_W".to_string();
    }

    let mut tokens: Vec<&str> = CARDINALS
        .iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    tokens.extend(
        DIAGONALS
            .iter()
            .filter(|(bit, flanks, _)| mask & bit != 0 && mask & flanks == 0)
            .map(|(_, _, name)| *name),
    );

    if tokens.is_empty() {
        "OTHER".to_string()
    } else {
        format!("WITH_WALL_{}", tokens.join("_"))
    }
}

/// The full sprite name for a ramp family and neighbour set.
pub fn sprite_name(family: &str, mask: u8) -> String {
    format!("{family}_{}", sprite_suffix(mask))
}

/// Every distinct ramp sprite name for one family — 47 of them, the whole set
/// DF ships, so the atlas can be filled before the first frame.
pub fn sprite_names(family: &str) -> Vec<String> {
    let mut names: Vec<String> = (0..=u8::MAX).map(|m| sprite_name(family, m)).collect();
    names.sort();
    names.dedup();
    names
}

/// The ramp sprite sheet a tiletype's material draws from.
///
/// DF also ships six sand sheets, but a sand ramp still reports `SOIL`, so
/// nothing in the tiletype distinguishes them.
pub fn family_for(material: TiletypeMaterial) -> &'static str {
    use TiletypeMaterial as M;
    match material {
        M::GrassLight | M::GrassDark | M::GrassDry | M::GrassDead | M::Plant | M::Mushroom => {
            "GRASS_RAMP"
        }
        M::Stone
        | M::Mineral
        | M::LavaStone
        | M::Feature
        | M::Construction
        | M::FrozenLiquid
        | M::Hfs
        | M::Root
        | M::TreeMaterial => "STONE_RAMP",
        _ => "SOIL_RAMP",
    }
}

/// Turns a ramp sprite into a shading pattern for the tile's own material.
///
/// DF paints a stone slope's shadow in a deep blue that reads as water on a
/// sunlit hillside, and paints every stone the same. Where the ground beside
/// the ramp is a pattern the material colours, the ramp becomes one too: its
/// shading survives as brightness, its hue does not. The strongest channel
/// stands in for that brightness, so a shadow that was darkened by hue-shifting
/// does not collapse to black.
pub fn neutralise(sprite: &Sprite) -> Sprite {
    let pixels = sprite
        .pixels
        .iter()
        .map(|p| {
            let value = p[0].max(p[1]).max(p[2]);
            [value, value, value, p[3]]
        })
        .collect();
    Sprite { width: sprite.width, height: sprite.height, pixels }
}

fn shade(color: [f32; 4], factor: f32) -> [f32; 4] {
    [color[0] * factor, color[1] * factor, color[2] * factor, color[3]]
}

/// Appends one triangle, facing whichever way its winding says.
fn push_tri(mesh: &mut MeshData, verts: [([f32; 3], [f32; 2]); 3], color: [f32; 4]) {
    let [a, b, c] = [verts[0].0, verts[1].0, verts[2].0];
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let mut n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 0.0 {
        n = [n[0] / len, n[1] / len, n[2] / len];
    } else {
        n = [0.0, 1.0, 0.0];
    }

    let base = mesh.positions.len() as u32;
    for (position, uv) in verts {
        mesh.positions.push(position);
        mesh.normals.push(n);
        mesh.colors.push(color);
        mesh.uvs.push(uv);
    }
    mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
}

/// A ramp's surface and skirt for one neighbour set.
///
/// The surface is a 3x3 vertex grid — corners, edge midpoints and centre —
/// lifted onto the height field and cut into eight triangles, each quadrant
/// split along the diagonal that runs from its tile corner to the centre so a
/// one-corner ramp folds along its own diagonal. The sprite is stretched across
/// the whole tile, north at `v0`, as the flat tiles do it.
///
/// Every side that does not face a wall gets a skirt down to the level below,
/// because the neighbouring tile there is a floor slab a fraction of a
/// z-level tall and the drop under a raised corner would otherwise be a hole.
pub fn build_ramp(uv: Rect, mask: u8, floor: f32) -> MeshData {
    let levels = levels(mask);
    let mut mesh = MeshData::default();
    let white = [1.0, 1.0, 1.0, 1.0];

    // Grid position, height and UV of one of the nine vertices.
    let vertex = |row: usize, col: usize| -> ([f32; 3], [f32; 2]) {
        let (x, z) = (col as f32 * 0.5, row as f32 * 0.5);
        (
            [x, height(levels[row][col], floor), z],
            [
                uv.u0 + (uv.u1 - uv.u0) * x,
                uv.v0 + (uv.v1 - uv.v0) * z,
            ],
        )
    };

    for row in 0..2 {
        for col in 0..2 {
            // Wound counter-clockwise seen from above, as the flat tile is.
            let a = vertex(row, col);
            let b = vertex(row + 1, col);
            let c = vertex(row + 1, col + 1);
            let d = vertex(row, col + 1);
            if row == col {
                // North-west and south-east quadrants: the centre is a-c.
                push_tri(&mut mesh, [a, b, c], white);
                push_tri(&mut mesh, [a, c, d], white);
            } else {
                push_tri(&mut mesh, [a, b, d], white);
                push_tri(&mut mesh, [b, c, d], white);
            }
        }
    }

    // Skirts, walked so each side's points run counter-clockwise seen from
    // outside the tile. Each column repeats the texel above it, so the face
    // reads as the same ground rather than as a white wall.
    let side = shade(white, 0.72);
    let sides: [(u8, [(usize, usize); 3], [f32; 3]); 4] = [
        (N, [(0, 0), (0, 1), (0, 2)], [0.0, 0.0, -1.0]),
        (S, [(2, 2), (2, 1), (2, 0)], [0.0, 0.0, 1.0]),
        (W, [(2, 0), (1, 0), (0, 0)], [-1.0, 0.0, 0.0]),
        (E, [(0, 2), (1, 2), (2, 2)], [1.0, 0.0, 0.0]),
    ];
    for (bit, points, normal) in sides {
        if mask & bit != 0 {
            // A wall stands here; the skirt would be buried inside it.
            continue;
        }
        for pair in points.windows(2) {
            let (a, uv_a) = vertex(pair[0].0, pair[0].1);
            let (b, uv_b) = vertex(pair[1].0, pair[1].1);
            mesh.push_textured_quad(
                [[a[0], 0.0, a[2]], a, b, [b[0], 0.0, b[2]]],
                normal,
                side,
                [uv_a, uv_a, uv_b, uv_b],
            );
        }
    }

    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use dwarf_eye_art::raws::parse_part;
    use std::collections::HashSet;

    #[test]
    fn df_ships_one_sprite_per_distinct_mask() {
        // The 47 names are what DF's ramp sheets hold; anything else means the
        // reduction has drifted from the one the art was cut with.
        assert_eq!(sprite_names("STONE_RAMP").len(), 47);
    }

    #[test]
    fn sprite_names_survive_the_raws_parser() {
        // `parse_part` eats a trailing direction group, so two sprite names
        // could in principle land on the same index key. They do not.
        let keys: HashSet<_> = sprite_names("STONE_RAMP")
            .iter()
            .map(|name| parse_part(name))
            .collect();
        assert_eq!(keys.len(), 47);
    }

    #[test]
    fn a_diagonal_wall_lifts_one_corner() {
        let l = levels(NW);
        assert_eq!(l[0][0], WALL);
        assert_eq!(l[2][2], OPEN);
        // And a cardinal already covering it changes nothing.
        assert_eq!(levels(NW | N | W), levels(N | W));
    }

    #[test]
    fn an_inside_corner_rises_on_two_sides() {
        let l = levels(N | W);
        assert_eq!([l[0][0], l[0][2], l[2][0]], [WALL, WALL, WALL]);
        assert_eq!(l[2][2], OPEN);
    }

    #[test]
    fn a_straight_ramp_is_a_plane() {
        let l = levels(N);
        // North edge full, middle row halfway, south edge on the floor.
        assert_eq!(l[0], [WALL, WALL, WALL]);
        assert_eq!(l[1], [3, 3, 3]);
        assert_eq!(l[2], [OPEN, OPEN, OPEN]);
    }

    #[test]
    fn every_mask_builds_a_closed_surface() {
        let uv = Rect { u0: 0.0, v0: 0.0, u1: 1.0, v1: 1.0 };
        for mask in 1..=u8::MAX {
            let mesh = build_ramp(uv, mask, 0.12);
            assert!(mesh.triangle_count() >= 8, "mask {mask} lost its surface");
            for p in &mesh.positions {
                assert!(p[1] >= 0.0 && p[1] <= Z_SCALE, "mask {mask} left the tile");
            }
        }
    }
}
