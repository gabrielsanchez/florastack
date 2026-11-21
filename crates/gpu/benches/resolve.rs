//! The depth map's resolve step at 24 MP (6000 x 4000): the whole-canvas filling on the CPU (florastack-stack's
//! `depth::fill`, on every thread) and on the GPU (`florastack_gpu::resolve::fill`, its planes' uploads and readbacks
//! included), on synthetic statistics with about a third of the pixels confident, at the default smoothing. Without
//! a hardware GPU adapter, only the CPU's is registered.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use florastack_core::testutil::scene;
use florastack_core::{ColorSpace, Image};
use florastack_gpu::resolve::{self, FillPlanes, FillSettings};
use florastack_gpu::{Gpu, GpuOptions};
use florastack_stack::depth::{EDGE, FILL_BLEND, Fill, MIN_SUPPORT, REACHED, fill};

const WIDTH: usize = 6000;
const HEIGHT: usize = 4000;
const FRAMES: usize = 30;
/// florastack-stack's default smoothing radius, in pixels.
const SMOOTHING: f64 = 16.0;

fn filling(c: &mut Criterion) {
    let (guide, depth) = scene::textured(WIDTH, HEIGHT, 3).unwrap();
    let guide = Image::from_fn(WIDTH, HEIGHT, 1, ColorSpace::LINEAR, |x, y, _| guide.get(x, y, 1).unwrap()).unwrap();
    #[allow(clippy::cast_precision_loss)]
    let depth = Image::from_fn(WIDTH, HEIGHT, 1, ColorSpace::LINEAR, |x, y, _| depth.get(x, y, 0).unwrap() * (FRAMES - 1) as f32).unwrap();
    let weights = Image::from_fn(WIDTH, HEIGHT, 1, ColorSpace::LINEAR, |x, y, _| if (x / 40 + y / 30) % 3 == 0 { 1.0 } else { 0.0 }).unwrap();
    let confident = weights.as_raw().iter().filter(|&&k| k > 0.0).count();
    let mut group = c.benchmark_group("resolve");
    group.sampling_mode(SamplingMode::Flat).sample_size(10).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(15));
    group.throughput(Throughput::Elements((WIDTH * HEIGHT) as u64));
    let size = format!("{WIDTH}x{HEIGHT}");
    group.bench_function(format!("cpu/fill/{size}"), |b| {
        b.iter(|| {
            let input =
                Fill { depth: &depth, weights: weights.clone(), confident, guide: &guide, covered: None, smoothing: SMOOTHING, frames: FRAMES };
            fill(input).unwrap()
        });
    });
    match Gpu::blocking(&GpuOptions::hardware()) {
        Ok(gpu) => {
            let settings =
                FillSettings { smoothing: SMOOTHING, edge: EDGE, min_support: MIN_SUPPORT, reached: REACHED, fill_blend: FILL_BLEND, frames: FRAMES };
            let planes = FillPlanes { depth: &depth, weights: &weights, confident, guide: &guide, covered: None };
            group.bench_function(format!("gpu/fill/{size}"), |b| b.iter(|| resolve::fill(&gpu, &planes, &settings).unwrap()));
        }
        Err(e) => println!("resolve benches: the GPU's skipped, no hardware GPU adapter: {e}"),
    }
    group.finish();
}

criterion_group!(benches, filling);
criterion_main!(benches);
