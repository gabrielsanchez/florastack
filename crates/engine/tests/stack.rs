//! The stacking commands on synthetic stacks written as frame files: `stack.average` stacks the aligned frames into a
//! new output, sharper than any frame, as one undo step; wide framing leaves the pixels no frame covers transparent;
//! `stack.preview` makes a quarter-size preview without touching the project; and stacks without a current alignment,
//! or cancelled, change nothing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use common::{MARGIN, TempDir, breathing, config, path_list, run, scene_frames, state, synthetic_frames};
use florastack_core::testutil::{StackParams, generate, scene};
use florastack_core::{AnyImage, Image, Rect};
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
    for name in ["stack.average", "stack.preview"] {
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
    for name in ["stack.average", "stack.preview"] {
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
    assert_eq!(run(&mut engine, "stack.get", "{}")["settings"], json!({ "average": { "radius": 3.0, "smoothing": 8.0, "power": 3.0 } }));
    run(&mut engine, "stack.set", r#"{ "average": { "radius": 3.0 } }"#);
    let s = run(&mut engine, "stack.set", r#"{ "average": { "power": 1.5 } }"#)["settings"].clone();
    assert_eq!(s, json!({ "average": { "radius": 3.0, "smoothing": 8.0, "power": 1.5 } }));
    for bad in
        [r#"{ "average": { "radius": -1 } }"#, r#"{ "average": { "power": 9 } }"#, r#"{ "average": { "radious": 1 } }"#, r#"{ "pyramid": {} }"#]
    {
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
