//! CPU against GPU, per method, on a stack of 100 frames of 24 MP (6000 x 4000, RGB): the weighted average, the
//! pyramid method and the depth map (both passes and the depth map between them), each frame warped onto the canvas
//! as the engine warps it (bicubic, with its coverage and gain), from florastack-stack's stackers and florastack-gpu's.
//! Each run is timed once from the first frame to the stitched result in host memory: uploads, kernels, readbacks and
//! the GPU's completion included. Frames are made once and warped differently each time (a few pixels of shift, a
//! little scale and rotation), so that no file is read and memory holds one frame.
//!
//! Not a criterion bench: a hundred 24 MP frames take minutes per method on the CPU. `cargo xtask bench` runs it with
//! the others and lists its records (`FLORASTACK_BENCH_METRICS`): seconds, milliseconds a frame, the GPU's speed-up
//! and adapter, and how far the two results are apart. `--quick` stacks 8 frames; `FLORASTACK_BENCH_FRAMES` sets the
//! count; a filter runs the methods whose id contains it. Without a hardware GPU (or the one `WGPU_ADAPTER_NAME`
//! names) it says so and records nothing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Instant;

use florastack_align::{Canvas, warp_to_canvas};
use florastack_core::{Affine, ColorSpace, Image, Rect, Unmonitored};
use florastack_gpu::ops::WarpGeometry;
use florastack_gpu::{Frame, Gpu, GpuOptions, GpuStacked, GpuStacker, GpuTiling, Method as GpuMethod};
use florastack_stack::{
    AverageSettings, DepthSettings, FrameSource, Gained, Method, Patch, PyramidSettings, StackError, StackShape, Stacked, Stacker, Tiling,
};
use rayon::prelude::*;
use serde_json::{Value, json};

const WIDTH: usize = 6000;
const HEIGHT: usize = 4000;

/// A frame warped onto each rectangle as it is read, as the engine's CPU stack reads frames.
struct Warped<'a> {
    frame: &'a Image<f32>,
    transform: Affine,
    canvas: Canvas,
}

impl FrameSource for Warped<'_> {
    fn width(&self) -> usize {
        self.canvas.width
    }

    fn height(&self) -> usize {
        self.canvas.height
    }

    fn channels(&self) -> usize {
        self.frame.channels()
    }

    fn read(&self, rect: Rect) -> Result<Patch, StackError> {
        let covered = warp_to_canvas(self.frame, &self.transform, &self.canvas, rect, 2.0).unwrap();
        Ok(Patch { image: covered.image, coverage: Some(covered.coverage) })
    }
}

/// A textured frame: detail at several scales, some of it sharp and some smooth, colored.
fn frame() -> Image<f32> {
    let mut image = Image::new(WIDTH, HEIGHT, 3, ColorSpace::LINEAR).unwrap();
    image.par_rows_mut().enumerate().for_each(|(y, row)| {
        for (x, pixel) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let h = (x as u64).wrapping_mul(0x9e37_79b9).wrapping_add((y as u64).wrapping_mul(0x85eb_ca6b)) >> 7;
            let grain = (h % 1000) as f32 / 1000.0 - 0.5;
            let (fx, fy) = (x as f32, y as f32);
            let coarse = (fx * 0.004).sin() * (fy * 0.003).cos();
            let fine = (fx * 0.31 + fy * 0.17).sin();
            let sharp = if (x / 400 + y / 400) % 2 == 0 { 0.15 * fine + 0.05 * grain } else { 0.0 };
            for (c, v) in pixel.iter_mut().enumerate() {
                *v = (0.45 + 0.2 * coarse + sharp + 0.05 * c as f32).clamp(0.0, 1.0);
            }
        }
    });
    image
}

/// Frame `i`'s alignment of `n`: shifted by a few pixels, scaled and rotated a little, its gain near 1.
fn alignment(i: usize, n: usize) -> (Affine, f32) {
    let t = i as f64 / n.max(1) as f64;
    let center = (WIDTH as f64 / 2.0, HEIGHT as f64 / 2.0);
    (Affine::similarity(center, 1.0 + 0.004 * t, 0.002 * (t - 0.5), (3.0 * t - 1.5, 2.0 * t - 1.0)), 1.0 + 0.002 * (i % 5) as f32)
}

fn cpu_pass(stacker: &mut dyn Stacker, frame: &Image<f32>, n: usize, canvas: &Canvas) -> Stacked {
    for i in 0..n {
        let (transform, gain) = alignment(i, n);
        let source = Gained { frame: Warped { frame, transform, canvas: *canvas }, gain };
        if i == 0 {
            stacker.begin(StackShape::of(&source)).unwrap();
        }
        stacker.add_frame(&source, &Unmonitored).unwrap();
    }
    stacker.finish(&Unmonitored).unwrap()
}

fn gpu_pass(stacker: &mut GpuStacker, frame: &Image<f32>, n: usize, canvas: &Canvas) -> GpuStacked {
    stacker.begin(canvas.width, canvas.height, frame.channels()).unwrap();
    for i in 0..n {
        let (transform, gain) = alignment(i, n);
        let geometry = WarpGeometry { to_frame: canvas.to_frame(&transform), frame_width: WIDTH, frame_height: HEIGHT, feather: 2.0, gain };
        stacker.add_frame(&Frame::Warp { image: frame, geometry }, &Unmonitored).unwrap();
    }
    stacker.finish(&Unmonitored).unwrap()
}

/// The mean absolute difference between two results.
fn mean_difference(a: &Image<f32>, b: &Image<f32>) -> f64 {
    let sum: f64 = a.rows().flatten().zip(b.rows().flatten()).map(|(u, v)| f64::from((u - v).abs())).sum();
    sum / (a.width() * a.height() * a.channels()) as f64
}

/// The stack of one method on the CPU and on the GPU, with their times in seconds.
fn run(method: &str, gpu: &Gpu, frame: &Image<f32>, n: usize, canvas: &Canvas) -> (f64, f64, f64) {
    let tiling = GpuTiling::default();
    match method {
        "average" => {
            let s = AverageSettings::default();
            let t = Instant::now();
            let cpu = cpu_pass(&mut *s.stacker(&Tiling::default()).unwrap(), frame, n, canvas);
            let cpu_s = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let method = GpuMethod::Average { radius: s.radius, smoothing: s.smoothing, power: s.power };
            let got = gpu_pass(&mut GpuStacker::new(gpu, method, tiling).unwrap(), frame, n, canvas);
            (cpu_s, t.elapsed().as_secs_f64(), mean_difference(&got.image, &cpu.image))
        }
        "pyramid" => {
            let s = PyramidSettings::default();
            let t = Instant::now();
            let cpu = cpu_pass(&mut *s.stacker(&Tiling::default()).unwrap(), frame, n, canvas);
            let cpu_s = t.elapsed().as_secs_f64();
            let t = Instant::now();
            #[allow(clippy::cast_possible_truncation)]
            let method = GpuMethod::Pyramid {
                levels: s.levels,
                blend: Some((1.0 / s.grit) as f32),
                channel_decisions: false,
                range_limit: s.range_limit as f32,
            };
            let got = gpu_pass(&mut GpuStacker::new(gpu, method, tiling).unwrap(), frame, n, canvas);
            (cpu_s, t.elapsed().as_secs_f64(), mean_difference(&got.image, &cpu.image))
        }
        _ => {
            // Both passes, and the depth map between them on the CPU for both.
            let s = DepthSettings::default();
            let t = Instant::now();
            let mut estimator = s.estimator(&Tiling::default()).unwrap();
            for i in 0..n {
                let (transform, gain) = alignment(i, n);
                let source = Gained { frame: Warped { frame, transform, canvas: *canvas }, gain };
                if i == 0 {
                    estimator.begin(StackShape::of(&source)).unwrap();
                }
                estimator.add_frame(&source, &Unmonitored).unwrap();
            }
            let stats = estimator.finish(&Unmonitored).unwrap();
            drop(estimator);
            let map = stats.resolve(s.contrast_threshold(), s.smoothing, None).unwrap();
            drop(stats);
            let cpu = cpu_pass(&mut map.compositor(&Tiling::default()).unwrap(), frame, n, canvas);
            let cpu_s = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let focus = gpu_pass(&mut GpuStacker::new(gpu, GpuMethod::Focus { radius: s.radius }, tiling.clone()).unwrap(), frame, n, canvas);
            let stats = to_stats(&focus, s.radius);
            let gpu_map = stats.resolve(s.contrast_threshold(), s.smoothing, None).unwrap();
            drop(stats);
            let method = GpuMethod::Composite { depth: std::sync::Arc::clone(&gpu_map.depth), frames: gpu_map.frames };
            let got = gpu_pass(&mut GpuStacker::new(gpu, method, tiling).unwrap(), frame, n, canvas);
            (cpu_s, t.elapsed().as_secs_f64(), mean_difference(&got.image, &cpu.image))
        }
    }
}

/// florastack-stack's focus statistics of the GPU's pass 1, as the engine makes them.
fn to_stats(s: &GpuStacked, radius: f64) -> florastack_stack::FocusStats {
    let (w, h) = (s.image.width(), s.image.height());
    let plane = |k: usize| {
        let samples = s.image.rows().flat_map(|row| row.as_chunks::<3>().0.iter().map(move |p| p[k])).collect();
        Image::from_vec(w, h, 1, ColorSpace::LINEAR, samples).unwrap()
    };
    florastack_stack::FocusStats {
        frames: s.frames,
        depth: plane(0),
        confidence: plane(1),
        guide: plane(2),
        covered: s.alpha.clone(),
        uncovered: s.uncovered,
        radius,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let quick = args.iter().any(|a| a == "--quick");
    let filter = args.iter().find(|a| !a.starts_with('-')).cloned();
    let n = std::env::var("FLORASTACK_BENCH_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(if quick { 8 } else { 100 });
    let methods: Vec<&str> = ["average", "pyramid", "depth-map"]
        .into_iter()
        .filter(|m| {
            filter.as_ref().is_none_or(|f| format!("compositors/{m}/cpu").contains(f.as_str()) || format!("compositors/{m}/gpu").contains(f.as_str()))
        })
        .collect();
    if methods.is_empty() {
        return;
    }
    let gpu = match Gpu::blocking(&GpuOptions::hardware()).and_then(|g| g.self_test().map(|()| g)) {
        Ok(gpu) => gpu,
        Err(e) => {
            println!("compositors: skipped, no hardware GPU adapter passed the self-test: {e}");
            return;
        }
    };
    let adapter = gpu.adapter().clone();
    println!("compositors: {n} frames of {WIDTH}x{HEIGHT} RGB, CPU ({} threads) against {adapter}", rayon::current_num_threads());
    let frame = frame();
    let canvas = Canvas::master(WIDTH, HEIGHT);
    let size = format!("{n}x{WIDTH}x{HEIGHT}");
    let mut records = Vec::new();
    for method in methods {
        let (cpu, gpu_s, difference) = run(method, &gpu, &frame, n, &canvas);
        let per_frame = |s: f64| 1000.0 * s / n as f64;
        println!(
            "  {method:<10} CPU {cpu:8.2} s ({:7.1} ms/frame)  GPU {gpu_s:8.2} s ({:7.1} ms/frame)  speed-up {:5.2}x  mean difference {difference:.2e}",
            per_frame(cpu),
            per_frame(gpu_s),
            cpu / gpu_s
        );
        records.push(json!({ "id": format!("compositors/{method}/cpu/{size}"), "seconds": cpu, "per_frame_ms": per_frame(cpu), "frames": n }));
        records.push(json!({
            "id": format!("compositors/{method}/gpu/{size}"),
            "seconds": gpu_s,
            "per_frame_ms": per_frame(gpu_s),
            "frames": n,
            "speedup": cpu / gpu_s,
            "mean_difference": difference,
            "adapter": adapter.name,
            "backend": adapter.backend,
            "budget_mb": GpuTiling::DEFAULT_BUDGET >> 20,
        }));
    }
    if let Some(dir) = std::env::var_os("FLORASTACK_BENCH_METRICS").map(PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        let file: Value = json!({ "bench": "florastack-gpu/compositors", "records": records });
        std::fs::write(dir.join("gpu-compositors.json"), serde_json::to_string_pretty(&file).unwrap()).unwrap();
    }
}
