//! Benchmarks of the weighted average and the pyramid method on 24 MP (6000x4000) RGB `f32` frames, the roadmap's
//! yardstick: one frame folded into the accumulators (the cost per frame of a stack of any depth), the end of a stack
//! (normalizing or collapsing, and stitching the tiles), and one frame of the quarter-scale preview (reducing the frame
//! included); and a whole 12-frame stack of 1500x1000 frames from beginning to end. For the depth-map method, one frame
//! of each pass (measured, then composited), the end of pass 1, and the depth map made from the focus statistics: the
//! step a new threshold takes, at full size. Run them with `cargo xtask bench`, which writes the summary to
//! `target/bench-summary.json`; the methods' quality on synthetic stacks is `quality.rs`.
//!
//! The frames are synthetic: the textured test scene, sharp and blurred (288 MB each, made once per method; the
//! pyramid's benchmarks keep one, as its accumulators take 1 GB at this size), read from memory as the engine reads a
//! frame it has just decoded and warped, so the times are stacking alone.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use florastack_core::ops::gaussian_blur;
use florastack_core::testutil::{StackParams, generate, scene};
use florastack_core::{Image, Unmonitored};
use florastack_stack::{
    AverageSettings, CompositeKernel, DepthSettings, Method, PyramidSettings, StackShape, Stacker, Threshold, Tiled, Tiling, depth_stack, preview,
    stack,
};

const WIDTH: usize = 6000;
const HEIGHT: usize = 4000;

fn average(c: &mut Criterion) {
    let (sharp, _) = scene::textured(WIDTH, HEIGHT, 1).unwrap();
    let blurred = gaussian_blur(&sharp, 3.0).unwrap();
    let frames: [Image<f32>; 2] = [sharp, blurred];
    let size = format!("{WIDTH}x{HEIGHT}");
    let pixels = (WIDTH * HEIGHT) as u64;
    let settings = AverageSettings::default();
    let tiling = Tiling::default();
    let shape = StackShape { width: WIDTH, height: HEIGHT, channels: 3 };

    let mut group = c.benchmark_group("stack");
    group.sampling_mode(SamplingMode::Flat).sample_size(10).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(20));
    group.throughput(Throughput::Elements(pixels));

    let mut stacker = settings.stacker(&tiling).unwrap();
    stacker.begin(shape).unwrap();
    let mut next = 0;
    group.bench_function(BenchmarkId::new("average/add-frame", &size), |b| {
        b.iter(|| {
            stacker.add_frame(&frames[next % 2], &Unmonitored).unwrap();
            next += 1;
        });
    });
    drop(stacker);

    group.bench_function(BenchmarkId::new("average/finish", &size), |b| {
        b.iter_batched(
            || {
                let mut stacker = settings.stacker(&tiling).unwrap();
                stacker.begin(shape).unwrap();
                stacker.add_frame(&frames[0], &Unmonitored).unwrap();
                stacker
            },
            |mut stacker| stacker.finish(&Unmonitored).unwrap(),
            BatchSize::PerIteration,
        );
    });

    let mut quarter = preview(&settings, &tiling).unwrap();
    quarter.begin(shape).unwrap();
    group.bench_function(BenchmarkId::new("average/preview-frame", &size), |b| {
        b.iter(|| {
            quarter.add_frame(&frames[next % 2], &Unmonitored).unwrap();
            next += 1;
        });
    });
    drop(quarter);
    drop(frames);

    // A whole small stack, defocused through the scene's depth.
    let (sharp, depth) = scene::textured(1500, 1000, 2).unwrap();
    let small = generate(&sharp, &depth, &StackParams { blur_per_depth: 8.0, max_blur: 8.0, ..StackParams::identity(12) }).unwrap().frames;
    group.throughput(Throughput::Elements(12 * 1500 * 1000)).measurement_time(Duration::from_secs(10));
    group.bench_function(BenchmarkId::new("average/stack", "12x1500x1000"), |b| {
        b.iter(|| {
            let mut stacker = settings.stacker(&tiling).unwrap();
            stack(&mut stacker, &small, &Unmonitored).unwrap()
        });
    });
    group.finish();
}

fn pyramid(c: &mut Criterion) {
    let (sharp, _) = scene::textured(WIDTH, HEIGHT, 1).unwrap();
    let size = format!("{WIDTH}x{HEIGHT}");
    let pixels = (WIDTH * HEIGHT) as u64;
    let settings = PyramidSettings::default();
    let tiling = Tiling::default();
    let shape = StackShape { width: WIDTH, height: HEIGHT, channels: 3 };

    let mut group = c.benchmark_group("stack");
    group.sampling_mode(SamplingMode::Flat).sample_size(10).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(30));
    group.throughput(Throughput::Elements(pixels));

    let mut stacker = settings.stacker(&tiling).unwrap();
    stacker.begin(shape).unwrap();
    group.bench_function(BenchmarkId::new("pyramid/add-frame", &size), |b| {
        b.iter(|| stacker.add_frame(&sharp, &Unmonitored).unwrap());
    });
    // One stack's accumulators at a time: they take 1 GB at this size.
    drop(stacker);
    group.bench_function(BenchmarkId::new("pyramid/finish", &size), |b| {
        b.iter_batched(
            || {
                let mut stacker = settings.stacker(&tiling).unwrap();
                stacker.begin(shape).unwrap();
                stacker.add_frame(&sharp, &Unmonitored).unwrap();
                stacker
            },
            |mut stacker| stacker.finish(&Unmonitored).unwrap(),
            BatchSize::PerIteration,
        );
    });

    let mut quarter = preview(&settings, &tiling).unwrap();
    quarter.begin(shape).unwrap();
    group.bench_function(BenchmarkId::new("pyramid/preview-frame", &size), |b| {
        b.iter(|| quarter.add_frame(&sharp, &Unmonitored).unwrap());
    });
    drop(quarter);
    drop(sharp);

    let (sharp, depth) = scene::textured(1500, 1000, 2).unwrap();
    let small = generate(&sharp, &depth, &StackParams { blur_per_depth: 8.0, max_blur: 8.0, ..StackParams::identity(12) }).unwrap().frames;
    group.throughput(Throughput::Elements(12 * 1500 * 1000)).measurement_time(Duration::from_secs(20));
    group.bench_function(BenchmarkId::new("pyramid/stack", "12x1500x1000"), |b| {
        b.iter(|| {
            let mut stacker = settings.stacker(&tiling).unwrap();
            stack(&mut stacker, &small, &Unmonitored).unwrap()
        });
    });
    group.finish();
}

fn depth_map(c: &mut Criterion) {
    let (sharp, _) = scene::textured(WIDTH, HEIGHT, 1).unwrap();
    let blurred = gaussian_blur(&sharp, 3.0).unwrap();
    let frames: [Image<f32>; 2] = [sharp, blurred];
    let size = format!("{WIDTH}x{HEIGHT}");
    let pixels = (WIDTH * HEIGHT) as u64;
    let settings = DepthSettings::default();
    let tiling = Tiling::default();
    let shape = StackShape { width: WIDTH, height: HEIGHT, channels: 3 };

    let mut group = c.benchmark_group("stack");
    group.sampling_mode(SamplingMode::Flat).sample_size(10).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(20));
    group.throughput(Throughput::Elements(pixels));

    // Pass 1, frame after frame; then the statistics of two frames for the steps after it.
    let mut estimator = settings.estimator(&tiling).unwrap();
    estimator.begin(shape).unwrap();
    let mut next = 0;
    group.bench_function(BenchmarkId::new("depth-map/measure-frame", &size), |b| {
        b.iter(|| {
            estimator.add_frame(&frames[next % 2], &Unmonitored).unwrap();
            next += 1;
        });
    });
    drop(estimator);
    let measured = || {
        let mut estimator = settings.estimator(&tiling).unwrap();
        estimator.begin(shape).unwrap();
        for frame in &frames {
            estimator.add_frame(frame, &Unmonitored).unwrap();
        }
        estimator
    };
    group.bench_function(BenchmarkId::new("depth-map/finish-pass-1", &size), |b| {
        b.iter_batched(measured, |mut estimator| estimator.finish(&Unmonitored).unwrap(), BatchSize::PerIteration);
    });
    let stats = measured().finish(&Unmonitored).unwrap();
    // A new threshold each time, as the interactive threshold makes them.
    let mut percentile = 10.0;
    group.bench_function(BenchmarkId::new("depth-map/resolve", &size), |b| {
        b.iter(|| {
            percentile = if percentile > 60.0 { 10.0 } else { percentile + 7.0 };
            stats.resolve(Threshold::Percentile(percentile), settings.smoothing, None).unwrap()
        });
    });
    let map = stats.resolve(settings.contrast_threshold(), settings.smoothing, None).unwrap();
    drop(stats);
    // A compositor that takes more frames than the two measured, so that frame after frame can be folded in.
    let mut compositor = Tiled::new(CompositeKernel::new(Arc::clone(&map.depth), 10_000).unwrap(), tiling.clone());
    compositor.begin(shape).unwrap();
    group.bench_function(BenchmarkId::new("depth-map/composite-frame", &size), |b| {
        b.iter(|| compositor.add_frame(&frames[0], &Unmonitored).unwrap());
    });
    drop((compositor, map, frames));

    let (sharp, depth) = scene::textured(1500, 1000, 2).unwrap();
    let small = generate(&sharp, &depth, &StackParams { blur_per_depth: 8.0, max_blur: 8.0, ..StackParams::identity(12) }).unwrap().frames;
    group.throughput(Throughput::Elements(12 * 1500 * 1000)).measurement_time(Duration::from_secs(20));
    group.bench_function(BenchmarkId::new("depth-map/stack", "12x1500x1000"), |b| {
        b.iter(|| depth_stack(&settings, &tiling, &small, None, &Unmonitored).unwrap());
    });
    group.finish();
}

criterion_group!(benches, average, pyramid, depth_map);
criterion_main!(benches);
