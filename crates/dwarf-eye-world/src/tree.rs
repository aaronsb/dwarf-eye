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

use crate::canopy::CanopyPart;
use crate::library::TileLibrary;
use crate::mesh::Z_SCALE;
use crate::world::World;
use dwarf_eye_art::raws::TreeGrowth;
use dwarf_eye_trees as trees;
use dwarf_eye_trees::rng;

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
        for z in (oz - 4)..=(oz + SPAN + 8) {
            for x in ox..=(ox + SPAN) {
                for y in oy..=(oy + SPAN) {
                    let Some(v) = world.voxel(x, y, z) else { continue };
                    let trunk = library.is_trunk(v.tile_id);
                    let part = library.canopy_part(v.tile_id);
                    if (!trunk && part.is_none()) || v.tree_origin(x, y, z) != origin {
                        continue;
                    }
                    cap |= part == Some(CanopyPart::Cap);
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
        })
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
    rng::hash3(ox + world_origin.0, oy + world_origin.1, oz + world_origin.2, env.species as u64)
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

/// The crown's typical radius in tiles, which is what the species is grown to.
///
/// [`extent`] is a worst case and makes a fat blob of every leaning tree; the
/// mean of the levels the game calls crown is the size it actually reports.
pub fn spread(env: &Envelope) -> f32 {
    let from = (env.crown_z0 - env.z0).max(0) as usize;
    let levels = &env.radius[from.min(env.radius.len().saturating_sub(1))..];
    let wide: f32 = levels.iter().sum();
    (wide / levels.len().max(1) as f32).max(1.0)
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
                    if (dx * dx + dz * dz).sqrt() <= radius {
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
    growth: TreeGrowth,
    habit: Habit,
    library: &mut TileLibrary,
) -> trees::TreeParams {
    let mut params = if env.cap {
        trees::mushroom_tree()
    } else {
        match habit {
            Habit::Conifer => trees::spruce(),
            Habit::Spreading => trees::oak(),
        }
    };

    params.height = (env.height() + headroom(env)) as f32;
    // A tall tree carries a thicker trunk, kept under a tile across either way.
    let tall = ((env.height() - 2) as f32 / 12.0).clamp(0.0, 1.0);
    params.trunk_width = 0.32 + 0.38 * tall;

    // Where the crown starts is the game's own answer, not the preset's.
    let clear = (env.crown_z0 - env.z0) as f32 / params.height.max(1.0);
    params.clear_frac = clear.clamp(0.08, 0.7);

    // Limbs reach for the extent the game gives, so the preset grows to this
    // tree's size rather than pressing against the bounds.
    params.limb_frac = (spread(env) * 0.75 / params.height.max(1.0)).clamp(0.12, 0.4);

    // DF's branch density is a percentage; it decides how full the crown is and
    // how much light comes through a leaf face.
    if growth.branch_density > 0 {
        let d = (growth.branch_density as f32 / 100.0).clamp(0.0, 1.0);
        params.leaf_density = (0.34 + 0.34 * d).clamp(0.3, 0.68);
        params.cutout_openness = (0.52 - 0.24 * d).clamp(0.22, 0.55);
    }
    if growth.max_trunk_diameter > 1 {
        params.trunk_width = params.trunk_width.max(growth.max_trunk_diameter as f32 * 0.45);
    }

    let leaf: Vec<trees::Rgb> =
        library.leaf_tones(env.species, 3).into_iter().map(|c| trees::Rgb(c[0], c[1], c[2])).collect();
    let bark: Vec<trees::Rgb> =
        library.bark_tones(env.species, 2).into_iter().map(|c| trees::Rgb(c[0], c[1], c[2])).collect();
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

/// Grows and voxelises one tree with the shared generator, so the game and the
/// tree lab produce the same tree from the same parameters and seed.
pub fn grow(
    env: &Envelope,
    growth: TreeGrowth,
    habit: Habit,
    library: &mut TileLibrary,
    world_origin: (i32, i32, i32),
) -> trees::VoxelTree {
    let params = params(env, growth, habit, library);
    let bounds = envelope(env);
    let skeleton = trees::grow(&params, seed(env, world_origin), Some(&bounds));
    trees::rasterise(&skeleton, DETAIL as u32)
}
