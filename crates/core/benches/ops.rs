//! Benchmarks of the basic image operations on a 24 MP (6000x4000) RGB `f32` image: Gaussian blur with the exact
//! kernel and with box filters, the bilinear and bicubic warps, and the sRGB curves. Run them with `cargo xtask bench`,
//! which writes the summary to `target/bench-summary.json`.
//!
//! Each image is 288 MB, so the benchmarks work in place where they can: the blurs and the curves change one image
//! (their cost doesn't depend on its content), and the warps write into one preallocated output. At most two images
//! are alive at a time.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use florastack_core::ops::{BlurMethod, Border, Interpolation, gaussian_blur_in_place, warp_into};
use florastack_core::{Affine, ColorSpace, Encoding, Image};
use rayon::prelude::*;

const WIDTH: usize = 6000;
const HEIGHT: usize = 4000;

/// A textured linear RGB image: smooth gradients with fine detail, filled in parallel.
fn scene() -> Image<f32> {
    let mut image = Image::<f32>::new(WIDTH, HEIGHT, 3, ColorSpace::LINEAR).unwrap();
    image.par_rows_mut().enumerate().for_each(|(y, row)| {
        for (i, v) in row.iter_mut().enumerate() {
            let (x, c) = ((i / 3) as f32, (i % 3) as f32);
            let y = y as f32;
            *v = 0.5 + 0.25 * (x * 0.013 + c).sin() * (y * 0.007).cos() + 0.1 * ((x * 0.9 + y * 1.3).sin() * (x * 0.31 - y * 0.47).cos());
        }
    });
    image
}

fn ops(c: &mut Criterion) {
    let mut image = scene();
    let size = format!("{WIDTH}x{HEIGHT}");
    let mut group = c.benchmark_group("ops");
    // Hundreds of milliseconds per call: ten samples of one call each.
    group.sampling_mode(SamplingMode::Flat).sample_size(10).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(5));
    group.throughput(Throughput::Elements((WIDTH * HEIGHT) as u64));

    group.bench_function(BenchmarkId::new("gaussian_blur_exact_sigma2", &size), |b| {
        b.iter(|| gaussian_blur_in_place(&mut image, 2.0, BlurMethod::Exact).unwrap());
    });
    group.bench_function(BenchmarkId::new("gaussian_blur_boxes_sigma16", &size), |b| {
        b.iter(|| gaussian_blur_in_place(&mut image, 16.0, BlurMethod::Auto).unwrap());
    });
    group.bench_function(BenchmarkId::new("srgb_encode_decode_in_place", &size), |b| {
        b.iter(|| {
            image.convert_encoding(Encoding::Srgb);
            image.convert_encoding(Encoding::Linear);
        });
    });

    // A frame's typical misalignment: a little focus breathing, a small turn and a sub-pixel shift.
    let center = ((WIDTH - 1) as f64 / 2.0, (HEIGHT - 1) as f64 / 2.0);
    let to_source = Affine::similarity(center, 1.004, 0.002, (0.37, -0.81));
    let mut warped = Image::<f32>::new(WIDTH, HEIGHT, 3, ColorSpace::LINEAR).unwrap();
    for (name, interpolation) in [("warp_bilinear", Interpolation::Bilinear), ("warp_bicubic", Interpolation::Bicubic)] {
        group.bench_function(BenchmarkId::new(name, &size), |b| {
            b.iter(|| warp_into(&image, &to_source, interpolation, Border::Clamp, &mut warped).unwrap());
        });
    }
    group.finish();
}

criterion_group!(benches, ops);
criterion_main!(benches);
