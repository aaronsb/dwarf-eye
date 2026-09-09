//! The land beyond the loaded map.
//!
//! Dwarf Fortress only loads a small window of the world at full detail: in
//! adventure mode, 144 tiles square. Past that, DFHack still serves two lower
//! resolutions: region maps, one sample per 48-tile region tile for the world
//! tiles around the player (five by five of them in practice), and the world
//! map, one sample per 768-tile world tile. Everything here is built from those
//! two surveys.
//!
//! What comes out is one heightfield mesh plus a set of crown instances:
//!
//! - inside the region details, a terraced heightfield ([`terrace`]) refined to
//!   a 6-tile pitch near the window, quantised to whole z-levels so it steps
//!   the way fine tiles do, and snapped at its rim to the fine map's own column
//!   heights;
//! - one canonical crown per species ([`scatter`]) scattered by the region
//!   tile's vegetation and tree materials, seeded from absolute coordinates;
//! - rivers and site buildings off the same region tiles ([`features`]);
//! - beyond the region details, a smooth grid off the world map, where a
//!   z-level is under a pixel and terracing would buy nothing.
//!
//! Elevations are in the same units as z-levels once the map's z origin is
//! subtracted: `z = elevation - block_pos_z`.

pub mod features;
pub mod field;
pub mod fine;
pub mod grown;
pub mod scatter;
pub mod shade;
pub mod skin;
pub mod stitch;
pub mod terrace;

use crate::mesh::{MeshData, Z_SCALE};
use crate::palette::Palette;
use crate::world::World;
use dfhack_remote::rfr::{MapInfo, RegionMaps, RegionTile, WorldMap};
use dwarf_eye_art::atlas::WHITE_UV;
use dwarf_eye_trees::Preset;
use std::collections::HashMap;

use field::{Cell, Field};
use fine::FineSurface;
use scatter::{Clearing, CrownInstance, Patch, STAGES, Stage};
use skin::Skins;
use shade::{WATER, jitter, to_linear, top_color};
use terrace::{FAR, Terrain, smooth_height};

/// Tiles per region tile, and region tiles per world tile.
pub const REGION_TILE: i32 = 48;
pub const REGIONS_PER_WORLD: i32 = 16;
/// Region maps carry a one-tile overlap on each side.
const REGION_MAP_SIDE: i32 = 17;
/// How far the survey is filled in around the window, in region tiles, so an
/// interpolation stencil at the outermost terrace still has four samples.
const FIELD_MARGIN: i32 = 3;

/// Everything the renderer needs for one horizon: the ground as a single mesh,
/// and the trees on it as instances of a handful of meshes.
#[derive(Default)]
pub struct Horizon {
    pub mesh: MeshData,
    /// One entry per species, growth variant and detail stage; Bevy batches
    /// instances that share a mesh, so this is a handful of draw calls however
    /// many trees there are.
    ///
    /// Every tree appears once in each stage: which one draws is the camera's
    /// distance to it, decided by the `VisibilityRange` on each entity.
    pub crowns: Vec<CrownBatch>,
    /// Canopy cover the fine map holds, over the whole window and at its
    /// centre: the cue the scatter blends away from. Reported, not used.
    pub fine_density: f32,
    pub edge_density: f32,
    /// Site footprints the scatter kept clear of.
    pub clearings: usize,
}

impl Horizon {
    /// Triangles the ground costs, plus the coarsest tree stage: what a view
    /// with no tree near it draws.
    pub fn triangle_count(&self) -> usize {
        self.mesh.triangle_count() + self.stage_triangles(Stage::Box)
    }

    /// Triangles one stage of the tree chain would cost if every tree drew at
    /// it. The stages are alternatives, never a sum.
    pub fn stage_triangles(&self, stage: Stage) -> usize {
        self.crowns
            .iter()
            .filter(|b| b.stage == stage)
            .map(|b| b.mesh.triangle_count() * b.instances.len())
            .sum()
    }

    /// Distinct trees. Each is one entity per stage, so the entity count is
    /// this times [`STAGES`]'s length.
    pub fn instance_count(&self) -> usize {
        self.crowns
            .iter()
            .filter(|b| b.stage == STAGES[0])
            .map(|b| b.instances.len())
            .sum()
    }

    pub fn entity_count(&self) -> usize {
        self.crowns.iter().map(|b| b.instances.len()).sum()
    }
}

/// One species at one growth variant and one detail stage: the mesh, and every
/// place it stands.
pub struct CrownBatch {
    pub preset: Preset,
    pub stage: Stage,
    /// Which of the species' canonical growths, for [`Stage::Grown`]. The two
    /// box stages have one mesh a species and leave this at zero.
    pub variant: u32,
    /// How tall the mesh itself stands, in tiles: an instance's transform is
    /// scaled by its own height over this.
    pub mesh_height: f32,
    pub mesh: MeshData,
    pub instances: Vec<CrownInstance>,
}

/// Where the detailed map sits, in region tiles, and where the render origin
/// is, in absolute tiles and z-level.
pub struct Window {
    pub x0: i32,
    pub y0: i32,
    pub origin: (i32, i32, i32),
    /// The live window's centre, in render-local tiles: what the bands are
    /// measured out from.
    pub centre: (i32, i32),
}

impl Window {
    pub fn from_info(info: &MapInfo, origin: (i32, i32, i32)) -> Self {
        let bounds = live_window(info, origin);
        Self {
            x0: info.block_pos_x(),
            y0: info.block_pos_y(),
            origin,
            centre: ((bounds.0 + bounds.2) / 2, (bounds.1 + bounds.3) / 2),
        }
    }

    /// Render height of an absolute elevation.
    pub fn height(&self, elevation: i32) -> f32 {
        (elevation - self.origin.2) as f32 * Z_SCALE
    }

    /// Render x/z of a region tile's centre.
    pub fn centre_of(&self, rx: i32, ry: i32) -> (f32, f32) {
        (
            (rx * REGION_TILE - self.origin.0 + REGION_TILE / 2) as f32,
            (ry * REGION_TILE - self.origin.1 + REGION_TILE / 2) as f32,
        )
    }
}

/// The live (detailed) window's footprint, in render-local tile coordinates.
fn live_window(info: &MapInfo, origin: (i32, i32, i32)) -> (i32, i32, i32, i32) {
    let x0 = info.block_pos_x() * REGION_TILE - origin.0;
    let y0 = info.block_pos_y() * REGION_TILE - origin.1;
    (x0, y0, x0 + info.block_size_x() * crate::world::BLOCK, y0 + info.block_size_y() * crate::world::BLOCK)
}

fn overlaps(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    a.0 < b.2 && a.2 > b.0 && a.1 < b.3 && a.3 > b.1
}

/// The local coordinates of a region map's `i`th tile. `RegionMap.tiles` is
/// indexed `y * 17 + x`, with the 17th row and column overlapping the next
/// world tile and skipped.
fn region_index(i: i32, transpose: bool) -> Option<(i32, i32)> {
    let (lx, ly) = if transpose {
        (i / REGION_MAP_SIDE, i % REGION_MAP_SIDE)
    } else {
        (i % REGION_MAP_SIDE, i / REGION_MAP_SIDE)
    };
    (lx < REGIONS_PER_WORLD && ly < REGIONS_PER_WORLD).then_some((lx, ly))
}

fn region_cell(palette: &Palette, tile: &RegionTile) -> Cell {
    let elevation = tile.elevation() as f32;
    Cell {
        elevation,
        water: tile.water_elevation.map(|w| w as f32).unwrap_or(elevation),
        vegetation: tile.vegetation() as f32,
        rainfall: tile.rainfall() as f32,
        drainage: tile.drainage() as f32,
        snow: tile.snow() as f32,
        ground: tile.surface_material.as_ref().and_then(|p| palette.material_color(p)),
        detail: true,
    }
}

/// Builds the outer terrain. `world` supplies the fine map's own surface, so
/// the coarse bands meet it at the right height and stand out of its way.
pub fn build(
    palette: &Palette,
    info: &MapInfo,
    origin: (i32, i32, i32),
    regions: &RegionMaps,
    world_map: &WorldMap,
    world: &World,
    skins: &Skins,
    transpose: bool,
) -> Horizon {
    let window = Window::from_info(info, origin);
    let fine = FineSurface::survey(world);
    let mut field = Field::default();

    // The region survey: the only source with species, rivers and sites in it.
    for map in &regions.region_maps {
        let (wx, wy) = (map.map_x(), map.map_y());
        for (i, tile) in map.tiles.iter().enumerate() {
            let Some((lx, ly)) = region_index(i as i32, transpose) else { continue };
            field.insert(
                wx * REGIONS_PER_WORLD + lx,
                wy * REGIONS_PER_WORLD + ly,
                region_cell(palette, tile),
            );
        }
    }

    // The world map is on its own elevation scale; shift it by the mean gap
    // between the two surveys where they overlap, so the far land joins the
    // near without a step.
    let bias = world_bias(&field, regions, world_map);
    let sample_world = |wx: i32, wy: i32| -> Option<Cell> {
        if wx < 0 || wy < 0 || wx >= world_map.world_width || wy >= world_map.world_height {
            return None;
        }
        let i = (wy * world_map.world_width + wx) as usize;
        let elevation = *world_map.elevation.get(i)? as f32;
        Some(Cell {
            elevation,
            water: world_map.water_elevation.get(i).map(|w| *w as f32).unwrap_or(elevation),
            vegetation: world_map.vegetation.get(i).copied().unwrap_or(0) as f32,
            rainfall: world_map.rainfall.get(i).copied().unwrap_or(0) as f32,
            drainage: world_map.drainage.get(i).copied().unwrap_or(0) as f32,
            snow: 0.0,
            ground: None,
            detail: false,
        })
    };

    // Fill the rest of the terraced bands' footprint from the world map, so the
    // field answers everywhere the bands ask.
    let reach = FAR / REGION_TILE + FIELD_MARGIN;
    let centre_rx = (window.centre.0 + origin.0).div_euclid(REGION_TILE);
    let centre_ry = (window.centre.1 + origin.1).div_euclid(REGION_TILE);
    for ry in centre_ry - reach..=centre_ry + reach {
        for rx in centre_rx - reach..=centre_rx + reach {
            if field.contains(rx, ry) {
                continue;
            }
            // Bilinear between the four nearest world tile centres.
            let fx = (rx as f32 + 0.5) / REGIONS_PER_WORLD as f32 - 0.5;
            let fy = (ry as f32 + 0.5) / REGIONS_PER_WORLD as f32 - 0.5;
            let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            let mut acc = Cell::default();
            let mut weight = 0.0;
            for ((cx, cy), w) in [
                ((x0, y0), (1.0 - tx) * (1.0 - ty)),
                ((x0 + 1, y0), tx * (1.0 - ty)),
                ((x0, y0 + 1), (1.0 - tx) * ty),
                ((x0 + 1, y0 + 1), tx * ty),
            ] {
                let Some(c) = sample_world(cx, cy) else { continue };
                acc.elevation += c.elevation * w;
                acc.water += c.water * w;
                acc.vegetation += c.vegetation * w;
                acc.rainfall += c.rainfall * w;
                acc.drainage += c.drainage * w;
                weight += w;
            }
            if weight <= 0.0 {
                continue;
            }
            acc.elevation = acc.elevation / weight - bias;
            acc.water = acc.water / weight - bias;
            acc.vegetation /= weight;
            acc.rainfall /= weight;
            acc.drainage /= weight;
            field.insert(rx, ry, acc);
        }
    }

    let terrain = Terrain {
        field: &field,
        fine: &fine,
        window: &window,
        skins,
        crown_near: scatter::NEAR,
        crown_reach: scatter::REACH,
    };

    let mut mesh = MeshData::default();
    terrain.emit(&mut mesh);

    // Rivers and sites, read off the same region tiles. Rivers skip wherever
    // the fine map already stands in for the ground; buildings skip only the
    // live window itself, since a walked-and-cached tile still needs its coarse
    // building drawn.
    let bounds = live_window(info, origin);
    let mut crowns: Vec<(Preset, CrownInstance)> = Vec::new();
    let mut clearings: Vec<Clearing> = Vec::new();
    let mut mixes: std::collections::BTreeMap<(i32, i32), (Vec<Preset>, i32, f32)> =
        Default::default();
    for map in &regions.region_maps {
        let (wx, wy) = (map.map_x(), map.map_y());
        for (i, tile) in map.tiles.iter().enumerate() {
            let Some((lx, ly)) = region_index(i as i32, transpose) else { continue };
            let (rx, ry) = (wx * REGIONS_PER_WORLD + lx, wy * REGIONS_PER_WORLD + ly);
            let (ox, oz) = (rx * REGION_TILE - origin.0, ry * REGION_TILE - origin.1);
            let covered = fine.covers(ox + REGION_TILE / 2, oz + REGION_TILE / 2);

            if !covered && let Some(river) = tile.river_tiles.as_ref() {
                features::emit_river(&mut mesh, &window, rx, ry, river);
            }
            let stone = features::stone_color(
                tile.stone_materials.first().and_then(|p| palette.material_color(p)),
            );
            for building in &tile.buildings {
                let footprint = features::building_bounds(&window, rx, ry, building);
                // A site keeps a clearing around itself whether or not its own
                // box is drawn: the live window draws the real thing.
                clearings.push(Clearing {
                    x0: footprint.0 as f32,
                    z0: footprint.1 as f32,
                    x1: footprint.2 as f32,
                    z1: footprint.3 as f32,
                });
                if overlaps(footprint, bounds) {
                    continue;
                }
                features::emit_building(&mut mesh, &terrain, &window, rx, ry, building, stone);
            }

            mixes.insert(
                (rx, ry),
                (species_mix(palette, tile), tile.vegetation(), tile.elevation() as f32),
            );
        }
    }

    // A patch takes a few of its trees from next door, so a biome boundary
    // interleaves rather than switching on a 48-tile line.
    for (&(rx, ry), (presets, vegetation, elevation)) in &mixes {
        let mut neighbours: Vec<Preset> = Vec::new();
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let Some((mix, _, _)) = mixes.get(&(rx + dx, ry + dy)) else { continue };
            for preset in mix {
                if !presets.contains(preset) && !neighbours.contains(preset) {
                    neighbours.push(*preset);
                }
            }
        }
        neighbours.sort_by_key(|p| p.name());
        scatter::scatter(
            &Patch {
                rx,
                ry,
                vegetation: *vegetation,
                elevation: *elevation,
                presets: presets.clone(),
                neighbours,
            },
            &terrain,
            &window,
            &clearings,
            &mut crowns,
        );
    }

    emit_world_grid(&mut mesh, &window, world_map, bias, &fine);
    Horizon {
        mesh,
        crowns: batch_crowns(crowns),
        fine_density: fine.mean_density(),
        edge_density: fine
            .nearby_density(window.centre.0, window.centre.1, scatter::BLEND_BLOCKS)
            .map(|(d, _)| d)
            .unwrap_or(0.0),
        clearings: clearings.len(),
    }
}

/// The species a region tile names, in the order DF listed them, deduplicated.
fn species_mix(palette: &Palette, tile: &RegionTile) -> Vec<Preset> {
    let mut mix: Vec<Preset> = Vec::new();
    for preset in tile
        .tree_materials
        .iter()
        .filter_map(|p| palette.material_name(p))
        .map(scatter::preset_for)
    {
        if !mix.contains(&preset) {
            mix.push(preset);
        }
    }
    mix
}

/// The mean gap between the world map's elevations and the region survey's,
/// over the world tiles both cover.
fn world_bias(field: &Field, regions: &RegionMaps, world_map: &WorldMap) -> f32 {
    let (mut gap, mut n) = (0.0, 0);
    for map in &regions.region_maps {
        let (wx, wy) = (map.map_x(), map.map_y());
        if wx < 0 || wy < 0 || wx >= world_map.world_width || wy >= world_map.world_height {
            continue;
        }
        let Some(&elevation) = world_map.elevation.get((wy * world_map.world_width + wx) as usize) else {
            continue;
        };
        let (mut sum, mut count) = (0.0, 0);
        for ly in 0..REGIONS_PER_WORLD {
            for lx in 0..REGIONS_PER_WORLD {
                if let Some(c) = field.get(wx * REGIONS_PER_WORLD + lx, wy * REGIONS_PER_WORLD + ly) {
                    sum += c.elevation;
                    count += 1;
                }
            }
        }
        if count > 0 {
            gap += elevation as f32 - sum / count as f32;
            n += 1;
        }
    }
    if n > 0 { gap / n as f32 } else { 0.0 }
}

/// The rest of the world at one sample per world tile, smooth because at that
/// range a z-level is under a pixel. It runs a world tile under the terraced
/// bands and sits lower there, so the join is a step hidden beneath the finer
/// surface rather than a crack.
fn emit_world_grid(mesh: &mut MeshData, window: &Window, world_map: &WorldMap, bias: f32, fine: &FineSurface) {
    let spacing = REGION_TILE * REGIONS_PER_WORLD;
    let mut samples: HashMap<(i32, i32), (f32, [f32; 4], bool)> = HashMap::new();
    for wy in 0..world_map.world_height {
        for wx in 0..world_map.world_width {
            let i = (wy * world_map.world_width + wx) as usize;
            let Some(&elevation) = world_map.elevation.get(i) else { continue };
            let elevation = elevation as f32;
            let cell = Cell {
                elevation: elevation - bias,
                water: world_map.water_elevation.get(i).map(|w| *w as f32 - bias).unwrap_or(elevation - bias),
                vegetation: world_map.vegetation.get(i).copied().unwrap_or(0) as f32,
                rainfall: world_map.rainfall.get(i).copied().unwrap_or(0) as f32,
                drainage: world_map.drainage.get(i).copied().unwrap_or(0) as f32,
                snow: 0.0,
                ground: None,
                detail: false,
            };
            let (px, pz) = (wx * spacing - window.origin.0 + spacing / 2, wy * spacing - window.origin.1 + spacing / 2);
            let r = (px - window.centre.0).abs().max((pz - window.centre.1).abs());
            let under = r < FAR + spacing;
            let color = if cell.underwater() {
                WATER
            } else {
                jitter(top_color(&cell, 1.0), px + window.origin.0, pz + window.origin.1, 0.05)
            };
            samples.insert(
                (wx, wy),
                (
                    smooth_height(window, cell.surface()) - if under { 4.0 } else { 0.0 },
                    to_linear(color),
                    // Quads wholly under the terraces or the fine map are not
                    // drawn at all.
                    r < FAR - spacing || fine.covers(px, pz),
                ),
            );
        }
    }

    let mut index_of: HashMap<(i32, i32), u32> = HashMap::new();
    let mut keys: Vec<(i32, i32)> = samples.keys().copied().collect();
    keys.sort_unstable();
    for key in &keys {
        let (y, color, _) = samples[key];
        let (x, z) = *key;
        let h = |dx: i32, dz: i32| samples.get(&(x + dx, z + dz)).map(|s| s.0).unwrap_or(y);
        let dx = (h(1, 0) - h(-1, 0)) / (2.0 * spacing as f32);
        let dz = (h(0, 1) - h(0, -1)) / (2.0 * spacing as f32);
        index_of.insert(*key, mesh.positions.len() as u32);
        mesh.positions.push([
            (x * spacing - window.origin.0 + spacing / 2) as f32,
            y,
            (z * spacing - window.origin.1 + spacing / 2) as f32,
        ]);
        mesh.normals.push(normalize([-dx, 1.0, -dz]));
        mesh.colors.push(color);
        mesh.uvs.push(WHITE_UV);
    }
    for key in &keys {
        let (x, z) = *key;
        let corners = [(x, z), (x + 1, z), (x, z + 1), (x + 1, z + 1)];
        let Some(indices) = corners.iter().map(|c| index_of.get(c).copied()).collect::<Option<Vec<u32>>>()
        else {
            continue;
        };
        if corners.iter().all(|c| samples[c].2) {
            continue;
        }
        mesh.indices.extend_from_slice(&[indices[0], indices[2], indices[1], indices[1], indices[2], indices[3]]);
    }
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-6);
    [v[0] / len, v[1] / len, v[2] / len]
}

/// Groups the placed trees by species, growth variant and stage, and builds
/// the one mesh each group shares.
///
/// Every tree lands in one group per stage. Only the nearest stage varies by
/// growth variant; the two box stages have one mesh a species, so their groups
/// hold the whole species.
fn batch_crowns(instances: Vec<(Preset, CrownInstance)>) -> Vec<CrownBatch> {
    let mut groups: HashMap<(Preset, Stage, u32), Vec<CrownInstance>> = HashMap::new();
    for (preset, instance) in instances {
        for stage in STAGES {
            let variant = if stage == Stage::Grown { instance.variant } else { 0 };
            groups.entry((preset, stage, variant)).or_default().push(instance);
        }
    }
    let mut batches: Vec<CrownBatch> = groups
        .into_iter()
        .map(|((preset, stage, variant), instances)| {
            let mesh = crown_mesh(preset, stage, variant);
            CrownBatch { preset, stage, variant, mesh_height: mesh_height(&mesh), mesh, instances }
        })
        .collect();
    // A stable order, so a rebuild spawns the same batches in the same order.
    batches.sort_by_key(|b| (b.preset.name(), b.stage, b.variant));
    batches
}

/// How tall a stage's mesh stands, so an instance can be scaled to the height
/// the scatter gave it whichever stage is drawing.
fn mesh_height(mesh: &MeshData) -> f32 {
    mesh.positions.iter().map(|p| p[1]).fold(0.0f32, f32::max).max(0.5)
}

/// One species' crown at one stage, in this crate's mesh buffers.
fn crown_mesh(preset: Preset, stage: Stage, variant: u32) -> MeshData {
    let source = match stage {
        Stage::Grown => return grown::mesh(preset, variant),
        Stage::Crown => dwarf_eye_trees::crown(preset),
        Stage::Box => dwarf_eye_trees::crown_box(preset),
    };
    MeshData {
        uvs: vec![WHITE_UV; source.positions.len()],
        positions: source.positions,
        normals: source.normals,
        colors: source.colors,
        indices: source.indices,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scatter::{Patch, crown_count};
    use terrace::cell_centre;

    fn flat_field(elevation: f32) -> Field {
        let mut field = Field::default();
        for ry in -40..40 {
            for rx in -40..40 {
                field.insert(
                    rx,
                    ry,
                    Cell {
                        elevation,
                        water: elevation,
                        vegetation: 60.0,
                        rainfall: 50.0,
                        drainage: 40.0,
                        snow: 0.0,
                        ground: None,
                        detail: true,
                    },
                );
            }
        }
        field
    }

    fn window() -> Window {
        Window { x0: 0, y0: 0, origin: (0, 0, 100), centre: (72, 72) }
    }

    /// Quantising must not move the ground more than half a level, plus the
    /// floor slab it stands on.
    #[test]
    fn quantisation_stays_within_a_level() {
        let field = flat_field(140.7);
        let window = window();
        let fine = FineSurface::default();
        let skins = Skins::default();
        let terrain = Terrain {
            field: &field,
            fine: &fine,
            window: &window,
            skins: &skins,
            crown_near: scatter::NEAR,
            crown_reach: scatter::REACH,
        };
        for tz in (-600..600).step_by(37) {
            for tx in (-600..600).step_by(41) {
                let Some(level) = terrain.level_at(tx, tz) else { continue };
                let pitch = terrain.pitch_at(tx, tz).unwrap();
                let smooth = field
                    .relieved(cell_centre(tx, pitch) + window.origin.0, cell_centre(tz, pitch) + window.origin.1)
                    .surface()
                    - window.origin.2 as f32;
                let step = (terrain.slab_top(level) - smooth).abs();
                assert!(step <= 0.5 + crate::mesh::FLOOR_HEIGHT + 1e-4, "at ({tx}, {tz}): moved {step}");
            }
        }
    }

    /// The stitch: a coarse cell beside a fine block column stands at that
    /// column's own level, so the two surfaces are one plane.
    #[test]
    fn rim_cells_snap_to_the_fine_columns() {
        let field = flat_field(140.0);
        let window = window();
        // One grounded block column, at block (0, 0), whose ground is 11
        // levels below what the survey says.
        let fine = FineSurface::from_columns(&[((0, 0), 29)]);
        let skins = Skins::default();
        let terrain = Terrain {
            field: &field,
            fine: &fine,
            window: &window,
            skins: &skins,
            crown_near: scatter::NEAR,
            crown_reach: scatter::REACH,
        };
        assert_eq!(terrain.level_at(8, 8), Some(29), "inside the fine column");
        // The 6-tile cells straddling the column's edge: their neighbour is
        // fine ground, so they take its level rather than the survey's 40.
        assert_eq!(terrain.level_at(18, 8), Some(29), "east of the column");
        assert_eq!(terrain.level_at(8, 18), Some(29), "south of the column");
        // Two cells out, the survey is back in charge.
        assert_eq!(terrain.level_at(28, 8), Some(40), "clear of the column");
    }

    const NO_SKINS: Skins = Skins::none();

    fn terrain_for<'a>(field: &'a Field, fine: &'a FineSurface, window: &'a Window) -> Terrain<'a> {
        Terrain {
            field,
            fine,
            window,
            skins: &NO_SKINS,
            crown_near: scatter::NEAR,
            crown_reach: scatter::REACH,
        }
    }

    fn placed(vegetation: i32, fine: &FineSurface) -> Vec<(Preset, CrownInstance)> {
        let field = flat_field(140.0);
        let window = window();
        let terrain = terrain_for(&field, fine, &window);
        let mut out = Vec::new();
        scatter::scatter(
            &Patch {
                rx: 5,
                ry: -3,
                vegetation,
                elevation: 120.0,
                presets: vec![Preset::Oak, Preset::Birch],
                neighbours: vec![Preset::Willow],
            },
            &terrain,
            &window,
            &[],
            &mut out,
        );
        out
    }

    #[test]
    fn scatter_is_deterministic() {
        let fine = FineSurface::default();
        let a = placed(80, &fine);
        let b = placed(80, &fine);
        assert!(!a.is_empty());
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.0, y.0);
            assert_eq!(x.1, y.1);
        }
    }

    /// Thinning a patch drops the tail; it never reshuffles what is left, so a
    /// tree does not hop as the density rule changes around it.
    #[test]
    fn scatter_never_shuffles() {
        let fine = FineSurface::default();
        let sparse = placed(40, &fine);
        let dense = placed(90, &fine);
        assert!(sparse.len() < dense.len(), "{} vs {}", sparse.len(), dense.len());
        for (i, tree) in sparse.iter().enumerate() {
            assert_eq!(tree.1, dense[i].1, "instance {i} moved");
            assert_eq!(tree.0, dense[i].0, "instance {i} changed species");
        }
    }

    /// The first ring of coarse cells beyond a treeless window carries no
    /// crowns at all, whatever the survey says.
    #[test]
    fn a_treeless_window_edge_leaves_the_first_ring_bare() {
        // Region tile (5, -3) starts at tile (240, -144). Fine ground right
        // beside it, with no trees on it.
        let columns: Vec<((i32, i32), i32)> =
            (-12..12).flat_map(|by| (11..16).map(move |bx| ((bx, by), 40))).collect();
        let bare: Vec<((i32, i32), f32)> = columns.iter().map(|(k, _)| (*k, 0.0)).collect();
        let fine = FineSurface::from_columns(&columns).with_canopy(&bare);
        assert!(!placed(90, &FineSurface::default()).is_empty());
        assert!(placed(90, &fine).is_empty(), "trees grew beside a bare window");

        // The same window with a forest on it fills the ring again.
        let dense: Vec<((i32, i32), f32)> = columns.iter().map(|(k, _)| (*k, 0.9)).collect();
        let wooded = FineSurface::from_columns(&columns).with_canopy(&dense);
        assert!(!placed(90, &wooded).is_empty(), "a wooded window edge grew nothing");
    }

    /// Crowns give way to the fine map exactly where the block mask does.
    #[test]
    fn scatter_skips_the_block_mask() {
        let clear = placed(90, &FineSurface::default());
        // Region tile (5, -3) starts at tile (240, -144); mask the whole of it.
        let columns: Vec<((i32, i32), i32)> = (-9..15)
            .flat_map(|by| (15..18).map(move |bx| ((bx, by), 40)))
            .collect();
        let masked = placed(90, &FineSurface::from_columns(&columns));
        assert!(!clear.is_empty());
        assert!(masked.is_empty(), "{} crowns stood on fine ground", masked.len());
    }

    #[test]
    fn every_preset_and_stage_meshes() {
        for preset in [Preset::Oak, Preset::Birch, Preset::Pine, Preset::Willow, Preset::MushroomTree] {
            for stage in STAGES {
                let mesh = crown_mesh(preset, stage, 1);
                assert!(!mesh.indices.is_empty(), "{preset:?} {stage:?}");
                assert_eq!(mesh.uvs.len(), mesh.positions.len());
                assert!(mesh_height(&mesh) > 1.0, "{preset:?} {stage:?} is flat");
            }
        }
    }

    /// Every tree carries every stage, so Bevy can swap between them by the
    /// camera's own distance; the stages are alternatives, never a sum.
    #[test]
    fn every_tree_carries_every_stage() {
        let trees = placed(90, &FineSurface::default());
        assert!(!trees.is_empty());
        let batches = batch_crowns(trees.clone());
        for stage in STAGES {
            let held: usize =
                batches.iter().filter(|b| b.stage == stage).map(|b| b.instances.len()).sum();
            assert_eq!(held, trees.len(), "{stage:?} is missing trees");
        }
        // Only the grown stage splits by growth variant.
        for stage in [Stage::Crown, Stage::Box] {
            assert!(
                batches.iter().filter(|b| b.stage == stage).all(|b| b.variant == 0),
                "{stage:?} split by variant"
            );
        }
    }

    /// A stage's mesh has a height of its own and the instance scales to the
    /// one the scatter gave it, so a tree is the same size whichever stage
    /// draws it.
    #[test]
    fn a_tree_is_the_same_height_at_every_stage() {
        let trees = placed(90, &FineSurface::default());
        let batches = batch_crowns(trees);
        let mut seen: HashMap<([i32; 3], Stage), f32> = HashMap::new();
        for batch in &batches {
            for instance in &batch.instances {
                let key = std::array::from_fn(|i| (instance.pos[i] * 8.0) as i32);
                seen.insert((key, batch.stage), instance.height / batch.mesh_height * batch.mesh_height);
            }
        }
        for ((key, stage), height) in &seen {
            let grown = seen[&(*key, Stage::Grown)];
            assert!((grown - height).abs() < 1e-3, "{stage:?} stands {height} not {grown}");
        }
    }

    #[test]
    fn density_rule_is_the_documented_one() {
        assert_eq!(crown_count(100, 0.0), 20);
        assert_eq!(crown_count(15, 0.0), 0);
    }
}
