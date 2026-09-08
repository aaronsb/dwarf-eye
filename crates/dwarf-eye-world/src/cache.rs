//! A disk cache of decoded chunks, keyed by absolute position.
//!
//! Dwarf Fortress only ever holds a 144-tile window, so everything the
//! character has walked past is gone from the game. The cache keeps it: each
//! chunk is written as it arrives and read back on the next connect, so the
//! land accumulates across sessions. Positions are absolute (world tiles and
//! elevation), so a later session with a different render origin places them
//! where they belong.
//!
//! Layout: one file per chunk, `<bx>_<by>_<z>.chunk`, 256 voxels of 19 bytes
//! after a 4-byte magic. The format is private to this crate, and the magic
//! carries its version: a file written by an older layout fails to read and is
//! deleted.
//!
//! Alongside them sits `floors`, one line per block column: the level at which
//! that column turned to unrevealed rock, so a later session knows how deep to
//! ask rather than rediscovering it. It carries its own version line and is
//! ignored outright if that does not match, and a build that knows nothing of
//! it simply leaves it alone.

use crate::palette::Solid;
use crate::world::{Chunk, TILES_PER_BLOCK, Voxel};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"DEC2";
const VOXEL_BYTES: usize = 19;

/// Name and version of the column-floor sidecar.
const FLOORS: &str = "floors";
const FLOORS_VERSION: &str = "def1";

fn solid_to_u8(s: Solid) -> u8 {
    match s {
        Solid::Empty => 0,
        Solid::Cube => 1,
        Solid::Floor => 2,
        Solid::Ramp => 3,
        Solid::Stair => 4,
        Solid::Fortification => 5,
        Solid::Foliage => 6,
    }
}

fn solid_from_u8(b: u8) -> Solid {
    match b {
        1 => Solid::Cube,
        2 => Solid::Floor,
        3 => Solid::Ramp,
        4 => Solid::Stair,
        5 => Solid::Fortification,
        6 => Solid::Foliage,
        _ => Solid::Empty,
    }
}

/// Where one world's chunks live.
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// Opens (creating) the cache for a world, under the user's cache
    /// directory. `DWARF_EYE_CACHE` overrides the root.
    pub fn open(world_name: &str, save_name: &str) -> Result<Self> {
        let root = std::env::var_os("DWARF_EYE_CACHE")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(|p| PathBuf::from(p).join("dwarf-eye")))
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache/dwarf-eye")))
            .context("no cache directory")?;
        let clean = |s: &str| -> String {
            s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
        };
        let dir = root.join(format!("{}-{}", clean(world_name), clean(save_name)));
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, key: (i32, i32, i32)) -> PathBuf {
        self.dir.join(format!("{}_{}_{}.chunk", key.0, key.1, key.2))
    }

    /// Writes a chunk under an absolute key.
    pub fn store(&self, key: (i32, i32, i32), chunk: &Chunk) -> Result<()> {
        let mut bytes = Vec::with_capacity(4 + TILES_PER_BLOCK * VOXEL_BYTES);
        bytes.extend_from_slice(MAGIC);
        for v in &chunk.voxels {
            bytes.push(solid_to_u8(v.solid));
            bytes.push(v.hidden as u8);
            bytes.push(v.outside as u8);
            bytes.push(v.water);
            bytes.push(v.magma);
            bytes.extend_from_slice(&v.color);
            bytes.extend_from_slice(&v.tile_id.to_le_bytes());
            bytes.extend_from_slice(&v.mat_index.to_le_bytes());
            bytes.push(v.tree_dx as u8);
            bytes.push(v.tree_dy as u8);
            bytes.push(v.tree_dz as u8);
        }
        let path = self.path(key);
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Deletes one chunk's file, if present.
    pub fn remove(&self, key: (i32, i32, i32)) {
        let _ = fs::remove_file(self.path(key));
    }

    /// How many chunk files the cache holds, and what they weigh.
    pub fn size(&self) -> (usize, u64) {
        let Ok(entries) = fs::read_dir(&self.dir) else { return (0, 0) };
        entries
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("chunk"))
            .filter_map(|e| e.metadata().ok())
            .fold((0, 0), |(n, bytes), m| (n + 1, bytes + m.len()))
    }

    /// Where each block column stopped being worth asking about, as an
    /// absolute block column and level.
    ///
    /// A file from another version is not an error, just nothing known: the
    /// floors are rediscovered by the next few passes.
    pub fn load_floors(&self) -> HashMap<(i32, i32), i32> {
        let mut floors = HashMap::new();
        let Ok(text) = fs::read_to_string(self.dir.join(FLOORS)) else { return floors };
        let mut lines = text.lines();
        if lines.next() != Some(FLOORS_VERSION) {
            return floors;
        }
        for line in lines {
            let n: Vec<i32> = line.split_whitespace().filter_map(|p| p.parse().ok()).collect();
            if n.len() == 3 {
                floors.insert((n[0], n[1]), n[2]);
            }
        }
        floors
    }

    pub fn store_floors(&self, floors: &HashMap<(i32, i32), i32>) {
        let mut text = String::from(FLOORS_VERSION);
        // Sorted, so the file is the same file when nothing has changed.
        let mut rows: Vec<_> = floors.iter().collect();
        rows.sort();
        for ((bx, by), z) in rows {
            text.push_str(&format!("\n{bx} {by} {z}"));
        }
        let path = self.dir.join(FLOORS);
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }

    /// Reads every chunk in the cache, with its absolute key.
    pub fn load_all(&self) -> Result<Vec<((i32, i32, i32), Vec<Voxel>)>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("chunk") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let parts: Vec<i32> = stem.split('_').filter_map(|p| p.parse().ok()).collect();
            if parts.len() != 3 {
                continue;
            }
            match read_chunk(&path) {
                Ok(voxels) => out.push(((parts[0], parts[1], parts[2]), voxels)),
                Err(_) => {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> Cache {
        let dir = std::env::temp_dir().join(format!("dwarf-eye-cache-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Cache { dir }
    }

    #[test]
    fn floors_survive_a_round_trip() {
        let cache = scratch("floors");
        assert!(cache.load_floors().is_empty(), "nothing written yet");
        let floors = HashMap::from([((3, -4), 118), ((0, 0), -7)]);
        cache.store_floors(&floors);
        assert_eq!(cache.load_floors(), floors);
    }

    #[test]
    fn floors_from_another_version_are_not_read() {
        let cache = scratch("floors-version");
        fs::write(cache.dir().join(FLOORS), "def0\n1 1 5").unwrap();
        assert!(cache.load_floors().is_empty(), "a version we do not know is nothing known");
    }
}

fn read_chunk(path: &Path) -> Result<Vec<Voxel>> {
    let bytes = fs::read(path)?;
    if bytes.len() != 4 + TILES_PER_BLOCK * VOXEL_BYTES || &bytes[..4] != MAGIC {
        bail!("bad chunk file");
    }
    let mut voxels = Vec::with_capacity(TILES_PER_BLOCK);
    for v in bytes[4..].chunks_exact(VOXEL_BYTES) {
        voxels.push(Voxel {
            solid: solid_from_u8(v[0]),
            hidden: v[1] != 0,
            outside: v[2] != 0,
            water: v[3],
            magma: v[4],
            color: [v[5], v[6], v[7]],
            tile_id: i32::from_le_bytes([v[8], v[9], v[10], v[11]]),
            mat_index: i32::from_le_bytes([v[12], v[13], v[14], v[15]]),
            tree_dx: v[16] as i8,
            tree_dy: v[17] as i8,
            tree_dz: v[18] as i8,
        });
    }
    Ok(voxels)
}
