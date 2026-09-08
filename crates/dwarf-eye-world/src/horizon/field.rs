//! The coarse survey as a continuous field, and the noise that roughens it.
//!
//! Region maps give one sample per 48 tiles and the world map one per 768.
//! Interpolating them straight to a finer mesh gives a quilt of soft mounds,
//! so a Catmull-Rom pass smooths between samples and a value-noise pass adds
//! relief whose amplitude follows the local slope: flats stay flat, a
//! mountainside gets spurs and gullies. Both are pure functions of position,
//! so the same tile has the same height however the window has moved.

use crate::palette::Rgb;
use std::collections::HashMap;

use super::REGION_TILE;

/// Coarse relief wavelengths in tiles, coarsest first, with their share of the
/// amplitude. The longest is several region tiles across, so a ridge spans
/// samples rather than sitting inside one.
const OCTAVES: [(f32, f32); 3] = [(192.0, 0.60), (64.0, 0.28), (24.0, 0.12)];
/// Relief on dead-flat ground, in z-levels: enough that a plain is not a
/// drawing-board.
const RELIEF_BASE: f32 = 1.6;
/// Extra relief per z-level of slope per tile, so steep ground breaks up most.
const RELIEF_SLOPE: f32 = 4.0;
/// Ceiling on relief, in z-levels.
const RELIEF_MAX: f32 = 6.0;

/// One region tile of the survey, with its colour inputs already resolved.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cell {
    /// DF elevation units, which are z-levels once the map origin is taken off.
    pub elevation: f32,
    pub water: f32,
    pub vegetation: f32,
    pub rainfall: f32,
    pub drainage: f32,
    pub snow: f32,
    /// The surface material's own colour, where DF named one.
    pub ground: Option<Rgb>,
    /// True where a region map supplied this, false where it came from the
    /// world map.
    pub detail: bool,
}

impl Cell {
    fn lerp(self, other: Cell, t: f32) -> Cell {
        let f = |a: f32, b: f32| a + (b - a) * t;
        Cell {
            elevation: f(self.elevation, other.elevation),
            water: f(self.water, other.water),
            vegetation: f(self.vegetation, other.vegetation),
            rainfall: f(self.rainfall, other.rainfall),
            drainage: f(self.drainage, other.drainage),
            snow: f(self.snow, other.snow),
            ground: if t < 0.5 { self.ground } else { other.ground },
            detail: if t < 0.5 { self.detail } else { other.detail },
        }
    }

    pub fn underwater(&self) -> bool {
        self.elevation < self.water - 0.5
    }

    /// Height of the surface in DF elevation units: the water table where the
    /// land is under it.
    pub fn surface(&self) -> f32 {
        if self.underwater() { self.water } else { self.elevation }
    }
}

/// The survey, keyed by absolute region tile.
#[derive(Default)]
pub struct Field {
    cells: HashMap<(i32, i32), Cell>,
}

impl Field {
    pub fn insert(&mut self, rx: i32, ry: i32, cell: Cell) {
        self.cells.entry((rx, ry)).or_insert(cell);
    }

    pub fn contains(&self, rx: i32, ry: i32) -> bool {
        self.cells.contains_key(&(rx, ry))
    }

    pub fn get(&self, rx: i32, ry: i32) -> Option<&Cell> {
        self.cells.get(&(rx, ry))
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The cell nearest `(rx, ry)`, so an interpolation stencil that hangs off
    /// the edge of the survey stops rather than falling to zero.
    fn cell(&self, rx: i32, ry: i32) -> Cell {
        if let Some(c) = self.cells.get(&(rx, ry)) {
            return *c;
        }
        // Walk inward until a sample turns up: the survey is a filled
        // rectangle, so at most a couple of steps.
        for step in 1..4 {
            for (dx, dy) in [(step, 0), (-step, 0), (0, step), (0, -step)] {
                if let Some(c) = self.cells.get(&(rx + dx, ry + dy)) {
                    return *c;
                }
            }
        }
        Cell::default()
    }

    /// The survey at fractional region-tile coordinates, where an integer is a
    /// sample. Elevation runs through Catmull-Rom, which passes exactly
    /// through every sample; the scalars that only drive colour are bilinear.
    pub fn at(&self, u: f32, v: f32) -> Cell {
        let (x0, y0) = (u.floor() as i32, v.floor() as i32);
        let (tx, ty) = (u - x0 as f32, v - y0 as f32);
        let mut rows = [0.0f32; 4];
        for (j, row) in rows.iter_mut().enumerate() {
            let dy = y0 + j as i32 - 1;
            *row = catmull(
                self.cell(x0 - 1, dy).elevation,
                self.cell(x0, dy).elevation,
                self.cell(x0 + 1, dy).elevation,
                self.cell(x0 + 2, dy).elevation,
                tx,
            );
        }
        let elevation = catmull(rows[0], rows[1], rows[2], rows[3], ty);
        // The rest is bilinear: colour has no business overshooting, and a
        // material index cannot be interpolated at all.
        let top = self.cell(x0, y0).lerp(self.cell(x0 + 1, y0), tx);
        let bottom = self.cell(x0, y0 + 1).lerp(self.cell(x0 + 1, y0 + 1), tx);
        let mut cell = top.lerp(bottom, ty);
        cell.elevation = elevation;
        cell
    }

    /// Slope at a region tile in z-levels per tile, from central differences.
    pub fn slope(&self, rx: i32, ry: i32) -> f32 {
        let dx = self.cell(rx + 1, ry).elevation - self.cell(rx - 1, ry).elevation;
        let dy = self.cell(rx, ry + 1).elevation - self.cell(rx, ry - 1).elevation;
        ((dx * dx + dy * dy).sqrt() / (2.0 * REGION_TILE as f32)).abs()
    }

    /// The survey plus relief, in DF elevation units, at an absolute tile.
    ///
    /// The relief amplitude follows the slope of the survey and the tile's
    /// drainage, so well-drained hillsides break up and a wet flat does not.
    pub fn relieved(&self, ax: i32, az: i32) -> Cell {
        let (u, v) = tile_to_region(ax, az);
        let mut cell = self.at(u, v);
        let slope = self.slope(u.round() as i32, v.round() as i32);
        let drainage = 0.7 + 0.6 * (cell.drainage / 100.0).clamp(0.0, 1.0);
        let amplitude = ((RELIEF_BASE + RELIEF_SLOPE * slope) * drainage).min(RELIEF_MAX);
        // Water finds its own level: relief would only ripple a lake.
        if !cell.underwater() {
            cell.elevation += relief(ax, az) * amplitude;
        }
        cell
    }
}

/// Fractional region-tile coordinates of an absolute tile, where an integer
/// lands on a region tile's centre.
pub fn tile_to_region(ax: i32, az: i32) -> (f32, f32) {
    let side = REGION_TILE as f32;
    ((ax as f32 + 0.5) / side - 0.5, (az as f32 + 0.5) / side - 0.5)
}

/// The uniform Catmull-Rom spline through four samples: at `t` 0 it is exactly
/// `p1`, at 1 exactly `p2`.
pub fn catmull(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

/// Multi-octave value noise in -1..1, seeded from absolute tile coordinates so
/// a hillside keeps its shape as the window moves under it.
pub fn relief(ax: i32, az: i32) -> f32 {
    let mut total = 0.0;
    for (i, (wavelength, weight)) in OCTAVES.iter().enumerate() {
        total += weight * value_noise(ax as f32 / wavelength, az as f32 / wavelength, i as u32);
    }
    total.clamp(-1.0, 1.0)
}

/// One octave: hashed lattice values, smoothstepped between.
fn value_noise(x: f32, y: f32, octave: u32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (sx, sy) = (smoothstep(fx), smoothstep(fy));
    let (ix, iy) = (x0 as i32, y0 as i32);
    let at = |dx: i32, dy: i32| hash01(ix + dx, iy + dy, octave) * 2.0 - 1.0;
    let top = at(0, 0) + (at(1, 0) - at(0, 0)) * sx;
    let bottom = at(0, 1) + (at(1, 1) - at(0, 1)) * sx;
    top + (bottom - top) * sy
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// A stable pseudo-random value in 0..1 from three integers.
pub fn hash01(x: i32, y: i32, z: u32) -> f32 {
    (hash(x, y, z) as f32) / (u32::MAX as f32)
}

pub fn hash(x: i32, y: i32, z: u32) -> u32 {
    let mut h = (x as u32)
        .wrapping_mul(374_761_393)
        .wrapping_add((y as u32).wrapping_mul(668_265_263))
        .wrapping_add(z.wrapping_mul(2_246_822_519));
    h ^= h >> 13;
    h = h.wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_field() -> Field {
        let mut field = Field::default();
        for ry in 0..8 {
            for rx in 0..8 {
                field.insert(
                    rx,
                    ry,
                    Cell {
                        elevation: 100.0 + (rx * 7 + ry * 3) as f32,
                        water: 0.0,
                        vegetation: 40.0,
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

    /// The whole point of Catmull-Rom here: refining must not move the samples
    /// DFHack actually gave us.
    #[test]
    fn interpolation_passes_through_the_samples() {
        let field = sample_field();
        for ry in 1..7 {
            for rx in 1..7 {
                let got = field.at(rx as f32, ry as f32).elevation;
                let want = field.get(rx, ry).unwrap().elevation;
                assert!((got - want).abs() < 1e-3, "at ({rx}, {ry}): {got} vs {want}");
            }
        }
    }

    /// The middle of a region tile maps back to that region tile's own index,
    /// so a sample is where the survey put it.
    #[test]
    fn region_centres_land_on_samples() {
        for rx in -3..4 {
            let (u, _) = tile_to_region(rx * REGION_TILE + REGION_TILE / 2, 0);
            // The centre falls between two tiles, so a tile index lands within
            // half a tile of it.
            assert!((u - rx as f32).abs() <= 0.5 / REGION_TILE as f32 + 1e-4, "region {rx} centre gave {u}");
        }
        // The boundary between two region tiles is exactly halfway between
        // their samples.
        let (u, _) = tile_to_region(REGION_TILE, 0);
        assert!((u - 0.5).abs() <= 0.5 / REGION_TILE as f32 + 1e-4, "boundary gave {u}");
    }

    /// Between samples it stays between the neighbours' values on a monotone
    /// field, so refinement adds no lumps of its own.
    #[test]
    fn interpolation_stays_in_range() {
        let field = sample_field();
        for step in 1..8 {
            let t = step as f32 / 8.0;
            let got = field.at(3.0 + t, 3.0).elevation;
            let (lo, hi) = (field.at(3.0, 3.0).elevation, field.at(4.0, 3.0).elevation);
            assert!(got >= lo - 0.01 && got <= hi + 0.01, "t {t}: {got} outside {lo}..{hi}");
        }
    }

    #[test]
    fn relief_is_seeded_by_absolute_tile() {
        assert_eq!(relief(1234, -567), relief(1234, -567));
        assert_ne!(relief(1234, -567), relief(1235, -567));
        for ax in -200..200 {
            let n = relief(ax, 91);
            assert!((-1.0..=1.0).contains(&n), "relief {n} out of range");
        }
    }
}
