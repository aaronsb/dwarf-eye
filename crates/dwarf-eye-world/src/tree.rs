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
//! Everything is seeded from the tree's origin, so a tree is the same tree
//! across a reload.

use crate::library::TileLibrary;
use crate::mesh::Z_SCALE;
use crate::world::World;
use dwarf_eye_art::raws::TreeGrowth;

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
        for z in (oz - 4)..=(oz + SPAN + 8) {
            for x in ox..=(ox + SPAN) {
                for y in oy..=(oy + SPAN) {
                    let Some(v) = world.voxel(x, y, z) else { continue };
                    let trunk = library.is_trunk(v.tile_id);
                    let crown = library.canopy_part(v.tile_id).is_some();
                    if (!trunk && !crown) || v.tree_origin(x, y, z) != origin {
                        continue;
                    }
                    tiles.push((x, y, z, crown));
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
        })
    }

    pub fn height(&self) -> i32 {
        self.z1 - self.z0 + 1
    }

    fn occupied(&self, x: i32, y: i32, z: i32) -> bool {
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

/// One length of wood, tapering from `r0` to `r1`.
pub struct Segment {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub r0: f32,
    pub r1: f32,
}

/// A grown tree, before it is voxelised.
pub struct Grown {
    pub wood: Vec<Segment>,
    /// Where leaves gather: the ends of the outermost limbs.
    pub leaves: Vec<[f32; 3]>,
}

/// Sub-steps a limb is walked in, so it can curve and be clipped part way.
const STEPS: i32 = 3;

/// Grows a tree inside its envelope.
pub fn grow(env: &Envelope, growth: TreeGrowth, habit: Habit) -> Grown {
    let (ox, oy, oz) = env.origin;
    let seed = |n: i32| hash01(ox, oy, oz, n);
    let mut out = Grown { wood: Vec::new(), leaves: Vec::new() };

    // A tall tree carries a thicker trunk. Half a tile across at the tallest,
    // a third at the shortest, so it stays slimmer than a tile either way.
    let tall = ((env.height() - 2) as f32 / 12.0).clamp(0.0, 1.0);
    let trunk_r = 0.16 + 0.19 * tall;

    let crown_top = env.z1 as f32 + 1.0;
    let crown_base = env.crown_z0 as f32;
    let trunk_top = match habit {
        Habit::Conifer => crown_top - 0.6,
        Habit::Spreading => crown_base + (0.55 + 0.15 * seed(1)) * (crown_top - crown_base),
    };

    // The trunk follows the middle of each level, so a leaning tree leans.
    let foot = [env.base.0 as f32 + 0.5, env.z0 as f32 * Z_SCALE, env.base.1 as f32 + 0.5];
    let at = |y: f32| -> [f32; 3] {
        let c = env.centre_at(y);
        let blend = ((y - env.z0 as f32) / (trunk_top - env.z0 as f32)).clamp(0.0, 1.0);
        [
            foot[0] + (c[0] - foot[0]) * blend,
            y * Z_SCALE,
            foot[2] + (c[1] - foot[2]) * blend,
        ]
    };
    let mut y = env.z0 as f32;
    while y < trunk_top {
        let next = (y + 0.5).min(trunk_top);
        let taper = |v: f32| {
            let t = ((v - env.z0 as f32) / (trunk_top - env.z0 as f32)).clamp(0.0, 1.0);
            trunk_r * (1.0 - 0.45 * t)
        };
        out.wood.push(Segment { a: at(y), b: at(next), r0: taper(y), r1: taper(next) });
        y = next;
    }

    // A few roots flaring at the foot.
    for n in 0..5 {
        let angle = std::f32::consts::TAU * (n as f32 + seed(20 + n)) / 5.0;
        let reach = 0.30 + 0.25 * seed(30 + n);
        out.wood.push(Segment {
            a: foot,
            b: [foot[0] + angle.cos() * reach, foot[1], foot[2] + angle.sin() * reach],
            r0: trunk_r * 0.8,
            r1: trunk_r * 0.35,
        });
    }

    let density = if growth.branch_density == 0 {
        1.0
    } else {
        0.7 + 0.6 * growth.branch_density as f32 / 100.0
    };

    match habit {
        // Whorls: several near-horizontal limbs at each level, drooping a
        // little, from the crown's base to the spike at the top.
        Habit::Conifer => {
            let mut level = crown_base;
            let mut n = 0;
            while level < crown_top - 0.5 {
                let t = (level - crown_base) / (crown_top - crown_base).max(1.0);
                let arms = (4.0 + 4.0 * (1.0 - t) * density).round() as i32;
                let reach = env.radius_at(level).max(0.6) * (0.55 + 0.45 * (1.0 - t));
                for a in 0..arms {
                    let spin = std::f32::consts::TAU
                        * (a as f32 + seed(100 + n * 8 + a)) / arms as f32;
                    let dir = [spin.cos(), -0.12 - 0.2 * seed(200 + n * 8 + a), spin.sin()];
                    limb(&mut out, env, at(level), dir, reach, trunk_r * 0.45, 1, n * 16 + a);
                }
                level += 1.0;
                n += 1;
            }
        }
        // Forks: limbs leave the trunk at a rising angle and branch again.
        Habit::Spreading => {
            let start = crown_base - 0.4;
            let mut level = start;
            let mut n = 0;
            while level < trunk_top {
                let arms = (2.0 + 2.0 * seed(300 + n) * density).round().clamp(2.0, 4.0) as i32;
                let reach = env.radius_at(level).max(0.8) * (0.55 + 0.25 * seed(400 + n));
                for a in 0..arms {
                    let spin = std::f32::consts::TAU
                        * (a as f32 + seed(500 + n * 8 + a)) / arms as f32;
                    // Thirty to sixty degrees off the trunk, rising.
                    let lift = 0.58 + 0.55 * seed(600 + n * 8 + a);
                    let dir = [spin.cos(), lift, spin.sin()];
                    limb(&mut out, env, at(level), dir, reach, trunk_r * 0.6, 2, n * 16 + a);
                }
                level += 1.1;
                n += 1;
            }
        }
    }
    out
}

fn normalise(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-6 { [0.0, 1.0, 0.0] } else { [v[0] / len, v[1] / len, v[2] / len] }
}

/// Walks one limb outward, curving as it goes, and forks at its end.
///
/// A step that would leave the envelope is bent back toward the middle of the
/// footprint at that height; if it still leaves, the limb stops there. That is
/// what keeps the tree inside what the game calls tree.
fn limb(
    out: &mut Grown,
    env: &Envelope,
    from: [f32; 3],
    dir: [f32; 3],
    len: f32,
    radius: f32,
    depth: i32,
    seed: i32,
) {
    let (ox, oy, oz) = env.origin;
    let wob = |n: i32| hash01(ox ^ seed, oy, oz, 700 + n) - 0.5;
    let mut p = from;
    let mut d = normalise(dir);
    let step = len / STEPS as f32;
    let mut reached = p;

    for s in 0..STEPS {
        let mut next = [p[0] + d[0] * step, p[1] + d[1] * step * Z_SCALE, p[2] + d[2] * step];
        if !env.contains(next) {
            let c = env.centre_at(next[1]);
            let pull = normalise([c[0] - p[0], d[1] * 0.4, c[1] - p[2]]);
            d = normalise([d[0] + pull[0] * 1.4, d[1] + pull[1], d[2] + pull[2] * 1.4]);
            next = [p[0] + d[0] * step, p[1] + d[1] * step * Z_SCALE, p[2] + d[2] * step];
            if !env.contains(next) {
                break;
            }
        }
        let t0 = s as f32 / STEPS as f32;
        let t1 = (s + 1) as f32 / STEPS as f32;
        out.wood.push(Segment {
            a: p,
            b: next,
            r0: radius * (1.0 - 0.5 * t0),
            r1: radius * (1.0 - 0.5 * t1),
        });
        p = next;
        reached = next;
        if depth <= 0 {
            out.leaves.push(next);
        }
        // Gentle curvature, plus a small tilt that holds still across reloads.
        d = normalise([
            d[0] + wob(s * 3) * 0.22,
            d[1] + 0.06 + wob(s * 3 + 1) * 0.12,
            d[2] + wob(s * 3 + 2) * 0.22,
        ]);
    }

    if depth <= 0 {
        out.leaves.push(reached);
        return;
    }
    let children = 2 + (hash01(ox, oy, oz ^ seed, 800) * 2.99) as i32;
    for c in 0..children {
        let spin = std::f32::consts::TAU * (c as f32 + hash01(ox, oy, oz, 900 + seed + c))
            / children as f32;
        let spread = 0.55 + 0.5 * hash01(ox, oy, oz, 950 + seed + c);
        let child = normalise([
            d[0] + spin.cos() * spread,
            d[1] + 0.25,
            d[2] + spin.sin() * spread,
        ]);
        limb(
            out,
            env,
            p,
            child,
            len * (0.60 + 0.15 * hash01(ox, oy, oz, 1000 + seed + c)),
            (radius * 0.6).max(0.5 / DETAIL as f32),
            depth - 1,
            seed * 7 + c + 1,
        );
    }
    // Leaves gather on the outermost wood, not only at its very tip.
    out.leaves.push(p);
}
