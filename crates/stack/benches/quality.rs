//! The stacking methods' quality on synthetic stacks whose layers occlude one another, scored against their ground
//! truth: SSIM, halo, flat-region noise, color shift (mean CIEDE2000), PSNR and the share of pixels outside `[0, 1]`,
//! for the weighted average and the pyramid method with its defaults and with each of its weakness settings changed.
//! The scores are computed once and, when `cargo xtask bench` runs this, written as JSON records into the folder that
//! `FLORASTACK_BENCH_METRICS` names, which xtask adds to `target/bench-summary.json` and prints; criterion times one
//! whole stack per method.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use florastack_core::testutil::{Backdrop, StackParams, SyntheticStack, generate_layered, scene};
use florastack_core::{Image, Unmonitored};
use florastack_metrics::{HaloParams, NoiseParams, color_shift, halo, noise, psnr, ssim};
use florastack_stack::{AverageSettings, Decisions, Method, PyramidSettings, Tiling, compress_range, out_of_range, stack};
use serde_json::{Value, json};

/// The environment variable `cargo xtask bench` names its metrics folder in.
const METRICS_DIR: &str = "FLORASTACK_BENCH_METRICS";

fn layered(width: usize, height: usize, scale: f64, backdrop: Backdrop, noisy: bool, seed: u64) -> SyntheticStack {
    let scene = scene::layered(width, height, scale, backdrop, seed).unwrap();
    #[allow(clippy::cast_possible_truncation)]
    let s = scale as f32;
    let mut params = StackParams {
        focus_range: Some((0.0, 1.0)),
        blur_per_depth: 8.0 * s,
        max_blur: 8.0 * s,
        blur_step: 0.5 * s,
        seed,
        ..StackParams::identity(12)
    };
    if noisy {
        params.read_noise = 0.005;
        params.shot_noise = 0.01;
    }
    generate_layered(&scene, &params).unwrap()
}

/// A method's output as the engine makes it: the stack, brought into range for the pyramid.
fn output(method: &Variant, frames: &[Image<f32>]) -> Image<f32> {
    match method {
        Variant::Average(settings) => {
            let mut stacker = settings.stacker(&Tiling::default()).unwrap();
            stack(&mut stacker, frames, &Unmonitored).unwrap().image
        }
        Variant::Pyramid(settings) => {
            let mut stacker = settings.stacker(&Tiling::default()).unwrap();
            let mut image = stack(&mut stacker, frames, &Unmonitored).unwrap().image;
            compress_range(&mut image, settings.tone).unwrap();
            image
        }
    }
}

enum Variant {
    Average(AverageSettings),
    Pyramid(PyramidSettings),
}

fn record(id: &str, image: &Image<f32>, truth: &Image<f32>) -> Value {
    json!({
        "id": id,
        "ssim": ssim(image, truth).unwrap(),
        "halo": halo(image, truth, &HaloParams::default()).unwrap().relative,
        "noise": noise(image, truth, &NoiseParams::default()).map_or(Value::Null, |n| json!(n.sigma)),
        "delta_e": color_shift(image, truth).unwrap().mean,
        "psnr": psnr(image, truth).unwrap(),
        "out_of_range": out_of_range(image) as f64 / (image.width() * image.height()) as f64,
    })
}

/// Every method and variant on every scene, as metric records.
fn metrics() -> Vec<Value> {
    let scenes = [
        ("layered-textured-720x540-clean", 720, 540, 3.0, Backdrop::Textured, false),
        ("layered-textured-720x540-noisy", 720, 540, 3.0, Backdrop::Textured, true),
        ("layered-smooth-720x540-noisy", 720, 540, 3.0, Backdrop::SmoothRight, true),
        ("layered-textured-320x240-noisy", 320, 240, 1.0, Backdrop::Textured, true),
    ];
    let mut out = Vec::new();
    for (name, w, h, scale, backdrop, noisy) in scenes {
        let synthetic = layered(w, h, scale, backdrop, noisy, 1);
        let truth = &synthetic.truth.sharp;
        let average = if scale >= 3.0 { AverageSettings::default() } else { AverageSettings { radius: 1.0, smoothing: 1.0, power: 3.0 } };
        let defaults = PyramidSettings::default();
        let variants = [
            ("average", Variant::Average(average)),
            ("pyramid", Variant::Pyramid(defaults.clone())),
            ("pyramid-grit-0", Variant::Pyramid(PyramidSettings { grit: 0.0, ..defaults.clone() })),
            ("pyramid-no-range-limit", Variant::Pyramid(PyramidSettings { range_limit: 0.0, ..defaults.clone() })),
            ("pyramid-channel-decisions", Variant::Pyramid(PyramidSettings { decisions: Decisions::Channels, ..defaults.clone() })),
            ("pyramid-coarse-limit-1", Variant::Pyramid(PyramidSettings { coarse_limit: Some(1), ..defaults.clone() })),
        ];
        let best = synthetic.frames.iter().map(|f| ssim(f, truth).unwrap()).fold(0.0, f64::max);
        out.push(json!({ "id": format!("stack/quality/{name}/best-frame"), "ssim": best }));
        for (method, variant) in &variants {
            out.push(record(&format!("stack/quality/{name}/{method}"), &output(variant, &synthetic.frames), truth));
        }
    }
    out
}

fn quality(c: &mut Criterion) {
    let synthetic = layered(720, 540, 3.0, Backdrop::Textured, true, 1);
    let mut group = c.benchmark_group("stack-quality");
    group.sampling_mode(SamplingMode::Flat).sample_size(10).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(12 * 720 * 540));
    let mut written = false;
    group.bench_function(BenchmarkId::new("pyramid", "12x720x540"), |b| {
        if !written {
            let records = metrics();
            for r in &records {
                println!("{r}");
            }
            if let Some(dir) = std::env::var_os(METRICS_DIR).map(PathBuf::from) {
                std::fs::create_dir_all(&dir).unwrap();
                let file = json!({ "source": "florastack-stack/benches/quality.rs", "records": records });
                std::fs::write(dir.join("florastack-stack.json"), serde_json::to_string_pretty(&file).unwrap() + "\n").unwrap();
            }
            written = true;
        }
        b.iter(|| output(&Variant::Pyramid(PyramidSettings::default()), &synthetic.frames));
    });
    group.bench_function(BenchmarkId::new("average", "12x720x540"), |b| {
        b.iter(|| output(&Variant::Average(AverageSettings::default()), &synthetic.frames));
    });
    group.finish();
}

criterion_group!(benches, quality);
criterion_main!(benches);
