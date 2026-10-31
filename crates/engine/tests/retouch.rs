//! Retouching through the engine, on an aligned synthetic stack: strokes and their undo and redo leave exactly the
//! pixels of a fresh paint, a stroke drawn live is committed as painted, a hidden layer's strokes leave the result,
//! the result opens again from its cache with no recomputation, and exporting writes it retouched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use common::{TempDir, breathing, config, path_list, run, synthetic_frames};
use florastack_engine::{Engine, Rect, Value};

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

/// A project in `dir` with an aligned stack, stacked by pyramid and by depth map; the two outputs and the frames.
fn stacked(dir: &Path) -> (Engine, u64, u64, Vec<u64>) {
    let files = synthetic_frames(dir, "stack", 160, 120, &breathing(5, 0.02, None, 31));
    let mut engine = Engine::new(config(dir));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Retouch", "folder": {} }}"#, quoted(dir)));
    let added = run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(&files)));
    let frames: Vec<u64> = added["added"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    run(&mut engine, "align.run", "{}");
    let pyramid = run(&mut engine, "stack.pyramid", "{}")["output"].as_u64().unwrap();
    let depth = run(&mut engine, "stack.depthMap", "{}")["output"].as_u64().unwrap();
    (engine, pyramid, depth, frames)
}

/// The retouched result as the viewer gets it: half floats of every pixel.
fn result(engine: &Engine) -> Vec<u16> {
    let view = engine.retouch().unwrap();
    let (w, h) = view.size();
    view.result(Rect::new(0, 0, w, h), 1).unwrap().0.rgba
}

fn stroke(brush: &str, source: &str, samples: &str) -> String {
    format!(r#"{{ "brush": "{brush}", {source} "size": 30, "hardness": 0.4, "opacity": 1, "samples": {samples} }}"#)
}

#[test]
fn strokes_undo_and_redo_to_exactly_the_pixels_painted() {
    let dir = TempDir::new("retouch-undo");
    let (mut engine, pyramid, depth, frames) = stacked(dir.path());
    let opened = run(&mut engine, "retouch.open", &format!(r#"{{ "output": {pyramid} }}"#));
    assert_eq!((opened["width"].as_u64(), opened["rendered"].as_str()), (Some(160), Some("none")), "{opened}");
    let untouched = result(&engine);
    run(&mut engine, "retouch.setSource", &format!(r#"{{ "frame": {} }}"#, frames[0]));
    run(
        &mut engine,
        "retouch.stroke",
        &format!(r#"{{ "stroke": {} }}"#, stroke("pixels", &format!(r#""source": {{ "frame": {} }},"#, frames[0]), "[[30, 30, 1], [100, 60, 1]]")),
    );
    let one = result(&engine);
    assert_ne!(one, untouched, "the stroke painted");
    run(&mut engine, "retouch.setSource", &format!(r#"{{ "output": {depth} }}"#));
    run(
        &mut engine,
        "retouch.stroke",
        &format!(r#"{{ "stroke": {} }}"#, stroke("details", &format!(r#""source": {{ "output": {depth} }},"#), "[[60, 20, 1], [70, 100, 0.6]]")),
    );
    let two = result(&engine);
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(result(&engine), one, "undo paints the second stroke's region again without it");
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(result(&engine), untouched);
    run(&mut engine, "edit.redo", "{}");
    run(&mut engine, "edit.redo", "{}");
    assert_eq!(result(&engine), two, "redo paints them again");
    // Hiding the layer takes its strokes out; showing it brings them back.
    let layer = engine.project().unwrap().outputs.iter().find(|o| o.id().get() == pyramid).unwrap().retouch[0].id().get();
    run(&mut engine, "retouch.setLayerVisible", &format!(r#"{{ "output": {pyramid}, "layer": {layer}, "visible": false }}"#));
    assert_eq!(result(&engine), untouched);
    run(&mut engine, "retouch.setLayerVisible", &format!(r#"{{ "output": {pyramid}, "layer": {layer}, "visible": true }}"#));
    assert_eq!(result(&engine), two);
    // An eraser over everything brings the output back.
    run(&mut engine, "retouch.stroke", r#"{ "stroke": { "brush": "eraser", "size": 400, "hardness": 1, "opacity": 1, "samples": [[80, 60, 1]] } }"#);
    assert_eq!(result(&engine), untouched);
}

#[test]
fn a_stroke_drawn_live_is_committed_as_painted() {
    let dir = TempDir::new("retouch-live");
    let (mut engine, pyramid, depth, _) = stacked(dir.path());
    let full = stroke("details", &format!(r#""source": {{ "output": {depth} }},"#), "[[20, 20, 1], [50, 30, 0.9], [90, 80, 0.5], [130, 90, 1]]");
    let directly = {
        run(&mut engine, "retouch.open", &format!(r#"{{ "output": {pyramid} }}"#));
        run(&mut engine, "retouch.setSource", &format!(r#"{{ "output": {depth} }}"#));
        run(&mut engine, "retouch.stroke", &format!(r#"{{ "stroke": {full} }}"#));
        let painted = result(&engine);
        run(&mut engine, "edit.undo", "{}");
        painted
    };
    let source = format!(r#""source": {{ "output": {depth} }},"#);
    run(&mut engine, "retouch.paint", &format!(r#"{{ "begin": true, "stroke": {} }}"#, stroke("details", &source, "[[20, 20, 1]]")));
    run(&mut engine, "retouch.paint", &format!(r#"{{ "stroke": {} }}"#, stroke("details", &source, "[[50, 30, 0.9], [90, 80, 0.5]]")));
    run(&mut engine, "retouch.paint", &format!(r#"{{ "stroke": {} }}"#, stroke("details", &source, "[[130, 90, 1]]")));
    assert_eq!(result(&engine), directly, "live, the stroke shows as it will be committed");
    run(&mut engine, "retouch.stroke", &format!(r#"{{ "stroke": {full} }}"#));
    assert_eq!(result(&engine), directly);
    // A stroke cancelled part way leaves nothing.
    let before = result(&engine);
    run(&mut engine, "retouch.paint", &format!(r#"{{ "begin": true, "stroke": {} }}"#, stroke("pixels", &source, "[[10, 100, 1], [150, 10, 1]]")));
    assert_ne!(result(&engine), before);
    assert_eq!(run(&mut engine, "retouch.cancelPaint", "{}")["cancelled"], true);
    assert_eq!(result(&engine), before);
}

#[test]
fn the_result_opens_again_with_no_recomputation_and_exports_retouched() {
    let dir = TempDir::new("retouch-reopen");
    let (mut engine, pyramid, depth, _) = stacked(dir.path());
    run(&mut engine, "retouch.open", &format!(r#"{{ "output": {pyramid} }}"#));
    run(&mut engine, "retouch.setSource", &format!(r#"{{ "output": {depth} }}"#));
    run(
        &mut engine,
        "retouch.stroke",
        &format!(r#"{{ "stroke": {} }}"#, stroke("pixels", &format!(r#""source": {{ "output": {depth} }},"#), "[[40, 40, 1], [120, 80, 1]]")),
    );
    let painted = result(&engine);
    // Exporting writes the retouched result.
    let file = dir.path().join("retouched.tif");
    let spec = r#"{ "format": { "format": "tiff", "compression": "deflate" }, "depth": "f32" }"#;
    run(&mut engine, "output.save", &format!(r#"{{ "output": {pyramid}, "path": {}, "spec": {spec} }}"#, quoted(&file)));
    let exported = florastack_engine::view::load(&file, None).unwrap();
    assert_eq!(exported.rgba, painted, "the file holds the retouched pixels");
    // Saved and opened again, the result is read from its cache.
    run(&mut engine, "project.save", "{}");
    let path = engine.project_path().unwrap().to_path_buf();
    run(&mut engine, "project.close", "{}");
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&path)));
    let opened: Value = run(&mut engine, "retouch.open", &format!(r#"{{ "output": {pyramid} }}"#));
    assert_eq!(opened["rendered"], "cached", "{opened}");
    assert_eq!(result(&engine), painted);
    // Without the cache, the strokes are painted again to the same pixels.
    run(&mut engine, "retouch.close", "{}");
    let output = engine.project().unwrap().outputs.iter().find(|o| o.id().get() == pyramid).unwrap().clone();
    let cached = output.maps.iter().find(|(name, _)| name.as_str() == "retouched").map(|(_, r)| r.path.resolve(&path)).unwrap();
    std::fs::remove_file(cached).unwrap();
    run(&mut engine, "project.save", "{}");
    run(&mut engine, "project.close", "{}");
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&path)));
    let opened = run(&mut engine, "retouch.open", &format!(r#"{{ "output": {pyramid} }}"#));
    assert_eq!(opened["rendered"], "painted", "{opened}");
    assert_eq!(result(&engine), painted);
}
