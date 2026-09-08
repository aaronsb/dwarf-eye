//! Recursive branching: a trunk, limbs that fork with seeded jitter, whorls for
//! conifers, and leaf clusters hung on the outer growth.

use crate::math::{Vec3, vec3};
use crate::params::{Envelope, Habit, TreeParams};
use crate::rng::Rng;

/// One straight length of wood. Limbs are chains of these.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub a: Vec3,
    pub b: Vec3,
    pub radius_a: f32,
    pub radius_b: f32,
    /// 0 is the trunk; each fork adds one.
    pub depth: u8,
}

/// A blob of foliage. `shell` is how far out of the crown it sits, 0 at the
/// core and 1 at the surface, and drives the lighter tip colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeafCluster {
    pub center: Vec3,
    pub radius: f32,
    pub shell: f32,
    pub seed: u64,
}

#[derive(Clone, Debug)]
pub struct Skeleton {
    pub segments: Vec<Segment>,
    pub leaves: Vec<LeafCluster>,
    pub params: TreeParams,
    /// Centre of the foliage, for the shell and underside bias.
    pub crown_center: Vec3,
    pub crown_radius: f32,
    /// Height actually reached, in tiles.
    pub height: f32,
}

/// Distance grown between skeleton segments, in tiles.
const STEP: f32 = 0.5;

struct Grower<'a> {
    params: &'a TreeParams,
    envelope: Option<&'a Envelope>,
    segments: Vec<Segment>,
    leaves: Vec<LeafCluster>,
}

/// Grow a skeleton. Same params and seed always give the same tree.
///
/// With an envelope, a limb bends toward the level centroid when it would leave
/// the allowed cells and stops if it cannot get back in; the result is then left
/// at the size the envelope implies rather than rescaled to `params.height`.
pub fn grow(params: &TreeParams, seed: u64, envelope: Option<&Envelope>) -> Skeleton {
    let mut rng = Rng::new(seed);
    let mut g = Grower { params, envelope, segments: Vec::new(), leaves: Vec::new() };

    let base_radius = (params.trunk_width * 0.5).max(0.12);
    g.roots(base_radius, &mut rng);

    match params.habit {
        Habit::Deciduous => g.deciduous(base_radius, &mut rng),
        Habit::Conifer => g.conifer(base_radius, &mut rng),
        Habit::Broad => g.broad(base_radius, &mut rng),
    }

    let mut skeleton = Skeleton {
        segments: g.segments,
        leaves: g.leaves,
        params: params.clone(),
        crown_center: Vec3::ZERO,
        crown_radius: 1.0,
        height: 0.0,
    };
    if envelope.is_none() {
        normalise_height(&mut skeleton, params.height);
    }
    measure(&mut skeleton);
    skeleton
}

impl Grower<'_> {
    /// Buttresses at the base, so the trunk meets the ground with a flare.
    fn roots(&mut self, base_radius: f32, rng: &mut Rng) {
        let count = self.params.roots;
        if count == 0 {
            return;
        }
        let spin = rng.range(0.0, std::f32::consts::TAU);
        for i in 0..count {
            let angle = spin + std::f32::consts::TAU * i as f32 / count as f32 + rng.signed() * 0.2;
            let out = vec3(angle.cos(), 0.0, angle.sin());
            let reach = base_radius * rng.range(1.3, 2.1);
            let top = out * (base_radius * 0.5) + Vec3::Y * rng.range(0.6, 1.2) * base_radius;
            let toe = out * reach;
            self.segments.push(Segment {
                a: top,
                b: toe,
                radius_a: base_radius * 0.5,
                radius_b: base_radius * 0.22,
                depth: 0,
            });
        }
    }

    fn deciduous(&mut self, base_radius: f32, rng: &mut Rng) {
        let p = self.params;
        let trunk_len = p.height * p.clear_frac;
        let dir = jitter_up(rng, 0.06);
        let top = self.limb(Vec3::ZERO, dir, trunk_len, base_radius, 0, rng, false);
        let limb_len = p.height * p.limb_frac;
        self.fork(top.pos, top.dir, limb_len, top.radius, 1, rng);
    }

    fn broad(&mut self, base_radius: f32, rng: &mut Rng) {
        let p = self.params;
        // Several stems leaning out of a common base.
        let stems = rng.range_u8(2, 4);
        let spin = rng.range(0.0, std::f32::consts::TAU);
        for i in 0..stems {
            let angle = spin + std::f32::consts::TAU * i as f32 / stems as f32;
            let lean = rng.range(0.12, 0.3);
            let dir = vec3(angle.cos() * lean, 1.0, angle.sin() * lean).normalize();
            let len = p.height * p.clear_frac * rng.range(0.8, 1.3);
            let radius = base_radius * rng.range(0.6, 0.85);
            let top = self.limb(Vec3::ZERO, dir, len, radius, 0, rng, false);
            let mut sub = rng.fork();
            self.fork(top.pos, top.dir, p.height * p.limb_frac, top.radius, 1, &mut sub);
        }
    }

    fn conifer(&mut self, base_radius: f32, rng: &mut Rng) {
        let p = self.params;
        // A single leader carries the whole height.
        let leader = self.limb(Vec3::ZERO, jitter_up(rng, 0.02), p.height, base_radius, 0, rng, true);
        // The leader's own tip tuft.
        self.leaves.push(LeafCluster {
            center: leader.pos - Vec3::Y * 0.2,
            radius: p.leaf_radius * 0.7,
            shell: 1.0,
            seed: rng.next_u64(),
        });

        let bottom = p.height * p.clear_frac;
        let top = p.height * 0.96;
        let step = p.whorl_step.max(0.5);
        let max_len = p.height * 0.30;
        let mut y = bottom;
        let mut spin = rng.range(0.0, std::f32::consts::TAU);
        while y < top {
            // Branches shorten toward the tip, which is what makes the cone.
            let t = ((y - bottom) / (top - bottom)).clamp(0.0, 1.0);
            let len = max_len * (1.0 - t).powf(0.85) * rng.range(0.82, 1.15) + 0.4;
            let count = (p.whorl_count as f32 * (1.0 - t * 0.4)).round().max(3.0) as u8;
            let trunk_r = base_radius * (1.0 - t * 0.8).max(0.12);
            for i in 0..count {
                let angle = spin + std::f32::consts::TAU * i as f32 / count as f32
                    + rng.signed() * 0.25;
                let tilt = p.spread_deg.0.to_radians()
                    + rng.unit() * (p.spread_deg.1 - p.spread_deg.0).to_radians();
                // Lower whorls sit flatter and droop more.
                let dir = vec3(angle.cos() * tilt.sin(), tilt.cos(), angle.sin() * tilt.sin())
                    .normalize();
                let start = vec3(angle.cos(), 0.0, angle.sin()) * (trunk_r * 0.7)
                    + Vec3::Y * (y + rng.signed() * 0.2);
                let mut sub = rng.fork();
                self.branch_with_foliage(start, dir, len, trunk_r * 0.42, 1, &mut sub);
            }
            spin += 1.05;
            y += step * rng.range(0.85, 1.2);
        }
    }

    /// A conifer branch: foliage hangs along its whole length, not just the tip.
    fn branch_with_foliage(
        &mut self,
        start: Vec3,
        dir: Vec3,
        len: f32,
        radius: f32,
        level: u8,
        rng: &mut Rng,
    ) {
        let first = self.segments.len();
        let end = self.limb(start, dir, len, radius, level, rng, false);
        // Needles clothe the whole branch, so clusters go along it, thickening
        // toward the tip.
        let p = self.params;
        for idx in first..self.segments.len() {
            let along = (idx - first + 1) as f32 / (self.segments.len() - first).max(1) as f32;
            if !rng.chance(0.45 + along * 0.5) {
                continue;
            }
            let at = self.segments[idx].b;
            let off = vec3(rng.signed(), rng.signed() * 0.6, rng.signed()) * (p.leaf_radius * 0.3);
            self.leaves.push(LeafCluster {
                center: at + off - Vec3::Y * 0.1,
                radius: p.leaf_radius * (0.55 + along * 0.55) * rng.range(0.8, 1.2),
                shell: 1.0,
                seed: rng.next_u64(),
            });
        }
        if level < self.params.branch_levels {
            let children = rng.range_u8(self.params.children_per_node.0, self.params.children_per_node.1);
            for i in 0..children {
                let side = if i % 2 == 0 { 1.0 } else { -1.0 };
                let axis = end.dir.any_perpendicular();
                let child = end
                    .dir
                    .rotate_around(axis, side * rng.range(0.3, 0.7))
                    .rotate_around(Vec3::Y, rng.signed() * 0.5)
                    .normalize();
                let mut sub = rng.fork();
                self.branch_with_foliage(
                    end.pos,
                    child,
                    len * self.params.length_ratio,
                    radius * self.params.thickness_ratio,
                    level + 1,
                    &mut sub,
                );
            }
        }
    }

    /// Fork a limb `branch_levels` deep, ending in foliage.
    fn fork(&mut self, pos: Vec3, dir: Vec3, len: f32, radius: f32, level: u8, rng: &mut Rng) {
        let p = self.params;
        if level > p.branch_levels || len < 0.35 {
            self.hang_leaves(pos, dir, rng, 1.0);
            return;
        }
        let children = rng.range_u8(p.children_per_node.0, p.children_per_node.1);
        let spin = rng.range(0.0, std::f32::consts::TAU);
        let mut grown = 0;
        for i in 0..children {
            let azimuth = spin + std::f32::consts::TAU * i as f32 / children as f32
                + rng.signed() * 0.35;
            let tilt = (p.spread_deg.0
                + rng.unit() * (p.spread_deg.1 - p.spread_deg.0))
                .to_radians();
            let axis = dir.any_perpendicular().rotate_around(dir, azimuth);
            let child_dir = dir.rotate_around(axis, tilt).normalize();
            let child_len = len * p.length_ratio * rng.range(0.82, 1.18);
            let child_radius = radius * p.thickness_ratio;
            let mut sub = rng.fork();
            let end = self.limb(pos, child_dir, child_len, child_radius, level, &mut sub, false);
            if end.grown < child_len * 0.4 {
                // The envelope cut it short; finish it with a tuft.
                self.hang_leaves(end.pos, end.dir, &mut sub, 0.8);
                continue;
            }
            grown += 1;
            // The outermost two levels carry foliage along the limb as well as
            // at the tip, which fills the crown without thickening the wood.
            if level + 1 >= p.branch_levels {
                self.hang_leaves(end.pos, end.dir, &mut sub, 1.0);
            }
            self.fork(end.pos, end.dir, child_len, end.radius, level + 1, &mut sub);
        }
        if grown == 0 {
            self.hang_leaves(pos, dir, rng, 0.9);
        }
    }

    fn hang_leaves(&mut self, pos: Vec3, dir: Vec3, rng: &mut Rng, scale: f32) {
        let p = self.params;
        let count = p.leaf_clusters.max(1);
        for i in 0..count {
            let back = i as f32 * 0.55;
            let off = vec3(rng.signed(), rng.signed() * 0.7, rng.signed()) * (p.leaf_radius * 0.45);
            self.leaves.push(LeafCluster {
                center: pos - dir * back + off,
                radius: p.leaf_radius * scale * rng.range(0.78, 1.22),
                shell: 1.0,
                seed: rng.next_u64(),
            });
        }
    }

    /// Grow one limb as a chain of segments, wandering and drooping as it goes.
    fn limb(
        &mut self,
        start: Vec3,
        dir: Vec3,
        len: f32,
        radius: f32,
        depth: u8,
        rng: &mut Rng,
        leader: bool,
    ) -> LimbEnd {
        let p = self.params;
        let steps = ((len / STEP).round() as usize).max(1);
        let mut pos = start;
        let mut dir = dir.normalize();
        let mut grown = 0.0;
        for i in 0..steps {
            let t = (i + 1) as f32 / steps as f32;
            // A trunk holds its line; limbs wander and sag toward their tips.
            let wander = if leader { p.wander * 0.25 } else { p.wander };
            let noise = vec3(rng.signed(), rng.signed() * 0.5, rng.signed()) * wander * STEP;
            let sag = -Vec3::Y * p.droop * STEP * t;
            let mut next_dir = (dir + noise + sag).normalize();

            let mut next = pos + next_dir * STEP;
            if let Some(envelope) = self.envelope
                && !envelope.contains(next)
            {
                // Bend back toward the open middle of this level.
                if let Some(centroid) = envelope.centroid_at(pos) {
                    let inward = (centroid - vec3(pos.x, pos.y, pos.z)).normalize();
                    next_dir = (next_dir + inward * 0.8).normalize();
                    next = pos + next_dir * STEP;
                }
                if !envelope.contains(next) {
                    break;
                }
            }

            let r0 = radius * (1.0 - (i as f32 / steps as f32) * (1.0 - p.thickness_ratio));
            let r1 = radius * (1.0 - t * (1.0 - p.thickness_ratio));
            self.segments.push(Segment { a: pos, b: next, radius_a: r0, radius_b: r1, depth });
            pos = next;
            dir = next_dir;
            grown += STEP;
        }
        LimbEnd { pos, dir, radius: radius * p.thickness_ratio, grown }
    }
}

struct LimbEnd {
    pos: Vec3,
    dir: Vec3,
    radius: f32,
    grown: f32,
}

fn jitter_up(rng: &mut Rng, amount: f32) -> Vec3 {
    vec3(rng.signed() * amount, 1.0, rng.signed() * amount).normalize()
}

/// Scale the whole skeleton so its top lands on the requested height.
fn normalise_height(skeleton: &mut Skeleton, target: f32) {
    let mut top: f32 = 0.0;
    for s in &skeleton.segments {
        top = top.max(s.a.y).max(s.b.y);
    }
    if top <= 0.01 {
        return;
    }
    let k = target / top;
    if (k - 1.0).abs() < 0.01 {
        return;
    }
    for s in &mut skeleton.segments {
        s.a = s.a * k;
        s.b = s.b * k;
    }
    for l in &mut skeleton.leaves {
        l.center = l.center * k;
        l.radius *= k.clamp(0.8, 1.25);
    }
}

/// Find the crown and score each cluster's distance out to its shell.
fn measure(skeleton: &mut Skeleton) {
    let mut top: f32 = 0.0;
    for s in &skeleton.segments {
        top = top.max(s.a.y).max(s.b.y);
    }
    skeleton.height = top;
    if skeleton.leaves.is_empty() {
        return;
    }
    let mut center = Vec3::ZERO;
    for l in &skeleton.leaves {
        center += l.center;
    }
    center = center / skeleton.leaves.len() as f32;
    let mut radius: f32 = 0.5;
    for l in &skeleton.leaves {
        radius = radius.max((l.center - center).length());
    }
    skeleton.crown_center = center;
    skeleton.crown_radius = radius;
    for l in &mut skeleton.leaves {
        l.shell = ((l.center - center).length() / radius).clamp(0.0, 1.0);
    }
}
