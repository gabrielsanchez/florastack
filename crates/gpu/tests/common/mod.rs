//! What the agreement tests share: the device they run on, and how they compare images.
//!
//! The device is the best adapter, software ones included ([`GpuOptions::any`]; `WGPU_ADAPTER_NAME` picks one by
//! name), made once per test binary. Without one a test calls `skip!(Gpu, ...)` and returns, so that `cargo xtask ci`
//! lists it, unless `FLORASTACK_REQUIRE_GPU` is set, as CI sets it on the runners that have a software adapter: there
//! a missing adapter fails the test instead of passing silently. A device that exists but computes wrong results
//! always fails.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::sync::OnceLock;

use florastack_core::Image;
use florastack_gpu::{Gpu, GpuOptions};

/// The test binary's device, or why there is none.
fn device() -> &'static Result<Gpu, String> {
    static DEVICE: OnceLock<Result<Gpu, String>> = OnceLock::new();
    DEVICE.get_or_init(|| {
        let gpu = Gpu::blocking(&GpuOptions::any()).map_err(|e| e.to_string())?;
        eprintln!("agreement tests on {}", gpu.adapter());
        Ok(gpu)
    })
}

/// Whether a missing adapter fails the tests: `FLORASTACK_REQUIRE_GPU` set and not empty.
pub fn required() -> bool {
    std::env::var_os("FLORASTACK_REQUIRE_GPU").is_some_and(|v| !v.is_empty())
}

/// The device, or `None` after recording the skip (see the module documentation).
#[must_use]
pub fn gpu() -> Option<&'static Gpu> {
    match device() {
        Ok(gpu) => Some(gpu),
        Err(reason) => {
            assert!(!required(), "FLORASTACK_REQUIRE_GPU is set, but no GPU adapter is usable: {reason}");
            florastack_testkit::skip!(Gpu, "no GPU adapter is usable: {reason}");
            None
        }
    }
}

/// The largest absolute difference between `a` and `b`'s samples where `keep` holds for the pixel, after checking
/// that their shapes agree.
pub fn max_difference_where(a: &Image<f32>, b: &Image<f32>, keep: impl Fn(usize, usize) -> bool) -> f32 {
    assert_eq!((a.width(), a.height(), a.channels()), (b.width(), b.height(), b.channels()), "shapes differ");
    let c = a.channels();
    let mut worst = 0.0f32;
    for y in 0..a.height() {
        for x in 0..a.width() {
            if !keep(x, y) {
                continue;
            }
            for ch in 0..c {
                let (u, v) = (a.get(x, y, ch).unwrap(), b.get(x, y, ch).unwrap());
                let d = if u.is_finite() && v.is_finite() {
                    (u - v).abs()
                } else if u.to_bits() == v.to_bits() {
                    0.0
                } else {
                    f32::INFINITY
                };
                worst = worst.max(d);
            }
        }
    }
    worst
}

/// The largest absolute difference between `a` and `b`'s samples.
pub fn max_difference(a: &Image<f32>, b: &Image<f32>) -> f32 {
    max_difference_where(a, b, |_, _| true)
}

/// The largest difference relative to the larger magnitude of the two (absolute below `floor`).
pub fn max_relative_difference(a: &Image<f32>, b: &Image<f32>, floor: f32) -> f32 {
    assert_eq!((a.width(), a.height(), a.channels()), (b.width(), b.height(), b.channels()), "shapes differ");
    a.rows().flatten().zip(b.rows().flatten()).map(|(u, v)| (u - v).abs() / u.abs().max(v.abs()).max(floor)).fold(0.0, f32::max)
}

/// A seeded pseudo-random image in `[lo, hi)`: xorshift, so that the tests need no dependency.
pub fn random(width: usize, height: usize, channels: usize, seed: u64, lo: f32, hi: f32) -> Image<f32> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / (1u64 << 24) as f32
    };
    let samples = (0..width * height * channels).map(|_| lo + (hi - lo) * next()).collect();
    Image::from_vec(width, height, channels, florastack_core::ColorSpace::LINEAR, samples).unwrap()
}
