//! Natural ground as one smoothed sheet instead of a stack of terraces.
//!
//! Dwarf Fortress puts every floor at a whole z-level, so drawing each one as
//! a slab makes a hillside a staircase. The ground the game *means* is the
//! surface those slabs sample, and this reconstructs it: a height per tile
//! corner, smoothed, meshed as one continuous sheet per chunk.
//!
//! ## The corner rule
//!
//! Heights live on tile **corners**, not tile centres. Every corner is the
//! mean of what the four tiles touching it ask for, and both chunks either
//! side of a border compute a shared corner from the same four tiles, so the
//! sheet has no seam — the trick `water.rs` already uses for its surface.
//!
//! What a tile asks for:
//!
//! - a natural floor asks for its own slab top, `z + FLOOR_HEIGHT`;
//! - a natural ramp asks for [`ramp::slopes`] at each of its corners, which is
//!   exactly the corner field `ramp.rs` built its wedge from. The heightfield
//!   subsumes the natural ramp: those numbers become the height samples and
//!   the wedge is never drawn;
//! - a natural wall with natural ground on top of it asks for that ground's
//!   height, which is what makes a ramp's high edge meet the floor a level up
//!   rather than the wall's own lid;
//! - a wall with no ground above it, a constructed floor, a stair, a
//!   building's tile: nothing, and the corners it touches are **pinned**.
//!
//! ## Smoothing
//!
//! [`SWEEPS`] Jacobi sweeps, each pulling a corner [`RELAX`] of the way toward
//! the mean of its four cardinal neighbours. Pinned corners never move, so a
//! cliff edge, a road and a workshop floor keep the height the game gave them
//! and the smooth ground meets them flush.
//!
//! Two sweeps reach two rings, so a chunk is built over its own tiles plus a
//! [`BORDER`] of two, and every corner it keeps is the corner its neighbour
//! computes. That is the whole crack-freedom argument: same inputs, same
//! stencil, same answer.
//!
//! ## What it does not cover
//!
//! Constructed floors, buildings, stairs, water surfaces, stockpiles and
//! anything a tree or a plant grows out of keep their tile geometry. The
//! classification is the factory's ([`crate::factory::footing`]).

use crate::factory::Footing;
use crate::library::TileLibrary;
use crate::mesh::{FLOOR_HEIGHT, MeshData, MeshOptions, Z_SCALE, damp, jitter, to_linear};
use crate::model::{Caps, RenderMode};
use crate::palette::Solid;
use crate::ramp;
use crate::world::{BLOCK, Chunk, World};
use dwarf_eye_art::atlas::Rect;
use std::sync::OnceLock;

/// Tiles of context around a chunk, so a corner it keeps is smoothed from the
/// same neighbourhood its neighbour smooths it from.
pub const BORDER: i32 = 2;
/// Laplacian sweeps. Two rings of support, two sweeps: any more and a chunk
/// would need a wider border to stay seamless.
pub const SWEEPS: usize = 2;
/// How far one sweep pulls a corner toward the mean of its neighbours.
pub const RELAX: f32 = 0.5;

/// Which surface model the ground takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Ground {
    /// One smoothed sheet over natural ground and natural slopes.
    #[default]
    Smooth,
    /// A slab per tile and a wedge per ramp, the way it was before issue #6.
    Stepped,
}

impl Ground {
    /// `DWARF_EYE_GROUND=stepped` puts the terraces back.
    ///
    /// Read once: the mesher asks per chunk and the answer cannot change while
    /// the process runs.
    pub fn current() -> Self {
        static MODE: OnceLock<Ground> = OnceLock::new();
        *MODE.get_or_init(|| match std::env::var("DWARF_EYE_GROUND").as_deref() {
            Ok("stepped") | Ok("terrace") | Ok("terraces") | Ok("steps") => Ground::Stepped,
            _ => Ground::Smooth,
        })
    }

    pub fn smooth(self) -> bool {
        self == Ground::Smooth
    }

    /// What the HUD and the startup banner call it.
    pub fn name(self) -> &'static str {
        match self {
            Ground::Smooth => "smooth",
            Ground::Stepped => "stepped",
        }
    }
}

/// What one tile of the padded window offers the corner grid.
#[derive(Clone, Copy, Default)]
struct Column {
    /// Heights this tile asks for at its four corners, NW, NE, SW, SE, in
    /// render units. `None` where the tile knows no ground.
    corners: Option<[f32; 4]>,
    /// The heightfield draws this tile's own top: it is natural ground sitting
    /// on this very level, not a neighbour's ground seen from one level off.
    draws: bool,
    /// Holds the corners it touches against smoothing.
    pin: bool,
}

/// Corner heights over a square of tiles at one z-level.
///
/// Built per chunk by the mesher and per character by walk mode, from the same
/// code, so what the eye rides is what the eye sees.
pub struct Surface {
    /// Lower tile corner of the square and the level it was read at.
    origin: (i32, i32, i32),
    /// Tiles across the square, not counting the border.
    span: i32,
    /// Corner heights over `-BORDER ..= span + BORDER`, row-major on y.
    corners: Vec<Option<f32>>,
    /// Tile columns over `-BORDER - 1 ..= span + BORDER`, row-major on y.
    columns: Vec<Column>,
}

impl Surface {
    /// Corners across the grid.
    fn grid(span: i32) -> i32 {
        span + 2 * BORDER + 1
    }

    /// Tiles across the grid: one more than the corners, because the tile
    /// outside a border corner is what gives that corner its value.
    fn tiles(span: i32) -> i32 {
        span + 2 * BORDER + 2
    }

    /// The heightfield over one chunk's tiles.
    pub fn build(world: &World, chunk: &Chunk, opts: MeshOptions) -> Self {
        Self::over(world, chunk.origin(), BLOCK, opts)
    }

    /// The heightfield over a `span`-tile square whose lower corner and level
    /// are `origin`, in render tiles.
    pub fn over(world: &World, origin: (i32, i32, i32), span: i32, opts: MeshOptions) -> Self {
        let (tiles, grid) = (Self::tiles(span), Self::grid(span));
        let mut columns = Vec::with_capacity((tiles * tiles) as usize);
        for ty in 0..tiles {
            for tx in 0..tiles {
                columns.push(probe(
                    world,
                    origin.0 + tx - BORDER - 1,
                    origin.1 + ty - BORDER - 1,
                    origin.2,
                    opts,
                ));
            }
        }

        // Every corner is the mean of what the tiles touching it ask for, and
        // is pinned if any of them holds it.
        let n = (grid * grid) as usize;
        let (mut sum, mut count, mut pinned) = (vec![0.0f32; n], vec![0u32; n], vec![false; n]);
        for ty in 0..tiles {
            for tx in 0..tiles {
                let column = columns[(ty * tiles + tx) as usize];
                // Tile index `t` touches corner indices `t - 1` and `t`.
                for (slot, (gx, gy)) in
                    [(tx - 1, ty - 1), (tx, ty - 1), (tx - 1, ty), (tx, ty)].into_iter().enumerate()
                {
                    if gx < 0 || gy < 0 || gx >= grid || gy >= grid {
                        continue;
                    }
                    let at = (gy * grid + gx) as usize;
                    if let Some(corners) = column.corners {
                        sum[at] += corners[slot];
                        count[at] += 1;
                    }
                    pinned[at] |= column.pin;
                }
            }
        }
        let mut corners: Vec<Option<f32>> = (0..n)
            .map(|i| (count[i] > 0).then(|| sum[i] / count[i] as f32))
            .collect();

        // Jacobi, so the answer never depends on the order corners are walked
        // in and two chunks agree on the border they share.
        for _ in 0..SWEEPS {
            let before = corners.clone();
            for gy in 1..grid - 1 {
                for gx in 1..grid - 1 {
                    let at = (gy * grid + gx) as usize;
                    let Some(here) = before[at] else { continue };
                    if pinned[at] {
                        continue;
                    }
                    let mut total = 0.0;
                    let mut seen = 0;
                    for (dx, dy) in [(0, -1), (0, 1), (-1, 0), (1, 0)] {
                        if let Some(h) = before[((gy + dy) * grid + gx + dx) as usize] {
                            total += h;
                            seen += 1;
                        }
                    }
                    if seen > 0 {
                        corners[at] = Some(here + RELAX * (total / seen as f32 - here));
                    }
                }
            }
        }

        Self { origin, span, corners, columns }
    }

    /// Height at a corner of the square, in tiles from its lower corner.
    pub fn corner(&self, cx: i32, cy: i32) -> Option<f32> {
        let grid = Self::grid(self.span);
        let (gx, gy) = (cx + BORDER, cy + BORDER);
        if gx < 0 || gy < 0 || gx >= grid || gy >= grid {
            return None;
        }
        self.corners[(gy * grid + gx) as usize]
    }

    fn column(&self, tx: i32, ty: i32) -> Column {
        let tiles = Self::tiles(self.span);
        let (ix, iy) = (tx + BORDER + 1, ty + BORDER + 1);
        if ix < 0 || iy < 0 || ix >= tiles || iy >= tiles {
            return Column::default();
        }
        self.columns[(iy * tiles + ix) as usize]
    }

    /// The four corner heights of a tile, NW, NE, SW, SE, or `None` where any
    /// of them is unknown.
    pub fn tile_corners(&self, tx: i32, ty: i32) -> Option<[f32; 4]> {
        Some([
            self.corner(tx, ty)?,
            self.corner(tx + 1, ty)?,
            self.corner(tx, ty + 1)?,
            self.corner(tx + 1, ty + 1)?,
        ])
    }

    /// Whether the heightfield draws this tile's ground.
    pub fn covers(&self, tx: i32, ty: i32) -> bool {
        if tx < 0 || ty < 0 || tx >= self.span || ty >= self.span {
            return false;
        }
        self.column(tx, ty).draws && self.tile_corners(tx, ty).is_some()
    }

    /// The ground at a tile's centre.
    pub fn height(&self, tx: i32, ty: i32) -> Option<f32> {
        let c = self.tile_corners(tx, ty)?;
        Some((c[0] + c[1] + c[2] + c[3]) * 0.25)
    }

    /// The lowest of a tile's four corners: what a footprint sinks to.
    pub fn low(&self, tx: i32, ty: i32) -> Option<f32> {
        let c = self.tile_corners(tx, ty)?;
        Some(c[0].min(c[1]).min(c[2]).min(c[3]))
    }

    /// The ground under a render-space position, bilinear across the tile it
    /// falls in. This is what walk mode's eye rides.
    pub fn at(&self, x: f32, y: f32) -> Option<f32> {
        let (fx, fy) = ((x - self.origin.0 as f32).floor(), (y - self.origin.1 as f32).floor());
        let (u, v) = (x - self.origin.0 as f32 - fx, y - self.origin.1 as f32 - fy);
        let [nw, ne, sw, se] = self.tile_corners(fx as i32, fy as i32)?;
        let north = nw + (ne - nw) * u;
        let south = sw + (se - sw) * u;
        Some(north + (south - north) * v)
    }

    /// The upward normal at a corner, from central differences across the
    /// grid. One-sided where a neighbour is unknown, flat where neither is.
    fn normal(&self, cx: i32, cy: i32) -> [f32; 3] {
        let slope = |a: Option<f32>, b: Option<f32>, here: f32| match (a, b) {
            (Some(a), Some(b)) => (b - a) * 0.5,
            (Some(a), None) => here - a,
            (None, Some(b)) => b - here,
            (None, None) => 0.0,
        };
        let Some(here) = self.corner(cx, cy) else { return [0.0, 1.0, 0.0] };
        let dx = slope(self.corner(cx - 1, cy), self.corner(cx + 1, cy), here);
        let dz = slope(self.corner(cx, cy - 1), self.corner(cx, cy + 1), here);
        let n = [-dx, 1.0, -dz];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        [n[0] / len, n[1] / len, n[2] / len]
    }

    /// Draws the ground for every tile of a chunk the heightfield covers:
    /// one two-triangle patch per tile with smooth normals and the tile's own
    /// ground sprite projected straight down onto it, plus a skirt wherever
    /// the sheet ends at a drop.
    ///
    /// Returns the triangles emitted.
    pub fn emit(
        &self,
        world: &World,
        chunk: &Chunk,
        opts: MeshOptions,
        library: Option<&mut TileLibrary>,
        mesh: &mut MeshData,
    ) -> usize {
        let before = mesh.indices.len();
        let (ox, oy, oz) = chunk.origin();
        let mut library = library;
        for ly in 0..BLOCK {
            for lx in 0..BLOCK {
                if !self.covers(lx, ly) {
                    continue;
                }
                let voxel = chunk.get(lx, ly);
                if voxel.hidden && !opts.show_hidden {
                    continue;
                }
                let Some([nw, ne, sw, se]) = self.tile_corners(lx, ly) else { continue };
                let (x, y) = (ox + lx, oy + ly);
                let (fx, fz) = (x as f32, y as f32);

                let wobble = jitter(x, y, oz);
                let skin = library.as_deref_mut().and_then(|lib| ground_uv(lib, voxel));
                let base = to_linear(voxel.color, 1.0);
                let (uv, color) = match skin {
                    // A near-grey ground sheet is a pattern the tile's own
                    // material colours, exactly as the flat slab wore it.
                    Some((rect, true)) => {
                        let d = damp([base[0], base[1], base[2]], 0.35);
                        (rect, [d[0] * wobble, d[1] * wobble, d[2] * wobble, 1.0])
                    }
                    Some((rect, false)) => (rect, [wobble, wobble, wobble, 1.0]),
                    None => (
                        Rect { u0: 0.0, v0: 0.0, u1: 0.0, v1: 0.0 },
                        [base[0] * wobble, base[1] * wobble, base[2] * wobble, 1.0],
                    ),
                };
                let flat = skin.is_none();
                let corner_uv = |u: f32, v: f32| {
                    if flat { dwarf_eye_art::atlas::WHITE_UV } else { [u, v] }
                };

                // North at `v0`, as `model.rs:build_flat_tile` lays a slab out,
                // so the sheet lines up with the floors around it.
                let quad = [
                    ([fx, nw, fz], corner_uv(uv.u0, uv.v0), self.normal(lx, ly)),
                    ([fx, sw, fz + 1.0], corner_uv(uv.u0, uv.v1), self.normal(lx, ly + 1)),
                    ([fx + 1.0, se, fz + 1.0], corner_uv(uv.u1, uv.v1), self.normal(lx + 1, ly + 1)),
                    ([fx + 1.0, ne, fz], corner_uv(uv.u1, uv.v0), self.normal(lx + 1, ly)),
                ];
                let vertex = mesh.positions.len() as u32;
                for (position, uv, normal) in quad {
                    mesh.positions.push(position);
                    mesh.normals.push(normal);
                    mesh.colors.push(color);
                    mesh.uvs.push(uv);
                }
                // Fold along whichever diagonal the surface is flatter across,
                // so a saddle does not read as a crease.
                if (nw - se).abs() <= (ne - sw).abs() {
                    mesh.indices.extend_from_slice(&[
                        vertex,
                        vertex + 1,
                        vertex + 2,
                        vertex,
                        vertex + 2,
                        vertex + 3,
                    ]);
                } else {
                    mesh.indices.extend_from_slice(&[
                        vertex,
                        vertex + 1,
                        vertex + 3,
                        vertex + 1,
                        vertex + 2,
                        vertex + 3,
                    ]);
                }

                // The rim, on the same rule the floor slab used: a side shows
                // only where the tile beside it is open and nothing solid
                // stands under it.
                let drop = |dx: i32, dy: i32| {
                    let beside = world.voxel(x + dx, y + dy, oz);
                    let below = world.voxel(x + dx, y + dy, oz - 1);
                    beside.is_some_and(|n| n.solid.is_empty())
                        && !below.is_some_and(|n| n.solid.occludes())
                };
                let rim = color_shade(color, 0.72);
                let skirt = |mesh: &mut MeshData, a: [f32; 3], b: [f32; 3], normal: [f32; 3]| {
                    mesh.push_quad(
                        [
                            [a[0], a[1] - FLOOR_HEIGHT, a[2]],
                            a,
                            b,
                            [b[0], b[1] - FLOOR_HEIGHT, b[2]],
                        ],
                        normal,
                        rim,
                    );
                };
                let (p_nw, p_ne) = ([fx, nw, fz], [fx + 1.0, ne, fz]);
                let (p_sw, p_se) = ([fx, sw, fz + 1.0], [fx + 1.0, se, fz + 1.0]);
                if drop(0, -1) {
                    skirt(mesh, p_ne, p_nw, [0.0, 0.0, -1.0]);
                }
                if drop(0, 1) {
                    skirt(mesh, p_sw, p_se, [0.0, 0.0, 1.0]);
                }
                if drop(-1, 0) {
                    skirt(mesh, p_nw, p_sw, [-1.0, 0.0, 0.0]);
                }
                if drop(1, 0) {
                    skirt(mesh, p_se, p_ne, [1.0, 0.0, 0.0]);
                }
            }
        }
        (mesh.indices.len() - before) / 3
    }
}

fn color_shade(color: [f32; 4], factor: f32) -> [f32; 4] {
    [color[0] * factor, color[1] * factor, color[2] * factor, color[3]]
}

/// Where a covered tile's ground sprite sits in the atlas, and whether the
/// sheet is a pattern the tile's material colours.
///
/// Read back off the cached model rather than out of the library's private
/// tables, so the heightfield always wears exactly the sprite the flat slab
/// wore: a flat tile's own sheet, a ramp's flat ground family, and the ground
/// packed to stand under a boulder or a shrub.
fn ground_uv(library: &mut TileLibrary, voxel: crate::world::Voxel) -> Option<(Rect, bool)> {
    let model = match library.mode(voxel.tile_id)? {
        RenderMode::FlatTile => library.model(
            voxel.tile_id,
            voxel.mat_index,
            voxel.sand,
            Caps { top: true, bottom: true },
        ),
        // Mask zero is the flat slab of the ramp's own ground family.
        RenderMode::Ramp => library.ramp(voxel.tile_id, 0, voxel.sand),
        RenderMode::Billboard => library.ground_beneath(voxel.tile_id),
        _ => None,
    }?;
    let uvs = &model.mesh.uvs;
    if uvs.len() < 3 {
        return None;
    }
    Some((
        Rect { u0: uvs[0][0], v0: uvs[0][1], u1: uvs[2][0], v1: uvs[2][1] },
        model.tint,
    ))
}

/// Which of a ramp's eight neighbours are walls, the set `mesh.rs` and
/// `walk.rs` both build a slope from.
fn ramp_mask(world: &World, x: i32, y: i32, z: i32) -> u8 {
    let mut mask = 0;
    for (bit, dx, dy) in ramp::NEIGHBOURS {
        if world
            .voxel(x + dx, y + dy, z)
            .is_some_and(|n| matches!(n.solid, Solid::Cube | Solid::Fortification))
        {
            mask |= bit;
        }
    }
    mask
}

/// What one tile column offers the corner grid at level `z`.
///
/// The search reaches one level either way, and that is what keeps two chunks
/// stacked on each other agreeing: a wall at `z` with ground on top answers
/// with the ground above it, and open air at `z` answers with the ground
/// below, so both chunks name the same height for the same column.
fn probe(world: &World, x: i32, y: i32, z: i32, opts: MeshOptions) -> Column {
    let unknown = Column::default();
    let pinned = Column { corners: None, draws: false, pin: true };

    let Some(voxel) = world.voxel(x, y, z) else { return unknown };
    if voxel.hidden && !opts.show_hidden {
        return unknown;
    }
    if z > opts.z_ceiling {
        return unknown;
    }

    // Air first: an open tile is not built work, it is a view of the ground a
    // level down.
    if voxel.solid.is_empty() {
        return match ground_of(world, x, y, z - 1, opts) {
            Some(corners) => Column { corners: Some(corners), draws: false, pin: false },
            None => unknown,
        };
    }

    match footing_of(world, voxel.tile_id) {
        Footing::Ground | Footing::Slope => match ground_of(world, x, y, z, opts) {
            Some(corners) => Column { corners: Some(corners), draws: true, pin: false },
            None => pinned,
        },
        // A wall carries the ground standing on it, which is what gives a ramp
        // its high edge. A wall with no ground above is a cliff: it offers no
        // height and holds the corners it touches where they are.
        Footing::Cliff => match ground_of(world, x, y, z + 1, opts) {
            Some(corners) => Column { corners: Some(corners), draws: false, pin: false },
            None => pinned,
        },
        Footing::Tile => pinned,
    }
}

/// The four corner heights a natural ground or slope tile asks for, or `None`
/// where the tile is neither.
fn ground_of(world: &World, x: i32, y: i32, z: i32, opts: MeshOptions) -> Option<[f32; 4]> {
    if z > opts.z_ceiling {
        return None;
    }
    let voxel = world.voxel(x, y, z)?;
    if voxel.hidden && !opts.show_hidden {
        return None;
    }
    let base = z as f32 * Z_SCALE + FLOOR_HEIGHT;
    match footing_of(world, voxel.tile_id) {
        Footing::Ground => Some([base; 4]),
        // The ramp's own corner field, the one `ramp.rs` cut its wedge from.
        // Row 0 is north and column 0 west, so the order is NW, NE, SW, SE.
        Footing::Slope => {
            let slopes = ramp::slopes(ramp_mask(world, x, y, z));
            Some([
                base + slopes[0][0] * Z_SCALE,
                base + slopes[0][2] * Z_SCALE,
                base + slopes[2][0] * Z_SCALE,
                base + slopes[2][2] * Z_SCALE,
            ])
        }
        _ => None,
    }
}

/// What the factory makes of a tiletype's ground, straight off the palette so
/// the mesher and walk mode can both ask without a sprite library.
pub fn footing_of(world: &World, tile_id: i32) -> Footing {
    let Some(t) = world.palette.tiletype(tile_id) else { return Footing::Tile };
    let tile = crate::factory::Tile {
        shape: t.shape(),
        material: t.material(),
        special: t.special(),
        name: t.name(),
        plant: "",
    };
    crate::factory::footing(tile, crate::factory::classify(tile, Default::default()))
}

/// Where an entity standing in a tile puts its base.
///
/// The heightfield only ever lowers a thing: an entity stood on the tile's own
/// slab before and it may sink to meet the ground, never rise off it. `here`
/// is the ground at the tile centre for something that fills one tile, and the
/// lowest corner of the footprint for something wider.
pub fn grounded(stepped: f32, here: Option<f32>) -> f32 {
    match here {
        Some(h) => stepped.min(h),
        None => stepped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::{Palette, Rgb, Solid};
    use crate::world::{TILES_PER_BLOCK, Voxel};
    use dfhack_remote::rfr::{Tiletype, TiletypeList, TiletypeMaterial, TiletypeShape};

    /// Tiletype ids the tests build a world out of.
    const SOIL_FLOOR: i32 = 1;
    const SOIL_WALL: i32 = 2;
    const SOIL_RAMP: i32 = 3;
    const BUILT_FLOOR: i32 = 4;

    fn tiletype(id: i32, shape: TiletypeShape, material: TiletypeMaterial, name: &str) -> Tiletype {
        Tiletype {
            id,
            name: Some(name.to_string()),
            shape: Some(shape as i32),
            material: Some(material as i32),
            special: Some(dfhack_remote::rfr::TiletypeSpecial::Normal as i32),
            ..Default::default()
        }
    }

    fn world() -> World {
        use TiletypeMaterial as M;
        use TiletypeShape as S;
        let list = TiletypeList {
            tiletype_list: vec![
                tiletype(SOIL_FLOOR, S::Floor, M::Soil, "SoilFloor"),
                tiletype(SOIL_WALL, S::Wall, M::Soil, "SoilWall"),
                tiletype(SOIL_RAMP, S::Ramp, M::Soil, "SoilRamp"),
                tiletype(BUILT_FLOOR, S::Floor, M::Construction, "ConstructedFloor"),
            ],
        };
        World::new(Palette::new(list, Default::default()))
    }

    fn voxel(solid: Solid, tile_id: i32) -> Voxel {
        Voxel { solid, tile_id, color: Rgb::default(), ..Default::default() }
    }

    /// A level filled with one tile, with overrides at named tiles.
    fn level(
        world: &mut World,
        z: i32,
        fill: Voxel,
        at: &[((i32, i32), Voxel)],
    ) {
        for by in -1..=1 {
            for bx in -1..=1 {
                let mut voxels = vec![fill; TILES_PER_BLOCK];
                for &((x, y), v) in at {
                    let (tx, ty) = (x - bx * BLOCK, y - by * BLOCK);
                    if (0..BLOCK).contains(&tx) && (0..BLOCK).contains(&ty) {
                        voxels[(ty * BLOCK + tx) as usize] = v;
                    }
                }
                world.restore((bx, by, z), voxels);
            }
        }
    }

    fn opts() -> MeshOptions {
        MeshOptions { z_ceiling: i32::MAX, show_hidden: true }
    }

    #[test]
    fn flat_ground_stays_flat() {
        let mut w = world();
        level(&mut w, 0, voxel(Solid::Floor, SOIL_FLOOR), &[]);
        let surface = Surface::over(&w, (0, 0, 0), BLOCK, opts());
        for y in 0..BLOCK {
            for x in 0..BLOCK {
                assert!(surface.covers(x, y), "{x},{y} is soil floor");
                let h = surface.height(x, y).unwrap();
                assert!((h - FLOOR_HEIGHT).abs() < 1e-5, "{x},{y} at {h}");
            }
        }
    }

    /// Everything north of row 8 is a wall with ground standing on it, row 8
    /// is the ramp, everything south of it is flat: one straight slope
    /// climbing north, the shape `ramp::N` describes.
    fn hillside() -> World {
        let mut w = world();
        let mut ground = Vec::new();
        let mut above = Vec::new();
        for x in -16..32 {
            for y in -16..=7 {
                ground.push(((x, y), voxel(Solid::Cube, SOIL_WALL)));
                above.push(((x, y), voxel(Solid::Floor, SOIL_FLOOR)));
            }
            ground.push(((x, 8), voxel(Solid::Ramp, SOIL_RAMP)));
        }
        level(&mut w, 0, voxel(Solid::Floor, SOIL_FLOOR), &ground);
        level(&mut w, 1, voxel(Solid::Empty, 0), &above);
        w
    }

    #[test]
    fn a_ramp_tile_samples_the_corner_heights_ramp_rs_gave() {
        // The samples the heightfield takes off a natural ramp are the corner
        // field `ramp.rs` cut its wedge from — the same numbers walk mode
        // reads out of `ramp::slopes`. What smoothing then does to them is the
        // next test's business.
        let w = hillside();
        let slopes = ramp::slopes(ramp::N);
        for x in 2..14 {
            let got = ground_of(&w, x, 8, 0, opts()).expect("the ramp offers a sample");
            let want = [
                FLOOR_HEIGHT + slopes[0][0] * Z_SCALE,
                FLOOR_HEIGHT + slopes[0][2] * Z_SCALE,
                FLOOR_HEIGHT + slopes[2][0] * Z_SCALE,
                FLOOR_HEIGHT + slopes[2][2] * Z_SCALE,
            ];
            for (n, (a, b)) in got.iter().zip(want).enumerate() {
                assert!((a - b).abs() < 1e-6, "ramp at {x},8 corner {n}: {a} wants {b}");
            }
        }
        // And the wall carries the ground standing on it, which is what the
        // ramp's high edge has to meet.
        let wall = ground_of(&w, 4, 7, 1, opts()).expect("the floor over the wall");
        assert!((wall[0] - (Z_SCALE + FLOOR_HEIGHT)).abs() < 1e-6);
    }

    #[test]
    fn a_slope_climbs_without_a_step_in_it() {
        // Smoothing rounds the shoulder off the ramp, but the sheet still runs
        // uphill the whole way and meets both floors it connects.
        let w = hillside();
        let surface = Surface::over(&w, (0, 0, 0), BLOCK, opts());
        let column: Vec<f32> = (5..=11).map(|y| surface.corner(8, y).unwrap()).collect();
        for pair in column.windows(2) {
            assert!(pair[0] >= pair[1] - 1e-6, "the slope steps back up: {column:?}");
        }
        assert!((column[0] - (Z_SCALE + FLOOR_HEIGHT)).abs() < 1e-6, "top of the slope");
        assert!((column[column.len() - 1] - FLOOR_HEIGHT).abs() < 1e-6, "foot of the slope");
        // The whole climb is one level and nothing overshoots it.
        for h in &column {
            assert!((FLOOR_HEIGHT..=Z_SCALE + FLOOR_HEIGHT).contains(h), "{h} left the level");
        }
    }

    #[test]
    fn smoothing_leaves_a_cliff_edge_where_it_was() {
        // Floor at level 0, a two-level wall along x >= 8 with nothing on top:
        // a cliff. The ground beside it must not lean into it.
        let mut w = world();
        let walls: Vec<_> = (-16..32)
            .flat_map(|y| (8..32).map(move |x| ((x, y), voxel(Solid::Cube, SOIL_WALL))))
            .collect();
        level(&mut w, 0, voxel(Solid::Floor, SOIL_FLOOR), &walls);
        level(&mut w, 1, voxel(Solid::Empty, 0), &walls);
        let surface = Surface::over(&w, (0, 0, 0), BLOCK, opts());
        for y in 2..14 {
            for x in 0..8 {
                let h = surface.height(x, y).unwrap();
                assert!((h - FLOOR_HEIGHT).abs() < 1e-5, "{x},{y} leaned to {h}");
            }
            assert!(!surface.covers(8, y), "the wall is not ground");
        }
    }

    #[test]
    fn a_constructed_floor_keeps_its_tile_and_pins_the_ground_beside_it() {
        let mut w = world();
        let built: Vec<_> = (-16..32)
            .flat_map(|y| (8..32).map(move |x| ((x, y), voxel(Solid::Floor, BUILT_FLOOR))))
            .collect();
        level(&mut w, 0, voxel(Solid::Floor, SOIL_FLOOR), &built);
        let surface = Surface::over(&w, (0, 0, 0), BLOCK, opts());
        assert!(!surface.covers(9, 4), "a constructed floor keeps its slab");
        assert!(surface.covers(4, 4), "the soil beside it does not");
        let h = surface.height(7, 4).unwrap();
        assert!((h - FLOOR_HEIGHT).abs() < 1e-5, "the join stands at {h}");
    }

    #[test]
    fn neighbouring_chunks_agree_on_the_corners_they_share() {
        // A ridge running across the border, so the smoothing has something to
        // do at the seam.
        let mut w = world();
        let walls: Vec<_> = (-16..32)
            .flat_map(|y| (14..18).map(move |x| ((x, y), voxel(Solid::Cube, SOIL_WALL))))
            .collect();
        let above: Vec<_> = (-16..32)
            .flat_map(|y| (14..18).map(move |x| ((x, y), voxel(Solid::Floor, SOIL_FLOOR))))
            .collect();
        level(&mut w, 0, voxel(Solid::Floor, SOIL_FLOOR), &walls);
        level(&mut w, 1, voxel(Solid::Empty, 0), &above);

        let left = Surface::over(&w, (0, 0, 0), BLOCK, opts());
        let right = Surface::over(&w, (BLOCK, 0, 0), BLOCK, opts());
        for y in 0..=BLOCK {
            let a = left.corner(BLOCK, y);
            let b = right.corner(0, y);
            assert_eq!(a.is_some(), b.is_some(), "corner 16,{y} known on one side only");
            if let (Some(a), Some(b)) = (a, b) {
                assert!((a - b).abs() < 1e-6, "corner 16,{y}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn grounding_only_ever_goes_down() {
        assert_eq!(grounded(1.12, Some(0.8)), 0.8);
        assert_eq!(grounded(1.12, Some(1.4)), 1.12);
        assert_eq!(grounded(1.12, None), 1.12);
    }

    #[test]
    fn stepped_mode_draws_what_it_always_drew() {
        // Every heightfield path hangs off `covered`, which is false whenever
        // no surface was built, so stepped mode is the pass as it stood: flat
        // slabs, tallied as floors, with nothing in the surface column.
        use crate::mesh::{Budget, build_chunk_in};
        let mut w = world();
        level(&mut w, 0, voxel(Solid::Floor, SOIL_FLOOR), &[]);
        let chunk = w.chunk(0, 0, 0).unwrap();

        let mut stepped = Budget::default();
        let mesh =
            build_chunk_in(&w, chunk, opts(), None, &mut stepped, Ground::Stepped);
        assert_eq!(stepped.surface, 0, "stepped mode builds no sheet");
        assert_eq!(stepped.floors, 2 * (BLOCK * BLOCK) as usize, "a lid per tile");
        for p in &mesh.positions {
            assert!(p[1] == 0.0 || p[1] == FLOOR_HEIGHT, "a slab has two heights, not {}", p[1]);
        }

        let mut smooth = Budget::default();
        let _ = build_chunk_in(&w, chunk, opts(), None, &mut smooth, Ground::Smooth);
        assert_eq!(smooth.floors, 0, "the sheet took the ground over");
        assert_eq!(smooth.surface, 2 * (BLOCK * BLOCK) as usize, "a patch per tile");
    }
}
