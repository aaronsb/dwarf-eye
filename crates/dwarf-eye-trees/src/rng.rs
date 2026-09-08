//! A hash-based PRNG, so growth is reproducible without a rand dependency.

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// SplitMix64. Cheap, well-distributed, and identical on every platform.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed ^ GOLDEN }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GOLDEN);
        mix(self.state)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }

    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    /// Uniform in `[-1, 1)`.
    pub fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }

    pub fn range_u8(&mut self, lo: u8, hi: u8) -> u8 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next_u64() % (hi - lo + 1) as u64) as u8
    }

    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }

    /// A child generator, so a branch's stream does not depend on how much its
    /// siblings drew.
    pub fn fork(&mut self) -> Rng {
        Rng::new(self.next_u64())
    }
}

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A stable hash of a voxel coordinate, for per-voxel jitter that does not
/// depend on the order voxels were visited in.
pub fn hash3(x: i32, y: i32, z: i32, salt: u64) -> u64 {
    let mut h = salt ^ GOLDEN;
    h = mix(h ^ (x as i64 as u64).wrapping_mul(0x27D4_EB2F_1652_1BD5));
    h = mix(h ^ (y as i64 as u64).wrapping_mul(0x1656_67B1_9E37_79F9));
    mix(h ^ (z as i64 as u64).wrapping_mul(0x9E37_79B1_85EB_CA87))
}

/// `hash3` mapped to `[0, 1)`.
pub fn hash_unit(x: i32, y: i32, z: i32, salt: u64) -> f32 {
    (hash3(x, y, z, salt) >> 40) as f32 / (1u32 << 24) as f32
}
