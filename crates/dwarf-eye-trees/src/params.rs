//! What a species is: a habit, a size, branching ratios and a palette.

use crate::math::{Vec3, vec3};

/// Which grammar to grow with. `Tree` defers to [`Habit`]; the rest are their
/// own shapes, and are what the main app's factory will select from a
/// classified Dwarf Fortress tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VegetationKind {
    #[default]
    Tree,
    /// A low dome on a few short woody stems, no trunk.
    Shrub,
    /// One thin stem with a tuft on top.
    Sapling,
    /// A tuft of thin blades, no wood at all.
    TallGrass,
    /// Bare limbs, no foliage.
    DeadTree,
    /// A trunk under a flat wide cap, as Dwarf Fortress's cap trees have.
    MushroomTree,
}

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
    /// The grammar. `TreeParams` carries the species knobs; this picks the
    /// shape they are applied to.
    pub kind: VegetationKind,
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
    /// Fraction of leaf voxels kept inside a cluster; the rest is air. This is
    /// the coarse porosity, the gaps you can see limbs through.
    pub leaf_density: f32,
    /// Fraction of a leaf face's cutout texture that is air. This is the fine
    /// porosity, what makes a conifer's spray read as see-through next to an
    /// oak's solid crown. Renderers author the cutout from it.
    pub cutout_openness: f32,
    /// How thickly streamers hang off the crown, 0 for none. A weeping tree
    /// carries its foliage on hanging strands as well as in clusters; this is
    /// the chance that a crown-edge leaf cluster grows one.
    pub streamer_density: f32,
    pub palette: Palette,

    // Habit tuning. Sensible for every preset, so callers rarely touch them.
    /// Fraction of the height with no branches on it.
    pub clear_frac: f32,
    /// Length of the first limbs as a fraction of the height. For a conifer
    /// this is the reach of the lowest whorl, so it sets the cone's width.
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
    /// How much the crown's core is hollowed out: 1 leaves only a shell with
    /// limbs showing through, 0 fills it solid. A canopy wants 1, a mushroom
    /// cap or a grass blade wants 0.
    pub crown_hollow: f32,
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
            Preset::Shrub => shrub(),
            Preset::Sapling => sapling(),
            Preset::TallGrass => tall_grass(),
            Preset::DeadTree => dead_tree(),
            Preset::MushroomTree => mushroom_tree(),
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
    Shrub,
    Sapling,
    TallGrass,
    DeadTree,
    MushroomTree,
}

impl Preset {
    pub const ALL: [Preset; 11] = [
        Preset::Oak,
        Preset::Birch,
        Preset::Pine,
        Preset::Spruce,
        Preset::Willow,
        Preset::Bush,
        Preset::Shrub,
        Preset::Sapling,
        Preset::TallGrass,
        Preset::DeadTree,
        Preset::MushroomTree,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Preset::Oak => "oak",
            Preset::Birch => "birch",
            Preset::Pine => "pine",
            Preset::Spruce => "spruce",
            Preset::Willow => "willow",
            Preset::Bush => "bush",
            Preset::Shrub => "shrub",
            Preset::Sapling => "sapling",
            Preset::TallGrass => "tall-grass",
            Preset::DeadTree => "dead-tree",
            Preset::MushroomTree => "mushroom-tree",
        }
    }
}

pub fn oak() -> TreeParams {
    TreeParams {
        kind: VegetationKind::Tree,
        habit: Habit::Deciduous,
        height: 15.0,
        trunk_width: 1.5,
        branch_levels: 4,
        children_per_node: (2, 3),
        spread_deg: (20.0, 40.0),
        length_ratio: 0.72,
        thickness_ratio: 0.62,
        leaf_density: 0.5,
        cutout_openness: 0.3,
        streamer_density: 0.0,
        palette: Palette {
            bark: vec![Rgb(94, 66, 44), Rgb(74, 51, 34), Rgb(112, 82, 55)],
            leaf: vec![Rgb(58, 106, 30), Rgb(98, 148, 44), Rgb(38, 74, 24)],
            tip: Rgb(132, 172, 58),
        },
        clear_frac: 0.34,
        limb_frac: 0.28,
        wander: 0.28,
        droop: -0.04,
        leaf_radius: 1.3,
        leaf_clusters: 3,
        crown_stretch: 0.86,
        whorl_step: 0.0,
        whorl_count: 0,
        crown_hollow: 1.0,
        roots: 5,
    }
}

pub fn birch() -> TreeParams {
    TreeParams {
        kind: VegetationKind::Tree,
        habit: Habit::Deciduous,
        height: 17.0,
        trunk_width: 0.9,
        branch_levels: 3,
        children_per_node: (2, 3),
        spread_deg: (18.0, 34.0),
        length_ratio: 0.7,
        thickness_ratio: 0.58,
        leaf_density: 0.42,
        cutout_openness: 0.36,
        streamer_density: 0.0,
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
        crown_hollow: 1.0,
        roots: 3,
    }
}

pub fn pine() -> TreeParams {
    TreeParams {
        kind: VegetationKind::Tree,
        habit: Habit::Conifer,
        height: 22.0,
        trunk_width: 1.2,
        branch_levels: 2,
        children_per_node: (2, 3),
        spread_deg: (62.0, 84.0),
        length_ratio: 0.6,
        thickness_ratio: 0.5,
        leaf_density: 0.6,
        cutout_openness: 0.52,
        streamer_density: 0.0,
        palette: Palette {
            bark: vec![Rgb(118, 74, 46), Rgb(88, 55, 34), Rgb(140, 96, 58)],
            leaf: vec![Rgb(44, 88, 46), Rgb(58, 108, 52), Rgb(32, 68, 38)],
            tip: Rgb(96, 148, 62),
        },
        // A pine carries its crown high, on a long bare bole.
        clear_frac: 0.4,
        limb_frac: 0.2,
        wander: 0.14,
        droop: 0.16,
        leaf_radius: 0.72,
        leaf_clusters: 2,
        crown_stretch: 1.6,
        whorl_step: 1.35,
        whorl_count: 7,
        crown_hollow: 1.0,
        roots: 5,
    }
}

pub fn spruce() -> TreeParams {
    TreeParams {
        kind: VegetationKind::Tree,
        habit: Habit::Conifer,
        height: 20.0,
        trunk_width: 1.0,
        branch_levels: 2,
        children_per_node: (2, 3),
        spread_deg: (72.0, 90.0),
        length_ratio: 0.55,
        thickness_ratio: 0.5,
        leaf_density: 0.52,
        cutout_openness: 0.56,
        streamer_density: 0.0,
        palette: Palette {
            bark: vec![Rgb(84, 62, 44), Rgb(64, 46, 32), Rgb(100, 76, 52)],
            leaf: vec![Rgb(34, 76, 44), Rgb(48, 96, 50), Rgb(24, 58, 34)],
            tip: Rgb(84, 138, 60),
        },
        // A spruce is a cone almost to the ground.
        clear_frac: 0.1,
        limb_frac: 0.3,
        wander: 0.12,
        droop: 0.26,
        leaf_radius: 0.72,
        leaf_clusters: 2,
        crown_stretch: 1.7,
        whorl_step: 1.15,
        whorl_count: 6,
        crown_hollow: 1.0,
        roots: 6,
    }
}

pub fn willow() -> TreeParams {
    TreeParams {
        kind: VegetationKind::Tree,
        // A willow's weeping is in its streamers, not its limbs: the crown
        // itself is an ordinary dense rounded canopy.
        habit: Habit::Deciduous,
        height: 13.0,
        trunk_width: 1.4,
        branch_levels: 4,
        children_per_node: (2, 3),
        spread_deg: (24.0, 46.0),
        length_ratio: 0.7,
        thickness_ratio: 0.58,
        leaf_density: 0.48,
        cutout_openness: 0.34,
        streamer_density: 0.85,
        palette: Palette {
            bark: vec![Rgb(96, 80, 56), Rgb(74, 60, 42), Rgb(114, 96, 68)],
            leaf: vec![Rgb(104, 136, 56), Rgb(126, 156, 68), Rgb(82, 110, 44)],
            tip: Rgb(158, 182, 88),
        },
        clear_frac: 0.34,
        limb_frac: 0.3,
        wander: 0.28,
        droop: 0.06,
        leaf_radius: 1.15,
        leaf_clusters: 3,
        crown_stretch: 0.82,
        whorl_step: 0.0,
        whorl_count: 0,
        crown_hollow: 1.0,
        roots: 4,
    }
}

pub fn bush() -> TreeParams {
    TreeParams {
        kind: VegetationKind::Tree,
        habit: Habit::Broad,
        height: 4.0,
        trunk_width: 0.5,
        branch_levels: 3,
        children_per_node: (2, 3),
        spread_deg: (28.0, 54.0),
        length_ratio: 0.72,
        thickness_ratio: 0.6,
        leaf_density: 0.52,
        cutout_openness: 0.3,
        streamer_density: 0.0,
        palette: Palette {
            bark: vec![Rgb(84, 66, 46), Rgb(66, 52, 36)],
            leaf: vec![Rgb(72, 116, 40), Rgb(96, 140, 48), Rgb(54, 92, 32)],
            tip: Rgb(142, 178, 62),
        },
        clear_frac: 0.12,
        limb_frac: 0.4,
        wander: 0.36,
        droop: 0.12,
        leaf_radius: 0.85,
        leaf_clusters: 2,
        crown_stretch: 0.8,
        whorl_step: 0.0,
        whorl_count: 0,
        crown_hollow: 1.0,
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

pub fn shrub() -> TreeParams {
    TreeParams {
        kind: VegetationKind::Shrub,
        habit: Habit::Broad,
        height: 2.2,
        trunk_width: 0.35,
        branch_levels: 2,
        children_per_node: (2, 3),
        spread_deg: (30.0, 60.0),
        length_ratio: 0.7,
        thickness_ratio: 0.6,
        leaf_density: 0.6,
        cutout_openness: 0.32,
        streamer_density: 0.0,
        palette: Palette {
            bark: vec![Rgb(78, 62, 44), Rgb(60, 48, 34)],
            leaf: vec![Rgb(66, 106, 40), Rgb(88, 128, 48), Rgb(50, 84, 32)],
            tip: Rgb(128, 162, 60),
        },
        clear_frac: 0.3,
        limb_frac: 0.55,
        wander: 0.34,
        droop: 0.1,
        leaf_radius: 0.7,
        leaf_clusters: 2,
        crown_stretch: 0.7,
        whorl_step: 0.0,
        whorl_count: 0,
        crown_hollow: 0.75,
        roots: 0,
    }
}

pub fn sapling() -> TreeParams {
    TreeParams {
        kind: VegetationKind::Sapling,
        habit: Habit::Deciduous,
        height: 3.0,
        trunk_width: 0.22,
        branch_levels: 1,
        children_per_node: (2, 2),
        spread_deg: (34.0, 60.0),
        length_ratio: 0.6,
        thickness_ratio: 0.6,
        leaf_density: 0.6,
        cutout_openness: 0.3,
        streamer_density: 0.0,
        palette: Palette {
            bark: vec![Rgb(96, 74, 48), Rgb(74, 58, 38)],
            leaf: vec![Rgb(86, 132, 46), Rgb(108, 152, 56), Rgb(66, 108, 38)],
            tip: Rgb(146, 184, 70),
        },
        clear_frac: 0.7,
        limb_frac: 0.35,
        wander: 0.2,
        droop: 0.05,
        leaf_radius: 0.6,
        leaf_clusters: 2,
        crown_stretch: 0.9,
        whorl_step: 0.0,
        whorl_count: 0,
        crown_hollow: 0.5,
        roots: 0,
    }
}

pub fn tall_grass() -> TreeParams {
    TreeParams {
        kind: VegetationKind::TallGrass,
        habit: Habit::Broad,
        height: 1.6,
        // Radius of the tuft on the ground rather than a stem thickness.
        trunk_width: 0.35,
        branch_levels: 0,
        children_per_node: (0, 0),
        spread_deg: (0.0, 0.0),
        length_ratio: 1.0,
        thickness_ratio: 1.0,
        leaf_density: 1.0,
        cutout_openness: 0.34,
        streamer_density: 0.0,
        palette: Palette {
            bark: vec![Rgb(96, 112, 52)],
            leaf: vec![Rgb(104, 142, 52), Rgb(126, 162, 62), Rgb(84, 120, 44)],
            tip: Rgb(166, 190, 88),
        },
        clear_frac: 0.0,
        limb_frac: 0.0,
        wander: 0.0,
        droop: 0.0,
        leaf_radius: 0.12,
        leaf_clusters: 1,
        crown_stretch: 1.4,
        whorl_step: 0.0,
        whorl_count: 0,
        crown_hollow: 0.0,
        roots: 0,
    }
}

pub fn dead_tree() -> TreeParams {
    TreeParams {
        kind: VegetationKind::DeadTree,
        habit: Habit::Deciduous,
        height: 13.0,
        trunk_width: 1.3,
        branch_levels: 5,
        children_per_node: (2, 3),
        spread_deg: (28.0, 58.0),
        length_ratio: 0.7,
        thickness_ratio: 0.6,
        leaf_density: 0.0,
        cutout_openness: 0.0,
        streamer_density: 0.0,
        palette: Palette {
            bark: vec![Rgb(112, 100, 84), Rgb(88, 78, 64), Rgb(134, 122, 104)],
            leaf: vec![Rgb(112, 100, 84)],
            tip: Rgb(134, 122, 104),
        },
        clear_frac: 0.32,
        limb_frac: 0.34,
        wander: 0.4,
        droop: 0.02,
        leaf_radius: 0.0,
        leaf_clusters: 0,
        crown_stretch: 1.0,
        whorl_step: 0.0,
        whorl_count: 0,
        crown_hollow: 1.0,
        roots: 4,
    }
}

pub fn mushroom_tree() -> TreeParams {
    TreeParams {
        kind: VegetationKind::MushroomTree,
        habit: Habit::Broad,
        height: 9.0,
        trunk_width: 1.5,
        branch_levels: 1,
        children_per_node: (0, 0),
        spread_deg: (0.0, 0.0),
        length_ratio: 1.0,
        thickness_ratio: 0.9,
        leaf_density: 0.95,
        cutout_openness: 0.18,
        streamer_density: 0.0,
        palette: Palette {
            bark: vec![Rgb(198, 190, 172), Rgb(172, 164, 148), Rgb(146, 138, 124)],
            leaf: vec![Rgb(124, 106, 132), Rgb(148, 128, 154), Rgb(102, 86, 110)],
            tip: Rgb(176, 156, 180),
        },
        // Nearly all trunk; the cap sits on top of it.
        clear_frac: 0.82,
        // Cap radius as a fraction of the height.
        limb_frac: 0.42,
        wander: 0.1,
        droop: 0.0,
        leaf_radius: 0.9,
        leaf_clusters: 1,
        crown_stretch: 0.35,
        whorl_step: 0.0,
        whorl_count: 0,
        crown_hollow: 0.15,
        roots: 5,
    }
}
