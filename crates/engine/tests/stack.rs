//! The stacking commands on synthetic stacks written as frame files: `stack.average` stacks the aligned frames into a
//! new output, sharper than any frame, as one undo step; `stack.pyramid` does better on a scene whose layers occlude
//! one another, and keeps its unclipped result for extended-range files when tone compression changes it; wide framing
//! leaves the pixels no frame covers transparent; `stack.depthMap` makes an output with its depth and confidence maps,
//! which `output.save` writes as data; `stack.preview` makes a quarter-size preview of any method without touching the
//! project; and stacks without a current alignment, or cancelled, change nothing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use common::{MARGIN, TempDir, breathing, config, path_list, run, scene_frames, state, synthetic_frames};
use florastack_codecs::{BitDepth, OutputFormat, OutputMetadata, OutputProfile, OutputSpec, TiffCompression};
use florastack_core::testutil::{Backdrop, StackParams, generate, generate_layered, scene};
use florastack_core::{AnyImage, ColorSpace, Image, Rect};
use florastack_engine::model::OutputId;
use florastack_engine::{CommandError, Engine, json};
use florastack_metrics::ssim;

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

/// An engine with a new project in `dir` holding `files`, in that order; and the frames' ids.
fn project_with(dir: &Path, files: &[PathBuf]) -> (Engine, Vec<u64>) {
    std::fs::create_dir_all(dir).unwrap();
    let mut engine = Engine::new(config(dir));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Stack", "folder": {} }}"#, quoted(dir)));
    let added = run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(files)));
    let ids: Vec<u64> = added["added"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert_eq!(ids.len(), files.len(), "{added}");
    (engine, ids)
}

/// The pixels of the project's output `id`, as linear `f32`.
fn output_image(engine: &Engine, id: u64) -> Image<f32> {
    let project = engine.project().unwrap();
    let output = project.outputs.iter().find(|o| o.id().get() == id).unwrap();
    let path = output.image.path.resolve(engine.project_path().unwrap());
    let options = florastack_codecs::ReadOptions { orientation: florastack_codecs::OrientationPolicy::Ignore, ..Default::default() };
    match florastack_codecs::read_file(&path, &options).unwrap().image {
        AnyImage::F32(image) => image,
        other => panic!("an output is stored as f32, not {:?}", other.format()),
    }
}

#[test]
fn a_weighted_average_beats_the_sharpest_frame_and_becomes_an_output() {
    let dir = TempDir::new("stack-average");
    // Eight frames focused through the scene, not moved: the master's grid is the scene's, cropped by the margin.
    let (sharp, depth) = scene::textured(200 + 2 * MARGIN, 150 + 2 * MARGIN, 21).unwrap();
    let params = StackParams { blur_per_depth: 8.0, max_blur: 8.0, seed: 21, ..StackParams::identity(8) };
    let files = scene_frames(dir.path(), "stack", &sharp, &depth, &params);
    let inner = Rect::new(MARGIN, MARGIN, 200, 150);
    let truth = sharp.crop(inner).unwrap();
    let frames = generate(&sharp, &depth, &params).unwrap().frames;
    let best = frames.iter().map(|f| ssim(&f.crop(inner).unwrap(), &truth).unwrap()).fold(0.0, f64::max);
    let (mut engine, ids) = project_with(dir.path(), &files);
    run(&mut engine, "align.run", "{}");
    let before = state(&engine);
    let steps = run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len();

    // Settings for the synthetic scene's small detail and its blur without occlusion (the defaults, for camera frames,
    // measure sharpness over 3 px and smooth over 8), given for this stack only.
    let stacked = run(&mut engine, "stack.average", r#"{ "name": "Average", "radius": 1.0, "smoothing": 1.0 }"#);
    println!("{stacked}");
    assert_eq!((stacked["width"].as_u64(), stacked["height"].as_u64(), stacked["uncovered"].as_u64()), (Some(200), Some(150), Some(0)));
    let mut used: Vec<u64> = stacked["frames"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    used.sort_unstable();
    assert_eq!(used, ids);
    let output = stacked["output"].as_u64().unwrap();
    let image = output_image(&engine, output);
    assert_eq!((image.width(), image.height(), image.channels()), (200, 150, 3), "no alpha: every frame covers the canvas");
    let score = ssim(&image, &truth).unwrap();
    println!("stack: SSIM {score:.4} against the best frame's {best:.4}");
    assert!(score > best + 0.03, "{score:.4} against {best:.4}");

    // The output is listed with its method, parameters and frames, and is one undo step.
    let listed = run(&mut engine, "output.list", "{}");
    let entry = &listed["outputs"][0];
    assert_eq!((&entry["name"], &entry["method"], &entry["preview"]), (&json!("Average"), &json!("average"), &json!(true)));
    assert_eq!(entry["parameters"], json!({ "radius": 1.0, "smoothing": 1.0, "power": 3.0, "framing": "narrow" }));
    assert_eq!(entry["frames"].as_array().unwrap().len(), 8);
    let history = run(&mut engine, "edit.history", "{}");
    assert_eq!(history["undo"].as_array().unwrap().len(), steps + 1);
    assert_eq!(run(&mut engine, "edit.undo", "{}")["label"], "Stack by weighted average");
    assert_eq!(state(&engine), before);
    run(&mut engine, "edit.redo", "{}");
    assert_eq!(engine.project().unwrap().outputs.len(), 1);

    // Settings given for one stack apply to it alone; a disabled frame is left out.
    run(&mut engine, "project.setFramesEnabled", &format!(r#"{{ "frames": [{}], "enabled": false }}"#, ids[0]));
    let smooth = run(&mut engine, "stack.average", r#"{ "smoothing": 2.0 }"#);
    assert_eq!((&smooth["settings"]["smoothing"], &smooth["settings"]["radius"]), (&json!(2.0), &json!(3.0)));
    assert_eq!(smooth["frames"].as_array().unwrap().len(), 7);
    assert_eq!(run(&mut engine, "stack.get", "{}")["settings"]["average"]["smoothing"], 8.0, "the project's settings are unchanged");
    let names: Vec<String> = engine.project().unwrap().outputs.iter().map(|o| o.name.clone()).collect();
    assert_eq!(names, ["Average", "Weighted average"]);
    let _: OutputId = engine.project().unwrap().outputs[1].id();
}

#[test]
#[allow(clippy::float_cmp)] // Alpha is exactly 0 or 1, and uncovered pixels exactly 0.
fn wide_framing_leaves_the_pixels_no_frame_covers_transparent() {
    let dir = TempDir::new("stack-wide");
    // Six frames, each 2% larger than the one before, shifted and turned a little: the wide canvas's corners are seen
    // by no frame.
    let files = synthetic_frames(dir.path(), "stack", 160, 120, &breathing(6, 0.02, None, 22));
    let (mut engine, _) = project_with(dir.path(), &files);
    run(&mut engine, "align.set", r#"{ "framing": "wide" }"#);
    run(&mut engine, "align.run", "{}");
    let stacked = run(&mut engine, "stack.average", "{}");
    let uncovered = stacked["uncovered"].as_u64().unwrap();
    let image = output_image(&engine, stacked["output"].as_u64().unwrap());
    let (w, h) = (usize::try_from(stacked["width"].as_u64().unwrap()).unwrap(), usize::try_from(stacked["height"].as_u64().unwrap()).unwrap());
    println!("{stacked}");
    assert!(w > 160 && h > 120, "{stacked}");
    assert!(uncovered > 0, "the wide canvas's corners: {stacked}");
    assert_eq!(image.channels(), 4, "an alpha channel");
    let (pixels, _) = image.as_raw().as_chunks::<4>();
    let transparent: Vec<&[f32; 4]> = pixels.iter().filter(|p| p[3] == 0.0).collect();
    assert_eq!(transparent.len() as u64, uncovered);
    assert!(transparent.iter().all(|p| p[..3] == [0.0; 3]), "uncovered pixels are 0");
    assert!(pixels.iter().all(|p| p[3] == 0.0 || p[3] == 1.0), "alpha is 0 or 1");
    assert!(image.as_raw().iter().all(|v| v.is_finite()));
    // The center is covered by every frame, and is the scene, not black.
    let center = image.pixel(w / 2, h / 2).unwrap();
    assert!(center[..3].iter().all(|&v| v > 0.01), "{center:?}");
}

#[test]
fn the_preview_is_a_quarter_of_the_stack_and_changes_nothing() {
    let dir = TempDir::new("stack-preview");
    let files = synthetic_frames(dir.path(), "stack", 160, 120, &breathing(5, 0.01, None, 23));
    let (mut engine, _) = project_with(dir.path(), &files);
    run(&mut engine, "align.run", "{}");
    let before = state(&engine);
    let steps = run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len();
    let previewed = run(&mut engine, "stack.preview", r#"{ "smoothing": 2.0 }"#);
    println!("{previewed}");
    assert_eq!((previewed["width"].as_u64(), previewed["height"].as_u64()), (Some(40), Some(30)), "{previewed}");
    assert_eq!(previewed["settings"]["smoothing"], 2.0, "the settings at full scale");
    let path = PathBuf::from(previewed["path"].as_str().unwrap());
    let decoded = florastack_codecs::read_file(&path, &florastack_codecs::ReadOptions::default()).unwrap();
    assert_eq!((decoded.image.width(), decoded.image.height()), (40, 30));
    assert_eq!(state(&engine), before, "a preview changes nothing in the project");
    assert_eq!(run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len(), steps);
}

#[test]
fn stacking_needs_a_current_alignment_and_cancelling_changes_nothing() {
    let dir = TempDir::new("stack-stale");
    let files = synthetic_frames(dir.path(), "stack", 160, 120, &breathing(5, 0.01, None, 24));
    let (mut engine, _) = project_with(dir.path(), &files[..4]);
    for name in ["stack.average", "stack.pyramid", "stack.depthMap", "stack.preview"] {
        let e = engine.dispatch_json(name, "{}").unwrap_err();
        assert_eq!(e.code(), "unavailable", "{name}: {e}");
        assert!(e.to_string().contains("align.run"), "{e}");
    }
    run(&mut engine, "align.run", "{}");
    run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, quoted(&files[4])));
    let e = engine.dispatch_json("stack.average", "{}").unwrap_err();
    assert!(e.to_string().contains("since the stack was aligned"), "{e}");
    run(&mut engine, "align.run", "{}");

    let before = state(&engine);
    for name in ["stack.average", "stack.pyramid", "stack.depthMap", "stack.preview"] {
        let job = common::job_of(engine.dispatch_json(name, "{}").unwrap());
        // Cancelling wins over a job that finishes anyway: its commit is dropped.
        assert!(engine.cancel(job));
        assert!(matches!(engine.wait(job), Err(CommandError::Cancelled)), "{name}");
        assert_eq!(state(&engine), before, "{name}");
    }
}

#[test]
fn settings_merge_are_checked_and_go_into_presets() {
    let dir = TempDir::new("stack-settings");
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Settings", "folder": {} }}"#, quoted(dir.path())));
    let pyramid = json!({ "levels": 0, "grit": 0.5, "decisions": "luminance", "range_limit": 1.0, "tone": "compress" });
    let depth_map = json!({ "radius": 2.0, "smoothing": 8.0, "threshold": "auto", "level": 0.001, "percentile": 30.0 });
    assert_eq!(
        run(&mut engine, "stack.get", "{}")["settings"],
        json!({ "average": { "radius": 3.0, "smoothing": 8.0, "power": 3.0 }, "pyramid": pyramid, "depth_map": depth_map })
    );
    run(&mut engine, "stack.set", r#"{ "average": { "radius": 3.0 } }"#);
    let s = run(&mut engine, "stack.set", r#"{ "average": { "power": 1.5 } }"#)["settings"].clone();
    assert_eq!(s, json!({ "average": { "radius": 3.0, "smoothing": 8.0, "power": 1.5 }, "pyramid": pyramid, "depth_map": depth_map }));
    let s = run(&mut engine, "stack.set", r#"{ "depth_map": { "threshold": "level", "level": 0.003 } }"#)["settings"].clone();
    assert_eq!((&s["depth_map"]["threshold"], &s["depth_map"]["level"], &s["depth_map"]["radius"]), (&json!("level"), &json!(0.003), &json!(2.0)));
    // Regression: the threshold changes kind over the stored one, key by key, as presets layer.
    let s = run(&mut engine, "stack.set", r#"{ "depth_map": { "threshold": "percentile", "percentile": 60 } }"#)["settings"].clone();
    assert_eq!(
        (&s["depth_map"]["threshold"], &s["depth_map"]["percentile"], &s["depth_map"]["level"]),
        (&json!("percentile"), &json!(60.0), &json!(0.003))
    );
    let s = run(&mut engine, "stack.set", r#"{ "depth_map": { "threshold": "auto" } }"#)["settings"].clone();
    assert_eq!(s["depth_map"]["threshold"], "auto");
    let s = run(&mut engine, "stack.set", r#"{ "pyramid": { "grit": 0.25, "coarse_limit": 2 } }"#)["settings"].clone();
    assert_eq!((&s["pyramid"]["grit"], &s["pyramid"]["coarse_limit"], &s["average"]["power"]), (&json!(0.25), &json!(2), &json!(1.5)));
    for bad in [
        r#"{ "average": { "radius": -1 } }"#,
        r#"{ "average": { "power": 9 } }"#,
        r#"{ "average": { "radious": 1 } }"#,
        r#"{ "pyramid": { "grit": 0.1 } }"#,
        r#"{ "pyramid": { "grits": 0.5 } }"#,
        r#"{ "depthmap": {} }"#,
        r#"{ "depth_map": { "percentile": 101 } }"#,
        r#"{ "depth_map": { "threshold": { "level": 1 } } }"#,
        r#"{ "depth_map": { "level": -1 } }"#,
        r#"{ "depth_map": { "smoothing": -2 } }"#,
    ] {
        assert_eq!(engine.dispatch_json("stack.set", bad).unwrap_err().code(), "params", "{bad}");
    }
    // A preset with stack settings is checked and applied like the other groups.
    let preset = dir.path().join("smooth.toml");
    std::fs::write(&preset, "[stack.average]\nsmoothing = 4.0\n").unwrap();
    assert_eq!(run(&mut engine, "preset.apply", &format!(r#"{{ "path": {} }}"#, quoted(&preset)))["groups"], json!(["stack"]));
    assert_eq!(run(&mut engine, "stack.get", "{}")["settings"]["average"]["smoothing"], 4.0);
    std::fs::write(&preset, "[stack.average]\npower = 40.0\n").unwrap();
    assert_eq!(engine.dispatch_json("preset.apply", &format!(r#"{{ "path": {} }}"#, quoted(&preset))).unwrap_err().code(), "params");
    assert_eq!(run(&mut engine, "stack.reset", "{}")["settings"], serde_json::to_value(florastack_engine::StackSettings::default()).unwrap());
}

/// A layered scene's frames (`florastack_core::testutil::layered`: bristles over discs over a backdrop, their
/// out-of-focus edges veiling what lies behind them), not moved, `scale` times the frames' given size of detail, and
/// the sharp truth, cropped by `MARGIN` like the other synthetic frames.
fn layered_frames(width: usize, height: usize, scale: f64, frames: usize, seed: u64, brightness: f32) -> (Vec<Image<f32>>, Image<f32>) {
    let scene = scene::layered(width + 2 * MARGIN, height + 2 * MARGIN, scale, Backdrop::Textured, seed).unwrap();
    #[allow(clippy::cast_possible_truncation)]
    let s = scale as f32;
    let params = StackParams {
        focus_range: Some((0.0, 1.0)),
        blur_per_depth: 8.0 * s,
        max_blur: 8.0 * s,
        blur_step: 0.5 * s,
        seed,
        ..StackParams::identity(frames)
    };
    let stack = generate_layered(&scene, &params).unwrap();
    let inner = Rect::new(MARGIN, MARGIN, width, height);
    let bright = |image: &Image<f32>| {
        let cropped = image.crop(inner).unwrap();
        Image::from_vec(width, height, 3, ColorSpace::LINEAR, cropped.as_raw().iter().map(|v| v * brightness).collect()).unwrap()
    };
    (stack.frames.iter().map(bright).collect(), bright(&stack.truth.sharp))
}

#[test]
fn a_pyramid_stack_is_sharper_than_the_average_on_occluding_layers_and_becomes_an_output() {
    let dir = TempDir::new("stack-pyramid");
    let (frames, truth) = layered_frames(240, 180, 1.0, 10, 31, 1.0);
    let files = common::write_frames(dir.path(), "stack", &frames);
    let (mut engine, ids) = project_with(dir.path(), &files);
    run(&mut engine, "align.run", "{}");
    let before = state(&engine);
    let steps = run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len();
    // Settings for the scene's own scale, for both methods.
    let average = run(&mut engine, "stack.average", r#"{ "radius": 1.0, "smoothing": 1.0 }"#);
    let pyramid = run(&mut engine, "stack.pyramid", r#"{ "name": "Pyramid" }"#);
    println!("{pyramid}");
    let score = |summary: &serde_json::Value| ssim(&output_image(&engine, summary["output"].as_u64().unwrap()), &truth).unwrap();
    let (a, p) = (score(&average), score(&pyramid));
    println!("SSIM: weighted average {a:.4}, pyramid {p:.4}");
    assert!(p > a + 0.005, "{p:.4} against {a:.4}");
    assert_eq!((pyramid["width"].as_u64(), pyramid["height"].as_u64(), pyramid["uncovered"].as_u64()), (Some(240), Some(180), Some(0)));
    assert_eq!(pyramid["levels"], 3, "log2 of the short side, less 4");
    assert_eq!(pyramid["range"], json!({ "outOfRange": 0, "extended": false }), "frames in range stack in range");
    assert_eq!(pyramid["settings"]["grit"], 0.5);
    assert!(average.get("range").is_none() && average.get("levels").is_none(), "{average}");
    let mut used: Vec<u64> = pyramid["frames"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    used.sort_unstable();
    assert_eq!(used, ids);

    // Listed with its method and parameters, each stack one undo step.
    let listed = run(&mut engine, "output.list", "{}");
    let entry = &listed["outputs"][1];
    assert_eq!((&entry["name"], &entry["method"]), (&json!("Pyramid"), &json!("pyramid")));
    assert_eq!(
        entry["parameters"],
        json!({ "levels": 3, "grit": 0.5, "decisions": "luminance", "rangeLimit": 1.0, "tone": "compress", "coarseLimit": null, "framing": "narrow" })
    );
    assert_eq!(run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len(), steps + 2);
    assert_eq!(run(&mut engine, "edit.undo", "{}")["label"], "Stack by pyramid");
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(state(&engine), before);

    // Settings for one stack apply to it alone, and are checked.
    let picked = run(&mut engine, "stack.pyramid", r#"{ "grit": 0, "decisions": "channels", "coarseLimit": 1 }"#);
    assert_eq!(
        (&picked["settings"]["grit"], &picked["settings"]["decisions"], &picked["settings"]["coarse_limit"]),
        (&json!(0.0), &json!("channels"), &json!(1))
    );
    assert_eq!(run(&mut engine, "stack.get", "{}")["settings"]["pyramid"]["grit"], 0.5);
    run(&mut engine, "stack.set", r#"{ "pyramid": { "coarse_limit": 3 } }"#);
    let off = run(&mut engine, "stack.pyramid", r#"{ "coarseLimit": null }"#);
    assert!(off["settings"].get("coarse_limit").is_none(), "null turns the project's coarse limit off for this stack: {off}");
    for bad in [r#"{ "grit": 0.1 }"#, r#"{ "rangeLimit": 2 }"#, r#"{ "levels": 11 }"#, r#"{ "tone": "squash" }"#, r#"{ "radius": 1 }"#] {
        assert_eq!(engine.dispatch_json("stack.pyramid", bad).unwrap_err().code(), "params", "{bad}");
    }
}

#[test]
fn a_depth_map_stack_becomes_an_output_with_its_depth_and_confidence_maps() {
    let dir = TempDir::new("stack-depth");
    let (frames, truth) = layered_frames(240, 180, 1.0, 10, 31, 1.0);
    let files = common::write_frames(dir.path(), "stack", &frames);
    let (mut engine, ids) = project_with(dir.path(), &files);
    run(&mut engine, "align.run", "{}");
    let before = state(&engine);
    let steps = run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len();
    // Settings for the scene's own scale, for both methods: a third of the depth map's defaults for camera frames.
    let average = run(&mut engine, "stack.average", r#"{ "radius": 1.0, "smoothing": 1.0 }"#);
    let depth = run(&mut engine, "stack.depthMap", r#"{ "name": "Depth", "radius": 0.7, "smoothing": 2.7 }"#);
    println!("{depth}");
    let score = |summary: &serde_json::Value| ssim(&output_image(&engine, summary["output"].as_u64().unwrap()), &truth).unwrap();
    let (a, d) = (score(&average), score(&depth));
    println!("SSIM: weighted average {a:.4}, depth map {d:.4}");
    assert!(d > a, "{d:.4} against {a:.4}");
    assert_eq!((depth["width"].as_u64(), depth["height"].as_u64(), depth["uncovered"].as_u64()), (Some(240), Some(180), Some(0)));
    assert_eq!(depth["settings"], json!({ "radius": 0.7, "smoothing": 2.7, "threshold": "auto", "level": 0.001, "percentile": 30.0 }));
    let filled = depth["depth"]["filled"].as_u64().unwrap();
    assert!(filled > 0 && filled < 240 * 180, "{filled}");
    assert!(depth["depth"]["level"].as_f64().unwrap() > 0.0);
    assert!(depth.get("range").is_none() && depth.get("levels").is_none(), "{depth}");
    let mut used: Vec<u64> = depth["frames"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    used.sort_unstable();
    assert_eq!(used, ids);

    // Listed with its method, parameters, normalized maps and full-precision frame coordinates; one undo step.
    let listed = run(&mut engine, "output.list", "{}");
    let entry = &listed["outputs"][1];
    assert_eq!((&entry["name"], &entry["method"]), (&json!("Depth"), &json!("depth-map")));
    assert_eq!(
        entry["parameters"],
        json!({ "radius": 0.7, "smoothing": 2.7, "threshold": "auto", "level": 0.001, "percentile": 30.0, "framing": "narrow" })
    );
    assert_eq!(entry["maps"], json!(["confidence", "depth", "depth-frames"]));
    let output = depth["output"].as_u64().unwrap();

    // The maps are saved as data, 16-bit: the depth map is the frame index scaled, 0 for the first frame.
    let untagged = json!({ "format": { "format": "tiff", "compression": "deflate" }, "depth": "u16", "profile": "untagged" });
    let mut read_map = |map: &str| {
        let path = dir.path().join(format!("{map}.tif"));
        run(&mut engine, "output.save", &json!({ "output": output, "map": map, "path": path, "spec": untagged }).to_string());
        match florastack_codecs::read_file(&path, &florastack_codecs::ReadOptions::default()).unwrap().image {
            AnyImage::U16(image) => image,
            other => panic!("a 16-bit map, not {:?}", other.format()),
        }
    };
    let (depth_map, confidence) = (read_map("depth"), read_map("confidence"));
    assert_eq!((depth_map.width(), depth_map.height(), depth_map.channels()), (240, 180, 1));
    let (low, high) = (depth_map.as_raw().iter().min().copied().unwrap(), depth_map.as_raw().iter().max().copied().unwrap());
    assert!(low < 10_000 && high > 50_000, "near and far layers: {low} to {high}");
    assert!(confidence.as_raw().iter().any(|&c| c > 32768) && confidence.as_raw().iter().any(|&c| c < 32768));
    let srgb = json!({ "format": { "format": "tiff", "compression": "deflate" }, "depth": "u16", "profile": "srgb" });
    let path = dir.path().join("wrong.tif");
    for (map, spec, code) in [("depth", &srgb, "params"), ("motion", &untagged, "params"), ("Depth", &untagged, "params")] {
        let e = engine.dispatch_json("output.save", &json!({ "output": output, "map": map, "path": path, "spec": spec }).to_string()).unwrap_err();
        assert_eq!(e.code(), code, "{map}: {e}");
    }
    let e = engine
        .dispatch_json("output.save", &json!({ "output": average["output"], "map": "depth", "path": path, "spec": untagged }).to_string())
        .unwrap_err();
    assert!(format!("{e:?}").contains("has none"), "{e:?}");

    assert_eq!(run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len(), steps + 2);
    assert_eq!(run(&mut engine, "edit.undo", "{}")["label"], "Stack by depth map");
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(state(&engine), before);

    // Settings for one stack apply to it alone, and are checked; the project's come from the `depth_map` table.
    let level = run(&mut engine, "stack.depthMap", r#"{ "threshold": "level", "level": 0.0, "smoothing": 0.0 }"#);
    assert_eq!(level["depth"]["level"], 0.0);
    assert!(level["depth"]["filled"].as_u64().unwrap() < filled);
    let share = run(&mut engine, "stack.depthMap", r#"{ "threshold": "percentile", "percentile": 30 }"#);
    let filled = share["depth"]["filled"].as_u64().unwrap() as f64 / (240.0 * 180.0);
    assert!((0.29..0.5).contains(&filled), "a percentile of 30 fills at least 30%: {filled}");
    run(&mut engine, "stack.set", r#"{ "depth_map": { "threshold": "percentile", "percentile": 60 } }"#);
    let project = run(&mut engine, "stack.depthMap", "{}");
    assert_eq!((&project["settings"]["threshold"], &project["settings"]["percentile"]), (&json!("percentile"), &json!(60.0)));
    for bad in [
        r#"{ "radius": -1 }"#,
        r#"{ "smoothing": 300 }"#,
        r#"{ "percentile": 120 }"#,
        r#"{ "level": -1 }"#,
        r#"{ "threshold": 30 }"#,
        r#"{ "threshold": { "level": 0.001 } }"#,
        r#"{ "grit": 0.5 }"#,
    ] {
        assert_eq!(engine.dispatch_json("stack.depthMap", bad).unwrap_err().code(), "params", "{bad}");
    }
}

/// Linear frames as 32-bit float TIFF files, values beyond 1 kept: a RAW development with exposure added.
fn write_float_frames(dir: &Path, name: &str, frames: &[Image<f32>]) -> Vec<PathBuf> {
    let folder = dir.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    let spec = OutputSpec {
        format: OutputFormat::Tiff { compression: TiffCompression::Deflate },
        depth: BitDepth::F32,
        profile: OutputProfile::Input,
        extended_range: true,
    };
    frames
        .iter()
        .enumerate()
        .map(|(i, frame)| {
            let path = folder.join(format!("frame-{i:02}.tif"));
            std::fs::write(&path, florastack_codecs::write(&AnyImage::F32(frame.clone()), &spec, &OutputMetadata::default()).unwrap().bytes).unwrap();
            path
        })
        .collect()
}

#[test]
fn a_pyramid_brought_into_range_keeps_its_extended_result_for_float_files() {
    let dir = TempDir::new("stack-extended");
    let (frames, _) = layered_frames(160, 120, 1.0, 6, 32, 1.6);
    assert!(frames.iter().any(|f| f.as_raw().iter().any(|&v| v > 1.2)), "the frames are brighter than white");
    let files = write_float_frames(dir.path(), "bright", &frames);
    let (mut engine, _) = project_with(dir.path(), &files);
    run(&mut engine, "align.run", "{}");
    let stacked = run(&mut engine, "stack.pyramid", "{}");
    println!("{stacked}");
    assert_eq!(stacked["range"]["extended"], true);
    assert!(stacked["range"]["outOfRange"].as_u64().unwrap() > 0);
    let output = stacked["output"].as_u64().unwrap();
    let image = output_image(&engine, output);
    assert!(image.as_raw().iter().all(|v| (0.0..=1.0).contains(v)), "the output itself is in range");
    let listed = run(&mut engine, "output.list", "{}");
    assert!(listed["outputs"][0]["maps"].as_array().unwrap().iter().any(|m| m == "extended"), "{listed}");

    let save = |engine: &mut Engine, file: &str, spec: serde_json::Value| {
        let path = dir.path().join(file);
        let written = run(engine, "output.save", &json!({ "output": output, "path": path, "spec": spec }).to_string());
        let decoded = florastack_codecs::read_file(&path, &florastack_codecs::ReadOptions::default()).unwrap().image;
        let max = match decoded {
            AnyImage::F32(i) => i.as_raw().iter().copied().fold(0.0, f32::max),
            other => other.into_image::<u16>().map_or(0.0, |i| f32::from(i.as_raw().iter().copied().max().unwrap_or(0)) / 65535.0),
        };
        (written, max)
    };
    let float = json!({ "format": { "format": "tiff", "compression": "deflate" }, "depth": "f32", "extended_range": true });
    let (_, kept) = save(&mut engine, "extended.tif", float);
    assert!(kept > 1.2, "retain extended range: {kept}");
    let exr = json!({ "format": { "format": "open-exr", "compression": "zip", "half": false }, "depth": "f32", "extended_range": true });
    assert!(save(&mut engine, "extended.exr", exr).1 > 1.2);
    let in_range = json!({ "format": { "format": "tiff", "compression": "deflate" }, "depth": "f32" });
    assert!(save(&mut engine, "in-range.tif", in_range).1 <= 1.0);
    let integer = json!({ "format": { "format": "tiff", "compression": "deflate" }, "depth": "u16", "extended_range": true });
    let (written, max) = save(&mut engine, "integer.tif", integer);
    assert!(max <= 1.0);
    assert!(written["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("32-bit float")), "{written}");

    // Keeping the range instead leaves the output itself extended, with no map.
    let kept = run(&mut engine, "stack.pyramid", r#"{ "tone": "keep" }"#);
    assert_eq!(kept["range"], json!({ "outOfRange": 0, "extended": false }));
    assert!(output_image(&engine, kept["output"].as_u64().unwrap()).as_raw().iter().any(|&v| v > 1.0));
}

#[test]
fn the_preview_takes_either_method_and_its_settings() {
    let dir = TempDir::new("stack-preview-pyramid");
    let files = synthetic_frames(dir.path(), "stack", 160, 120, &breathing(5, 0.01, None, 33));
    let (mut engine, _) = project_with(dir.path(), &files);
    run(&mut engine, "align.run", "{}");
    let before = state(&engine);
    let previewed = run(&mut engine, "stack.preview", r#"{ "method": "pyramid", "grit": 0.25 }"#);
    assert_eq!((previewed["width"].as_u64(), previewed["height"].as_u64()), (Some(40), Some(30)), "{previewed}");
    assert_eq!((&previewed["method"], &previewed["settings"]["grit"]), (&json!("pyramid"), &json!(0.25)));
    assert_eq!(run(&mut engine, "stack.preview", "{}")["method"], "average", "the weighted average by default");
    let previewed = run(&mut engine, "stack.preview", r#"{ "method": "depth-map", "smoothing": 16, "threshold": "level", "level": 0.002 }"#);
    assert_eq!((previewed["width"].as_u64(), previewed["height"].as_u64()), (Some(40), Some(30)), "{previewed}");
    assert_eq!(
        previewed["settings"],
        json!({ "radius": 2.0, "smoothing": 16.0, "threshold": "level", "level": 0.002, "percentile": 30.0 }),
        "full-scale settings"
    );
    assert_eq!(state(&engine), before);
    for bad in [
        r#"{ "method": "pyramid", "radius": 2 }"#,
        r#"{ "grit": 0.5 }"#,
        r#"{ "method": "depthmap" }"#,
        r#"{ "method": "pyramid", "grit": 0.1 }"#,
        r#"{ "threshold": "level" }"#,
        r#"{ "method": "pyramid", "percentile": 20 }"#,
        r#"{ "method": "depth-map", "power": 2 }"#,
        r#"{ "method": "depth-map", "tone": "clip" }"#,
        r#"{ "method": "depth-map", "smoothing": 300 }"#,
        r#"{ "method": "depth-map", "percentile": -5 }"#,
    ] {
        assert_eq!(engine.dispatch_json("stack.preview", bad).unwrap_err().code(), "params", "{bad}");
    }
}
