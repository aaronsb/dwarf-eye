//! A tree reassembled from the tiles DFHack reports.
//!
//! Every tile of a tree carries the offset to the tree's origin, so tiles can
//! be grouped back into the tree they grew on however they arrived. Each tile
//! also carries which of its four neighbours it joins, which is what turns a
//! cloud of branch tiles into a graph of limbs.
//!
//! Parts are gathered per chunk with a halo, and everything about a part comes
//! from that part alone, so two chunks either side of a seam describe the tiles
//! they share identically.

use crate::canopy::CanopyPart;
use crate::library::TileLibrary;
use crate::mesh::MeshOptions;
use crate::world::{BLOCK, Chunk, World};
use dwarf_eye_art::raws;

/// One tile of a tree, with what it joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Part {
    pub pos: (i32, i32, i32),
    pub kind: CanopyPart,
    /// Neighbours this tile joins, as N/S/W/E bits from `dwarf_eye_art::raws`.
    pub links: u8,
    /// The tree this tile grew on, for seeding variation that holds still.
    pub tree: (i32, i32, i32),
    /// Plant index, which selects the species.
    pub species: i32,
}

/// Turns DFHack's direction string into the neighbours a branch joins.
///
/// The string means two different things depending on its length. Two or more
/// letters is the connection set directly. A single letter is the direction the
/// branch *heads*, so what it joins is the opposite one — `TreeBranchN` grows
/// north from the tile south of it.
pub fn links_from_direction(direction: &str) -> u8 {
    let bits = raws::direction_mask(direction);
    if bits.count_ones() != 1 {
        return bits;
    }
    match bits {
        raws::NORTH => raws::SOUTH,
        raws::SOUTH => raws::NORTH,
        raws::WEST => raws::EAST,
        _ => raws::WEST,
    }
}

/// The tile offset a direction bit points at, in DF's x-east / y-south grid.
pub fn step(bit: u8) -> (i32, i32) {
    match bit {
        raws::NORTH => (0, -1),
        raws::SOUTH => (0, 1),
        raws::WEST => (-1, 0),
        _ => (1, 0),
    }
}

/// Collects the tree parts that reach into a chunk, out to `reach` tiles.
///
/// Sorted by position, so a chunk describes the same parts in the same order
/// however its neighbours arrived.
pub fn gather(
    world: &World,
    chunk: &Chunk,
    opts: MeshOptions,
    library: &TileLibrary,
    reach: i32,
) -> Vec<Part> {
    let (ox, oy, oz) = chunk.origin();
    let mut parts = Vec::new();

    for z in (oz - reach)..=(oz + reach) {
        if z > opts.z_ceiling {
            continue;
        }
        for bx in (ox - reach).div_euclid(BLOCK)..=(ox + BLOCK - 1 + reach).div_euclid(BLOCK) {
            for by in (oy - reach).div_euclid(BLOCK)..=(oy + BLOCK - 1 + reach).div_euclid(BLOCK) {
                let Some(near) = world.chunk(bx, by, z) else { continue };
                for ly in 0..BLOCK {
                    for lx in 0..BLOCK {
                        let (x, y) = (bx * BLOCK + lx, by * BLOCK + ly);
                        if x < ox - reach
                            || x >= ox + BLOCK + reach
                            || y < oy - reach
                            || y >= oy + BLOCK + reach
                        {
                            continue;
                        }
                        let v = near.get(lx, ly);
                        if v.hidden && !opts.show_hidden {
                            continue;
                        }
                        let Some(kind) = library.canopy_part(v.tile_id) else { continue };
                        parts.push(Part {
                            pos: (x, y, z),
                            kind,
                            links: library.branch_links(v.tile_id),
                            tree: v.tree_origin(x, y, z),
                            species: v.mat_index,
                        });
                    }
                }
            }
        }
    }
    parts.sort_unstable_by_key(|p| p.pos);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_letter_names_the_way_a_branch_heads() {
        // TreeBranchN grows north, so it joins the tile to its south.
        assert_eq!(links_from_direction("N-------"), raws::SOUTH);
        assert_eq!(links_from_direction("----W---"), raws::EAST);
    }

    #[test]
    fn several_letters_are_the_connections_themselves() {
        assert_eq!(
            links_from_direction("N-S-W-E-"),
            raws::NORTH | raws::SOUTH | raws::WEST | raws::EAST
        );
        assert_eq!(links_from_direction("N-----E-"), raws::NORTH | raws::EAST);
    }

    #[test]
    fn no_letters_joins_nothing() {
        assert_eq!(links_from_direction("--------"), 0);
    }
}
