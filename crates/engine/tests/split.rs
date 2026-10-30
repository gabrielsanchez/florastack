//! Finding the stacks in a batch, `project.findStacks`, on synthetic stacks written as frame files into one folder:
//! three stacks shot one after another (a scene, the same scene framed anew, another scene) are cut exactly where each
//! starts, with no project open; a file that can't be read is left out without cutting its stack.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{TempDir, config, run, write_frames};
use florastack_core::ops::resize;
use florastack_core::testutil::{StackParams, generate, scene};
use florastack_core::{Image, Rect};
use florastack_engine::{Engine, json};

const WIDTH: usize = 240;
const HEIGHT: usize = 180;

/// A stack of `frames` of the textured scene of `seed`, focused from its near end to its far end, seen through a
/// window `zoom` times smaller than the frame (1 for none) in the middle of the scene.
fn stack(frames: usize, seed: u64, zoom: f64) -> Vec<Image<f32>> {
    let (sharp, depth) = scene::textured(WIDTH * 2, HEIGHT * 2, seed).unwrap();
    let params = StackParams { blur_per_depth: 10.0, ..StackParams::clean(frames, seed) };
    let generated = generate(&sharp, &depth, &params).unwrap();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (w, h) = ((WIDTH as f64 / zoom).round() as usize, (HEIGHT as f64 / zoom).round() as usize);
    let window = Rect::new(WIDTH - w / 2, HEIGHT - h / 2, w, h);
    generated.frames.iter().map(|f| resize(&f.crop(window).unwrap(), WIDTH, HEIGHT).unwrap()).collect()
}

#[test]
fn three_concatenated_stacks_are_split_apart() {
    let dir = TempDir::new("split-three");
    let batch: Vec<Image<f32>> = [stack(8, 51, 1.0), stack(8, 51, 1.25), stack(8, 52, 1.0)].concat();
    let files = write_frames(dir.path(), "batch", &batch);
    // A file with a frame's name that isn't one, among the first stack's frames, and a file that isn't a frame.
    let folder = dir.path().join("batch");
    std::fs::write(folder.join("frame-03b.tif"), b"not a TIFF").unwrap();
    std::fs::write(folder.join("notes.txt"), b"three stacks").unwrap();
    let mut engine = Engine::new(config(dir.path()));
    let found =
        run(&mut engine, "project.findStacks", &format!(r#"{{ "paths": [{}] }}"#, serde_json::to_string(&folder.display().to_string()).unwrap()));
    println!("{}", serde_json::to_string_pretty(&found["boundaries"]).unwrap());
    let stacks: Vec<Vec<String>> = found["stacks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_owned()).collect())
        .collect();
    let expected: Vec<Vec<String>> = files.chunks(8).map(|c| c.iter().map(|p| p.display().to_string()).collect()).collect();
    assert_eq!(stacks, expected);
    let boundaries = found["boundaries"].as_array().unwrap();
    assert_eq!(boundaries.iter().map(|b| b["index"].clone()).collect::<Vec<_>>(), [json!(8), json!(16)]);
    assert_eq!(boundaries[0]["file"], json!(files[8].display().to_string()));
    for b in boundaries {
        assert_eq!(b["reasons"][0]["reason"], "scene", "{b}");
    }
    // The broken file is left out, and named; the synthetic files have no capture times, so no gap is chosen.
    let skipped = found["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{found}");
    assert!(skipped[0]["path"].as_str().unwrap().ends_with("frame-03b.tif"), "{found}");
    assert_eq!(found.get("gap"), None);
    assert!(engine.project().is_none(), "no project needed, none opened");

    // The files given one by one, in another order: the stacks follow it.
    let reordered: Vec<String> = files[16..].iter().chain(&files[..8]).map(|p| serde_json::to_string(&p.display().to_string()).unwrap()).collect();
    let found = run(&mut engine, "project.findStacks", &format!(r#"{{ "paths": [{}] }}"#, reordered.join(", ")));
    let sizes: Vec<usize> = found["stacks"].as_array().unwrap().iter().map(|s| s["files"].as_array().unwrap().len()).collect();
    assert_eq!(sizes, [8, 8]);
    // A gap given that no frame's capture time can say is reported as given.
    let found = run(&mut engine, "project.findStacks", &format!(r#"{{ "paths": [{}], "split": {{ "gap": 5 }} }}"#, reordered[0]));
    assert_eq!((found["stacks"].as_array().unwrap().len(), &found["gap"]), (1, &json!(5.0)), "{found}");
}
