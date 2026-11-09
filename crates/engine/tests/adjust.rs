//! Adjusting outputs through the engine, on an aligned synthetic stack: files are written with their adjustments,
//! neutral adjustments write the output as it is, undo brings the unadjusted file back, the adjust session previews
//! what is set, and a reproducibility sidecar replays the adjustments.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use common::{TempDir, breathing, config, path_list, run, synthetic_frames};
use florastack_engine::{Engine, json};

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

/// A project in `dir` with an aligned stack, stacked by pyramid; the output.
fn stacked(dir: &Path) -> (Engine, u64) {
    let files = synthetic_frames(dir, "stack", 160, 120, &breathing(5, 0.02, None, 17));
    let mut engine = Engine::new(config(dir));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Adjust", "folder": {} }}"#, quoted(dir)));
    run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(&files)));
    run(&mut engine, "align.run", "{}");
    let pyramid = run(&mut engine, "stack.pyramid", "{}")["output"].as_u64().unwrap();
    (engine, pyramid)
}

/// The output written as a 16-bit TIFF, read back as linear floats.
fn saved(engine: &mut Engine, output: u64, path: &Path) -> florastack_engine::Image<f32> {
    run(
        engine,
        "output.save",
        &json!({"output": output, "path": path, "spec": {"format": {"format": "tiff", "compression": "none"}, "depth": "u16"}, "overwrite": true})
            .to_string(),
    );
    match florastack_codecs::read_file(path, &florastack_engine::ReadOptions::default()).unwrap().image {
        florastack_engine::AnyImage::U16(i) => i.to_linear_f32().unwrap(),
        florastack_engine::AnyImage::U8(i) => i.to_linear_f32().unwrap(),
        florastack_engine::AnyImage::F32(i) => i,
    }
}

fn mean(image: &florastack_engine::Image<f32>) -> f64 {
    let c = image.channels();
    let color: Vec<f64> = image.as_raw().chunks(c).flat_map(|p| p.iter().take(3).map(|v| f64::from(*v))).collect();
    color.iter().sum::<f64>() / color.len() as f64
}

#[test]
fn files_are_written_with_their_adjustments_and_undo_takes_them_back() {
    let dir = TempDir::new("adjust-save");
    let (mut engine, output) = stacked(dir.path());
    let plain = saved(&mut engine, output, &dir.path().join("plain.tif"));
    let plain_bytes = std::fs::read(dir.path().join("plain.tif")).unwrap();

    // Neutral adjustments write the output as it is.
    run(&mut engine, "adjust.set", &json!({"output": output, "adjustments": {"global": {"sharpen_radius": 2.0}}}).to_string());
    saved(&mut engine, output, &dir.path().join("neutral.tif"));
    assert_eq!(std::fs::read(dir.path().join("neutral.tif")).unwrap(), plain_bytes);

    // A stop brighter.
    run(&mut engine, "adjust.set", &json!({"output": output, "adjustments": {"global": {"exposure": 1.0}}}).to_string());
    let brighter = saved(&mut engine, output, &dir.path().join("brighter.tif"));
    assert_eq!((brighter.width(), brighter.height(), brighter.channels()), (plain.width(), plain.height(), plain.channels()));
    let (a, b) = (mean(&plain), mean(&brighter));
    assert!(b > 1.6 * a, "{a} -> {b}");

    // A mask darkening the top half only.
    run(&mut engine, "adjust.reset", &json!({"output": output}).to_string());
    run(
        &mut engine,
        "adjust.addMask",
        &json!({"output": output, "mask": {"components": [{"shape": {"kind": "linear", "start": [0, 0.4], "end": [0, 0.5]}}], "adjust": {"exposure": -1}}})
            .to_string(),
    );
    let masked = saved(&mut engine, output, &dir.path().join("masked.tif"));
    let rows = |image: &florastack_engine::Image<f32>, y0: usize, y1: usize| {
        mean(&image.crop(florastack_engine::Rect::new(0, y0, image.width(), y1 - y0)).unwrap())
    };
    assert!(rows(&masked, 0, 40) < 0.6 * rows(&plain, 0, 40), "the top is darker");
    assert!((rows(&masked, 70, 120) / rows(&plain, 70, 120) - 1.0).abs() < 0.01, "the bottom is not");

    // Undo, twice: the mask, then the reset; again, the exposure.
    run(&mut engine, "edit.undo", "{}");
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(run(&mut engine, "adjust.get", &json!({"output": output}).to_string())["adjustments"]["global"]["exposure"], 1.0);
    run(&mut engine, "edit.undo", "{}");
    run(&mut engine, "edit.undo", "{}");
    saved(&mut engine, output, &dir.path().join("undone.tif"));
    assert_eq!(std::fs::read(dir.path().join("undone.tif")).unwrap(), plain_bytes, "back to the output as stacked");

    // Adjustments are saved with the project and open again.
    run(&mut engine, "edit.redo", "{}");
    run(&mut engine, "project.save", "{}");
    let folder = engine.project_path().unwrap().to_path_buf();
    run(&mut engine, "project.close", "{}");
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&folder)));
    assert_eq!(run(&mut engine, "adjust.get", &json!({"output": output}).to_string())["neutral"], true, "the first redo is the neutral setting");
}

#[test]
fn the_session_previews_what_is_set() {
    let dir = TempDir::new("adjust-preview");
    let (mut engine, output) = stacked(dir.path());
    let opened = run(&mut engine, "adjust.open", &json!({"output": output}).to_string());
    assert_eq!((opened["width"].as_u64(), opened["height"].as_u64()), (Some(160), Some(120)), "{opened}");
    assert!(engine.take_adjust_changes().unwrap().opened);
    let preview = |engine: &mut Engine| {
        run(engine, "adjust.preview", r#"{ "width": 80, "height": 80 }"#);
        assert!(engine.take_adjust_changes().unwrap().preview);
        let view = engine.adjust().unwrap();
        let p = view.preview().unwrap();
        assert_eq!((p.width, p.height), (80, 60));
        let sum: f64 = p.rgba.as_chunks::<4>().0.iter().map(|px| f64::from(half_to_f32(px[1]))).sum();
        sum / (p.width * p.height) as f64
    };
    let before = preview(&mut engine);
    run(&mut engine, "adjust.set", &json!({"output": output, "adjustments": {"global": {"exposure": -1.0}}}).to_string());
    let after = preview(&mut engine);
    assert!(after < 0.6 * before, "{before} -> {after}");
    // An overlay tints a mask's region.
    let added = run(&mut engine, "adjust.addMask", &json!({"output": output, "mask": {"components": [{"shape": {"kind": "whole"}}]}}).to_string());
    run(&mut engine, "adjust.preview", &json!({"width": 80, "height": 80, "overlay": added["mask"]}).to_string());
    // A stroke changes what adjusting starts from: the preview asks to open the output again.
    run(&mut engine, "retouch.open", &json!({"output": output}).to_string());
    run(&mut engine, "retouch.stroke", r#"{ "stroke": { "brush": "eraser", "size": 10, "hardness": 0.5, "opacity": 1, "samples": [[20, 20, 1]] } }"#);
    let e = engine.dispatch("adjust.preview", json!({"width": 80, "height": 80})).unwrap_err();
    assert_eq!(e.code(), "unavailable", "{e}");
    run(&mut engine, "adjust.open", &json!({"output": output}).to_string());
    run(&mut engine, "adjust.preview", r#"{ "width": 80, "height": 80 }"#);
    assert_eq!(run(&mut engine, "adjust.close", "{}")["closed"], true);
    assert!(engine.take_adjust_changes().unwrap().closed);
    assert!(engine.adjust().is_none());
}

fn half_to_f32(bits: u16) -> f32 {
    // IEEE half: sign, 5 exponent bits, 10 fraction bits.
    let exponent = i32::from((bits >> 10) & 0x1f);
    let fraction = f32::from(bits & 0x3ff);
    let magnitude = if exponent == 0 { fraction * 2f32.powi(-24) } else { (1.0 + fraction / 1024.0) * 2f32.powi(exponent - 15) };
    if bits & 0x8000 == 0 { magnitude } else { -magnitude }
}

#[test]
fn masks_of_the_focus_say_what_they_need() {
    let dir = TempDir::new("adjust-focus");
    let (mut engine, output) = stacked(dir.path());
    run(
        &mut engine,
        "adjust.addMask",
        &json!({"output": output, "mask": {"components": [{"shape": {"kind": "subject"}}], "adjust": {"exposure": 0.5}}}).to_string(),
    );
    let params = json!({"output": output, "path": dir.path().join("subject.tif"), "spec": {"format": {"format": "tiff", "compression": "none"}, "depth": "u16"}});
    let e = match engine.dispatch("output.save", params) {
        Ok(florastack_engine::Dispatched::Started(job)) => engine.wait(job).unwrap_err(),
        Ok(florastack_engine::Dispatched::Done(v)) => panic!("written: {v}"),
        Err(e) => e,
    };
    assert!(e.to_string().contains("focus"), "{e}");
}

#[test]
fn a_sidecar_replays_the_adjustments() {
    let dir = TempDir::new("adjust-reproduce");
    let (mut engine, output) = stacked(dir.path());
    run(&mut engine, "adjust.set", &json!({"output": output, "adjustments": {"global": {"exposure": 0.5, "clarity": 30}}}).to_string());
    let path = dir.path().join("adjusted.tif");
    saved(&mut engine, output, &path);
    let sidecar = dir.path().join("adjusted.tif.json");
    assert!(std::fs::read_to_string(&sidecar).unwrap().contains("\"clarity\""), "the sidecar records the adjustments");
    let replay = dir.path().join("replay");
    let result = run(&mut engine, "export.reproduce", &json!({"sidecar": sidecar, "folder": replay}).to_string());
    let written = result["result"]["path"].as_str().unwrap();
    assert_eq!(std::fs::read(written).unwrap(), std::fs::read(&path).unwrap(), "the same file");
}
