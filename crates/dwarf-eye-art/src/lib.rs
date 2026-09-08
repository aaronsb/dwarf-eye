//! Reads Dwarf Fortress's own tile sprites off disk.
//!
//! DFHack's remote API never sends art — it sends tiletype ids. The sprites live
//! in the game's `data/vanilla/*/graphics/` trees, indexed by raws that this
//! crate parses.

pub mod raws;

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use raws::{GraphicsIndex, SpriteRef};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A single tile lifted off a sheet, row-major RGBA.
#[derive(Clone)]
pub struct Sprite {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[u8; 4]>,
}

impl Sprite {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        self.pixels[(y * self.width + x) as usize]
    }

    /// Fraction of pixels that are at least half opaque.
    pub fn coverage(&self) -> f32 {
        let solid = self.pixels.iter().filter(|p| p[3] >= 128).count();
        solid as f32 / self.pixels.len().max(1) as f32
    }

    /// Reduces the sprite to an `n` x `n` occupancy-and-colour grid.
    ///
    /// Each cell takes the alpha-weighted mean colour of the block it covers and
    /// counts as solid when most of that block is opaque.
    pub fn downsample(&self, n: u32) -> Grid {
        let mut cells = vec![Cell::default(); (n * n) as usize];
        for gy in 0..n {
            for gx in 0..n {
                let (x0, x1) = (gx * self.width / n, (gx + 1) * self.width / n);
                let (y0, y1) = (gy * self.height / n, (gy + 1) * self.height / n);
                let (mut sum, mut weight, mut opaque, mut total) = ([0u32; 3], 0u32, 0u32, 0u32);

                for y in y0..y1.max(y0 + 1) {
                    for x in x0..x1.max(x0 + 1) {
                        let p = self.pixel(x.min(self.width - 1), y.min(self.height - 1));
                        total += 1;
                        if p[3] >= 128 {
                            opaque += 1;
                            for c in 0..3 {
                                sum[c] += p[c] as u32 * p[3] as u32;
                            }
                            weight += p[3] as u32;
                        }
                    }
                }

                cells[(gy * n + gx) as usize] = Cell {
                    solid: total > 0 && opaque * 2 >= total,
                    color: if weight > 0 {
                        [
                            (sum[0] / weight) as u8,
                            (sum[1] / weight) as u8,
                            (sum[2] / weight) as u8,
                        ]
                    } else {
                        [0, 0, 0]
                    },
                };
            }
        }
        Grid { size: n, cells }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Cell {
    pub solid: bool,
    pub color: [u8; 3],
}

/// A square occupancy grid derived from a sprite's alpha channel.
pub struct Grid {
    pub size: u32,
    pub cells: Vec<Cell>,
}

impl Grid {
    pub fn get(&self, x: u32, y: u32) -> Cell {
        self.cells[(y * self.size + x) as usize]
    }

    pub fn solid(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= self.size as i32 || y >= self.size as i32 {
            return false;
        }
        self.get(x as u32, y as u32).solid
    }

    pub fn solid_count(&self) -> usize {
        self.cells.iter().filter(|c| c.solid).count()
    }
}

/// Sprite sheets and the index that addresses them.
pub struct Art {
    pub index: GraphicsIndex,
    sheets: HashMap<usize, RgbaImage>,
    cache: HashMap<(usize, u32, u32), Sprite>,
}

/// Finds the Dwarf Fortress install, honouring `DF_DIR`.
pub fn find_install() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("DF_DIR") {
        let path = PathBuf::from(dir);
        if path.join("data").is_dir() {
            return Ok(path);
        }
        bail!("DF_DIR is set to {}, which has no data/ directory", path.display());
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.local/share/Steam/steamapps/common/Dwarf Fortress"),
        format!("{home}/.steam/steam/steamapps/common/Dwarf Fortress"),
        format!("{home}/Games/Dwarf Fortress"),
    ];
    for c in candidates {
        let path = PathBuf::from(&c);
        if path.join("data").is_dir() {
            return Ok(path);
        }
    }
    bail!("could not find Dwarf Fortress; set DF_DIR to its directory")
}

impl Art {
    /// Loads every graphics index under the install's `data/` tree.
    pub fn load(install: &Path) -> Result<Self> {
        let mut index = GraphicsIndex::default();
        let vanilla = install.join("data/vanilla");
        let entries = std::fs::read_dir(&vanilla)
            .with_context(|| format!("reading {}", vanilla.display()))?;

        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path().join("graphics")))
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in &dirs {
            index.load_dir(dir)?;
        }

        Ok(Self { index, sheets: HashMap::new(), cache: HashMap::new() })
    }

    pub fn page_count(&self) -> usize {
        self.index.pages.len()
    }

    pub fn plant_count(&self) -> usize {
        self.index.plants.len()
    }

    /// Cuts one sprite out of its sheet, loading and caching as needed.
    pub fn sprite(&mut self, at: SpriteRef) -> Result<&Sprite> {
        let key = (at.page, at.col, at.row);
        if !self.cache.contains_key(&key) {
            let page = self.index.page(at.page).clone();
            if !self.sheets.contains_key(&at.page) {
                let image = image::open(&page.file)
                    .with_context(|| format!("loading {}", page.file.display()))?
                    .to_rgba8();
                self.sheets.insert(at.page, image);
            }
            let sheet = &self.sheets[&at.page];

            let (ox, oy) = (at.col * page.tile_w, at.row * page.tile_h);
            if ox + page.tile_w > sheet.width() || oy + page.tile_h > sheet.height() {
                bail!(
                    "sprite {}:{},{} falls outside {}",
                    page.name,
                    at.col,
                    at.row,
                    page.file.display()
                );
            }

            let mut pixels = Vec::with_capacity((page.tile_w * page.tile_h) as usize);
            for y in 0..page.tile_h {
                for x in 0..page.tile_w {
                    pixels.push(sheet.get_pixel(ox + x, oy + y).0);
                }
            }
            self.cache.insert(
                key,
                Sprite { width: page.tile_w, height: page.tile_h, pixels },
            );
        }
        Ok(&self.cache[&key])
    }

    /// Resolves a species and tile family straight to a sprite.
    pub fn tree_sprite(&mut self, plant_id: &str, family: &str, dirs: u8) -> Option<&Sprite> {
        let at = self.index.sprite(plant_id, family, dirs)?;
        self.sprite(at).ok()
    }
}
