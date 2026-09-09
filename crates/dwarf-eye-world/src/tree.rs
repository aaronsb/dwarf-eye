//! Growing a tree inside the envelope Dwarf Fortress gives us.
//!
//! Following DF's tile skeleton literally produced trees that read as wrong,
//! because the game's branch tiles are a coarse connectivity graph rather than
//! a description of limbs. So the tiles are used only to say where a tree is
//! allowed to be: the union of its tiles per level, widened by half a tile, is
//! an envelope, and a small grammar grows a trunk and limbs inside it. Nothing
//! leaves the envelope, so the tree still occupies exactly what the game calls
//! tree.
//!
//! A tree is grown and voxelised once, keyed by its origin, and sliced per
//! chunk when it is meshed. Chunk boundaries therefore cannot change its shape.
//! Everything is seeded from the tree's absolute position, so a tree is the
//! same tree across a reload and wherever the render origin happens to sit.
//!
//! The grammar itself lives in `dwarf-eye-trees`, which knows nothing about
//! Dwarf Fortress. This module is the translation: DF tiles become that crate's
//! envelope, plant raws and sprite palettes become its parameters, and the
//! result comes back as voxels for `canopy.rs` to slice.
//!
//! The shape the game shows comes from five knobs, and the accepted look is
//! what these values give. Change one at a time.
//!
//! - `height` is the envelope's levels plus [`headroom`], so a crown may dome
//!   or spike above the flat plane DF's tiles stop at.
//! - `limb_frac` is [`spread`] (the mean crown radius DF reports) times 0.75
//!   over the height, so the preset grows to this tree's size instead of
//!   pressing against the bounds.
//! - [`envelope`] is a cylinder of [`extent`], not the per-level footprints: a
//!   bound on gross overshoot, never a mould.
//! - The generator's own `SLACK` lets a limb reach half a tile past that, which
//!   is what lets a trunk follow DF's jogging tile column.
//! - `clear_frac` is where the game says the crown starts; the crown's dome
//!   fall-off and porosity are the species preset's own.

use crate::canopy::CanopyPart;
use crate::factory::{self, Class};
use crate::library::TileLibrary;
use crate::mesh::Z_SCALE;
use crate::world::World;
use dwarf_eye_art::raws::TreeGrowth;
use dwarf_eye_trees as trees;

/// Sub-voxels per tile edge.
pub const DETAIL: i32 = 4;

/// How a species carries its crown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Habit {
    /// Limbs fork and rise; the trunk stops partway up the crown.
    Spreading,
    /// Whorls of near-horizontal limbs around a trunk that runs to the top.
    Conifer,
}

/// A deterministic value in 0..1 from four integers.
pub fn hash01(a: i32, b: i32, c: i32, d: i32) -> f32 {
    let mut h = (a as u32).wrapping_mul(0x9E3779B1)
        ^ (b as u32).wrapping_mul(0x85EBCA77)
        ^ (c as u32).wrapping_mul(0xC2B2AE3D)
        ^ (d as u32).wrapping_mul(0x27D4EB2F);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545F491);
    h ^= h >> 13;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Where a tree is allowed to be, from the tiles DF reports for it.
pub struct Envelope {
    pub origin: (i32, i32, i32),
    pub species: i32,
    /// Tile the trunk stands on.
    pub base: (i32, i32, i32),
    /// Lowest and highest level the tree occupies.
    pub z0: i32,
    pub z1: i32,
    /// Lowest level that holds crown rather than bare trunk.
    pub crown_z0: i32,
    /// Footprint box, in tiles.
    pub x0: i32,
    pub y0: i32,
    pub w: i32,
    pub d: i32,
    /// Occupancy per level, indexed `[z - z0][(y - y0) * w + (x - x0)]`.
    level: Vec<Vec<bool>>,
    /// Middle of each level's footprint, and how far it reaches.
    pub centre: Vec<[f32; 2]>,
    pub radius: Vec<f32>,
    /// The tree ran off the top of what the game has sent us, so its height
    /// came from the raws instead of from tiles.
    pub truncated: bool,
    /// The game reports this tree's crown as cap, not as branches and twigs.
    pub cap: bool,
    /// The game reports this tree as dead, so it carries no foliage at all.
    pub dead: bool,
    /// Built work inside the tree's reach, per level, over a box centred on the
    /// trunk. Indexed `[z - z0][(y - by0) * bw + (x - bx0)]`.
    blocked: Vec<Vec<bool>>,
    bx0: i32,
    by0: i32,
    bw: i32,
    bd: i32,
}

/// How far from its origin a tree's tiles can reach.
const SPAN: i32 = 16;

impl Envelope {
    /// Reads one tree's tiles out of the world.
    ///
    /// The scan box is anchored at the tree's origin rather than at any chunk,
    /// so every chunk that asks about this tree gets the same answer.
    pub fn read(
        world: &World,
        library: &TileLibrary,
        origin: (i32, i32, i32),
        species: i32,
    ) -> Option<Self> {
        let (ox, oy, oz) = origin;
        let mut tiles: Vec<(i32, i32, i32, bool)> = Vec::new();
        let mut cap = false;
        let mut dead = false;
        for z in (oz - 4)..=(oz + SPAN + 8) {
            for x in ox..=(ox + SPAN) {
                for y in oy..=(oy + SPAN) {
                    let Some(v) = world.voxel(x, y, z) else { continue };
                    // The factory says what belongs to a tree; DF's own crown
                    // classification then says which part of one this is.
                    let Some(plan) = library.plan(v.tile_id) else { continue };
                    if !plan.of_tree() || v.tree_origin(x, y, z) != origin {
                        continue;
                    }
                    let part = library.canopy_part(v.tile_id);
                    cap |= part == Some(CanopyPart::Cap);
                    dead |= plan.class == Class::DeadTree;
                    tiles.push((x, y, z, part.is_some()));
                }
            }
        }
        if tiles.is_empty() {
            return None;
        }

        let z0 = tiles.iter().map(|t| t.2).min()?;
        let mut z1 = tiles.iter().map(|t| t.2).max()?;
        let crown_z0 = tiles.iter().filter(|t| t.3).map(|t| t.2).min().unwrap_or(z0 + 1);
        let x0 = tiles.iter().map(|t| t.0).min()?;
        let x1 = tiles.iter().map(|t| t.0).max()?;
        let y0 = tiles.iter().map(|t| t.1).min()?;
        let y1 = tiles.iter().map(|t| t.1).max()?;
        let (w, d) = (x1 - x0 + 1, y1 - y0 + 1);

        // The base is the lowest tile nearest the middle of the footprint.
        let mid = [(x0 + x1) as f32 * 0.5, (y0 + y1) as f32 * 0.5];
        let base = tiles
            .iter()
            .filter(|t| t.2 == z0)
            .min_by(|a, b| {
                let far = |t: &(i32, i32, i32, bool)| {
                    (t.0 as f32 - mid[0]).powi(2) + (t.1 as f32 - mid[1]).powi(2)
                };
                far(a).total_cmp(&far(b))
            })
            .map(|t| (t.0, t.1, t.2))?;

        let mut level = vec![vec![false; (w * d) as usize]; (z1 - z0 + 1) as usize];
        for &(x, y, z, _) in &tiles {
            level[(z - z0) as usize][((y - y0) * w + (x - x0)) as usize] = true;
        }

        // A tree whose top level sits against the edge of what has been sent is
        // not a short tree, it is a tree we cannot see the top of. Carry the
        // last footprint upward to the height the raws give the species.
        let truncated = world.chunk(base.0.div_euclid(16), base.1.div_euclid(16), z1 + 1).is_none();
        if truncated {
            let growth = library.growth(species);
            let wanted = oz + growth.max_trunk_height.max(4) as i32 + 5;
            while z1 < wanted {
                level.push(level.last().cloned().unwrap_or_default());
                z1 += 1;
            }
        }

        let mut centre = Vec::with_capacity(level.len());
        let mut radius = Vec::with_capacity(level.len());
        for slab in &level {
            let (mut sx, mut sy, mut n) = (0.0f32, 0.0f32, 0.0f32);
            for (i, &on) in slab.iter().enumerate() {
                if on {
                    sx += (x0 + i as i32 % w) as f32 + 0.5;
                    sy += (y0 + i as i32 / w) as f32 + 0.5;
                    n += 1.0;
                }
            }
            if n == 0.0 {
                centre.push([mid[0] + 0.5, mid[1] + 0.5]);
                radius.push(0.0);
                continue;
            }
            let c = [sx / n, sy / n];
            let mut far = 0.0f32;
            for (i, &on) in slab.iter().enumerate() {
                if on {
                    let p = [(x0 + i as i32 % w) as f32 + 0.5, (y0 + i as i32 / w) as f32 + 0.5];
                    far = far.max(((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt());
                }
            }
            centre.push(c);
            radius.push(far + 0.5);
        }

        // Built work bounds a tree as surely as the ground does. DF never puts
        // a tree tile inside a wall someone raised, but the envelope a crown
        // grows in is a cylinder of the tree's whole reach rather than its own
        // tiles, so without this a tree standing beside a building grows
        // through its roof.
        let base_xy = [base.0 as f32 + 0.5, base.1 as f32 + 0.5];
        let mut reach = 1.0f32;
        for (n, c) in centre.iter().enumerate() {
            let lean = ((c[0] - base_xy[0]).powi(2) + (c[1] - base_xy[1]).powi(2)).sqrt();
            reach = reach.max(lean + radius[n]);
        }
        let span = reach.ceil() as i32 + 1;
        let (bx0, by0) = (base.0 - span, base.1 - span);
        let (bw, bd) = (2 * span + 1, 2 * span + 1);
        let mut blocked = vec![vec![false; (bw * bd) as usize]; level.len()];
        for (n, slab) in blocked.iter_mut().enumerate() {
            let z = z0 + n as i32;
            for j in 0..bd {
                for i in 0..bw {
                    if world
                        .voxel(bx0 + i, by0 + j, z)
                        .is_some_and(|v| library.is_built(v.tile_id) || v.built_over())
                    {
                        slab[(j * bw + i) as usize] = true;
                    }
                }
            }
        }

        Some(Self {
            origin,
            species,
            base,
            z0,
            z1,
            crown_z0,
            x0,
            y0,
            w,
            d,
            level,
            centre,
            radius,
            truncated,
            cap,
            dead,
            blocked,
            bx0,
            by0,
            bw,
            bd,
        })
    }

    /// Whether built work stands in a tile, so no crown may fill it.
    ///
    /// Levels above the tree's own top read the top level's mask: a roof over
    /// the tree is still a roof for whatever rises into the headroom.
    pub fn blocked(&self, x: i32, y: i32, z: i32) -> bool {
        if self.blocked.is_empty() {
            return false;
        }
        let (i, j) = (x - self.bx0, y - self.by0);
        if i < 0 || j < 0 || i >= self.bw || j >= self.bd {
            return false;
        }
        let level = (z - self.z0).clamp(0, self.blocked.len() as i32 - 1) as usize;
        self.blocked[level][(j * self.bw + i) as usize]
    }

    pub fn height(&self) -> i32 {
        self.z1 - self.z0 + 1
    }

    pub fn occupied(&self, x: i32, y: i32, z: i32) -> bool {
        if z < self.z0 || z > self.z1 {
            return false;
        }
        let (i, j) = (x - self.x0, y - self.y0);
        if i < 0 || j < 0 || i >= self.w || j >= self.d {
            return false;
        }
        self.level[(z - self.z0) as usize][(j * self.w + i) as usize]
    }

    /// Whether a point in render space lies inside the envelope.
    ///
    /// The tiles are widened by half a tile, which is the same as asking
    /// whether any tile overlaps the unit square around the point.
    pub fn contains(&self, p: [f32; 3]) -> bool {
        let z = (p[1] / Z_SCALE).floor() as i32;
        for dx in [-0.5f32, 0.5] {
            for dy in [-0.5f32, 0.5] {
                if self.occupied((p[0] + dx).floor() as i32, (p[2] + dy).floor() as i32, z) {
                    return true;
                }
            }
        }
        false
    }

    /// Middle of the footprint at a height, for a limb to bend back toward.
    pub fn centre_at(&self, y: f32) -> [f32; 2] {
        let n = ((y / Z_SCALE).floor() as i32 - self.z0).clamp(0, self.centre.len() as i32 - 1);
        self.centre[n as usize]
    }

    pub fn radius_at(&self, y: f32) -> f32 {
        let n = ((y / Z_SCALE).floor() as i32 - self.z0).clamp(0, self.radius.len() as i32 - 1);
        self.radius[n as usize]
    }

    /// Whether the footprint narrows level by level toward the top, which is
    /// what a conifer looks like from above.
    fn tapers(&self) -> bool {
        let n = self.radius.len();
        if n < 5 {
            return false;
        }
        self.radius[n - 5..]
            .windows(2)
            .all(|w| w[1] <= w[0] + 0.01)
    }

    /// How this tree carries its crown.
    ///
    /// DF gives pine, cedar and larch no heavy branches at all and the
    /// broadleaves a quarter density of them, which decides it outright; a
    /// species with no tokens falls back to the shape of its own footprint.
    pub fn habit(&self, growth: TreeGrowth) -> Habit {
        if growth.branch_density == 0 && growth.heavy_branch_density == 0 {
            return if self.tapers() { Habit::Conifer } else { Habit::Spreading };
        }
        if growth.heavy_branch_density == 0 {
            Habit::Conifer
        } else {
            Habit::Spreading
        }
    }
}

/// Where the growth crate's tile space sits in render space: the centre of the
/// tile the trunk stands on, at that level's floor.
pub fn anchor(env: &Envelope) -> [f32; 3] {
    [env.base.0 as f32 + 0.5, env.base.2 as f32 * Z_SCALE, env.base.1 as f32 + 0.5]
}

/// The tree's seed: where it stands in the world, and what it is.
///
/// Absolute tiles, not render coordinates, so a tree keeps its shape when the
/// render origin moves; and not the tile configuration, so a tree does not
/// change shape as neighbouring tiles arrive.
pub fn seed(env: &Envelope, world_origin: (i32, i32, i32)) -> u64 {
    let (ox, oy, oz) = env.origin;
    factory::seed(
        ox + world_origin.0,
        oy + world_origin.1,
        oz + world_origin.2,
        env.species,
    )
}

/// How far the tree's tiles reach from its trunk, in tiles.
///
/// This and the height are the whole of what the game tells us about a tree's
/// size. Every level's own footprint is DF's bookkeeping, not a silhouette.
pub fn extent(env: &Envelope) -> f32 {
    let base = [env.base.0 as f32 + 0.5, env.base.1 as f32 + 0.5];
    let mut reach: f32 = 1.0;
    for (level, centre) in env.centre.iter().enumerate() {
        let lean = ((centre[0] - base[0]).powi(2) + (centre[1] - base[1]).powi(2)).sqrt();
        reach = reach.max(lean + env.radius[level]);
    }
    reach
}

/// The widest the game says this tree's crown gets, in tiles.
///
/// A ceiling on the preset's own spread, not a size to grow to.
pub fn cap_radius(env: &Envelope) -> f32 {
    let from = (env.crown_z0 - env.z0).max(0) as usize;
    env.radius[from.min(env.radius.len().saturating_sub(1))..]
        .iter()
        .copied()
        .fold(1.0f32, f32::max)
}

/// Extra levels a crown may rise into, a quarter of its own depth.
///
/// DF's footprint stops in a flat plane. A crown grown to fill it ends in a
/// slab, so the tree is allowed to dome or spike above what the tiles say.
pub fn headroom(env: &Envelope) -> i32 {
    let crown = (env.z1 - env.crown_z0 + 1).max(1);
    ((crown as f32 * 0.25).round() as i32).clamp(1, 3)
}

/// The bounds the tree may grow inside, as the growth crate wants them.
///
/// A box, not a mould. DF's tiles say how tall a tree is, how far it reaches
/// and where it stands; the shape inside that is the species' own, grown the
/// same way the tree lab grows it. Only a limb leaving the whole extent is
/// stopped, never one leaving a particular level's exact footprint — moulding a
/// tree to those gives a flat-topped, straight-sided slab.
pub fn envelope(env: &Envelope) -> trees::Envelope {
    let reach = extent(env);
    let r = reach.ceil() as i32 + 1;
    let side = (2 * r + 1) as u32;
    let over = headroom(env);

    let levels = (0..env.height() + over)
        .map(|i| {
            // The headroom narrows, so what rises into it is an apex.
            let above = (i - env.height() + 1).max(0) as f32;
            let radius = (reach * (1.0 - 0.3 * above)).max(0.9);
            let mut foot = trees::Footprint {
                width: side,
                depth: side,
                cells: vec![false; (side * side) as usize],
            };
            for iz in 0..side as i32 {
                for ix in 0..side as i32 {
                    let (dx, dz) = ((ix - r) as f32, (iz - r) as f32);
                    // The grid is centred on the trunk's tile, so a cell is the
                    // tile that far from it.
                    let blocked =
                        env.blocked(env.base.0 + ix - r, env.base.1 + iz - r, env.z0 + i);
                    if !blocked && (dx * dx + dz * dz).sqrt() <= radius {
                        foot.cells[(iz * side as i32 + ix) as usize] = true;
                    }
                }
            }
            foot
        })
        .collect();
    trees::Envelope { levels }
}

/// What a species looks like, from its growth tokens and its own sprites.
///
/// The preset carries the habit's proportions; everything the game actually
/// knows about this tree then overrides them.
pub fn params(
    env: &Envelope,
    _growth: TreeGrowth,
    habit: Habit,
    library: &mut TileLibrary,
) -> trees::TreeParams {
    // A weeping species is not something the growth tokens say; the raws only
    // name it. Its crown is an ordinary one, so all this changes is whether
    // strands hang off it.
    let weeping = factory::weeping(&library.plant_id(env.species));
    let mut params = if env.dead {
        // What the factory resolves a dead tree to: bare limbs, no foliage.
        // Everything below still sizes it from DF's own bounds.
        match factory::resolve(Class::DeadTree, factory::Style::current()) {
            factory::Treatment::Grown(kind, preset) => {
                let mut params = *preset;
                params.kind = kind;
                params
            }
            _ => trees::dead_tree(),
        }
    } else if env.cap {
        trees::mushroom_tree()
    } else if weeping {
        trees::willow()
    } else {
        match habit {
            Habit::Conifer => trees::spruce(),
            Habit::Spreading => trees::oak(),
        }
    };

    // Everything below this line is the whole of what the game contributes to
    // the shape: how tall the tree is and how wide it may be. The preset keeps
    // its own clear trunk, crown depth, dome and porosity, so a tree of this
    // species at this height is the tree the lab draws at that height.
    let natural_height = params.height;
    let natural_limb = params.limb_frac;
    let height = env.height() as f32;
    params.height = height;

    // The trunk thickens with the tree, in the preset's own proportion.
    params.trunk_width *= (height / natural_height).clamp(0.45, 2.2);

    // The radius is a cap, never a target. A tree DF says is slim grows a
    // narrower crown; one it gives room to keeps the preset's own spread.
    let cap = cap_radius(env) / height.max(1.0);
    params.limb_frac = natural_limb.min(cap.max(0.08));

    let leaf: Vec<trees::Rgb> = library
        .leaf_tones(env.species, 3)
        .into_iter()
        .map(|c| trees::Rgb(c[0], c[1], c[2]))
        .collect();
    let bark: Vec<trees::Rgb> = library
        .bark_tones(env.species, 2)
        .into_iter()
        .map(|c| trees::Rgb(c[0], c[1], c[2]))
        .collect();
    // `leaf_tones` hands back the darkest first, so the last is the lit one.
    let tip = leaf.last().copied().unwrap_or(trees::Rgb(132, 172, 58));
    if !leaf.is_empty() {
        params.palette.leaf = leaf;
    }
    if !bark.is_empty() {
        params.palette.bark = bark;
    }
    // The species' own lightest shade, unwhitened: DF's leaf sprites are
    // already pale, and lifting them further washes the crown out.
    params.palette.tip = tip;
    params
}

/// Grows one tree with the shared generator, so the game and the tree lab
/// produce the same tree from the same parameters and seed.
///
/// The skeleton is where the shape is decided and where the time goes; cutting
/// it into voxels is [`rasterise`], and a tree is cut once per detail band.
pub fn skeleton(
    env: &Envelope,
    growth: TreeGrowth,
    habit: Habit,
    library: &mut TileLibrary,
    world_origin: (i32, i32, i32),
) -> trees::Skeleton {
    use crate::canopy::timing::PHASES;
    let params = params(env, growth, habit, library);
    let bounds = envelope(env);
    let started = std::time::Instant::now();
    let grown = trees::grow(&params, seed(env, world_origin), Some(&bounds));
    PHASES.skeleton.since(started);
    grown
}

/// Cuts a grown tree into voxels at a band's own resolution and fill.
///
/// `cut` is the band's compensation, not the species': a coarse cut fattens
/// every twig and closes up the foliage, so the band asks for less of both and
/// its crown reads like the near band's (`canopy::Band::cut`).
pub fn rasterise(skeleton: &trees::Skeleton, detail: i32, cut: trees::Cut) -> trees::VoxelTree {
    use crate::canopy::timing::PHASES;
    let started = std::time::Instant::now();
    let voxels = trees::rasterise_cut(skeleton, detail.max(1) as u32, cut);
    PHASES.rasterise.since(started);
    voxels
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-tile tree standing four levels tall, with `built` tiles marked in
    /// a box around it.
    fn envelope_with(built: &[(i32, i32, i32)]) -> Envelope {
        let (base, height) = ((10, 10, 5), 4);
        let span = 4;
        let (bx0, by0) = (base.0 - span, base.1 - span);
        let (bw, bd) = (2 * span + 1, 2 * span + 1);
        let mut blocked = vec![vec![false; (bw * bd) as usize]; height as usize];
        for &(x, y, z) in built {
            let (i, j, level) = (x - bx0, y - by0, z - base.2);
            blocked[level as usize][(j * bw + i) as usize] = true;
        }
        Envelope {
            origin: base,
            species: 0,
            base,
            z0: base.2,
            z1: base.2 + height - 1,
            crown_z0: base.2 + 1,
            x0: base.0,
            y0: base.1,
            w: 1,
            d: 1,
            level: vec![vec![true]; height as usize],
            centre: vec![[base.0 as f32 + 0.5, base.1 as f32 + 0.5]; height as usize],
            radius: vec![2.0; height as usize],
            truncated: false,
            cap: false,
            dead: false,
            blocked,
            bx0,
            by0,
            bw,
            bd,
        }
    }

    /// Where a tile sits in the growth crate's own footprint grid, which is
    /// centred on the tile the trunk stands on.
    fn cell(env: &Envelope, foot: &trees::Footprint, x: i32, y: i32) -> bool {
        let r = foot.width as i32 / 2;
        foot.get(x - env.base.0 + r, y - env.base.1 + r)
    }

    #[test]
    fn built_work_is_cut_out_of_the_envelope() {
        let plain = envelope(&envelope_with(&[]));
        assert!(cell(&envelope_with(&[]), &plain.levels[1], 11, 10), "the crown reaches here");

        // A constructed wall one tile east, on the second level.
        let env = envelope_with(&[(11, 10, 6)]);
        let bounds = envelope(&env);
        assert!(!cell(&env, &bounds.levels[1], 11, 10), "the crown grew into built work");
        assert!(cell(&env, &bounds.levels[1], 9, 10), "and stopped growing anywhere else");
        // Only that level: the tile below the wall is still open air.
        assert!(cell(&env, &bounds.levels[0], 11, 10), "a wall above closed the level below it");
    }

    #[test]
    fn a_tree_with_nothing_built_near_it_is_unchanged() {
        let env = envelope_with(&[]);
        let bounds = envelope(&env);
        assert!(bounds.levels.iter().any(|f| f.cells.iter().any(|c| *c)));
        assert!(!env.blocked(11, 10, 6));
    }
}
