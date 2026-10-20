//! Benchmarks of the image-quality metrics on a 512x512 synthetic scene and a noisy, shifted frame of it. Run them
//! with `cargo xtask bench`, which writes the summary to `target/bench-summary.json`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use criterion::{BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use florastack_core::testutil::{StackParams, generate, scene};
use florastack_metrics::{HaloParams, NoiseParams, color_shift, halo, noise, psnr, ssim};

fn metrics(c: &mut Criterion) {
    let (w, h) = (512, 512);
    let (sharp, depth) = scene::textured(w, h, 2).unwrap();
    let params = StackParams { blur_per_depth: 2.0, max_blur: 2.0, ..StackParams::noisy_dusty(1, w, h, 3) };
    let result = generate(&sharp, &depth, &params).unwrap().frames.remove(0);
    let size = format!("{w}x{h}");
    let mut group = c.benchmark_group("metrics");
    // Milliseconds per call: flat sampling (the same iteration count in every sample) fits 50 samples into
    // criterion's default 5 s measurement.
    group.sampling_mode(SamplingMode::Flat).sample_size(50);
    group.throughput(Throughput::Elements(u64::try_from(w * h).unwrap()));
    group.bench_function(BenchmarkId::new("psnr", &size), |b| b.iter(|| psnr(&result, &sharp).unwrap()));
    group.bench_function(BenchmarkId::new("ssim", &size), |b| b.iter(|| ssim(&result, &sharp).unwrap()));
    group.bench_function(BenchmarkId::new("color_shift", &size), |b| b.iter(|| color_shift(&result, &sharp).unwrap()));
    group.bench_function(BenchmarkId::new("halo", &size), |b| b.iter(|| halo(&result, &sharp, &HaloParams::default()).unwrap()));
    group.bench_function(BenchmarkId::new("noise", &size), |b| b.iter(|| noise(&result, &sharp, &NoiseParams::default()).unwrap()));
    group.finish();
}

criterion_group!(benches, metrics);
criterion_main!(benches);
