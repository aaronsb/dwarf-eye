//! What a species is: a habit, a size, branching ratios and a palette.

use crate::math::{Vec3, vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Habit {
    /// A clear trunk that forks into a rounded crown.
    Deciduous,
    /// A single leader with whorls of near-horizontal branches.
    Conifer,
    /// Low, many-stemmed, branches arching outward and down.
    Broad,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Linear-space floats, for a vertex colour that a PBR shader can use
    /// directly.
    pub fn to_linear(self) -> [f32; 4] {
        fn channel(v: u8) -> f32 {
            let s = v as f32 / 255.0;
            if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        }
        [channel(self.0), channel(self.1), channel(self.2), 1.0]
    }

    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8;
        Rgb(f(self.0, other.0), f(self.1, other.1), f(self.2, other.2))
    }

    /// Multiply every channel, for shading a voxel without leaving the palette.
    pub fn scale(self, f: f32) -> Rgb {
        let g = |v: u8| (v as f32 * f).round().clamp(0.0, 255.0) as u8;
        Rgb(g(self.0), g(self.1), g(self.2))
    }
}

/// Two or three bark shades, two or three leaf shades, and a lighter tip
/// colour for the outermost leaves.
#[derive(Clone, Debug)]
pub struct Palette {
    pub bark: Vec<Rgb>,
    pub leaf: Vec<Rgb>,
    pub tip: Rgb,
}

#[derive(Clone, Debug)]
pub struct TreeParams {
    pub habit: Habit,
    /// Total height in tiles.
    pub height: f32,
    /// Trunk diameter at the base, in tiles.
    pub trunk_width: f32,
    /// How many times a limb forks before it ends in leaves.
    pub branch_levels: u8,
    /// Inclusive range of children at a fork.
    pub children_per_node: (u8, u8),
    /// Inclusive range of the angle a child leaves its parent at, in degrees.
    pub spread_deg: (f32, f32),
    /// Child length as a fraction of its parent's.
    pub length_ratio: f32,
    /// Child radius as a fraction of its parent's.
    pub thickness_ratio: f32,
    /// Fraction of leaf voxels kept inside a cluster; the rest is air.
    pub leaf_density: f32,
    pub palette: Palette,

    // Habit tuning. Sensible for every preset, so callers rarely touch them.
    /// Fraction of the height with no branches on it.
    pub clear_frac: f32,
    /// Length of the first limbs as a fraction of the height.
    pub limb_frac: f32,
    /// How far a segment wanders per tile grown, in radians.
    pub wander: f32,
    /// Downward pull applied along a limb; negative lifts the tips.
    pub droop: f32,
    /// Radius of a leaf cluster in tiles.
    pub leaf_radius: f32,
    /// Leaf clusters per terminal segment.
    pub leaf_clusters: u8,
    /// Vertical squash of the crown; below 1 flattens it, above 1 stretches it.
    pub crown_stretch: f32,
    /// Conifer only: tiles between whorls.
    pub whorl_step: f32,
    /// Conifer only: branches in a whorl.
    pub whorl_count: u8,
    /// How many root buttresses flare out at the base.
    pub roots: u8,
}

impl Default for TreeParams {
    fn default() -> Self {
        oak()
    }
}

impl TreeParams {
    pub fn preset(name: Preset) -> Self {
        match name {
            Preset::Oak => oak(),
            Preset::Birch => birch(),
            Preset::Pine => pine(),
            Preset::Spruce => spruce(),
            Preset::Willow => willow(),
            Preset::Bush => bush(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    Oak,
    Birch,
    Pine,
    Spruce,
    Willow,
    Bush,
}

impl Preset {
    pub const ALL: [Preset; 6] =
        [Preset::Oak, Preset::Birch, Preset::Pine, Preset::Spruce, Preset::Willow, Preset::Bush];

    pub fn name(self) -> &'static str {
        match self {
            Preset::Oak => "oak",
            Preset::Birch => "birch",
            Preset::Pine => "pine",
            Preset::Spruce => "spruce",
            Preset::Willow => "willow",
            Preset::Bush => "bush",
        }
    }
}

pub fn oak() -> TreeParams {
    TreeParams {
        habit: Habit::Deciduous,
        height: 15.0,
        trunk_width: 1.5,
        branch_levels: 4,
        children_per_node: (2, 3),
        spread_deg: (26.0, 52.0),
        length_ratio: 0.74,
        thickness_ratio: 0.62,
        leaf_density: 0.5,
        palette: Palette {
            bark: vec![Rgb(94, 66, 44), Rgb(74, 51, 34), Rgb(112, 82, 55)],
            leaf: vec![Rgb(62, 108, 34), Rgb(88, 136, 42), Rgb(46, 84, 28)],
            tip: Rgb(132, 172, 58),
        },
        clear_frac: 0.36,
        limb_frac: 0.34,
        wander: 0.30,
        droop: 0.06,
        leaf_radius: 1.15,
        leaf_clusters: 2,
        crown_stretch: 0.86,
        whorl_step: 0.0,
        whorl_count: 0,
        roots: 5,
    }
}

pub fn birch() -> TreeParams {
    TreeParams {
        habit: Habit::Deciduous,
        height: 17.0,
        trunk_width: 0.9,
        branch_levels: 3,
        children_per_node: (2, 3),
        spread_deg: (18.0, 34.0),
        length_ratio: 0.7,
        thickness_ratio: 0.58,
        leaf_density: 0.42,
        palette: Palette {
            bark: vec![Rgb(216, 214, 204), Rgb(186, 184, 176), Rgb(96, 94, 90)],
            leaf: vec![Rgb(104, 148, 52), Rgb(126, 168, 62), Rgb(80, 122, 44)],
            tip: Rgb(164, 198, 84),
        },
        clear_frac: 0.5,
        limb_frac: 0.24,
        wander: 0.22,
        droop: 0.14,
        leaf_radius: 0.95,
        leaf_clusters: 2,
        crown_stretch: 1.25,
        whorl_step: 0.0,
        whorl_count: 0,
        roots: 3,
    }
}

pub fn pine() -> TreeParams {
    TreeParams {
        habit: Habit::Conifer,
        height: 22.0,
        trunk_width: 1.2,
        branch_levels: 2,
        children_per_node: (2, 3),
        spread_deg: (60.0, 80.0),
        length_ratio: 0.6,
        thickness_ratio: 0.5,
        leaf_density: 0.38,
        palette: Palette {
            bark: vec![Rgb(118, 74, 46), Rgb(88, 55, 34), Rgb(140, 96, 58)],
            leaf: vec![Rgb(44, 88, 46), Rgb(58, 108, 52), Rgb(32, 68, 38)],
            tip: Rgb(96, 148, 62),
        },
        // A pine carries its crown high, on a long bare bole.
        clear_frac: 0.58,
        limb_frac: 0.2,
        wander: 0.14,
        droop: 0.1,
        leaf_radius: 0.85,
        leaf_clusters: 2,
        crown_stretch: 1.0,
        whorl_step: 1.6,
        whorl_count: 5,
        roots: 5,
    }
}

pub fn spruce() -> TreeParams {
    TreeParams {
        habit: Habit::Conifer,
        height: 20.0,
        trunk_width: 1.0,
        branch_levels: 2,
        children_per_node: (2, 2),
        spread_deg: (70.0, 88.0),
        length_ratio: 0.55,
        thickness_ratio: 0.5,
        leaf_density: 0.42,
        palette: Palette {
            bark: vec![Rgb(96, 66, 44), Rgb(72, 48, 32), Rgb(116, 84, 54)],
            leaf: vec![Rgb(34, 76, 44), Rgb(48, 96, 50), Rgb(24, 58, 34)],
            tip: Rgb(84, 138, 60),
        },
        // A spruce is a cone almost to the ground.
        clear_frac: 0.12,
        limb_frac: 0.26,
        wander: 0.12,
        droop: 0.26,
        leaf_radius: 0.8,
        leaf_clusters: 2,
        crown_stretch: 1.0,
        whorl_step: 1.2,
        whorl_count: 6,
        roots: 6,
    }
}

pub fn willow() -> TreeParams {
    TreeParams {
        habit: Habit::Broad,
        height: 13.0,
        trunk_width: 1.6,
        branch_levels: 4,
        children_per_node: (2, 3),
        spread_deg: (40.0, 70.0),
        length_ratio: 0.76,
        thickness_ratio: 0.6,
        leaf_density: 0.4,
        palette: Palette {
            bark: vec![Rgb(96, 80, 56), Rgb(74, 60, 42), Rgb(114, 96, 68)],
            leaf: vec![Rgb(122, 152, 66), Rgb(146, 174, 80), Rgb(96, 126, 52)],
            tip: Rgb(178, 200, 100),
        },
        clear_frac: 0.24,
        limb_frac: 0.36,
        wander: 0.3,
        droop: 0.62,
        leaf_radius: 1.0,
        leaf_clusters: 3,
        crown_stretch: 0.72,
        whorl_step: 0.0,
        whorl_count: 0,
        roots: 4,
    }
}

pub fn bush() -> TreeParams {
    TreeParams {
        habit: Habit::Broad,
        height: 4.5,
        trunk_width: 0.5,
        branch_levels: 3,
        children_per_node: (2, 3),
        spread_deg: (30.0, 62.0),
        length_ratio: 0.72,
        thickness_ratio: 0.6,
        leaf_density: 0.52,
        palette: Palette {
            bark: vec![Rgb(84, 66, 46), Rgb(66, 52, 36)],
            leaf: vec![Rgb(72, 116, 40), Rgb(96, 140, 48), Rgb(54, 92, 32)],
            tip: Rgb(142, 178, 62),
        },
        clear_frac: 0.1,
        limb_frac: 0.5,
        wander: 0.36,
        droop: 0.12,
        leaf_radius: 0.85,
        leaf_clusters: 2,
        crown_stretch: 0.8,
        whorl_step: 0.0,
        whorl_count: 0,
        roots: 0,
    }
}

/// Per-level occupancy at tile resolution: a grid centred on the trunk, `true`
/// where the tree may grow.
#[derive(Clone, Debug)]
pub struct Footprint {
    pub width: u32,
    pub depth: u32,
    pub cells: Vec<bool>,
}

impl Footprint {
    pub fn filled(width: u32, depth: u32) -> Self {
        Self { width, depth, cells: vec![true; (width * depth) as usize] }
    }

    pub fn get(&self, ix: i32, iz: i32) -> bool {
        if ix < 0 || iz < 0 || ix >= self.width as i32 || iz >= self.depth as i32 {
            return false;
        }
        self.cells[(iz as u32 * self.width + ix as u32) as usize]
    }

    /// Centre of mass of the open cells, in cell coordinates.
    fn centroid(&self) -> Option<(f32, f32)> {
        let mut sx = 0.0;
        let mut sz = 0.0;
        let mut n = 0.0;
        for iz in 0..self.depth {
            for ix in 0..self.width {
                if self.cells[(iz * self.width + ix) as usize] {
                    sx += ix as f32 + 0.5;
                    sz += iz as f32 + 0.5;
                    n += 1.0;
                }
            }
        }
        (n > 0.0).then(|| (sx / n, sz / n))
    }
}

/// A stack of footprints the tree has to stay inside. One level per tile of
/// height; the trunk sits at the centre of the grid.
#[derive(Clone, Debug)]
pub struct Envelope {
    pub levels: Vec<Footprint>,
}

impl Envelope {
    /// A plain box, for the lab.
    pub fn box_of(width_tiles: u32, height_tiles: u32) -> Self {
        Self {
            levels: (0..height_tiles)
                .map(|_| Footprint::filled(width_tiles.max(1), width_tiles.max(1)))
                .collect(),
        }
    }

    fn level_at(&self, y: f32) -> Option<&Footprint> {
        let i = y.floor() as i32;
        if i < 0 { self.levels.first() } else { self.levels.get(i as usize) }
    }

    /// Cell coordinates of a world position, with the grid centred on x=z=0.
    fn cell(footprint: &Footprint, p: Vec3) -> (i32, i32) {
        let cx = footprint.width as f32 * 0.5;
        let cz = footprint.depth as f32 * 0.5;
        ((p.x + cx).floor() as i32, (p.z + cz).floor() as i32)
    }

    pub fn contains(&self, p: Vec3) -> bool {
        let Some(footprint) = self.level_at(p.y) else { return false };
        let (ix, iz) = Self::cell(footprint, p);
        footprint.get(ix, iz)
    }

    /// World-space centre of the open cells on the level `p` sits in.
    pub fn centroid_at(&self, p: Vec3) -> Option<Vec3> {
        let footprint = self.level_at(p.y)?;
        let (cx, cz) = footprint.centroid()?;
        Some(vec3(cx - footprint.width as f32 * 0.5, p.y, cz - footprint.depth as f32 * 0.5))
    }
}
