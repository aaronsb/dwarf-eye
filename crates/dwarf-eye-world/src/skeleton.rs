//! How DFHack describes which way a tree tile connects.
//!
//! Geometry no longer follows this graph — trees are grown inside an envelope
//! instead, in `tree.rs` — but the reading itself is subtle enough to be worth
//! keeping, and anything that draws branch sprites will want it.

use dwarf_eye_art::raws;

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
