//! Agreement of the depth map's resolve step on the GPU (`florastack_gpu::resolve`) with its CPU references in
//! florastack-stack (AGENTS.md, CPU oracle): the domain's distances and the recursive filter's feedback
//! (`depth::filter::Domain`), the edge-aware normalized smoothing (`depth::filter::smooth`), push-pull
//! (`depth::filter::push_pull`) and the whole filling (`depth::fill`), on seeded random planes of odd and narrow sizes
//! and on synthetic depth statistics. Each tolerance says why it is enough next to its assertion.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{gpu, max_difference, max_relative_difference, random};
use florastack_core::{ColorSpace, Image};
use florastack_gpu::resolve::{self, FillPlanes, FillSettings};
use florastack_stack::depth::filter::{Domain, push_pull, smooth};
use florastack_stack::depth::{EDGE, FILL_BLEND, Fill, MIN_SUPPORT, REACHED, fill};

/// Sizes that catch the edge cases: odd, narrow, a single row or column, and one large enough for long recursions.
const SIZES: [(usize, usize); 6] = [(37, 23), (64, 48), (1, 9), (13, 1), (130, 7), (301, 203)];

fn plane(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Image<f32> {
    Image::from_fn(w, h, 1, ColorSpace::LINEAR, |x, y, _| f(x, y)).unwrap()
}

/// A guide with flat regions and edges: blocks of random levels.
fn guide(w: usize, h: usize, seed: u64) -> Image<f32> {
    let levels = random(w.div_ceil(9), h.div_ceil(7), 1, seed, 0.0, 1.0);
    plane(w, h, |x, y| levels.get(x / 9, y / 7, 0).unwrap())
}

/// 0 or 1 at random, about `share` of them 1.
fn mask(w: usize, h: usize, seed: u64, share: f32) -> Image<f32> {
    let r = random(w, h, 1, seed, 0.0, 1.0);
    plane(w, h, |x, y| if r.get(x, y, 0).unwrap() < share { 1.0 } else { 0.0 })
}

#[test]
fn the_feedback_and_the_distances_agree_with_the_cpu() {
    let Some(gpu) = gpu() else { return };
    for sigma in [0.0, 0.5, 4.0, 16.0, 256.0] {
        let g = guide(40, 30, 1);
        let cpu = Domain::new(&g, sigma, EDGE).unwrap();
        // The same f64 arithmetic on the host: bit for bit.
        assert_eq!(resolve::feedback(sigma), cpu.feedback(), "sigma {sigma}");
    }
    for (i, &(w, h)) in SIZES.iter().enumerate() {
        let g = guide(w, h, i as u64);
        for sigma in [0.0, 8.0, 64.0] {
            let cpu = Domain::new(&g, sigma, EDGE).unwrap();
            let got = resolve::distances(gpu, &g, sigma, EDGE).unwrap();
            match (cpu.distances(), got) {
                (None, None) => {}
                (Some((across, down)), Some((a, d))) => {
                    // `1 + ratio |difference|`: a fused multiply-add on the GPU rounds once where the CPU rounds
                    // twice, a relative difference of an ulp or two.
                    assert!(max_relative_difference(&a, across, 1.0) < 1e-6, "{w}x{h}, sigma {sigma}: across");
                    assert!(max_relative_difference(&d, down, 1.0) < 1e-6, "{w}x{h}, sigma {sigma}: down");
                }
                (cpu, gpu) => panic!("{w}x{h}, sigma {sigma}: the CPU has distances {}, the GPU {}", cpu.is_some(), gpu.is_some()),
            }
        }
    }
}

#[test]
fn edge_aware_smoothing_agrees_with_the_cpu() {
    let Some(gpu) = gpu() else { return };
    for (i, &(w, h)) in SIZES.iter().enumerate() {
        let seed = 10 + i as u64;
        let g = guide(w, h, seed);
        let values = random(w, h, 1, seed + 1, 0.0, 20.0);
        for (weights, what) in [(mask(w, h, seed + 2, 0.4), "sparse"), (random(w, h, 1, seed + 3, 0.0, 1.0), "fractional")] {
            for sigma in [0.0, 4.0, 32.0, 256.0] {
                let (cpu_values, cpu_weights) = smooth(&values, &weights, &Domain::new(&g, sigma, EDGE).unwrap()).unwrap();
                let (v, k) = resolve::smooth(gpu, &values, &weights, &g, sigma, EDGE).unwrap();
                // The recursion's feedback is `exp` of the same product, which the GPU's `exp` gives within a few
                // ulps of the CPU's; each step mixes two neighbors by it, so the error stays of the order of a few
                // ulps of the values smoothed (up to 20 here) and of the weights (up to the canvas's extent), and
                // the quotient's relative error is the sum of theirs.
                let dk = max_relative_difference(&k, &cpu_weights, 1e-3);
                let dv = max_difference(&v, &cpu_values);
                assert!(dk < 1e-4, "{w}x{h} {what}, sigma {sigma}: weights {dk}");
                assert!(dv < 2e-3, "{w}x{h} {what}, sigma {sigma}: values {dv}");
            }
        }
    }
}

#[test]
fn push_pull_agrees_with_the_cpu() {
    let Some(gpu) = gpu() else { return };
    for (i, &(w, h)) in SIZES.iter().chain(&[(1, 1), (2, 1), (1000, 3)]).enumerate() {
        let seed = 30 + i as u64;
        let values = random(w, h, 1, seed, -5.0, 5.0);
        // Holes of every size, fractional weights, and weights beyond [0, 1] and not finite, which both clamp.
        let r = random(w, h, 1, seed + 1, -0.5, 1.5);
        let weights = plane(w, h, |x, y| match (x * 7 + y * 3) % 11 {
            0 => f32::NAN,
            1 => f32::INFINITY,
            _ if (x / 5 + y / 4) % 3 == 0 => 0.0,
            _ => r.get(x, y, 0).unwrap(),
        });
        let want = push_pull(&values, &weights).unwrap();
        let got = resolve::push_pull(gpu, &values, &weights).unwrap();
        // The sums of each block in the CPU's order, exact but for a fused multiply-add in the pull's
        // interpolation: an ulp or two of the values (up to 5) per level, a dozen levels at most.
        let d = max_difference(&got, &want);
        assert!(d < 1e-5, "{w}x{h}: {d}");
    }
    // No weight anywhere: zeros, as on the CPU.
    let none = resolve::push_pull(gpu, &random(9, 9, 1, 1, 0.0, 1.0), &plane(9, 9, |_, _| 0.0)).unwrap();
    assert!(none.as_raw().iter().all(|&v| v == 0.0));
}

/// Synthetic focus statistics: a depth sloping across the canvas in steps, with noise where the confidence is low;
/// the confidence high on a guide's edges; a coverage with an uncovered border.
fn statistics(w: usize, h: usize, frames: usize, seed: u64) -> (Image<f32>, Image<f32>, Image<f32>, Image<f32>) {
    let g = guide(w, h, seed);
    let noise = random(w, h, 1, seed + 1, -1.0, 1.0);
    #[allow(clippy::cast_precision_loss)]
    let depth = plane(w, h, |x, y| {
        let slope = (x as f32 / w as f32) * (frames - 1) as f32;
        (slope + 2.0 * noise.get(x, y, 0).unwrap()).clamp(0.0, (frames - 1) as f32)
    });
    let confidence = plane(w, h, |x, y| {
        let edge = (g.get(x, y, 0).unwrap() - g.get(x.saturating_sub(1), y, 0).unwrap()).abs();
        edge + 0.05 * noise.get(x, y, 0).unwrap().abs()
    });
    let covered = plane(w, h, |x, y| if x < 2 || y + 2 >= h { 0.0 } else { 1.0 });
    (depth, confidence, g, covered)
}

#[test]
fn the_whole_filling_agrees_with_the_cpu() {
    let Some(gpu) = gpu() else { return };
    for (i, &(w, h, frames)) in [(37, 23, 5), (301, 203, 12), (640, 480, 30), (130, 7, 3)].iter().enumerate() {
        let (depth, confidence, guide, covered) = statistics(w, h, frames, 50 + i as u64);
        for (smoothing, level, covered) in [(16.0, 0.1, Some(&covered)), (0.0, 0.1, None), (64.0, 0.3, Some(&covered)), (4.0, 10.0, None)] {
            let weights = plane(w, h, |x, y| {
                let c = confidence.get(x, y, 0).unwrap();
                let covers = covered.is_none_or(|k| k.get(x, y, 0).unwrap() > 0.0);
                if covers && c > 0.0 && c >= level { 1.0 } else { 0.0 }
            });
            let confident = weights.as_raw().iter().filter(|&&k| k > 0.0).count();
            let cpu = fill(Fill { depth: &depth, weights: weights.clone(), confident, guide: &guide, covered, smoothing, frames }).unwrap();
            let planes = FillPlanes { depth: &depth, weights: &weights, confident, guide: &guide, covered };
            let settings = FillSettings { smoothing, edge: EDGE, min_support: MIN_SUPPORT, reached: REACHED, fill_blend: FILL_BLEND, frames };
            let got = resolve::fill(gpu, &planes, &settings).unwrap();
            let what = format!("{w}x{h}, {frames} frames, smoothing {smoothing}, level {level}");
            // The support test compares a smoothed share with one half: a pixel whose share is within the
            // smoothing's rounding of it can fall either way, and with it the pixels its weight reaches. None did
            // here; allow a thousandth.
            let tolerance = confident / 1000 + 1;
            assert!(got.confident.abs_diff(cpu.confident) <= tolerance, "{what}: confident {} against {}", got.confident, cpu.confident);
            // The depth is the smoothing's quotient, filled by push-pull and clamped: within the smoothing's error
            // of a few ulps of the frames (up to 29 here).
            let d = max_difference(&got.depth, &cpu.depth);
            println!("{what}: confident {} ({} on the CPU), depth within {d:.2e} frames", got.confident, cpu.confident);
            assert!(d < 2e-3, "{what}: depth {d}");
            match (&got.blend, &cpu.blend) {
                (Some(a), Some(b)) => {
                    // 1 + 2 (1 - reach): the reach's error, doubled.
                    let d = max_difference(a, b);
                    assert!(d < 1e-4, "{what}: blend {d}");
                }
                (None, None) => {}
                (a, b) => panic!("{what}: the GPU's blend is {}, the CPU's {}", a.is_some(), b.is_some()),
            }
        }
    }
    // A plane of another size is an error.
    let (depth, _, guide, _) = statistics(20, 10, 3, 1);
    let weights = plane(21, 10, |_, _| 1.0);
    let planes = FillPlanes { depth: &depth, weights: &weights, confident: 0, guide: &guide, covered: None };
    let settings = FillSettings { smoothing: 4.0, edge: EDGE, min_support: MIN_SUPPORT, reached: REACHED, fill_blend: FILL_BLEND, frames: 3 };
    assert!(resolve::fill(gpu, &planes, &settings).is_err());
}
