//! Adjusting outputs through the engine, on an aligned synthetic stack: files are written with their adjustments,
//! neutral adjustments write the output as it is, undo brings the unadjusted file back, the adjust session previews
//! what is set, and a reproducibility sidecar replays the adjustments.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Test scenes in fractions and pixels, all small: f32 holds them as exactly as the checks need.
#![allow(clippy::cast_possible_truncation)]

mod common;

use std::path::Path;

use common::{MARGIN, TempDir, breathing, config, path_list, run, scene_frames, synthetic_frames};
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
    // A stroke changes what adjusting starts from: the next preview reads the output again, with the stroke.
    let shown = |engine: &Engine| engine.adjust().unwrap().preview().unwrap().rgba.clone();
    run(&mut engine, "adjust.reset", &json!({"output": output}).to_string());
    run(&mut engine, "adjust.preview", r#"{ "width": 160, "height": 120 }"#);
    let unretouched = shown(&engine);
    run(&mut engine, "retouch.open", &json!({"output": output}).to_string());
    run(&mut engine, "retouch.setSource", r#"{ "frame": 1 }"#);
    run(
        &mut engine,
        "retouch.stroke",
        r#"{ "stroke": { "brush": "pixels", "source": { "frame": 1 }, "size": 60, "hardness": 1, "opacity": 1, "samples": [[80, 60, 1]] } }"#,
    );
    run(&mut engine, "adjust.preview", r#"{ "width": 160, "height": 120 }"#);
    assert_ne!(shown(&engine), unretouched, "the preview shows the stroke");
    // And again after an undo.
    run(&mut engine, "edit.undo", "{}");
    run(&mut engine, "adjust.preview", r#"{ "width": 160, "height": 120 }"#);
    assert_eq!(shown(&engine), unretouched, "the stroke undone");
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

/// The mean of the pixels `keep` selects, over the color channels.
fn mean_where(image: &florastack_engine::Image<f32>, keep: impl Fn(usize, usize) -> bool) -> f64 {
    let (mut sum, mut n) = (0.0, 0.0);
    for y in 0..image.height() {
        for x in 0..image.width() {
            if keep(x, y) {
                let p = image.pixel(x, y).unwrap();
                sum += p.iter().take(3).map(|v| f64::from(*v)).sum::<f64>();
                n += 3.0;
            }
        }
    }
    sum / n
}

#[test]
fn every_shape_selects_its_region_through_the_commands() {
    let dir = TempDir::new("adjust-shapes");
    let (mut engine, output) = stacked(dir.path());
    let plain = saved(&mut engine, output, &dir.path().join("plain.tif"));
    let (w, h) = (plain.width(), plain.height());
    // Each shape darkens by a stop where it selects: the ratio to the plain output there, and elsewhere.
    let darkened = |engine: &mut Engine, name: &str, shape: florastack_engine::Value| {
        run(engine, "adjust.reset", &json!({"output": output}).to_string());
        let mask = json!({"components": if shape.is_null() { json!([]) } else { json!([{"shape": shape}]) }, "adjust": {"exposure": -1}});
        let added = run(engine, "adjust.addMask", &json!({"output": output, "mask": mask}).to_string());
        if shape.is_null() {
            // A brush: two strokes along the middle row, the second erasing its right end.
            let id = &added["mask"];
            run(
                engine,
                "adjust.stroke",
                &json!({"output": output, "mask": id, "stroke": {"points": [[0.1, 0.5], [0.9, 0.5]], "size": 0.06, "feather": 0}}).to_string(),
            );
            let erased = run(
                engine,
                "adjust.stroke",
                &json!({"output": output, "mask": id, "stroke": {"points": [[0.75, 0.5], [0.95, 0.5]], "size": 0.08, "feather": 0, "erase": true}})
                    .to_string(),
            );
            assert_eq!((&erased["component"], &erased["strokes"]), (&json!(0), &json!(2)), "{erased}");
        }
        saved(engine, output, &dir.path().join(format!("{name}.tif")))
    };
    let ratio = |image: &florastack_engine::Image<f32>, keep: &dyn Fn(usize, usize) -> bool| mean_where(image, keep) / mean_where(&plain, keep);
    let darker = |r: f64| r < 0.6;
    let same = |r: f64| (r - 1.0).abs() < 0.01;

    let out = darkened(&mut engine, "whole", json!({"kind": "whole"}));
    assert!(darker(ratio(&out, &|_, _| true)));

    let out = darkened(&mut engine, "linear", json!({"kind": "linear", "start": [0, 0.3], "end": [0, 0.4]}));
    assert!(darker(ratio(&out, &|_, y| y < h * 3 / 10)) && same(ratio(&out, &|_, y| y > h / 2)));

    let out =
        darkened(&mut engine, "radial", json!({"kind": "radial", "center": [0.5, 0.5], "radius_x": 0.2, "radius_y": 0.2, "angle": 0, "feather": 0}));
    let distance = |x: usize, y: usize| (x as f64 - w as f64 / 2.0).hypot(y as f64 - h as f64 / 2.0) / w as f64;
    assert!(darker(ratio(&out, &|x, y| distance(x, y) < 0.15)) && same(ratio(&out, &|x, y| distance(x, y) > 0.3)));

    let out = darkened(&mut engine, "brush", florastack_engine::Value::Null);
    let row = |x0: usize, x1: usize| move |x: usize, y: usize| (x0..x1).contains(&x) && y.abs_diff(h / 2) < 3;
    assert!(darker(ratio(&out, &row(w / 5, w * 3 / 5))), "painted");
    assert!(same(ratio(&out, &row(w * 4 / 5, w * 17 / 20))), "erased");
    assert!(same(ratio(&out, &|_, y: usize| y.abs_diff(h / 2) > h / 4)), "never painted");

    // Brightness: the brightest tenth of the pixels against the darkest third.
    let mut luma: Vec<f64> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| mean_where(&plain, |i, j| (i, j) == (x, y))).collect();
    luma.sort_by(f64::total_cmp);
    let (dark, bright) = (luma[luma.len() / 3], luma[luma.len() * 9 / 10]);
    // On the range's scale, a stop an eighth: 0 eight stops below white, 1 white.
    let stops = |v: f64| ((v.log2() + 8.0) / 8.0).clamp(0.0, 1.0);
    let low = f64::midpoint(stops(dark), stops(bright));
    let out = darkened(&mut engine, "luminance", json!({"kind": "luminance", "low": low, "high": 1.0, "low_feather": 0.01, "high_feather": 0}));
    let value = |x: usize, y: usize| plain.pixel(x, y).unwrap().iter().take(3).map(|v| f64::from(*v)).sum::<f64>() / 3.0;
    let (r_bright, r_dark) = (ratio(&out, &|x, y| value(x, y) >= bright), ratio(&out, &|x, y| value(x, y) <= dark));
    assert!(r_bright < r_dark && (r_dark - 1.0).abs() < 0.05, "bright {r_bright}, dark {r_dark}");

    // A color sampled at the center: its pixels darken more than the image does.
    run(&mut engine, "adjust.reset", &json!({"output": output}).to_string());
    run(&mut engine, "adjust.open", &json!({"output": output}).to_string());
    let color = run(&mut engine, "adjust.sampleColor", r#"{ "x": 0.5, "y": 0.5 }"#)["color"].clone();
    let out = darkened(&mut engine, "color", json!({"kind": "color", "samples": [color], "refine": 50}));
    let center = |x: usize, y: usize| x.abs_diff(w / 2) <= 1 && y.abs_diff(h / 2) <= 1;
    assert!(ratio(&out, &center) < ratio(&out, &|_, _| true), "{} {}", ratio(&out, &center), ratio(&out, &|_, _| true));
}

/// The subject scene: a textured ellipse whose depth runs from 0 at its left end to 0.5 at its right one, over a smooth
/// background at depth 1 that no frame brings into focus. The frames are 160 x 120; `inside(x, y)` is where a point of
/// a frame lies across the ellipse, from 0 at its center to 1 on its outline, and `across(x)` where it lies from its
/// left end (0) to its right one (1).
const SUBJECT: (f64, f64, f64, f64) = (80.0, 60.0, 55.0, 40.0);

fn inside(x: usize, y: usize) -> f64 {
    let (cx, cy, rx, ry) = SUBJECT;
    ((x as f64 + 0.5 - cx) / rx).hypot((y as f64 + 0.5 - cy) / ry)
}

fn across(x: usize) -> f64 {
    let (cx, _, rx, _) = SUBJECT;
    (x as f64 + 0.5 - (cx - rx)) / (2.0 * rx)
}

/// A project in `dir` with a stack of the subject scene, focused from depth 0 to 0.5, aligned.
fn subject_stack(dir: &Path) -> Engine {
    use florastack_engine::{ColorSpace, Image};
    let (w, h) = (160 + 2 * MARGIN, 120 + 2 * MARGIN);
    let hash = |x: usize, y: usize, c: usize| {
        let mut v = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ (c as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
        v ^= v >> 29;
        v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        v ^= v >> 32;
        (v % 1000) as f32 / 1000.0
    };
    let frame = |x: usize, y: usize| (x.saturating_sub(MARGIN), y.saturating_sub(MARGIN));
    let sharp = Image::from_fn(w, h, 3, ColorSpace::LINEAR, |x, y, c| {
        let (fx, fy) = frame(x, y);
        if inside(fx, fy) <= 1.0 { 0.15 + 0.6 * hash(x / 2, y / 2, c) } else { 0.3 + 0.05 * (y as f32 / h as f32) }
    })
    .unwrap();
    let depth = Image::from_fn(w, h, 1, ColorSpace::LINEAR, |x, y, _| {
        let (fx, fy) = frame(x, y);
        if inside(fx, fy) <= 1.0 { (0.5 * across(fx)).clamp(0.0, 0.5) as f32 } else { 1.0 }
    })
    .unwrap();
    // With a camera's noise: on frames without any, the smooth background's least contrast stands out too.
    // Defocus that grows fast enough with depth that neighboring frames differ by 2 px of blur.
    let params = florastack_core::testutil::StackParams {
        read_noise: 0.005,
        shot_noise: 0.01,
        blur_per_depth: 30.0,
        ..breathing(8, 0.0, Some((0.0, 0.5)), 23)
    };
    let files = scene_frames(dir, "subject", &sharp, &depth, &params);
    let mut engine = Engine::new(config(dir));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Focus", "folder": {} }}"#, quoted(dir)));
    run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(&files)));
    run(&mut engine, "align.run", "{}");
    engine
}

/// The output with only a mask of `shape` darkening it by a stop, against the output without it: the ratio where
/// `keep` selects.
fn darkened_by(
    engine: &mut Engine,
    dir: &Path,
    output: u64,
    shape: &florastack_engine::Value,
) -> impl Fn(&dyn Fn(usize, usize) -> bool) -> f64 + use<> {
    run(engine, "adjust.reset", &json!({"output": output}).to_string());
    let plain = saved(engine, output, &dir.join("plain.tif"));
    run(engine, "adjust.addMask", &json!({"output": output, "mask": {"components": [{"shape": shape}], "adjust": {"exposure": -1}}}).to_string());
    let out = saved(engine, output, &dir.join("masked.tif"));
    move |keep: &dyn Fn(usize, usize) -> bool| mean_where(&out, keep) / mean_where(&plain, keep)
}

#[test]
fn focus_masks_select_the_subject_and_its_depths() {
    let dir = TempDir::new("adjust-focus-masks");
    let mut engine = subject_stack(dir.path());
    let pyramid = run(&mut engine, "stack.pyramid", "{}")["output"].as_u64().unwrap();
    let depth_map = run(&mut engine, "stack.depthMap", "{}")["output"].as_u64().unwrap();
    // The subject's inside and the background, away from the outline the masks soften.
    let subject = |x: usize, y: usize| inside(x, y) < 0.75;
    let background = |x: usize, y: usize| inside(x, y) > 1.35;
    let near = |x: usize, y: usize| inside(x, y) < 0.8 && across(x) < 0.25;
    let far = |x: usize, y: usize| inside(x, y) < 0.8 && across(x) > 0.75;
    // The session: the pyramid's depth is measured once and kept as its map; the depth map's output needs none.
    let opened = run(&mut engine, "adjust.open", &json!({"output": depth_map}).to_string());
    assert_eq!(opened["depth"], true);
    let opened = run(&mut engine, "adjust.open", &json!({"output": pyramid}).to_string());
    assert_eq!(opened["depth"], false);
    assert!(!engine.adjust().unwrap().has_depth());
    run(
        &mut engine,
        "adjust.addMask",
        &json!({"output": pyramid, "mask": {"components": [{"shape": {"kind": "depth", "from": 0, "to": 0.5, "feather": 0.1}}], "adjust": {"exposure": -1}}})
            .to_string(),
    );
    let e = engine.dispatch("adjust.preview", json!({"width": 80, "height": 60})).and_then(|d| match d {
        florastack_engine::Dispatched::Started(job) => engine.wait(job),
        florastack_engine::Dispatched::Done(v) => Ok(v),
    });
    assert_eq!(e.unwrap_err().code(), "unavailable", "the depth mask waits for the depth map");
    let mapped = run(&mut engine, "adjust.mapDepth", "{}");
    assert_eq!((&mapped["measured"], &mapped["width"]), (&json!(true), &json!(40)), "a quarter of the output: {mapped}");
    assert!(engine.adjust().unwrap().has_depth());
    assert_eq!(run(&mut engine, "adjust.mapDepth", "{}")["measured"], false, "kept as the output's map");
    let opened = run(&mut engine, "adjust.open", &json!({"output": pyramid}).to_string());
    assert_eq!(opened["depth"], true, "read back from its map");
    run(&mut engine, "adjust.preview", r#"{ "width": 80, "height": 60 }"#);
    for (name, output) in [("depth map", depth_map), ("pyramid", pyramid)] {
        // The subject is found in the output itself. The pyramid's export measures its depth from the frames itself; the
        // depth map has its own.
        let ratio = darkened_by(&mut engine, dir.path(), output, &json!({"kind": "subject"}));
        let (s, b) = (ratio(&subject), ratio(&background));
        println!("{name}: subject {s:.3}, background {b:.3}");
        assert!(s < 0.6 && (b - 1.0).abs() < 0.03, "{name}: subject {s}, background {b}");
        let ratio = darkened_by(&mut engine, dir.path(), output, &json!({"kind": "background"}));
        let (s, b) = (ratio(&subject), ratio(&background));
        assert!(b < 0.6 && (s - 1.0).abs() < 0.05, "{name}: background mask: subject {s}, background {b}");
        // A range around the depth of the subject's left end, which the depth map tells (the stack's first frame may be
        // its nearest or its farthest): the left end darkens, the right end and the background don't.
        let map = if output == depth_map { "depth" } else { "focus-depth" };
        if output == pyramid {
            run(&mut engine, "adjust.open", &json!({"output": pyramid}).to_string());
            run(&mut engine, "adjust.mapDepth", "{}");
        }
        let (left, right) = (depth_where(&engine, output, map, &near), depth_where(&engine, output, map, &far));
        assert!((left - right).abs() > 0.3, "{name}: the depth runs across the subject: {left} to {right}");
        let range = json!({"kind": "depth", "from": (left - 0.15).max(0.0), "to": (left + 0.15).min(1.0), "feather": 0.05});
        let ratio = darkened_by(&mut engine, dir.path(), output, &range);
        let (n, f, b) = (ratio(&near), ratio(&far), ratio(&background));
        println!("{name}: depth {left:.2} (left) to {right:.2} (right), a range around the left: left {n:.3}, right {f:.3}, background {b:.3}");
        assert!(n < 0.65 && (f - 1.0).abs() < 0.05 && (b - 1.0).abs() < 0.03, "{name}: left {n}, right {f}, background {b}");
    }
}

/// The mean of output `id`'s 16-bit map `name` where `keep` selects (output pixels; the map may be smaller), as a
/// fraction.
fn depth_where(engine: &Engine, id: u64, name: &str, keep: &dyn Fn(usize, usize) -> bool) -> f64 {
    let folder = engine.project_path().unwrap();
    let project = engine.project().unwrap();
    let output = project.outputs.iter().find(|o| o.id().get() == id).unwrap();
    let reference = output.maps.iter().find(|(n, _)| n.as_str() == name).unwrap().1;
    let decoded = florastack_codecs::read_file(&reference.path.resolve(folder), &florastack_engine::ReadOptions::default()).unwrap();
    let florastack_engine::AnyImage::U16(map) = decoded.image else { panic!("a 16-bit map") };
    let (scale_x, scale_y) = (map.width() as f64 / 160.0, map.height() as f64 / 120.0);
    let (mut sum, mut n) = (0.0, 0.0);
    for y in 0..120 {
        for x in 0..160 {
            if keep(x, y) {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Within the map.
                let (mx, my) = ((x as f64 * scale_x) as usize, (y as f64 * scale_y) as usize);
                sum += f64::from(map.get(mx, my, 0).unwrap()) / 65535.0;
                n += 1.0;
            }
        }
    }
    sum / n
}

#[test]
fn an_output_renders_as_it_is_written_and_is_kept() {
    let dir = TempDir::new("adjust-render");
    let (mut engine, output) = stacked(dir.path());
    let render = |engine: &mut Engine| run(engine, "output.render", &json!({"output": output}).to_string());
    let plain = render(&mut engine);
    assert_eq!(plain["how"], "stacked", "{plain}");
    // Adjusted: rendered once, then read back.
    run(&mut engine, "adjust.set", &json!({"output": output, "adjustments": {"global": {"exposure": 1.0}}}).to_string());
    let first = render(&mut engine);
    assert_eq!(first["how"], "rendered", "{first}");
    assert_eq!(render(&mut engine)["how"], "cached");
    let read = |path: &str| match florastack_codecs::read_file(Path::new(path), &florastack_engine::ReadOptions::default()).unwrap().image {
        florastack_engine::AnyImage::F32(i) => i,
        florastack_engine::AnyImage::U16(i) => i.to_linear_f32().unwrap(),
        florastack_engine::AnyImage::U8(i) => i.to_linear_f32().unwrap(),
    };
    let (before, after) = (read(plain["path"].as_str().unwrap()), read(first["path"].as_str().unwrap()));
    assert!(mean(&after) > 1.6 * mean(&before), "the render is adjusted");
    // The same pixels a file is written with, once a 16-bit file has clipped them to [0, 1].
    let saved = saved(&mut engine, output, &dir.path().join("saved.tif"));
    assert_eq!((after.width(), after.height(), after.channels()), (saved.width(), saved.height(), saved.channels()));
    let d = after.as_raw().iter().zip(saved.as_raw()).map(|(a, b)| (a.clamp(0.0, 1.0) - b).abs()).fold(0.0f32, f32::max);
    assert!(d < 1.0 / 16384.0, "{d}");
    // A stroke changes it; undoing the stroke brings the cached one back.
    run(&mut engine, "retouch.open", &json!({"output": output}).to_string());
    run(&mut engine, "retouch.stroke", r#"{ "stroke": { "brush": "eraser", "size": 10, "hardness": 0.5, "opacity": 1, "samples": [[20, 20, 1]] } }"#);
    // (An eraser over an unretouched output changes no pixel, but the strokes are part of what it is made from.)
    assert_eq!(render(&mut engine)["how"], "rendered");
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(render(&mut engine)["how"], "rendered", "the map kept is the latest render's");
    run(&mut engine, "adjust.reset", &json!({"output": output}).to_string());
    assert_eq!(render(&mut engine)["how"], "stacked");
}
