//! Where the time goes while crowns are built.
//!
//! Meshing the restored cache is one long serial pass over thousands of
//! chunks, so a regression in it shows up as a number nobody can attribute.
//! These counters split that pass into its phases — reading a tree's tiles,
//! growing it, rasterising it, slicing it into a chunk, meshing the slice — and
//! the worker prints them when the pass ends.
//!
//! They are process-wide atomics rather than a threaded-through struct: growth
//! runs on a pool of threads and the phases are entered from three modules, and
//! a counter that costs one relaxed add is not worth a plumbing exercise.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// One phase: total time inside it and how often it ran.
pub struct Phase {
    nanos: AtomicU64,
    count: AtomicU64,
}

impl Phase {
    const fn new() -> Self {
        Self { nanos: AtomicU64::new(0), count: AtomicU64::new(0) }
    }

    /// Records one run, given the instant it started.
    pub fn since(&self, started: Instant) {
        self.nanos.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts a run whose time is already inside another phase.
    pub fn tick(&self) {
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn seconds(&self) -> f32 {
        self.nanos.load(Ordering::Relaxed) as f32 / 1e9
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    fn clear(&self) {
        self.nanos.store(0, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
    }
}

/// Every phase a crown pass goes through.
pub struct Phases {
    /// Finding which trees reach into a chunk.
    pub nearby: Phase,
    /// Reading one tree's tiles out of the world.
    pub envelope: Phase,
    /// Growing one tree's skeleton.
    pub skeleton: Phase,
    /// Turning a skeleton into voxels.
    pub rasterise: Phase,
    /// Laying those voxels out in render space with an interned palette.
    pub voxelise: Phase,
    /// Copying one tree's voxels into one chunk.
    pub absorb: Phase,
    /// Meshing a chunk's slice.
    pub emit: Phase,
    /// Growing and meshing one standing plant.
    pub plants: Phase,
    /// Chunks built.
    pub chunks: Phase,
}

pub static PHASES: Phases = Phases {
    nearby: Phase::new(),
    envelope: Phase::new(),
    skeleton: Phase::new(),
    rasterise: Phase::new(),
    voxelise: Phase::new(),
    absorb: Phase::new(),
    emit: Phase::new(),
    plants: Phase::new(),
    chunks: Phase::new(),
};

/// A one-line summary of the pass, which also resets the counters so the next
/// pass measures only itself.
///
/// Growth phases are wall time summed across threads, so they can add up to
/// more than the pass took.
pub fn report() -> String {
    let p = &PHASES;
    let line = format!(
        "canopy: {} chunks, {} trees grown | nearby {:.1}s envelope {:.1}s skeleton {:.1}s \
         rasterise {:.1}s voxelise {:.1}s absorb {:.1}s ({} slices) emit {:.1}s | \
         {} plants grown in {:.1}s",
        p.chunks.count(),
        p.skeleton.count(),
        p.nearby.seconds(),
        p.envelope.seconds(),
        p.skeleton.seconds(),
        p.rasterise.seconds(),
        p.voxelise.seconds(),
        p.absorb.seconds(),
        p.absorb.count(),
        p.emit.seconds(),
        p.plants.count(),
        p.plants.seconds(),
    );
    for phase in [
        &p.nearby, &p.envelope, &p.skeleton, &p.rasterise, &p.voxelise, &p.absorb, &p.emit,
        &p.plants, &p.chunks,
    ] {
        phase.clear();
    }
    line
}
