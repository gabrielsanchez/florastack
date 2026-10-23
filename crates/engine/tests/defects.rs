//! The defect mask through the engine (roadmap 2.3): detected from frame files, written out for editing and read back,
//! inpainted in every frame before alignment, kept with the project, and detected by `align.run` itself when the
//! settings ask for it, in the same undo step as the alignment.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use common::{MARGIN, TempDir, config, path_list, run, scene_frames, state};
use florastack_codecs::{OutputMetadata, OutputSpec, ReadOptions};
use florastack_core::ops::{BlurMethod, gaussian_blur_in_place};
use florastack_core::testutil::{DustSpot, StackParams, scene};
use florastack_core::{AnyImage, ColorSpace, Image};
use florastack_engine::{Engine, Value, json};

const WIDTH: usize = 200;
const HEIGHT: usize = 150;
const FRAMES: usize = 10;

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

/// `scene::textured`'s scene, `MARGIN` pixels larger on every side than the frames, with its background (depth 0.55
/// and beyond) blurred by 10 pixels, as a backdrop far behind the subject is.
fn backdrop(seed: u64) -> (Image<f32>, Image<f32>) {
    let (mut sharp, depth) = scene::textured(WIDTH + 2 * MARGIN, HEIGHT + 2 * MARGIN, seed).unwrap();
    let mut soft = sharp.clone();
    gaussian_blur_in_place(&mut soft, 10.0, BlurMethod::Exact).unwrap();
    for y in 0..sharp.height() {
        for x in 0..sharp.width() {
            if depth.get(x, y, 0).unwrap() >= 0.55 {
                let far = soft.pixel(x, y).unwrap().to_vec();
                sharp.pixel_mut(x, y).unwrap().copy_from_slice(&far);
            }
        }
    }
    (sharp, depth)
}

/// A noisy stack with four dark dust spots on the backdrop, well inside the cropped frames.
fn dusty(depth: &Image<f32>, seed: u64) -> StackParams {
    let fits = |s: &DustSpot| {
        let reach = 4.0 * s.radius;
        let inside = s.x - reach > MARGIN as f32 && s.y - reach > MARGIN as f32;
        let inside = inside && s.x + reach < (WIDTH + MARGIN) as f32 && s.y + reach < (HEIGHT + MARGIN) as f32;
        let near = |x: usize, y: usize| (x as f32 - s.x).hypot(y as f32 - s.y) <= reach;
        inside && (0..depth.height()).all(|y| (0..depth.width()).all(|x| !near(x, y) || depth.get(x, y, 0).unwrap() >= 0.55))
    };
    let dust: Vec<DustSpot> = (0..)
        .flat_map(|k| DustSpot::random(8, WIDTH + 2 * MARGIN, HEIGHT + 2 * MARGIN, seed ^ (k << 8)))
        .map(|s| DustSpot { radius: s.radius + 1.0, opacity: s.opacity + 0.2, ..s })
        .filter(fits)
        .take(4)
        .collect();
    StackParams { dust, ..StackParams::noisy_dusty(FRAMES, WIDTH, HEIGHT, seed) }
}

/// An engine with a new project in `dir` holding `files`; and the frames' ids.
fn project_with(dir: &Path, files: &[PathBuf]) -> (Engine, Vec<u64>) {
    let mut engine = Engine::new(config(dir));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Dust", "folder": {} }}"#, quoted(dir)));
    let added = run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(files)));
    let ids: Vec<u64> = added["added"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    (engine, ids)
}

fn read_png(path: &Path) -> Image<u8> {
    match florastack_codecs::read(&std::fs::read(path).unwrap(), &ReadOptions::default()).unwrap().image {
        AnyImage::U8(image) => image,
        other => panic!("not 8-bit: {:?}", other.format()),
    }
}

fn write_png(path: &Path, image: Image<u8>) {
    let spec = OutputSpec::new(florastack_codecs::Format::Png).unwrap();
    std::fs::write(path, florastack_codecs::write(&AnyImage::U8(image), &spec, &OutputMetadata::default()).unwrap().bytes).unwrap();
}

/// A spot's center in the cropped frames.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A spot well inside the frames.
fn center(spot: &DustSpot) -> (usize, usize) {
    (spot.x.round() as usize - MARGIN, spot.y.round() as usize - MARGIN)
}

#[test]
fn detected_dust_is_inpainted_before_alignment_and_can_be_edited() {
    let dir = TempDir::new("defects");
    let (sharp, depth) = backdrop(3);
    let params = dusty(&depth, 3);
    let files = scene_frames(dir.path(), "dusty", &sharp, &depth, &params);
    let (mut engine, _) = project_with(dir.path(), &files);
    assert_eq!(run(&mut engine, "defects.info", "{}"), json!({ "mask": false, "detect": false }));

    let detected = run(&mut engine, "defects.detect", "{}");
    println!("{detected}");
    let summary = &detected["summary"];
    assert_eq!((&summary["source"], &summary["width"], &summary["height"]), (&json!("detected"), &json!(WIDTH), &json!(HEIGHT)));
    assert!(summary["spots"].as_u64().unwrap() >= 1 && summary["pixels"].as_u64().unwrap() > 100, "{detected}");
    assert_eq!(run(&mut engine, "defects.info", "{}")["summary"], *summary);

    // Written out, every dust spot's center is black on white.
    let mask = dir.path().join("mask.png");
    run(&mut engine, "defects.export", &format!(r#"{{ "path": {} }}"#, quoted(&mask)));
    let image = read_png(&mask);
    assert_eq!((image.width(), image.height(), image.channels()), (WIDTH, HEIGHT, 1));
    for spot in &params.dust {
        let (x, y) = center(spot);
        assert_eq!(image.get(x, y, 0), Some(0), "{spot:?}");
    }
    assert_eq!(image.get(2, 2, 0), Some(255));
    let e = engine.dispatch_json("defects.export", &format!(r#"{{ "path": {} }}"#, quoted(&mask))).unwrap_err();
    assert!(e.to_string().contains("overwrite"), "{e}");

    // Aligned with the dust inpainted, no frame moves: the dust that stays on the sensor while the frames shift is gone.
    let aligned = run(&mut engine, "align.run", "{}");
    assert_eq!((&aligned["moved"], &aligned["rejected"]), (&json!([]), &json!([])), "{aligned}");
    assert!(aligned.get("defects").is_none(), "the project's mask is used, not detected again");

    // Edited by the user (a hair added) and read back.
    let mut edited = image.clone();
    for x in 20..60 {
        edited.pixel_mut(x, 100).unwrap()[0] = 0;
    }
    write_png(&mask, edited);
    let imported = run(&mut engine, "defects.import", &format!(r#"{{ "path": {} }}"#, quoted(&mask)));
    assert_eq!(imported["summary"]["source"], "imported");
    assert_eq!(imported["summary"]["pixels"].as_u64().unwrap(), summary["pixels"].as_u64().unwrap() + 40);
    // A mask of another size than the aligned frames is refused.
    let small = dir.path().join("small.png");
    write_png(&small, Image::from_fn(100, 80, 1, ColorSpace::SRGB, |_, _, _| 255).unwrap());
    let job = common::job_of(engine.dispatch_json("defects.import", &format!(r#"{{ "path": {} }}"#, quoted(&small))).unwrap());
    let e = engine.wait(job).unwrap_err();
    assert!(e.to_string().contains("100x80") && e.to_string().contains(&format!("{WIDTH}x{HEIGHT}")), "{e}");

    // Kept with the project; removed in one step, which undo takes back.
    run(&mut engine, "project.save", "{}");
    let path = engine.project_path().unwrap().to_path_buf();
    run(&mut engine, "project.close", "{}");
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&path)));
    let info = run(&mut engine, "defects.info", "{}");
    assert_eq!(info["summary"], imported["summary"]);
    let before = state(&engine);
    assert_eq!(run(&mut engine, "defects.clear", "{}")["cleared"], true);
    assert_eq!(run(&mut engine, "defects.info", "{}")["mask"], false);
    assert_eq!(run(&mut engine, "defects.clear", "{}")["cleared"], false, "nothing to remove");
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(state(&engine), before);
}

#[test]
fn aligning_detects_a_mask_first_when_asked_to() {
    let dir = TempDir::new("defects-align");
    let (sharp, depth) = backdrop(5);
    let files = scene_frames(dir.path(), "dusty", &sharp, &depth, &dusty(&depth, 5));
    let (mut engine, _) = project_with(dir.path(), &files);
    run(&mut engine, "align.set", r#"{ "detect_defects": true }"#);
    assert_eq!(run(&mut engine, "defects.info", "{}")["detect"], true);
    let before = state(&engine);
    let aligned = run(&mut engine, "align.run", "{}");
    let summary: &Value = &aligned["defects"];
    assert_eq!(summary["source"], "detected", "{aligned}");
    assert_eq!(aligned["moved"], json!([]));
    assert_eq!(run(&mut engine, "defects.info", "{}")["summary"], *summary);
    // The mask and the alignment are one step.
    let history = run(&mut engine, "edit.history", "{}");
    assert_eq!(history["undo"][0]["command"], "align.run");
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(state(&engine), before);
    run(&mut engine, "edit.redo", "{}");
    // With a mask in the project, aligning again uses it.
    let again = run(&mut engine, "align.run", "{}");
    assert!(again.get("defects").is_none(), "{again}");
}
