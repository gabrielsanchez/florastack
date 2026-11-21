//! When the GPU fails part way through a stack, the stack starts again on the CPU and gives exactly the CPU's result
//! (`commands::stack`'s `stack_feed`). The failure is injected (florastack-gpu's `testutil::fail_after`), at checks
//! of the device from the first to well into the stack, so that it lands in every phase: beginning, the frames, each
//! pass of the depth map, the end. The fault is the process's, so this test has a binary of its own.
//!
//! The GPU is any adapter that passes the self-test, software ones included. Without one, the test records a skip,
//! unless `FLORASTACK_REQUIRE_GPU` is set.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use common::{MARGIN, TempDir, config, path_list, run, scene_frames};
use florastack_core::testutil::{StackParams, scene};
use florastack_core::{AnyImage, Image};
use florastack_engine::{Compute, Device, Engine, Value};
use florastack_gpu::testutil;

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

#[test]
fn a_gpu_that_fails_part_way_leaves_the_stack_to_the_cpu() {
    let dir = TempDir::new("gpu-fallback");
    let (sharp, depth) = scene::textured(160 + 2 * MARGIN, 120 + 2 * MARGIN, 34).unwrap();
    let params = StackParams { blur_per_depth: 6.0, max_blur: 6.0, breathing: 0.01, max_shift: 1.5, seed: 34, ..StackParams::identity(6) };
    let files = scene_frames(dir.path(), "frames", &sharp, &depth, &params);
    let mut gpu = aligned(&dir.path().join("gpu"), &files, Device::Gpu);
    let info = run(&mut gpu, "compute.info", "{}");
    if info["device"] != "gpu" {
        let reason = info["reason"].as_str().unwrap_or_default().to_owned();
        assert!(std::env::var_os("FLORASTACK_REQUIRE_GPU").is_none_or(|v| v.is_empty()), "no GPU: {reason}");
        florastack_testkit::skip!(Gpu, "no GPU adapter passed the self-test: {reason}");
        return;
    }
    println!("stacking on {}", info["adapter"]);
    let mut cpu = aligned(&dir.path().join("cpu"), &files, Device::Cpu);
    for (command, params) in
        [("stack.average", "{}"), ("stack.pyramid", "{}"), ("stack.depthMap", "{}"), ("stack.pyramid", r#"{ "levels": 5, "coarseLimit": 1 }"#)]
    {
        let made = run(&mut cpu, command, params);
        let expected = output_image(&cpu, &made["output"]);
        let mut landed = 0;
        for checks in [0, 1, 2, 4, 8, 16, 32, 64] {
            let before = testutil::fired();
            testutil::fail_after(checks);
            let stacked = run(&mut gpu, command, params);
            testutil::disarm();
            if testutil::fired() == before {
                // The stack needed fewer checks: it ended on the GPU.
                continue;
            }
            landed += 1;
            let got = output_image(&gpu, &stacked["output"]);
            assert!(got.as_raw() == expected.as_raw(), "{command} {params}, failing after {checks} checks: not the CPU's stack");
        }
        println!("{command} {params}: {landed} of 8 faults landed");
        assert!(landed >= 3, "{command} {params}: only {landed} faults landed in the stack");
        // A fault spent, the GPU stacks again.
        assert_eq!(run(&mut gpu, "compute.info", "{}")["device"], "gpu");
    }
}
