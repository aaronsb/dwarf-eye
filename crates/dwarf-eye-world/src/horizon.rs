//! Coarse terrain beyond the loaded map.
//!
//! Dwarf Fortress only loads a small window of the world at full detail: in
//! adventure mode, 144 tiles square. Past that, DFHack still serves two lower
//! resolutions: region maps, one sample per 48-tile region tile for the world
//! tiles around the player, and the world map, one sample per 768-tile world
//! tile. This stitches both into a heightfield with a hole where the detailed
//! map sits, so the land runs to the horizon.
//!
//! Elevations are in the same units as z-levels once the map's z origin is
//! subtracted: `z = elevation - block_pos_z`.

use crate::mesh::{MeshData, Z_SCALE};
use crate::palette::{Palette, Rgb};
use crate::world::{BLOCK, World};
use crate::Solid;
use dfhack_remote::rfr::{MapInfo, RegionMaps, RegionTile, RiverEdge, RiverTile, SiteRealizationBuilding, WorldMap};
use dwarf_eye_art::atlas::WHITE_UV;
use std::collections::HashMap;

/// Tiles per region tile, and region tiles per world tile.
pub const REGION_TILE: i32 = 48;
pub const REGIONS_PER_WORLD: i32 = 16;
/// Region maps carry a one-tile overlap on each side.
const REGION_MAP_SIDE: i32 = 17;
/// How many world tiles out the 48-tile grid runs before the world grid takes
/// over.
const REACH: i32 = 12;

const WATER: Rgb = [60, 110, 190];
/// Rivers read as the same water, a touch brighter so a thin strip still
/// reads against the darker ground shadow it sits on.
const RIVER: Rgb = [70, 122, 198];
const GRASS: Rgb = [96, 142, 62];
/// Rainfall drives the grass hue between these two, so the midpoint lands on
/// `GRASS` and the fine map's tone is matched on average.
const DRY_GRASS: Rgb = [150, 156, 90];
const WET_GRASS: Rgb = [58, 138, 52];
const CANOPY: Rgb = [66, 106, 52];
const SNOW: Rgb = [226, 232, 240];
const SOIL: Rgb = [134, 96, 67];
/// Bare rock, above the treeline.
const ROCK: Rgb = [142, 136, 126];
/// A site building's walls: grey-brown masonry.
const BUILDING: Rgb = [128, 112, 96];

/// Elevation above which the ground fades from grass to bare rock: DF's
/// world-tile scale runs 0-99 ocean, 100-149 normal biomes, 150+ mountains.
const ROCK_ELEVATION: f32 = 150.0;
const ROCK_ELEVATION_FULL: f32 = 200.0;

/// River edges with no data read back as this sentinel rather than absent.
const RIVER_SENTINEL: i32 = -30000;

/// One heightfield sample.
#[derive(Clone, Copy)]
struct Sample {
    /// Height in render units.
    y: f32,
    color: Rgb,
    /// Inside the detailed map, so quads entirely within are left out.
    interior: bool,
}

/// Where the detailed map sits, in region tiles, and where the render origin
/// is, in absolute tiles and z-level.
struct Window {
    x0: i32,
    y0: i32,
    origin: (i32, i32, i32),
}

impl Window {
    fn from_info(info: &MapInfo, origin: (i32, i32, i32)) -> Self {
        Self { x0: info.block_pos_x(), y0: info.block_pos_y(), origin }
    }

    /// Render height of an absolute elevation.
    fn height(&self, elevation: i32) -> f32 {
        (elevation - self.origin.2) as f32 * Z_SCALE
    }

    /// Render x/z of a region tile's centre.
    fn centre(&self, rx: i32, ry: i32) -> (f32, f32) {
        (
            (rx * REGION_TILE - self.origin.0 + REGION_TILE / 2) as f32,
            (ry * REGION_TILE - self.origin.1 + REGION_TILE / 2) as f32,
        )
    }
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}

/// A deterministic pseudo-random value in 0..1 from a pair of tile
/// coordinates, so vertex brightness jitter is stable across rebuilds.
fn hash01(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374_761_393).wrapping_add((y as u32).wrapping_mul(668_265_263));
    h ^= h >> 13;
    h = h.wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h as f32) / (u32::MAX as f32)
}

/// Brightens or dims a colour by up to `amount`, keyed on grid coordinates so
/// the same vertex always gets the same jitter.
fn jitter(rgb: Rgb, x: i32, y: i32, amount: f32) -> Rgb {
    let factor = 1.0 + (hash01(x, y) * 2.0 - 1.0) * amount;
    [
        (rgb[0] as f32 * factor).round().clamp(0.0, 255.0) as u8,
        (rgb[1] as f32 * factor).round().clamp(0.0, 255.0) as u8,
        (rgb[2] as f32 * factor).round().clamp(0.0, 255.0) as u8,
    ]
}

fn to_linear(rgb: Rgb) -> [f32; 4] {
    let f = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    [f(rgb[0]), f(rgb[1]), f(rgb[2]), 1.0]
}

/// Ground colour for a coarse sample: the surface material under grass,
/// canopy where the vegetation is dense and rainfall is high, bare rock
/// above the treeline, snow on top, water where the land sits below the
/// water table.
fn shade(
    palette: &Palette,
    surface: Option<&dfhack_remote::rfr::MatPair>,
    vegetation: i32,
    rainfall: i32,
    snow: i32,
    elevation: i32,
    underwater: bool,
) -> Rgb {
    if underwater {
        return WATER;
    }
    let ground = surface.and_then(|p| palette.material_color(p)).unwrap_or(SOIL);
    // Substance colours are strong; pull toward luminance as the mesher does.
    let luma = 0.2126 * ground[0] as f32 + 0.7152 * ground[1] as f32 + 0.0722 * ground[2] as f32;
    let ground = [
        (luma + (ground[0] as f32 - luma) * 0.35) as u8,
        (luma + (ground[1] as f32 - luma) * 0.35) as u8,
        (luma + (ground[2] as f32 - luma) * 0.35) as u8,
    ];
    // Rainfall drives grass hue from a dry pale yellow-green to a deeper wet
    // green; the midpoint (moderate rainfall) matches GRASS, the fine map's
    // average tone.
    let r = rainfall.clamp(0, 100) as f32 / 100.0;
    let grass = mix(DRY_GRASS, WET_GRASS, r);
    // The detailed map is grass at vegetation 30, so bare ground shows only
    // where vegetation is sparse; denser growth darkens toward canopy, most
    // strongly where it's also wet.
    let v = vegetation.clamp(0, 100) as f32 / 100.0;
    let mut color = mix(ground, grass, (v / 0.2).clamp(0.0, 1.0));
    color = mix(color, CANOPY, ((v - 0.3) / 0.7).clamp(0.0, 1.0) * (0.25 + 0.75 * r) * 0.6);
    // Above the treeline the ground reads as bare rock regardless of the
    // vegetation/rainfall gradient.
    let rock_t = ((elevation as f32 - ROCK_ELEVATION) / (ROCK_ELEVATION_FULL - ROCK_ELEVATION)).clamp(0.0, 1.0);
    color = mix(color, ROCK, rock_t);
    if snow > 0 {
        color = mix(color, SNOW, snow.clamp(0, 100) as f32 / 100.0);
    }
    color
}

fn region_sample(palette: &Palette, tile: &RegionTile, window: &Window) -> Sample {
    let elevation = tile.elevation();
    let water = tile.water_elevation();
    let underwater = tile.water_elevation.is_some() && elevation < water;
    let height = if underwater { water } else { elevation };
    Sample {
        // Floors sit a fraction above their z-level, so the surface does too.
        y: window.height(height) + 0.1,
        color: shade(
            palette,
            tile.surface_material.as_ref(),
            tile.vegetation(),
            tile.rainfall(),
            tile.snow(),
            elevation,
            underwater,
        ),
        interior: false,
    }
}

/// The 30th percentile of surface heights over a region tile of the loaded
/// map: low enough that tree canopy does not pull it up, high enough to sit
/// on the ground rather than in a pit.
fn measured_surface(world: &World, window: &Window, rx: i32, ry: i32) -> Option<f32> {
    let (tx0, ty0) = (rx * REGION_TILE - window.origin.0, ry * REGION_TILE - window.origin.1);
    let (mut z_lo, mut z_hi) = (i32::MAX, i32::MIN);
    for chunk in world.chunks() {
        z_lo = z_lo.min(chunk.z);
        z_hi = z_hi.max(chunk.z);
    }
    if z_lo > z_hi {
        return None;
    }
    let mut heights = Vec::with_capacity((REGION_TILE * REGION_TILE) as usize);
    for y in ty0..ty0 + REGION_TILE {
        for x in tx0..tx0 + REGION_TILE {
            for z in (z_lo..=z_hi).rev() {
                if let Some(v) = world.voxel(x, y, z)
                    && v.solid != Solid::Empty
                {
                    heights.push(z);
                    break;
                }
            }
        }
    }
    if heights.len() < 64 {
        return None;
    }
    heights.sort_unstable();
    Some(heights[heights.len() * 3 / 10] as f32 * Z_SCALE)
}

/// Builds the outer terrain. `world` supplies the detailed map's surface, so
/// the coarse mesh meets it at the right height.
pub fn build(
    palette: &Palette,
    info: &MapInfo,
    origin: (i32, i32, i32),
    regions: &RegionMaps,
    world_map: &WorldMap,
    world: &World,
    transpose: bool,
) -> MeshData {
    let window = Window::from_info(info, origin);
    let mut samples: HashMap<(i32, i32), Sample> = HashMap::new();

    // Region tiles: the fine outer layer.
    for map in &regions.region_maps {
        let (wx, wy) = (map.map_x(), map.map_y());
        for (i, tile) in map.tiles.iter().enumerate() {
            let i = i as i32;
            let (lx, ly) = if transpose {
                (i / REGION_MAP_SIDE, i % REGION_MAP_SIDE)
            } else {
                (i % REGION_MAP_SIDE, i / REGION_MAP_SIDE)
            };
            if lx >= REGIONS_PER_WORLD || ly >= REGIONS_PER_WORLD {
                continue;
            }
            let key = (wx * REGIONS_PER_WORLD + lx, wy * REGIONS_PER_WORLD + ly);
            samples.entry(key).or_insert_with(|| region_sample(palette, tile, &window));
        }
    }

    // Wherever fine chunks exist, in the live window or cached from a walk,
    // take the measured surface and sink it a little so the coarse mesh stays
    // under the real floors.
    let mut detailed: Vec<(i32, i32)> = world
        .chunks()
        .map(|c| {
            (
                (c.block_x * BLOCK + window.origin.0).div_euclid(REGION_TILE),
                (c.block_y * BLOCK + window.origin.1).div_euclid(REGION_TILE),
            )
        })
        .collect();
    detailed.sort_unstable();
    detailed.dedup();
    for (rx, ry) in detailed {
        {
            if let Some(surface) = measured_surface(world, &window, rx, ry) {
                let sample = samples.entry((rx, ry)).or_insert(Sample {
                    y: surface,
                    color: GRASS,
                    interior: true,
                });
                sample.y = surface - 0.6;
                sample.interior = true;
            } else if let Some(sample) = samples.get_mut(&(rx, ry)) {
                sample.interior = true;
            }
        }
    }

    // World tiles: past the region maps, one sample per 768 tiles. Those are
    // interpolated onto the same 48-tile grid, so the far land joins the near
    // without a seam, and shifted by the mean gap between the two surveys
    // where they overlap.
    let (width, height) = (world_map.world_width, world_map.world_height);
    let world_at = |wx: i32, wy: i32| -> Option<(i32, i32, i32, i32)> {
        if wx < 0 || wy < 0 || wx >= width || wy >= height {
            return None;
        }
        let i = (wy * width + wx) as usize;
        Some((
            *world_map.elevation.get(i)?,
            world_map.water_elevation.get(i).copied().unwrap_or(i32::MIN),
            world_map.vegetation.get(i).copied().unwrap_or(0),
            world_map.rainfall.get(i).copied().unwrap_or(0),
        ))
    };
    let covered: std::collections::HashSet<(i32, i32)> =
        regions.region_maps.iter().map(|m| (m.map_x(), m.map_y())).collect();
    let (mut gap, mut gap_n) = (0.0, 0);
    for &(wx, wy) in &covered {
        let Some((elevation, _, _, _)) = world_at(wx, wy) else { continue };
        let mut sum = 0.0;
        let mut n = 0;
        for ly in 0..REGIONS_PER_WORLD {
            for lx in 0..REGIONS_PER_WORLD {
                if let Some(s) = samples.get(&(wx * REGIONS_PER_WORLD + lx, wy * REGIONS_PER_WORLD + ly)) {
                    sum += s.y;
                    n += 1;
                }
            }
        }
        if n > 0 {
            gap += elevation as f32 - (sum / n as f32 + window.origin.2 as f32);
            gap_n += 1;
        }
    }
    let bias = if gap_n > 0 { gap / gap_n as f32 } else { 0.0 };

    let (pwx, pwy) = (window.x0.div_euclid(REGIONS_PER_WORLD), window.y0.div_euclid(REGIONS_PER_WORLD));
    for ry in (pwy - REACH) * REGIONS_PER_WORLD..(pwy + REACH + 1) * REGIONS_PER_WORLD {
        for rx in (pwx - REACH) * REGIONS_PER_WORLD..(pwx + REACH + 1) * REGIONS_PER_WORLD {
            if samples.contains_key(&(rx, ry)) {
                continue;
            }
            // Bilinear between the four nearest world tile centres.
            let fx = (rx as f32 + 0.5) / REGIONS_PER_WORLD as f32 - 0.5;
            let fy = (ry as f32 + 0.5) / REGIONS_PER_WORLD as f32 - 0.5;
            let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            let corners = [
                (world_at(x0, y0), (1.0 - tx) * (1.0 - ty)),
                (world_at(x0 + 1, y0), tx * (1.0 - ty)),
                (world_at(x0, y0 + 1), (1.0 - tx) * ty),
                (world_at(x0 + 1, y0 + 1), tx * ty),
            ];
            let (mut elevation, mut water, mut vegetation, mut rainfall, mut weight) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for (tile, w) in corners {
                let Some((e, wl, v, r)) = tile else { continue };
                elevation += e as f32 * w;
                water += (if wl == i32::MIN { e } else { wl }) as f32 * w;
                vegetation += v as f32 * w;
                rainfall += r as f32 * w;
                weight += w;
            }
            if weight <= 0.0 {
                continue;
            }
            let elevation_raw = elevation / weight;
            let elevation = elevation_raw - bias;
            let water = water / weight;
            let underwater = elevation < water - 0.5;
            let height = if underwater { water } else { elevation };
            samples.insert(
                (rx, ry),
                Sample {
                    y: window.height(0) + height * Z_SCALE + 0.1,
                    color: shade(
                        palette,
                        None,
                        (vegetation / weight) as i32,
                        (rainfall / weight) as i32,
                        0,
                        elevation_raw as i32,
                        underwater,
                    ),
                    interior: false,
                },
            );
        }
    }

    let mut mesh = MeshData::default();
    emit_region_grid(&mut mesh, &samples, &window);

    // Rivers and site buildings, read off the same region tiles. Rivers skip
    // wherever the fine map already stands in for the ground; buildings skip
    // only the live window itself, since a walked-and-cached tile still needs
    // its coarse building drawn.
    let window_bounds = live_window(info, &window);
    for map in &regions.region_maps {
        let (wx, wy) = (map.map_x(), map.map_y());
        for (i, tile) in map.tiles.iter().enumerate() {
            let i = i as i32;
            let (lx, ly) = if transpose {
                (i / REGION_MAP_SIDE, i % REGION_MAP_SIDE)
            } else {
                (i % REGION_MAP_SIDE, i / REGION_MAP_SIDE)
            };
            if lx >= REGIONS_PER_WORLD || ly >= REGIONS_PER_WORLD {
                continue;
            }
            let (rx, ry) = (wx * REGIONS_PER_WORLD + lx, wy * REGIONS_PER_WORLD + ly);
            let sample = samples.get(&(rx, ry));
            if !sample.map(|s| s.interior).unwrap_or(false)
                && let Some(river) = tile.river_tiles.as_ref()
            {
                emit_river(&mut mesh, &window, rx, ry, river);
            }
            if tile.buildings.is_empty() {
                continue;
            }
            let ground_y = sample.map(|s| s.y).unwrap_or_else(|| window.height(tile.elevation()) + 0.1);
            for building in &tile.buildings {
                if overlaps(building_bounds(&window, rx, ry, building), window_bounds) {
                    continue;
                }
                emit_building(&mut mesh, &window, ground_y, rx, ry, building);
            }
        }
    }

    // The rest of the world at one sample per world tile. It runs a world
    // tile under the fine ring and sits two levels lower there, so the join
    // is a step hidden beneath the finer surface rather than a crack.
    let mut far: HashMap<(i32, i32), Sample> = HashMap::new();
    for wy in 0..height {
        for wx in 0..width {
            let Some((elevation, water, vegetation, rainfall)) = world_at(wx, wy) else { continue };
            let inside = (wx - pwx).abs() < REACH && (wy - pwy).abs() < REACH;
            let deep = (wx - pwx).abs() < REACH - 1 && (wy - pwy).abs() < REACH - 1;
            let water = if water == i32::MIN { elevation } else { water };
            let underwater = elevation < water;
            let height = (if underwater { water } else { elevation }) as f32 - bias;
            far.insert(
                (wx, wy),
                Sample {
                    y: window.height(0) + height * Z_SCALE + 0.1 - if inside { 2.0 } else { 0.0 },
                    color: shade(palette, None, vegetation, rainfall, 0, elevation, underwater),
                    interior: deep,
                },
            );
        }
    }
    let spacing = REGION_TILE * REGIONS_PER_WORLD;
    emit(&mut mesh, &far, spacing as f32, |wx, wy, s| {
        [
            (wx * spacing - window.origin.0 + spacing / 2) as f32,
            s.y,
            (wy * spacing - window.origin.1 + spacing / 2) as f32,
        ]
    });
    mesh
}

/// Triangulates the region-tile samples. Vertices sit at region tile centres,
/// in tile coordinates relative to the loaded map's origin.
fn emit_region_grid(mesh: &mut MeshData, samples: &HashMap<(i32, i32), Sample>, window: &Window) {
    let position = |rx: i32, ry: i32, s: &Sample| {
        let (x, z) = window.centre(rx, ry);
        [x, s.y, z]
    };
    emit(mesh, samples, REGION_TILE as f32, position);
}

fn emit(
    mesh: &mut MeshData,
    samples: &HashMap<(i32, i32), Sample>,
    spacing: f32,
    position: impl Fn(i32, i32, &Sample) -> [f32; 3],
) {
    let mut index_of: HashMap<(i32, i32), u32> = HashMap::new();
    let mut keys: Vec<&(i32, i32)> = samples.keys().collect();
    keys.sort();

    for &key in &keys {
        let s = &samples[key];
        let (x, y) = *key;
        // Central differences for a smooth normal.
        let h = |dx: i32, dy: i32| samples.get(&(x + dx, y + dy)).map(|n| n.y).unwrap_or(s.y);
        let dx = (h(1, 0) - h(-1, 0)) / (2.0 * spacing);
        let dz = (h(0, 1) - h(0, -1)) / (2.0 * spacing);
        let normal = normalize([-dx, 1.0, -dz]);

        // Deterministic per-vertex brightness jitter, so flat grass doesn't
        // read as a painted colour next to the fine ground's texture noise.
        // Water stays flat.
        let color = if s.color == WATER { s.color } else { jitter(s.color, x, y, 0.06) };

        index_of.insert(*key, mesh.positions.len() as u32);
        mesh.positions.push(position(x, y, s));
        mesh.normals.push(normal);
        mesh.colors.push(to_linear(color));
        mesh.uvs.push(WHITE_UV);
    }

    for &key in &keys {
        let (x, y) = *key;
        let corners = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)];
        let Some(indices) = corners.iter().map(|c| index_of.get(c).copied()).collect::<Option<Vec<u32>>>()
        else {
            continue;
        };
        if corners.iter().all(|c| samples[c].interior) {
            continue;
        }
        let [p00, p10, p01, p11] = [indices[0], indices[1], indices[2], indices[3]];
        mesh.indices.extend_from_slice(&[p00, p01, p10, p10, p01, p11]);
    }
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-6);
    [v[0] / len, v[1] / len, v[2] / len]
}

/// The live (detailed) window's footprint, in render-local tile coordinates.
/// Buildings inside it are left for the fine map to draw.
fn live_window(info: &MapInfo, window: &Window) -> (i32, i32, i32, i32) {
    let x0 = info.block_pos_x() * REGION_TILE - window.origin.0;
    let y0 = info.block_pos_y() * REGION_TILE - window.origin.1;
    (x0, y0, x0 + info.block_size_x() * BLOCK, y0 + info.block_size_y() * BLOCK)
}

fn overlaps(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    a.0 < b.2 && a.2 > b.0 && a.1 < b.3 && a.3 > b.1
}

/// A building's footprint in render-local tile coordinates: the region
/// tile's origin plus its bounds, which are tiles relative to that tile.
fn building_bounds(window: &Window, rx: i32, ry: i32, building: &SiteRealizationBuilding) -> (i32, i32, i32, i32) {
    let ox = rx * REGION_TILE - window.origin.0;
    let oy = ry * REGION_TILE - window.origin.1;
    let (x0, x1) = (building.min_x().min(building.max_x()), building.min_x().max(building.max_x()));
    let (y0, y1) = (building.min_y().min(building.max_y()), building.min_y().max(building.max_y()));
    (ox + x0, oy + y0, ox + x1, oy + y1)
}

/// A ground-sitting box for a site building: walls at 3 tiles, towers at 8,
/// trenches as a shallow slab.
fn emit_building(mesh: &mut MeshData, window: &Window, ground_y: f32, rx: i32, ry: i32, building: &SiteRealizationBuilding) {
    let (x0, z0, x1, z1) = building_bounds(window, rx, ry, building);
    if x1 - x0 < 1 || z1 - z0 < 1 {
        return;
    }
    let height = if building.tower_info.is_some() {
        8.0
    } else if building.trench_info.is_some() {
        0.4
    } else {
        3.0
    } * Z_SCALE;
    push_box(mesh, [x0 as f32, x1 as f32], [ground_y, ground_y + height], [z0 as f32, z1 as f32], BUILDING);
}

/// A box open on the bottom, since it always sits on the ground: a top cap
/// and four walls, wound the same way as the tile mesher's caps so back-face
/// culling doesn't eat one.
fn push_box(mesh: &mut MeshData, [x0, x1]: [f32; 2], [y0, y1]: [f32; 2], [z0, z1]: [f32; 2], color: Rgb) {
    let c = to_linear(color);
    mesh.push_quad([[x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]], [0.0, 1.0, 0.0], c);
    mesh.push_quad([[x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0]], [0.0, 0.0, -1.0], c);
    mesh.push_quad([[x1, y0, z1], [x1, y1, z1], [x0, y1, z1], [x0, y0, z1]], [0.0, 0.0, 1.0], c);
    mesh.push_quad([[x0, y0, z1], [x0, y1, z1], [x0, y1, z0], [x0, y0, z0]], [-1.0, 0.0, 0.0], c);
    mesh.push_quad([[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]], [1.0, 0.0, 0.0], c);
}

/// River edges with no data read back as a `-30000` sentinel rather than
/// being absent, so a live edge is one that differs from it and has real
/// width.
fn river_edge_valid(edge: &RiverEdge) -> bool {
    edge.min_pos() != RIVER_SENTINEL && edge.max_pos() != RIVER_SENTINEL && edge.max_pos() > edge.min_pos()
}

enum Border {
    North,
    South,
    East,
    West,
}

/// The two endpoints of an edge's span, in local tile coordinates (0..48).
fn border_points(border: Border, lo: f32, hi: f32) -> ([f32; 2], [f32; 2]) {
    let side = REGION_TILE as f32;
    match border {
        Border::North => ([lo, 0.0], [hi, 0.0]),
        Border::South => ([lo, side], [hi, side]),
        Border::West => ([0.0, lo], [0.0, hi]),
        Border::East => ([side, lo], [side, hi]),
    }
}

/// Orders three points so they wind the way this file's ground quads do
/// (matching `emit`'s `p00, p01, p10` triangle), so a river strip isn't
/// culled from above.
fn ccw_order(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> ([f32; 2], [f32; 2], [f32; 2]) {
    let cross = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    if cross > 0.0 { (a, c, b) } else { (a, b, c) }
}

/// A flat strip for each river edge a region tile carries, converging on the
/// tile centre: this both connects two edges that pass through the same tile
/// and gives a one-edge tile (a spring or a river mouth) a sensible taper.
fn emit_river(mesh: &mut MeshData, window: &Window, rx: i32, ry: i32, river: &RiverTile) {
    let ox = (rx * REGION_TILE - window.origin.0) as f32;
    let oz = (ry * REGION_TILE - window.origin.1) as f32;
    let side = REGION_TILE as f32;
    let center = [ox + side * 0.5, oz + side * 0.5];
    let color = to_linear(RIVER);
    for (edge, border) in [
        (river.north.as_ref(), Border::North),
        (river.south.as_ref(), Border::South),
        (river.east.as_ref(), Border::East),
        (river.west.as_ref(), Border::West),
    ] {
        let Some(edge) = edge else { continue };
        if !river_edge_valid(edge) {
            continue;
        }
        let (lo, hi) = border_points(border, edge.min_pos() as f32, edge.max_pos() as f32);
        let a = [ox + lo[0], oz + lo[1]];
        let b = [ox + hi[0], oz + hi[1]];
        // Sits a hair above the ground sample so it doesn't z-fight.
        let y = window.height(edge.elevation()) + 0.15;
        let (p0, p1, p2) = ccw_order(a, b, center);
        mesh.push_quad(
            [[p0[0], y, p0[1]], [p1[0], y, p1[1]], [p2[0], y, p2[1]], [p2[0], y, p2[1]]],
            [0.0, 1.0, 0.0],
            color,
        );
    }
}
