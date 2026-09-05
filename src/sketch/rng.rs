//! Deterministic pseudo-random number generator for the hand-drawn pass.
//!
//! Sketching must be **reproducible**: given the same seed and the same scene, two runs have to
//! produce point-identical paths, otherwise golden / regression tests are meaningless and every
//! re-render of the same diagram looks different. Hence a self-contained PRNG (mulberry32) with
//! no global state and no extra dependency.

use std::sync::atomic::{AtomicU32, Ordering};

/// mulberry32: 32-bit state, one word of memory, uniform enough for jitter.
#[derive(Debug, Clone, Copy)]
pub struct Rng {
    state: u32,
}

impl Rng {
    /// Build from a seed (only the low 32 bits are used).
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            state: (seed & 0xffff_ffff) as u32,
        }
    }

    /// Seedless: mix wall-clock nanoseconds with a process-wide counter, so two consecutive
    /// renders in the same process still differ.
    ///
    /// Only used when [`crate::sketch::SketchOptions::seed`] is `None`; tests must always pass an
    /// explicit seed.
    #[must_use]
    pub fn from_entropy() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9);
        let n = u64::from(COUNTER.fetch_add(1, Ordering::Relaxed));
        Self::new(nanos ^ (n << 32) ^ (nanos >> 17))
    }

    fn next_u32(&mut self) -> u32 {
        self.state = self.state.wrapping_add(0x6d2b_79f5);
        let mut t = self.state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add(t ^ (t >> 7)).wrapping_mul(t | 61);
        t ^ (t >> 14)
    }

    /// Uniform in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        f64::from(self.next_u32()) / (f64::from(u32::MAX) + 1.0)
    }

    /// Uniform in `[lo, hi)`.
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }

    /// rough.js' `_offsetOpt`: a symmetric jitter in `±x`, scaled by `roughness` and by the
    /// length-dependent damping `gain`.
    ///
    /// This is the single place where `roughness` enters the geometry — endpoint jitter and the
    /// bowing displacement both go through it, so `roughness = 0` degrades to the exact input
    /// geometry (useful for tests and for "sketch colour, crisp shapes").
    pub fn offset(&mut self, x: f64, roughness: f64, gain: f64) -> f64 {
        roughness * gain * self.range(-x, x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_yields_identical_stream() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        let va: Vec<f64> = (0..32).map(|_| a.next_f64()).collect();
        let vb: Vec<f64> = (0..32).map(|_| b.next_f64()).collect();
        assert_eq!(va, vb, "同种子必须逐点相同");
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        let n = (0..16)
            .filter(|_| (a.next_f64() - b.next_f64()).abs() > 0.1)
            .count();
        assert!(n > 4, "不同种子的序列应明显不同，got {n}/16");
    }

    #[test]
    fn output_stays_in_unit_range() {
        let mut r = Rng::new(7);
        for _ in 0..10_000 {
            let v = r.next_f64();
            assert!((0.0..1.0).contains(&v), "got {v}");
        }
    }

    #[test]
    fn range_respects_bounds() {
        let mut r = Rng::new(9);
        for _ in 0..1000 {
            let v = r.range(-3.0, 5.0);
            assert!((-3.0..5.0).contains(&v), "got {v}");
        }
    }

    /// `roughness = 0` 必须让抖动完全消失（可回归到精确几何）。
    #[test]
    fn zero_roughness_means_no_jitter() {
        let mut r = Rng::new(3);
        for _ in 0..100 {
            assert_eq!(r.offset(2.0, 0.0, 1.0), 0.0);
        }
    }

    #[test]
    fn entropy_seeds_differ_within_one_process() {
        let mut a = Rng::from_entropy();
        let mut b = Rng::from_entropy();
        assert_ne!(a.next_u32(), b.next_u32());
    }
}
