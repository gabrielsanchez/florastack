//! Benchmarks of frame alignment on 24 MP (6000x4000) RGB `f32` frames: a 100-frame stack, the roadmap's yardstick,
//! and a pair of frames that only shift, aligned with every term and with the shifts alone, which shows what turning
//! off the terms a stack doesn't need saves. Run them with `cargo xtask bench`, which writes the summary to
//! `target/bench-summary.json`.
//!
//! The frames are synthetic: the textured test scene, moved by sub-pixel shifts, a small rotation and focus breathing
//! (the pair: shifts only), without defocus or noise, which would only make the generator slower. Eight frames
//! (2.3 GB) are made once, and the stack takes them in turn, so that frame 8 is frame 0 again; the aligner reads each
//! from memory, as it would read a frame the engine has just developed, so the times are alignment alone. A pair is
//! two frames prepared and one aligned to the other. A stack takes about half a minute and criterion measures it ten
//! times, so this file takes several minutes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use florastack_align::{AlignSettings, StackAligner, align_frames};
use florastack_core::Unmonitored;
use florastack_core::testutil::{StackParams, generate, scene};

const WIDTH: usize = 6000;
const HEIGHT: usize = 4000;
/// The distinct frames made, and the frames of the stack.
const DISTINCT: usize = 8;
const STACK: usize = 100;

fn align(c: &mut Criterion) {
    let (sharp, depth) = scene::textured(WIDTH, HEIGHT, 1).unwrap();
    let moving = StackParams { breathing: 0.005, max_shift: 3.0, max_rotation_deg: 0.1, seed: 1, ..StackParams::identity(DISTINCT) };
    let frames = generate(&sharp, &depth, &moving).unwrap().frames;
    let shifted = generate(&sharp, &depth, &StackParams { max_shift: 3.0, seed: 2, ..StackParams::identity(2) }).unwrap().frames;
    drop((sharp, depth));
    let size = format!("{WIDTH}x{HEIGHT}");
    let pixels = (WIDTH * HEIGHT) as u64;

    let mut group = c.benchmark_group("align");
    // Seconds per call: ten samples of one call each.
    group.sampling_mode(SamplingMode::Flat).sample_size(10).warm_up_time(Duration::from_secs(1));

    group.throughput(Throughput::Elements(2 * pixels)).measurement_time(Duration::from_secs(10));
    let shifts = AlignSettings { scale: false, rotation: false, brightness: false, ..AlignSettings::default() };
    for (name, settings) in [("all-terms", AlignSettings::default()), ("shift-only", shifts)] {
        group.bench_function(BenchmarkId::new(format!("pair/{name}"), &size), |b| {
            b.iter(|| align_frames(&shifted, 0, &settings, &Unmonitored).unwrap());
        });
    }

    // About half a minute each: one per sample (criterion warns that ten don't fit in this time, and runs them anyway).
    group.throughput(Throughput::Elements(STACK as u64 * pixels)).measurement_time(Duration::from_secs(240));
    group.bench_function(BenchmarkId::new("stack", format!("{STACK}x{size}")), |b| {
        b.iter(|| {
            let mut aligner = StackAligner::new(&AlignSettings::default(), STACK, 0).unwrap();
            while let Some(index) = aligner.next_frame() {
                aligner.add(&frames[index % DISTINCT], &Unmonitored).unwrap();
            }
            aligner.finish().unwrap()
        });
    });
    group.finish();
}

criterion_group!(benches, align);
criterion_main!(benches);
