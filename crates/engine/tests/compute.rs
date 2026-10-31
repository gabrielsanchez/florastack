//! Stacking on the GPU through the commands: `compute.set` chooses the device and `compute.info` says which stacks
//! and why; every stacking command (the three methods, the preview and slabs) gives on the GPU what it gives on the
//! CPU, within the tolerances florastack-gpu's agreement tests document (`crates/gpu/tests/stackers.rs`), and the
//! settings the GPU lacks stack on the CPU.
//!
//! The GPU is any adapter that passes the self-test, software ones included (`compute.set` with `gpu`). Without one,
//! the tests record a skip, unless `FLORASTACK_REQUIRE_GPU` is set, as CI sets it where a software adapter is
//! installed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use common::{MARGIN, TempDir, config, path_list, run, scene_frames};
use florastack_core::testutil::{StackParams, scene};
use florastack_core::{AnyImage, Image};
use florastack_engine::{Compute, Device, Engine, Value};

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

/// An engine stacking on `device`, with a new aligned project in `dir` holding `files`.
fn aligned(dir: &Path, files: &[PathBuf], device: Device) -> Engine {
    std::fs::create_dir_all(dir).unwrap();
    let mut engine = Engine::new(florastack_engine::EngineConfig { compute: Compute { device, ..Compute::default() }, ..config(dir) });
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Stack", "folder": {} }}"#, quoted(dir)));
    run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(files)));
    run(&mut engine, "align.run", "{}");
    engine
}

/// Whether `engine` stacks on the GPU; records a skip when it can't (see the module documentation).
fn on_gpu(engine: &mut Engine) -> bool {
    let info = run(engine, "compute.info", "{}");
    if info["device"] == "gpu" {
        println!("stacking on {}", info["adapter"]);
        return true;
    }
    let reason = info["reason"].as_str().unwrap_or_default().to_owned();
    let required = std::env::var_os("FLORASTACK_REQUIRE_GPU").is_some_and(|v| !v.is_empty());
    assert!(!required, "FLORASTACK_REQUIRE_GPU is set, but the engine stacks on the CPU: {reason}");
    florastack_testkit::skip!(Gpu, "no GPU adapter passed the self-test: {reason}");
    false
}

/// The pixels of output `id`.
fn output_image(engine: &Engine, id: &Value) -> Image<f32> {
    let project = engine.project().unwrap();
    let output = project.outputs.iter().find(|o| Some(o.id().get()) == id.as_u64()).unwrap();
    let path = output.image.path.resolve(engine.project_path().unwrap());
    let options = florastack_codecs::ReadOptions { orientation: florastack_codecs::OrientationPolicy::Ignore, ..Default::default() };
    match florastack_codecs::read_file(&path, &options).unwrap().image {
        AnyImage::F32(image) => image,
        other => panic!("an output is stored as f32, not {:?}", other.format()),
    }
}

/// The mean difference between two images and the share of samples more than `tolerance` apart.
fn apart(a: &Image<f32>, b: &Image<f32>, tolerance: f32) -> (f64, f64) {
    assert_eq!((a.width(), a.height(), a.channels()), (b.width(), b.height(), b.channels()));
    let (mut sum, mut over, mut n) = (0.0f64, 0usize, 0usize);
    for (u, v) in a.rows().flatten().zip(b.rows().flatten()) {
        let d = (u - v).abs();
        sum += f64::from(d);
        over += usize::from(d > tolerance);
        n += 1;
    }
    (sum / n as f64, over as f64 / n as f64)
}

/// Eight frames of a textured scene focused through it, breathing and shifting a little.
fn files(dir: &Path) -> Vec<PathBuf> {
    let (sharp, depth) = scene::textured(160 + 2 * MARGIN, 120 + 2 * MARGIN, 33).unwrap();
    let params = StackParams {
        blur_per_depth: 6.0,
        max_blur: 6.0,
        breathing: 0.01,
        max_shift: 1.5,
        max_rotation_deg: 0.5,
        seed: 33,
        ..StackParams::identity(8)
    };
    scene_frames(dir, "compute", &sharp, &depth, &params)
}

#[test]
fn every_stacking_command_gives_on_the_gpu_what_it_gives_on_the_cpu() {
    let dir = TempDir::new("compute-agree");
    let files = files(&dir.path().join("frames"));
    let mut gpu = aligned(&dir.path().join("gpu"), &files, Device::Gpu);
    if !on_gpu(&mut gpu) {
        return;
    }
    let mut cpu = aligned(&dir.path().join("cpu"), &files, Device::Cpu);
    assert_eq!(run(&mut cpu, "compute.info", "{}")["device"], "cpu");
    let commands = [
        ("stack.average", r#"{ "radius": 1.0, "smoothing": 2.0 }"#),
        ("stack.average", "{}"),
        ("stack.pyramid", "{}"),
        ("stack.pyramid", r#"{ "grit": 0, "decisions": "channels" }"#),
        ("stack.depthMap", r#"{ "radius": 1.5, "smoothing": 4.0 }"#),
        ("stack.slabbed", r#"{ "slabSize": 4, "overlap": 1, "final": { "method": "depth-map" } }"#),
    ];
    for (command, params) in commands {
        let (stacked, reference) = (run(&mut gpu, command, params), run(&mut cpu, command, params));
        assert_eq!((&stacked["width"], &stacked["height"]), (&reference["width"], &reference["height"]), "{command} {params}");
        let (g, c) = (output_image(&gpu, &stacked["output"]), output_image(&cpu, &reference["output"]));
        // The agreement tests' bounds: the weighted average and the composite within 1e-4 but at coverage edges, the
        // pyramid's and the depth map's choices flipping where frames tie to the last bits; a few hundredths of a
        // percent of the samples there, a tenth of a percent at most here.
        let (mean, over) = apart(&g, &c, 1e-3);
        println!("{command} {params}: mean {mean:.2e}, beyond 1e-3 {:.4}%", 100.0 * over);
        assert!(mean < 1e-4 && over < 2e-3, "{command} {params}: mean {mean}, over {over}");
    }
    // The preview: a quarter-size stack, written as a file.
    for method in ["average", "pyramid", "depth-map"] {
        let params = format!(r#"{{ "method": "{method}" }}"#);
        let read = |engine: &mut Engine| {
            let previewed = run(engine, "stack.preview", &params);
            match florastack_codecs::read_file(Path::new(previewed["path"].as_str().unwrap()), &florastack_codecs::ReadOptions::default())
                .unwrap()
                .image
            {
                AnyImage::F32(image) => image,
                other => panic!("a preview is f32, not {:?}", other.format()),
            }
        };
        let (g, c) = (read(&mut gpu), read(&mut cpu));
        let (mean, over) = apart(&g, &c, 1e-3);
        assert!(mean < 1e-4 && over < 5e-3, "preview {method}: mean {mean}, over {over}");
    }
}

#[test]
fn the_preference_chooses_the_device_and_the_settings_the_gpu_lacks_stack_on_the_cpu() {
    let dir = TempDir::new("compute-preference");
    let files = files(&dir.path().join("frames"));
    let mut engine = aligned(&dir.path().join("project"), &files, Device::Gpu);
    if !on_gpu(&mut engine) {
        return;
    }
    let mut cpu = aligned(&dir.path().join("cpu"), &files, Device::Cpu);
    // The coarse limit isn't ported: the GPU engine stacks it on the CPU, so the outputs are the same bit for bit.
    let params = r#"{ "coarseLimit": 2 }"#;
    let (a, b) = (run(&mut engine, "stack.pyramid", params), run(&mut cpu, "stack.pyramid", params));
    assert_eq!(output_image(&engine, &a["output"]), output_image(&cpu, &b["output"]));
    // `compute.set` to the CPU: the next stacks are the CPU's, bit for bit.
    let info = run(&mut engine, "compute.set", r#"{ "device": "cpu" }"#);
    assert_eq!((&info["preference"], &info["device"]), (&Value::from("cpu"), &Value::from("cpu")), "{info}");
    let (a, b) = (run(&mut engine, "stack.average", "{}"), run(&mut cpu, "stack.average", "{}"));
    assert_eq!(output_image(&engine, &a["output"]), output_image(&cpu, &b["output"]));
    // And back to the GPU, with a budget of its accumulators.
    let info = run(&mut engine, "compute.set", r#"{ "device": "gpu", "budgetMb": 64 }"#);
    assert_eq!((&info["device"], &info["budgetMb"]), (&Value::from("gpu"), &Value::from(64)), "{info}");
    assert!(info["adapter"]["name"].as_str().is_some_and(|n| !n.is_empty()), "{info}");
}
