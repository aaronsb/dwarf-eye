//! A tree reassembled from the tiles DFHack reports.
//!
//! Every tile of a tree carries the offset to the tree's origin, so tiles can
//! be grouped back into the tree they grew on however they arrived. Each tile
//! also carries which of its four neighbours it joins, which is what turns a
//! cloud of branch tiles into a graph of limbs.
//!
//! Parts are gathered per chunk with a halo, and everything a part knows comes
//! from that part and its immediate neighbours, so two chunks either side of a
//! seam describe the tiles they share identically.

use crate::canopy::CanopyPart;
use crate::library::TileLibrary;
use crate::mesh::MeshOptions;
use crate::world::{BLOCK, Chunk, World};
use dwarf_eye_art::raws;
use std::collections::{HashMap, HashSet};

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

/// The tree tiles that reach into one chunk, and where they sit relative to
/// each other.
pub struct Skeleton {
    pub parts: Vec<Part>,
    filled: HashSet<(i32, i32, i32)>,
    /// Trunk tiles, which the crown hangs on. Their geometry comes from the
    /// sprite models, but the ones standing among the crown still want leaves
    /// over them, so each keeps its tree and species.
    trunk: HashMap<(i32, i32, i32), ((i32, i32, i32), i32)>,
}

impl Skeleton {
    /// Collects the tree tiles within `reach` tiles of a chunk.
    ///
    /// Sorted by position, so a chunk describes the same parts in the same
    /// order however its neighbours arrived.
    pub fn build(
        world: &World,
        chunk: &Chunk,
        opts: MeshOptions,
        library: &TileLibrary,
        reach: i32,
    ) -> Self {
        let (ox, oy, oz) = chunk.origin();
        let mut parts = Vec::new();
        let mut trunk = HashMap::new();

        for z in (oz - reach)..=(oz + reach) {
            if z > opts.z_ceiling {
                continue;
            }
            for bx in (ox - reach).div_euclid(BLOCK)..=(ox + BLOCK - 1 + reach).div_euclid(BLOCK) {
                for by in (oy - reach).div_euclid(BLOCK)..=(oy + BLOCK - 1 + reach).div_euclid(BLOCK)
                {
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
                            if library.is_trunk(v.tile_id) {
                                trunk.insert(
                                    (x, y, z),
                                    (v.tree_origin(x, y, z), v.mat_index),
                                );
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
        let filled = parts.iter().map(|p| p.pos).collect();
        Self { parts, filled, trunk }
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Whether a tile holds crown.
    pub fn holds(&self, pos: (i32, i32, i32)) -> bool {
        self.filled.contains(&pos)
    }

    /// Whether a tile holds the woody column.
    pub fn is_trunk(&self, pos: (i32, i32, i32)) -> bool {
        self.trunk.contains_key(&pos)
    }

    /// Trunk tiles with crown beside them, and the tree and species each
    /// belongs to. These are where a trunk runs up through the leaves, so the
    /// crown closes over them instead of leaving a capped stump in the open.
    pub fn crowned_trunks(&self) -> Vec<((i32, i32, i32), (i32, i32, i32), i32)> {
        let mut found: Vec<_> = self
            .trunk
            .iter()
            .filter(|((x, y, z), _)| {
                (-1..=1).any(|dx| {
                    (-1..=1).any(|dy| {
                        (-1..=1).any(|dz| self.filled.contains(&(x + dx, y + dy, z + dz)))
                    })
                })
            })
            .map(|(&pos, &(tree, species))| (pos, tree, species))
            .collect();
        found.sort_unstable_by_key(|(pos, ..)| *pos);
        found
    }

    /// How much of a tile's six faces open onto nothing.
    ///
    /// Zero deep inside the crown, where leaves would never be seen, and it
    /// rises toward the outside. Clump density follows it, which is what keeps
    /// the crown a shell around an open frame of limbs rather than a solid
    /// block.
    pub fn openness(&self, pos: (i32, i32, i32)) -> f32 {
        let (x, y, z) = pos;
        let open = [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)]
            .into_iter()
            .filter(|&(dx, dy, dz)| {
                let at = (x + dx, y + dy, z + dz);
                !self.filled.contains(&at) && !self.trunk.contains_key(&at)
            })
            .count();
        open as f32 / 6.0
    }
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
