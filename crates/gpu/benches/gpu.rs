//! Per-frame times of the GPU's stackers on 24 MP (6000 x 4000) RGB frames, beside florastack-stack's
//! `stack/*/add-frame` benches: a frame on the canvas folded in (uploaded a tile at a time), and a frame warped onto
//! the canvas on the GPU, as the engine stacks its frames. Each time includes the frame's upload and waits for the GPU
//! to finish it. Without a hardware GPU adapter the benches aren't registered (and say so).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use florastack_core::ops::gaussian_blur;
use florastack_core::testutil::scene;
use florastack_core::{Affine, Image, Unmonitored};
use florastack_gpu::ops::WarpGeometry;
use florastack_gpu::{Frame, Gpu, GpuOptions, GpuStacker, GpuTiling, Method};

const WIDTH: usize = 6000;
const HEIGHT: usize = 4000;

fn stackers(c: &mut Criterion) {
    let gpu = match Gpu::blocking(&GpuOptions::hardware()) {
        Ok(gpu) => gpu,
        Err(e) => {
            println!("gpu benches: skipped, no hardware GPU adapter: {e}");
            return;
        }
    };
    let (sharp, _) = scene::textured(WIDTH, HEIGHT, 1).unwrap();
    let blurred = gaussian_blur(&sharp, 3.0).unwrap();
    let frames: [Image<f32>; 2] = [sharp, blurred];
    let size = format!("{WIDTH}x{HEIGHT}");
    let mut group = c.benchmark_group("gpu");
    group.sampling_mode(SamplingMode::Flat).sample_size(10).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements((WIDTH * HEIGHT) as u64));
    let methods = [
        ("average/add-frame", Method::Average { radius: 3.0, smoothing: 8.0, power: 3.0 }),
        ("pyramid/add-frame", Method::Pyramid { levels: 0, blend: Some(2.0), channel_decisions: false, range_limit: 1.0 }),
        ("depth-map/measure-frame", Method::Focus { radius: 2.0 }),
    ];
    for (name, method) in methods {
        let mut stacker = GpuStacker::new(&gpu, method, GpuTiling::default()).unwrap();
        stacker.begin(WIDTH, HEIGHT, 3).unwrap();
        let mut next = 0;
        group.bench_function(BenchmarkId::new(name, &size), |b| {
            b.iter(|| {
                stacker.add_frame(&Frame::Canvas { image: &frames[next % 2], coverage: None }, &Unmonitored).unwrap();
                next += 1;
            });
        });
    }
    // Warped on the GPU from the decoded frame, as the engine stacks: a fraction of a pixel's shift and a scale.
    let mut stacker = GpuStacker::new(&gpu, Method::Average { radius: 3.0, smoothing: 8.0, power: 3.0 }, GpuTiling::default()).unwrap();
    stacker.begin(WIDTH, HEIGHT, 3).unwrap();
    let to_frame = Affine::similarity((3000.0, 2000.0), 1.002, 0.001, (0.4, -0.3));
    let geometry = WarpGeometry { to_frame, frame_width: WIDTH, frame_height: HEIGHT, feather: 2.0, gain: 1.0 };
    let mut next = 0;
    group.bench_function(BenchmarkId::new("average/add-warped-frame", &size), |b| {
        b.iter(|| {
            stacker.add_frame(&Frame::Warp { image: &frames[next % 2], geometry }, &Unmonitored).unwrap();
            next += 1;
        });
    });
    group.finish();
}

criterion_group!(benches, stackers);
criterion_main!(benches);
