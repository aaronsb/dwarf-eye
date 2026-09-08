//! Packs sprites into one texture, so a floor costs a quad instead of a mesh.
//!
//! A tile sprite is a picture rather than a cross-section, and downsampling it
//! into geometry throws away the detail that made it worth using. Everything
//! that is not sprite-shaped points at a white cell and keeps its vertex colour.

use crate::Sprite;
use std::collections::HashMap;

/// Side length in cells. 32 cells of 32px each fits 1024 sprites in 1024x1024.
pub const GRID: u32 = 32;
/// Sprites are 32x32. The padding is edge-bled from the sprite and keeps
/// neighbours out of its mip levels: at level four a cell is 4 px with 1 px
/// of its own bleed each side, so four levels are safe.
pub const CELL: u32 = 32;
pub const PAD: u32 = 16;
pub const STRIDE: u32 = CELL + PAD * 2;
/// Atlas side length in pixels.
pub const SIDE: u32 = GRID * STRIDE;

/// UV of the white cell's centre. Slot zero is always white, so this is fixed
/// and geometry that carries its own colour can default to it.
pub const WHITE_UV: [f32; 2] = [
    (PAD + CELL / 2) as f32 / SIDE as f32,
    (PAD + CELL / 2) as f32 / SIDE as f32,
];

/// UV rectangle of one packed sprite.
#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
}

impl Rect {
    /// The UV of a single point, for geometry that carries its own colour.
    pub fn point(self) -> [f32; 2] {
        [(self.u0 + self.u1) * 0.5, (self.v0 + self.v1) * 0.5]
    }
}

/// A growing texture atlas of tile sprites.
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    /// RGBA, row-major.
    pub pixels: Vec<u8>,
    slots: HashMap<u64, Rect>,
    next: u32,
    white: Rect,
}

impl Atlas {
    pub fn new() -> Self {
        let side = GRID * STRIDE;
        let mut atlas = Self {
            width: side,
            height: side,
            pixels: vec![0; (side * side * 4) as usize],
            slots: HashMap::new(),
            next: 0,
            white: Rect { u0: 0.0, v0: 0.0, u1: 0.0, v1: 0.0 },
        };
        // Slot zero is opaque white, so untextured geometry can share the
        // material without being tinted by it.
        let slot = atlas.claim();
        atlas.fill(slot, [255, 255, 255, 255]);
        atlas.white = atlas.rect(slot);
        atlas
    }

    /// UV of the white cell, for vertex-coloured geometry.
    pub fn white(&self) -> Rect {
        self.white
    }

    pub fn capacity_used(&self) -> u32 {
        self.next
    }

    fn claim(&mut self) -> u32 {
        let slot = self.next;
        self.next += 1;
        slot
    }

    fn origin(slot: u32) -> (u32, u32) {
        ((slot % GRID) * STRIDE + PAD, (slot / GRID) * STRIDE + PAD)
    }

    fn rect(&self, slot: u32) -> Rect {
        let (x, y) = Self::origin(slot);
        // Half-texel inset keeps sampling off the padding.
        let half = 0.5;
        Rect {
            u0: (x as f32 + half) / self.width as f32,
            v0: (y as f32 + half) / self.height as f32,
            u1: (x as f32 + CELL as f32 - half) / self.width as f32,
            v1: (y as f32 + CELL as f32 - half) / self.height as f32,
        }
    }

    fn put(&mut self, x: u32, y: u32, rgba: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = ((y * self.width + x) * 4) as usize;
        self.pixels[i..i + 4].copy_from_slice(&rgba);
    }

    fn fill(&mut self, slot: u32, rgba: [u8; 4]) {
        let (ox, oy) = Self::origin(slot);
        for y in 0..CELL {
            for x in 0..CELL {
                self.put(ox + x, oy + y, rgba);
            }
        }
    }

    /// Adds a sprite, returning its UV rect. Repeat calls with the same `key`
    /// return the cached rect rather than packing it twice.
    ///
    /// `flatten` replaces transparent pixels with `backdrop`, for ground that
    /// must be solid where the sprite is a sparse scatter.
    pub fn insert(
        &mut self,
        key: u64,
        sprite: &Sprite,
        flatten: Option<[u8; 3]>,
    ) -> Option<Rect> {
        if let Some(found) = self.slots.get(&key) {
            return Some(*found);
        }
        if self.next >= GRID * GRID {
            return None;
        }

        let slot = self.claim();
        let (ox, oy) = Self::origin(slot);
        for y in 0..CELL {
            for x in 0..CELL {
                let px = sprite.pixel(x.min(sprite.width - 1), y.min(sprite.height - 1));
                let rgba = match (flatten, px[3] >= 128) {
                    (Some(back), false) => [back[0], back[1], back[2], 255],
                    _ => px,
                };
                self.put(ox + x, oy + y, rgba);
            }
        }

        // Bleed the edges outward so bilinear sampling never reaches a gap.
        for p in 1..=PAD {
            for i in 0..CELL {
                let top = self.sample(ox + i, oy);
                let bottom = self.sample(ox + i, oy + CELL - 1);
                let left = self.sample(ox, oy + i);
                let right = self.sample(ox + CELL - 1, oy + i);
                self.put(ox + i, oy.wrapping_sub(p), top);
                self.put(ox + i, oy + CELL - 1 + p, bottom);
                self.put(ox.wrapping_sub(p), oy + i, left);
                self.put(ox + CELL - 1 + p, oy + i, right);
            }
        }

        let rect = self.rect(slot);
        self.slots.insert(key, rect);
        Some(rect)
    }

    fn sample(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let i = ((y * self.width + x) * 4) as usize;
        [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2], self.pixels[i + 3]]
    }
}

impl Default for Atlas {
    fn default() -> Self {
        Self::new()
    }
}
