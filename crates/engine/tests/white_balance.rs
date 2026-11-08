//! White balance through the engine: with `align.set`'s `white_balance`, `align.run` stores each frame's gains per color
//! channel, `align.info` reports them, and stacking applies them, so that a stack whose frames' white balance changes
//! comes out in the master's colors; without it, nothing changes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use common::{MARGIN, TempDir, config, path_list, run, write_frames};
use florastack_core::testutil::{StackParams, generate, scene};
use florastack_core::{AnyImage, ColorSpace, Image, Rect};
use florastack_engine::{Engine, json};
use florastack_metrics::color_shift;

const WIDTH: usize = 240;
const HEIGHT: usize = 180;

/// A defocused stack whose white balance changes from frame to frame, as 16-bit files; the sharp truth, and each
/// frame's color gains times its brightness gain.
fn stack(dir: &Path) -> (Vec<std::path::PathBuf>, Image<f32>, Vec<[f32; 3]>) {
    let (sharp, depth) = scene::textured(WIDTH + 2 * MARGIN, HEIGHT + 2 * MARGIN, 31).unwrap();
    let params =
        StackParams { blur_per_depth: 8.0, max_blur: 8.0, color_jitter: 0.08, brightness_jitter: 0.03, seed: 31, ..StackParams::identity(6) };
    let made = generate(&sharp, &depth, &params).unwrap();
    let inner = Rect::new(MARGIN, MARGIN, WIDTH, HEIGHT);
    let frames: Vec<Image<f32>> = made.frames.iter().map(|f| f.crop(inner).unwrap()).collect();
    let colors = made.truth.frames.iter().map(|t| t.channel_gains.map(|g| g * t.gain)).collect();
    (write_frames(dir, "frames", &frames), sharp.crop(inner).unwrap(), colors)
}

fn output_image(engine: &Engine, id: &serde_json::Value) -> Image<f32> {
    let project = engine.project().unwrap();
    let output = project.outputs.iter().find(|o| o.id().get() == id.as_u64().unwrap()).unwrap();
    let path = output.image.path.resolve(engine.project_path().unwrap());
    match florastack_codecs::read_file(&path, &florastack_codecs::ReadOptions::default()).unwrap().image {
        AnyImage::F32(image) => image,
        other => panic!("an output is stored as f32, not {:?}", other.format()),
    }
}

#[test]
fn a_stack_whose_white_balance_changes_comes_out_in_the_masters_colors() {
    let dir = TempDir::new("white-balance");
    let (files, sharp, colors) = stack(dir.path());
    let mut shifts = Vec::new();
    for corrected in [false, true] {
        let folder = dir.path().join(format!("project-{corrected}"));
        std::fs::create_dir_all(&folder).unwrap();
        let mut engine = Engine::new(config(&folder));
        let quoted = serde_json::to_string(&folder.display().to_string()).unwrap();
        run(&mut engine, "project.new", &format!(r#"{{ "name": "Stack", "folder": {quoted} }}"#));
        let added = run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(&files)));
        let ids: Vec<u64> = added["added"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        run(
            &mut engine,
            "align.set",
            &json!({ "white_balance": corrected, "shift_x": false, "shift_y": false, "scale": false, "rotation": false }).to_string(),
        );
        run(&mut engine, "align.run", "{}");
        let info = run(&mut engine, "align.info", "{}");
        let master = info["run"]["master"].as_u64().unwrap();
        for f in info["run"]["frames"].as_array().unwrap() {
            let has = f.get("whiteBalance").is_some();
            assert_eq!(has, corrected && f["frame"] != master, "{f}");
        }
        // The output is in the master's colors: the truth, through the master's gains.
        let stacked = run(&mut engine, "stack.average", r#"{ "radius": 1.0, "smoothing": 1.0 }"#);
        let output = output_image(&engine, &stacked["output"]);
        let gains = colors[ids.iter().position(|&i| i == master).unwrap()];
        let truth = Image::from_fn(WIDTH, HEIGHT, 3, ColorSpace::LINEAR, |x, y, c| sharp.get(x, y, c).unwrap() * gains[c]).unwrap();
        let shift = color_shift(&output, &truth).unwrap().mean;
        println!("white balance {corrected}: mean deltaE {shift:.3}");
        shifts.push(shift);
    }
    assert!(shifts[1] < 0.6 * shifts[0], "corrected {} against uncorrected {}", shifts[1], shifts[0]);
}
