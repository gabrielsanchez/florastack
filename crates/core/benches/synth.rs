//! Benchmarks of the synthetic stack generator, so that test and bench setup costs stay visible. Run them with
//! `cargo xtask bench`, which writes the summary to `target/bench-summary.json`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use florastack_core::testutil::{StackParams, generate, scene};

fn synth(c: &mut Criterion) {
    let (w, h, frames) = (256, 256, 8);
    let (sharp, depth) = scene::textured(w, h, 1).unwrap();
    let mut group = c.benchmark_group("synth");
    group.sample_size(10);
    group.throughput(Throughput::Elements(u64::try_from(w * h * frames).unwrap()));
    group.bench_function(BenchmarkId::new("scene", format!("{w}x{h}")), |b| b.iter(|| scene::textured(w, h, 1).unwrap()));
    let params = StackParams::noisy_dusty(frames, w, h, 7);
    group.bench_function(BenchmarkId::new("noisy_dusty", format!("{w}x{h}x{frames}")), |b| b.iter(|| generate(&sharp, &depth, &params).unwrap()));
    group.finish();
}

criterion_group!(benches, synth);
criterion_main!(benches);
