//! Movement and the filmstrip through the engine (roadmap 2.3): `align.run` records the motion it finds in each frame
//! in the project, flags the frames where the subject moved, and makes every frame's previews, as shot and aligned;
//! `filmstrip.list` serves them with the motion, and `filmstrip.update` makes again only what a change of framing or of
//! frames left out of date, with no undo step.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use common::{MARGIN, TempDir, breathing, config, path_list, run, scene_frames, write_frames};
use florastack_codecs::ReadOptions;
use florastack_core::ops::{Border, Interpolation, warp};
use florastack_core::testutil::{StackParams, generate, scene};
use florastack_core::{Affine, ColorSpace, Image, Rect};
use florastack_engine::{Engine, Value, json};

const WIDTH: usize = 240;
const HEIGHT: usize = 180;
const FRAMES: usize = 8;
/// A patch of the scene, in the generated frames' pixels, that moves in frame 5 only.
const PATCH: (f64, f64) = (128.0, 98.0);
const RADIUS: f64 = 36.0;

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

fn params(seed: u64) -> StackParams {
    StackParams { read_noise: 0.003, shot_noise: 0.005, ..breathing(FRAMES, 0.005, None, seed) }
}

/// The frames of a stack whose subject, a disc around `PATCH`, moved by 4 pixels right and 3 up in frame 5 only.
fn moving_stack(dir: &Path, seed: u64) -> Vec<std::path::PathBuf> {
    let (sharp, depth) = scene::textured(WIDTH + 2 * MARGIN, HEIGHT + 2 * MARGIN, seed).unwrap();
    let still = generate(&sharp, &depth, &params(seed)).unwrap().frames;
    let shift = |image: &Image<f32>| {
        let shifted = warp(image, &Affine::translation(-4.0, 3.0), Interpolation::Bicubic, Border::Clamp).unwrap();
        Image::from_fn(image.width(), image.height(), image.channels(), ColorSpace::LINEAR, |x, y, c| {
            let inside = (x as f64 - PATCH.0).hypot(y as f64 - PATCH.1) <= RADIUS;
            if inside { shifted.get(x, y, c).unwrap() } else { image.get(x, y, c).unwrap() }
        })
        .unwrap()
    };
    let moved = generate(&shift(&sharp), &shift(&depth), &params(seed)).unwrap().frames;
    let frames: Vec<Image<f32>> =
        (0..FRAMES).map(|i| if i == 5 { &moved[i] } else { &still[i] }).map(|f| f.crop(Rect::new(MARGIN, MARGIN, WIDTH, HEIGHT)).unwrap()).collect();
    write_frames(dir, "moving", &frames)
}

fn num(value: &Value) -> usize {
    usize::try_from(value.as_u64().unwrap()).unwrap()
}

fn ids_of(value: &Value) -> Vec<u64> {
    value.as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect()
}

fn size_of(path: &Value) -> (usize, usize) {
    let bytes = std::fs::read(path.as_str().unwrap()).unwrap();
    let image = florastack_codecs::read(&bytes, &ReadOptions::default()).unwrap().image;
    (image.width(), image.height())
}

#[test]
fn a_subject_that_moved_is_flagged_and_shown_in_the_filmstrip() {
    let dir = TempDir::new("filmstrip-moved");
    let files = moving_stack(dir.path(), 7);
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Moving", "folder": {} }}"#, quoted(dir.path())));
    let added = run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(&files)));
    let ids = ids_of(&added["added"]);

    // Before alignment, previews as shot only.
    let film = run(&mut engine, "filmstrip.list", "{}");
    assert_eq!(film["stale"], FRAMES, "{film}");
    let updated = run(&mut engine, "filmstrip.update", "{}");
    assert_eq!((&updated["previews"], &updated["alignedPreviews"]), (&json!(FRAMES), &json!(0)));
    let film = run(&mut engine, "filmstrip.list", "{}");
    assert_eq!(film["stale"], 0);
    assert_eq!(size_of(&film["frames"][0]["preview"]), (WIDTH, HEIGHT), "frames smaller than 512 pixels keep their size");
    assert!(film["frames"][0].get("alignedPreview").is_none());

    // Aligned from its middle frame (4): frame 5 is aligned to 4, where the subject is still, and 6 to 5.
    let aligned = run(&mut engine, "align.run", "{}");
    println!("{aligned}");
    assert_eq!(ids_of(&aligned["moved"]), [ids[5], ids[6]], "{aligned}");
    let listed = run(&mut engine, "project.frames", "{}");
    let frame = &listed["frames"][5];
    assert!(frame["flags"].as_array().unwrap().contains(&json!("moved")) && frame["motionScore"].as_f64().unwrap() > 0.0, "{frame}");
    assert!(listed["frames"][2].get("motionScore").is_some_and(|s| s == 0.0), "{listed}");

    // The filmstrip has the previews align.run made, and where the subject moved.
    let film = run(&mut engine, "filmstrip.list", "{}");
    assert_eq!(film["stale"], 0, "{film}");
    let motion = &film["frames"][5]["motion"];
    let region = &motion["regions"][0];
    let (x, y) = (region["x"].as_f64().unwrap(), region["y"].as_f64().unwrap());
    let (w, h) = (region["width"].as_f64().unwrap(), region["height"].as_f64().unwrap());
    let (px, py) = (PATCH.0 - MARGIN as f64, PATCH.1 - MARGIN as f64);
    assert!(x >= px - RADIUS - 12.0 && y >= py - RADIUS - 12.0 && x + w <= px + RADIUS + 12.0 && y + h <= py + RADIUS + 12.0, "{motion}");
    let (dx, dy) = (region["dx"].as_f64().unwrap(), region["dy"].as_f64().unwrap());
    assert!((dx - 4.0).abs() < 0.3 && (dy + 3.0).abs() < 0.3, "4 right and 3 up: {motion}");
    let info = run(&mut engine, "align.info", "{}");
    let narrow = info["run"]["framings"].as_array().unwrap().iter().find(|f| f["framing"] == "narrow").unwrap().clone();
    let canvas = (num(&narrow["width"]), num(&narrow["height"]));
    for f in film["frames"].as_array().unwrap() {
        assert!(Path::new(f["preview"].as_str().unwrap()).is_file());
        assert_eq!(size_of(&f["alignedPreview"]), canvas, "the aligned previews show the output's canvas");
    }

    // A new framing leaves the aligned previews out of date; updating makes them from the previews, reading no frame.
    run(&mut engine, "align.set", r#"{ "framing": "wide" }"#);
    assert_eq!(run(&mut engine, "filmstrip.list", "{}")["stale"], FRAMES);
    let updated = run(&mut engine, "filmstrip.update", "{}");
    assert_eq!((&updated["previews"], &updated["alignedPreviews"]), (&json!(0), &json!(FRAMES)), "{updated}");
    let wide = run(&mut engine, "align.info", "{}")["run"]["framings"].as_array().unwrap().iter().find(|f| f["framing"] == "wide").unwrap().clone();
    let film = run(&mut engine, "filmstrip.list", "{}");
    assert_eq!(size_of(&film["frames"][0]["alignedPreview"]), (num(&wide["width"]), num(&wide["height"])));
    // Nothing left to make: no change at all, and never an undo step.
    let steps = run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len();
    let updated = run(&mut engine, "filmstrip.update", "{}");
    assert_eq!((&updated["previews"], &updated["alignedPreviews"]), (&json!(0), &json!(0)));
    assert_eq!(run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len(), steps);

    // Saved and reopened, the previews are still current.
    run(&mut engine, "project.save", "{}");
    let path = engine.project_path().unwrap().to_path_buf();
    run(&mut engine, "project.close", "{}");
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&path)));
    assert_eq!(run(&mut engine, "filmstrip.list", "{}"), film);
}

#[test]
fn a_still_stack_flags_no_movement() {
    let dir = TempDir::new("filmstrip-still");
    let (sharp, depth) = scene::textured(WIDTH + 2 * MARGIN, HEIGHT + 2 * MARGIN, 8).unwrap();
    let files = scene_frames(dir.path(), "still", &sharp, &depth, &params(8));
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Still", "folder": {} }}"#, quoted(dir.path())));
    run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(&files)));
    let aligned = run(&mut engine, "align.run", "{}");
    assert_eq!(aligned["moved"], json!([]), "{aligned}");
}
