//! What a Dwarf Fortress entity is, and who draws it.
//!
//! Dwarf Fortress hands us one undifferentiated stream of tiles. Deciding what
//! each one is was scattered across the mesher as early-outs — a check for a
//! trunk here, a canopy part there, a render mode somewhere else — so a new
//! treatment meant editing three places and hoping nothing else matched first.
//!
//! This is the one place that decides. [`classify`] says what an entity is,
//! [`extent`] says how much of the map it is allowed to fill, and [`resolve`]
//! says who draws it: the sprite library that has always drawn everything, the
//! vegetation crate, or a crossed billboard. Nothing here knows about meshes
//! or chunks, so it can be tested on its own.
//!
//! Two rules hold for every override:
//!
//! - Dwarf Fortress gives the *bounds*, never the shape. A tree gets the height
//!   and reach of its tiles; a one-tile plant gets its tile. What grows inside
//!   that is the species preset's own, the same one the tree lab draws.
//! - A plant's seed is [`seed`]: a hash of the absolute tile it stands on and
//!   its species. Never render coordinates, never the tile configuration, so a
//!   plant is the same plant across a reload and wherever the origin sits.

use dfhack_remote::rfr::{TiletypeMaterial, TiletypeShape, TiletypeSpecial};
use dwarf_eye_trees as trees;
use dwarf_eye_trees::{TreeParams, VegetationKind};

/// What Dwarf Fortress says about one tile: everything the classifier reads.
#[derive(Clone, Copy, Debug)]
pub struct Tile<'a> {
    pub shape: TiletypeShape,
    pub material: TiletypeMaterial,
    pub special: TiletypeSpecial,
    /// DFHack's own name for the tiletype, such as `TreeDeadTrunkPillar`.
    pub name: &'a str,
    /// The raw id of the species standing here, empty when there is none.
    pub plant: &'a str,
}

impl Default for Tile<'_> {
    fn default() -> Self {
        Self {
            shape: TiletypeShape::NoShape,
            material: TiletypeMaterial::NoMaterial,
            special: TiletypeSpecial::NoSpecial,
            name: "",
            plant: "",
        }
    }
}

/// What the tiles around one tile say about it.
///
/// Only what a treatment cannot work out from the tile alone belongs here. A
/// cap tile is a floor or a wall as far as its shape goes, and DF's own tree
/// link is what separates a treetop from masonry.
#[derive(Clone, Copy, Debug, Default)]
pub struct Near {
    /// DF links this tile to a multi-tile plant standing somewhere else.
    pub in_tree: bool,
}

/// What an entity is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// Any part of a living multi-tile tree: trunk, limb, twig, cap.
    Tree,
    /// A low woody plant filling one tile.
    Shrub,
    /// A young tree, one tile of it so far.
    Sapling,
    /// Dead vegetation, whatever size: bare wood, no foliage.
    DeadTree,
    /// A tuft of blades.
    TallGrass,
    /// A rock lying on the floor.
    Boulder,
    /// Built work: a constructed wall or floor, or a building's tile. Nothing
    /// grows into it, and no crown is allowed to fill it.
    Built,
    /// Ground, walls, everything the sprite library already draws well.
    Other,
}

/// How much of the map an entity may fill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Extent {
    /// One tile, floor to ceiling. A shrub never leans into its neighbour.
    Tile,
    /// The tiles DF reports for a tree, read by `tree::Envelope`.
    Tree,
}

/// Who draws an entity.
#[derive(Clone, Debug)]
pub enum Treatment {
    /// The sprite library: DF's own art, extruded or laid flat.
    Sprite,
    /// Grown by `dwarf-eye-trees` from a species preset, inside the entity's
    /// extent.
    Grown(VegetationKind, Box<TreeParams>),
    /// Two crossed planes of sprite. The old treatment for standing plants,
    /// kept for comparison.
    Billboard,
}

impl Treatment {
    pub fn is_grown(&self) -> bool {
        matches!(self, Treatment::Grown(..))
    }
}

/// Which treatment one-tile plants get. Trees are always grown.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Style {
    /// Grown from the presets, the same way the lab grows them.
    #[default]
    Grown,
    /// The crossed sprite planes, for comparing against.
    Billboard,
}

impl Style {
    /// `DWARF_EYE_PLANTS=billboard` puts standing plants back on sprites.
    ///
    /// Read once. The mesher asks per tile, and the answer cannot change while
    /// the process runs.
    pub fn current() -> Self {
        static STYLE: std::sync::OnceLock<Style> = std::sync::OnceLock::new();
        *STYLE.get_or_init(|| match std::env::var("DWARF_EYE_PLANTS").as_deref() {
            Ok("billboard") | Ok("sprite") => Style::Billboard,
            _ => Style::Grown,
        })
    }
}

/// What an entity is, from its tile and what surrounds it.
pub fn classify(tile: Tile, near: Near) -> Class {
    use TiletypeShape as S;
    let dead = matches!(tile.special, TiletypeSpecial::Dead | TiletypeSpecial::SmoothDead)
        || tile.name.contains("Dead");
    let woody = tile.name.starts_with("Tree") || near.in_tree;

    // Built work first: someone raised it, so whatever shape it wears it is not
    // vegetation and nothing vegetal may grow into it.
    if tile.material == TiletypeMaterial::Construction {
        return Class::Built;
    }

    match tile.shape {
        // Limbs and twigs are only ever a tree's.
        S::Branch | S::TrunkBranch | S::Twig => {
            if dead {
                Class::DeadTree
            } else {
                Class::Tree
            }
        }
        S::Sapling => {
            if dead {
                Class::DeadTree
            } else {
                Class::Sapling
            }
        }
        // DF calls a tuft of blades a shrub too; its material is the giveaway.
        S::Shrub if grassy(tile.material) => Class::TallGrass,
        S::Shrub => {
            if dead {
                Class::DeadTree
            } else {
                Class::Shrub
            }
        }
        S::Boulder => Class::Boulder,
        // A trunk, a root, or the solid top of a cap tree: DF gives all three
        // an ordinary wall, ramp or floor shape, and only the name or the tree
        // link says otherwise.
        _ if woody && matches!(tile.material, TiletypeMaterial::TreeMaterial | TiletypeMaterial::Root)
            || woody && tile.name.starts_with("Tree") =>
        {
            if dead {
                Class::DeadTree
            } else {
                Class::Tree
            }
        }
        _ => Class::Other,
    }
}

/// Whether a material is one of DF's grasses.
fn grassy(material: TiletypeMaterial) -> bool {
    matches!(
        material,
        TiletypeMaterial::GrassLight
            | TiletypeMaterial::GrassDark
            | TiletypeMaterial::GrassDry
            | TiletypeMaterial::GrassDead
    )
}

/// How much room an entity has: its own tile, or the tiles DF gives its tree.
pub fn extent(tile: Tile) -> Extent {
    use TiletypeShape as S;
    match tile.shape {
        S::Sapling | S::Shrub | S::Boulder => Extent::Tile,
        _ => Extent::Tree,
    }
}

/// Who draws a class, and with what.
///
/// The parameters are the preset: the look the tree lab shows. Callers then
/// override the size from DF's bounds and the palette from the species' own
/// colours, and nothing else.
pub fn resolve(class: Class, style: Style) -> Treatment {
    let grown = |kind, params: TreeParams| Treatment::Grown(kind, Box::new(params));
    let standing = |kind, params: TreeParams| match style {
        Style::Grown => grown(kind, params),
        Style::Billboard => Treatment::Billboard,
    };
    match class {
        Class::Tree => grown(VegetationKind::Tree, trees::oak()),
        Class::DeadTree => grown(VegetationKind::DeadTree, trees::dead_tree()),
        Class::Shrub => standing(VegetationKind::Shrub, trees::shrub()),
        Class::Sapling => standing(VegetationKind::Sapling, trees::sapling()),
        Class::TallGrass => standing(VegetationKind::TallGrass, trees::tall_grass()),
        Class::Boulder => Treatment::Billboard,
        // Built work keeps DF's own art until the building prefabs land.
        Class::Built | Class::Other => Treatment::Sprite,
    }
}

/// What DF says about an entity, decided once per tiletype.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
    pub class: Class,
    pub extent: Extent,
}

impl Plan {
    /// Whether this entity is part of a multi-tile tree, whose bounds are read
    /// from the tiles DF reports for it rather than from this one tile.
    pub fn of_tree(self) -> bool {
        self.extent == Extent::Tree && matches!(self.class, Class::Tree | Class::DeadTree)
    }

    /// Whether this entity is grown rather than drawn from its sprite, so the
    /// tile mesher leaves it alone.
    ///
    /// Trees are always grown; the style only decides what happens to the
    /// plants that stand in a single tile.
    pub fn grown(self, style: Style) -> bool {
        match self.extent {
            Extent::Tree => self.of_tree(),
            Extent::Tile => {
                style == Style::Grown
                    && matches!(
                        self.class,
                        Class::Shrub | Class::Sapling | Class::TallGrass | Class::DeadTree
                    )
            }
        }
    }
}

/// The plan for one tile: what it is and how much room it has.
pub fn plan(tile: Tile, near: Near) -> Plan {
    Plan { class: classify(tile, near), extent: extent(tile) }
}

/// A plant's seed: the absolute tile it stands on, and what it is.
///
/// Absolute tiles rather than render coordinates, so a plant keeps its shape
/// when the origin moves; and not the tile configuration, so it does not change
/// as neighbouring tiles arrive.
pub fn seed(x: i32, y: i32, z: i32, species: i32) -> u64 {
    trees::rng::hash3(x, y, z, species as u64)
}

/// Whether a class is built work that vegetation may not grow into.
pub fn built(class: Class) -> bool {
    class == Class::Built
}

/// Paints a preset in the colour Dwarf Fortress gives this plant.
///
/// The preset carries the shape and the shading between its tones; DF's own
/// colour for the tile — which for a plant is the species' colour out of its
/// raws — carries the hue, so a rhubarb is not an oak-green shrub and a dead
/// stem is not a living one. The tones stay a spread rather than becoming one
/// flat colour, because a single-colour plant reads as a cardboard cut-out.
pub fn recolour(params: &mut TreeParams, class: Class, hint: [u8; 3]) {
    let hint = trees::Rgb(hint[0], hint[1], hint[2]);
    if class == Class::DeadTree {
        // Nothing but wood on a dead plant, so the hue belongs to the bark.
        for bark in &mut params.palette.bark {
            *bark = bark.lerp(hint, 0.5);
        }
        return;
    }
    for leaf in &mut params.palette.leaf {
        *leaf = leaf.lerp(hint, 0.6);
    }
    params.palette.tip = params.palette.tip.lerp(hint, 0.45);
}

/// How tall a plant standing in one tile grows before it is fitted into that
/// tile, in the preset's own units.
///
/// The preset's proportions are the look, so a plant is grown at the size the
/// grammar expects and then scaled into the tile DF gave it. A dead plant is
/// the exception: the dead-tree preset is a thirteen-tile snag, and a dead
/// shrub is a handful of bare stems.
pub fn standing_height(class: Class, params: &TreeParams) -> f32 {
    match class {
        Class::DeadTree => 2.4,
        _ => params.height,
    }
}

/// Whether a species hangs its foliage on streamers.
///
/// Nothing in DF's growth tokens says weeping; only the raw id does.
pub fn weeping(plant_id: &str) -> bool {
    plant_id.contains("WILLOW")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile<'a>(shape: TiletypeShape, name: &'a str) -> Tile<'a> {
        Tile { shape, name, ..Default::default() }
    }

    #[test]
    fn standing_plants_classify_by_shape() {
        assert_eq!(classify(tile(TiletypeShape::Shrub, "Shrub"), Near::default()), Class::Shrub);
        assert_eq!(
            classify(tile(TiletypeShape::Sapling, "Sapling"), Near::default()),
            Class::Sapling
        );
        assert_eq!(classify(tile(TiletypeShape::Boulder, "Boulder"), Near::default()), Class::Boulder);
        assert_eq!(
            classify(tile(TiletypeShape::Floor, "GrassLightFloor1"), Near::default()),
            Class::Other
        );
    }

    #[test]
    fn a_tree_is_every_part_of_one() {
        for (shape, name) in [
            (TiletypeShape::Wall, "TreeTrunkPillar"),
            (TiletypeShape::Ramp, "TreeTrunkSlopeNS"),
            (TiletypeShape::TrunkBranch, "TreeTrunkBranchN"),
            (TiletypeShape::Branch, "TreeBranchesNSEW"),
            (TiletypeShape::Twig, "TreeTwigs"),
            (TiletypeShape::Floor, "TreeCapFloor1"),
        ] {
            assert_eq!(classify(tile(shape, name), Near::default()), Class::Tree, "{name}");
            assert_eq!(extent(tile(shape, name)), Extent::Tree, "{name}");
        }
    }

    #[test]
    fn a_wall_is_masonry_until_df_links_it_to_a_tree() {
        let wall = Tile {
            shape: TiletypeShape::Wall,
            material: TiletypeMaterial::Stone,
            ..Default::default()
        };
        assert_eq!(classify(wall, Near::default()), Class::Other);
        let cap = Tile { material: TiletypeMaterial::TreeMaterial, ..wall };
        assert_eq!(classify(cap, Near { in_tree: true }), Class::Tree);
    }

    #[test]
    fn dead_vegetation_keeps_its_own_extent() {
        let dead_shrub = Tile {
            shape: TiletypeShape::Shrub,
            special: TiletypeSpecial::Dead,
            name: "ShrubDead",
            ..Default::default()
        };
        assert_eq!(classify(dead_shrub, Near::default()), Class::DeadTree);
        // Bare twigs in one tile, not a thirteen-tile snag.
        assert_eq!(extent(dead_shrub), Extent::Tile);

        let dead_trunk = tile(TiletypeShape::Wall, "TreeDeadTrunkPillar");
        assert_eq!(classify(dead_trunk, Near::default()), Class::DeadTree);
        assert_eq!(extent(dead_trunk), Extent::Tree);
    }

    #[test]
    fn a_grassy_shrub_is_a_tuft() {
        let grass = Tile {
            shape: TiletypeShape::Shrub,
            material: TiletypeMaterial::GrassLight,
            ..Default::default()
        };
        assert_eq!(classify(grass, Near::default()), Class::TallGrass);
    }

    #[test]
    fn built_work_is_never_vegetation() {
        // A constructed floor of the same stone as a cave floor, and a wall
        // someone raised: both are built, whatever shape DF gives them.
        for shape in [TiletypeShape::Floor, TiletypeShape::Wall, TiletypeShape::Fortification] {
            let built = Tile {
                shape,
                material: TiletypeMaterial::Construction,
                name: "ConstructedFloor",
                ..Default::default()
            };
            assert_eq!(classify(built, Near::default()), Class::Built);
            assert!(super::built(classify(built, Near::default())));
            assert!(matches!(resolve(Class::Built, Style::Grown), Treatment::Sprite));
        }
    }

    #[test]
    fn treatments_follow_the_class() {
        assert!(matches!(resolve(Class::Other, Style::Grown), Treatment::Sprite));
        assert!(matches!(resolve(Class::Boulder, Style::Grown), Treatment::Billboard));
        for (class, kind) in [
            (Class::Tree, VegetationKind::Tree),
            (Class::DeadTree, VegetationKind::DeadTree),
            (Class::Shrub, VegetationKind::Shrub),
            (Class::Sapling, VegetationKind::Sapling),
            (Class::TallGrass, VegetationKind::TallGrass),
        ] {
            match resolve(class, Style::Grown) {
                Treatment::Grown(got, params) => {
                    assert_eq!(got, kind, "{class:?}");
                    assert_eq!(params.kind, kind, "{class:?} preset");
                }
                other => panic!("{class:?} resolved to {other:?}"),
            }
        }
    }

    #[test]
    fn billboard_style_only_moves_the_standing_plants() {
        for class in [Class::Shrub, Class::Sapling, Class::TallGrass] {
            assert!(matches!(resolve(class, Style::Billboard), Treatment::Billboard));
        }
        // A tree is grown whatever the style says: the billboard path never
        // drew one in the first place.
        assert!(resolve(Class::Tree, Style::Billboard).is_grown());
    }

    #[test]
    fn a_plan_says_who_meshes_a_tile() {
        let shrub = plan(tile(TiletypeShape::Shrub, "Shrub"), Near::default());
        assert!(shrub.grown(Style::Grown));
        assert!(!shrub.grown(Style::Billboard), "the mesher draws the sprite again");
        let trunk = plan(tile(TiletypeShape::Wall, "TreeTrunkPillar"), Near::default());
        assert!(trunk.grown(Style::Billboard), "a tree is grown either way");
        let floor = plan(tile(TiletypeShape::Floor, "SoilFloor1"), Near::default());
        assert!(!floor.grown(Style::Grown));
    }

    #[test]
    fn a_seed_is_the_tile_and_the_species() {
        // Two plants of one species in neighbouring tiles are two plants.
        assert_ne!(seed(10, 20, 30, 4), seed(11, 20, 30, 4));
        assert_ne!(seed(10, 20, 30, 4), seed(10, 21, 30, 4));
        assert_ne!(seed(10, 20, 30, 4), seed(10, 20, 31, 4));
        // One species apart from another on the same tile.
        assert_ne!(seed(10, 20, 30, 4), seed(10, 20, 30, 5));
        // And the same plant every time it is asked for.
        assert_eq!(seed(-7, 3, 128, 9), seed(-7, 3, 128, 9));
    }

    #[test]
    fn only_willows_weep() {
        assert!(weeping("WILLOW"));
        assert!(weeping("BLACK_WILLOW"));
        assert!(!weeping("OAK"));
    }
}
