//! Slabbing, `stack.slabbed`, on synthetic stacks written as frame files: on a 300-frame stack of occluding layers,
//! pyramid slabs then a depth map of the slabs score about as well as the unslabbed pyramid, far better than the
//! unslabbed depth map, and a new depth-map setting reprocesses only the slabs, several times faster than a stack of
//! the frames; slabs are outputs of their own, reused while what they are made from stays the same, and the whole
//! stack is one undo step; wide framing's coverage goes through the slabs; and what can't be slabbed is refused. Each
//! test prints what it measured (`cargo test -p florastack-engine --test slab -- --nocapture`).
//!
//! The tests run one at a time, so that the timings of the 300-frame test aren't another test's.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::{MARGIN, TempDir, breathing, config, path_list, run, state, synthetic_frames, write_frames};
use florastack_core::testutil::{Backdrop, StackParams, generate_layered, scene};
use florastack_core::{AnyImage, ColorSpace, Encoding, Image, Rect};
use florastack_engine::{CommandError, Engine, Value, json};
use florastack_metrics::ssim;

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

/// An engine with a new project in `dir` holding `files`, in that order, aligned with `align` (`align.set`'s
/// parameters).
fn aligned_project(dir: &Path, files: &[PathBuf], align: &str) -> Engine {
    let mut engine = Engine::new(config(dir));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Deep", "folder": {} }}"#, quoted(dir)));
    let added = run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(files)));
    assert_eq!(added["added"].as_array().unwrap().len(), files.len(), "{added}");
    run(&mut engine, "align.set", align);
    run(&mut engine, "align.run", "{}");
    engine
}

/// The pixels of the project's output `id`, as stored.
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

/// The SSIM of output `summary["output"]`'s color against `truth`.
fn score(engine: &Engine, summary: &Value, truth: &Image<f32>) -> f64 {
    let image = output_image(engine, summary["output"].as_u64().unwrap());
    let rgb = Image::from_fn(image.width(), image.height(), 3, ColorSpace::LINEAR, |x, y, c| image.get(x, y, c).unwrap()).unwrap();
    ssim(&rgb, truth).unwrap()
}

/// How many pixels of `image` have an alpha of exactly 0.
#[allow(clippy::float_cmp)] // Alpha is exactly 0 or 1.
fn transparent(image: &Image<f32>) -> u64 {
    let alpha = image.channels() - 1;
    let pixels = (0..image.height()).flat_map(|y| (0..image.width()).map(move |x| (x, y)));
    pixels.filter(|&(x, y)| image.get(x, y, alpha) == Some(0.0)).count() as u64
}

/// A deep stack of a scene whose layers occlude one another: `frames` frames focused through the scene's depths (0.3
/// to 1), each sharp over a few frames only (a quarter of a pixel more blur a frame away from focus, up to 8), lightly
/// noisy, in register, cropped by `MARGIN`; and the sharp truth.
fn deep_stack(width: usize, height: usize, frames: usize, seed: u64) -> (Vec<Image<f32>>, Image<f32>) {
    let scene = scene::layered(width + 2 * MARGIN, height + 2 * MARGIN, 1.0, Backdrop::Textured, seed).unwrap();
    #[allow(clippy::cast_precision_loss)]
    let per_frame = 0.7 / frames as f32;
    let params =
        StackParams { blur_per_depth: 0.25 / per_frame, max_blur: 8.0, read_noise: 0.003, shot_noise: 0.005, seed, ..StackParams::identity(frames) };
    let stack = generate_layered(&scene, &params).unwrap();
    let inner = Rect::new(MARGIN, MARGIN, width, height);
    let crop = |image: &Image<f32>| image.crop(inner).unwrap();
    (stack.frames.iter().map(crop).collect(), crop(&stack.truth.sharp))
}

/// Runs a command and times it.
fn timed(engine: &mut Engine, name: &str, params: &str) -> (Value, Duration) {
    let start = Instant::now();
    let value = run(engine, name, params);
    (value, start.elapsed())
}

fn slabs(summary: &Value) -> &Vec<Value> {
    summary["slabs"].as_array().unwrap()
}

/// The slabs' outputs' ids.
fn ids_of(summary: &Value) -> Vec<u64> {
    slabs(summary).iter().map(|s| s["output"].as_u64().unwrap()).collect()
}

fn reused(summary: &Value) -> usize {
    slabs(summary).iter().filter(|s| s["reused"] == true).count()
}

#[test]
fn slabbed_pyramid_then_depth_map_matches_unslabbed_quality_on_300_frames_and_runs_faster() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = TempDir::new("slab-300");
    let (frames, truth) = deep_stack(200, 150, 300, 71);
    let best = frames.iter().map(|f| ssim(f, &truth).unwrap()).fold(0.0, f64::max);
    let files = write_frames(dir.path(), "stack", &frames);
    drop(frames);
    // The frames are in register, as a perfect rail shoots them: chained over 300 frames, many with next to no detail
    // in common, alignment with every term drifts (a roadmap follow-up of 3.4).
    let mut engine =
        aligned_project(dir.path(), &files, r#"{ "shift_x": false, "shift_y": false, "scale": false, "rotation": false, "brightness": false }"#);
    // The depth map's estimation and smoothing at the scene's scale (its defaults are for camera frames), for both.
    run(&mut engine, "stack.set", r#"{ "depth_map": { "radius": 1.0, "smoothing": 2.0 } }"#);

    let (pyramid, t_pyramid) = timed(&mut engine, "stack.pyramid", "{}");
    let (depth, t_depth) = timed(&mut engine, "stack.depthMap", "{}");
    let (slabbed, t_slabbed) = timed(&mut engine, "stack.slabbed", r#"{ "final": { "method": "depth-map" } }"#);
    // Exploring the final stack's settings: only the slabs' outputs are stacked again.
    let (explored, t_explored) = timed(&mut engine, "stack.slabbed", r#"{ "final": { "method": "depth-map", "smoothing": 4.0 } }"#);
    // The default: a pyramid of the pyramid slabs.
    let (pyramids, t_pyramids) = timed(&mut engine, "stack.slabbed", "{}");
    let [s_pyramid, s_depth, s_slabbed, s_explored, s_pyramids] =
        [&pyramid, &depth, &slabbed, &explored, &pyramids].map(|s| score(&engine, s, &truth));
    println!("the sharpest frame: SSIM {best:.4}");
    for (what, s, t) in [
        ("pyramid", s_pyramid, t_pyramid),
        ("depth map", s_depth, t_depth),
        ("slabbed, pyramid then depth map", s_slabbed, t_slabbed),
        ("again, depth map smoothing 4", s_explored, t_explored),
        ("again, pyramid then pyramid", s_pyramids, t_pyramids),
    ] {
        println!("{what}: SSIM {s:.4} in {:.2} s", t.as_secs_f64());
    }

    // 22 slabs of 18 frames, neighbors sharing 4 or 5, every frame in.
    assert_eq!((slabbed["slabSize"].as_u64(), slabbed["overlap"].as_u64(), slabs(&slabbed).len()), (Some(18), Some(4), 22), "{slabbed}");
    assert_eq!(slabbed["frames"].as_array().unwrap().len(), 300);
    assert_eq!((reused(&slabbed), reused(&explored), reused(&pyramids)), (0, 22, 22));
    // Quality: about the unslabbed pyramid's (0.8857 against 0.8679 measured), far above the unslabbed depth map's
    // (0.7333), which a stack this deep defeats; a pyramid of pyramid slabs is better than either (0.8961).
    assert!(s_slabbed > s_pyramid - 0.03, "slabbed {s_slabbed:.4}, pyramid {s_pyramid:.4}");
    assert!(s_slabbed > s_depth + 0.08, "slabbed {s_slabbed:.4}, depth map {s_depth:.4}");
    assert!(s_pyramids > s_pyramid - 0.01, "{s_pyramids:.4} against {s_pyramid:.4}");
    // Speed: a new setting of the final stack reprocesses 22 slabs instead of 300 frames, ten times faster than the
    // unslabbed depth map measured (0.94 s against 9.32 s in the debug build) and five times the pyramid (5.24 s).
    assert!(t_explored * 4 < t_depth, "{t_explored:?} against {t_depth:?}");
    assert!(t_explored * 3 < t_pyramid, "{t_explored:?} against {t_pyramid:?}");
    // The first run costs more than a stack of the frames: it reads and stacks the frames neighbors share for each slab
    // (396 frames for 300), and finishes and stores 22 slabs. Measured 9.49 s, 1.81 times the pyramid's time; the
    // bound catches a slab stage gone wrong, as when two slabs open at once spilled their accumulators (nine times).
    assert!(t_slabbed < t_pyramid * 3, "{t_slabbed:?} against {t_pyramid:?}");
}

#[test]
fn slabs_are_outputs_reused_while_what_they_are_made_from_stays_the_same() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = TempDir::new("slab-reuse");
    let files = synthetic_frames(dir.path(), "stack", 160, 120, &breathing(12, 0.01, None, 72));
    let mut engine = aligned_project(dir.path(), &files, "{}");
    let before = state(&engine);
    let steps = run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len();

    // Twelve frames: four slabs of four, neighbors sharing one, by default.
    let slabbed = run(&mut engine, "stack.slabbed", "{}");
    println!("{slabbed}");
    assert_eq!((slabbed["slabSize"].as_u64(), slabbed["overlap"].as_u64(), slabs(&slabbed).len()), (Some(4), Some(1), 4));
    let starts: Vec<usize> = slabs(&slabbed).iter().map(|s| usize::try_from(s["start"].as_u64().unwrap()).unwrap()).collect();
    assert_eq!(starts, [0, 3, 5, 8]);
    assert_eq!((slabbed.get("depth"), &slabbed["finalSettings"]["grit"]), (None, &json!(0.5)), "a pyramid by default: {slabbed}");
    assert!(slabbed["range"]["outOfRange"].as_u64().is_some(), "{slabbed}");
    let listed = run(&mut engine, "output.list", "{}");
    let outputs = listed["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 5, "four slabs and the final output");
    let ids: Vec<u64> = slabbed["frames"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    for (j, slab) in outputs[..4].iter().enumerate() {
        assert_eq!((&slab["method"], &slab["name"]), (&json!("pyramid"), &json!(format!("Pyramid slab {} of 4", j + 1))));
        assert_eq!(slab["parameters"]["slab"], json!({ "index": j, "of": 4 }));
        assert_eq!(slab["frames"], json!(ids[starts[j]..starts[j] + 4]), "a slab is made from its frames");
        assert_eq!(slab["id"], slabs(&slabbed)[j]["output"]);
    }
    let last = &outputs[4];
    let slab_ids: Vec<Value> = outputs[..4].iter().map(|o| o["id"].clone()).collect();
    assert_eq!((&last["method"], &last["name"], &last["outputs"]), (&json!("pyramid"), &json!("Slabbed pyramid"), &json!(slab_ids)));
    assert_eq!(last["parameters"]["slabs"]["method"], "pyramid");
    assert_eq!((&last["parameters"]["slabs"]["size"], &last["parameters"]["slabs"]["count"]), (&json!(4), &json!(4)));
    let key = engine.project().unwrap().outputs[0].image.key.clone().unwrap();
    assert!(key.starts_with("slab sha256:"), "{key}");
    // Slabs read back from the cache as linear light, as they were stacked, and the final stack keeps it.
    let (slab, last_image) = (output_image(&engine, ids_of(&slabbed)[0]), output_image(&engine, slabbed["output"].as_u64().unwrap()));
    assert_eq!(slab.color_space().encoding, Encoding::Linear);
    assert_eq!(last_image.color_space(), slab.color_space());

    // One undo step for the five outputs.
    let history = run(&mut engine, "edit.history", "{}");
    assert_eq!(history["undo"].as_array().unwrap().len(), steps + 1);
    assert_eq!(run(&mut engine, "edit.undo", "{}")["label"], "Stack in slabs");
    assert_eq!(state(&engine), before);
    run(&mut engine, "edit.redo", "{}");
    assert_eq!(engine.project().unwrap().outputs.len(), 5);

    // Another final stack takes the slabs as they are: one new output.
    let average = run(&mut engine, "stack.slabbed", r#"{ "final": { "method": "average" }, "name": "Average of slabs" }"#);
    assert_eq!(reused(&average), 4);
    assert_eq!(engine.project().unwrap().outputs.len(), 6);
    let outputs_of = |s: &Value| slabs(s).iter().map(|s| s["output"].clone()).collect::<Vec<_>>();
    assert_eq!(outputs_of(&average), outputs_of(&slabbed));
    // A slab removed is stacked again, the others are reused.
    let gone = slabs(&slabbed)[2]["output"].as_u64().unwrap();
    run(&mut engine, "output.remove", &format!(r#"{{ "output": {gone} }}"#));
    let again = run(&mut engine, "stack.slabbed", "{}");
    assert_eq!(slabs(&again).iter().map(|s| s["reused"] == true).collect::<Vec<_>>(), [true, true, false, true]);
    // Without reuse, or with other slab settings, every slab is stacked again.
    assert_eq!(reused(&run(&mut engine, "stack.slabbed", r#"{ "reuse": false }"#)), 0);
    assert_eq!(reused(&run(&mut engine, "stack.slabbed", r#"{ "slabs": { "grit": 0.25 } }"#)), 0);
    // A frame disabled moves the slabs around it: of the eleven frames' four slabs, the first and the last hold the
    // same frames as before, and only they are reused.
    run(&mut engine, "project.setFramesEnabled", &format!(r#"{{ "frames": [{}], "enabled": false }}"#, ids[5]));
    let fewer = run(&mut engine, "stack.slabbed", "{}");
    assert_eq!(fewer["frames"].as_array().unwrap().len(), 11);
    assert_eq!(slabs(&fewer).iter().map(|s| s["reused"] == true).collect::<Vec<_>>(), [true, false, false, true], "{fewer}");
}

#[test]
fn slabs_carry_their_frames_coverage_into_the_final_stack() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = TempDir::new("slab-wide");
    // Nine frames, each 2% larger than the one before, shifted and turned a little: in wide framing, the canvas's
    // corners are seen by no frame, and the slab at the narrow end sees less of the canvas than the stack.
    let files = synthetic_frames(dir.path(), "stack", 160, 120, &breathing(9, 0.02, None, 73));
    let mut engine = aligned_project(dir.path(), &files, r#"{ "framing": "wide" }"#);
    let average = run(&mut engine, "stack.average", "{}");
    // Depth-map slabs, which read their frames twice, stacked by weighted average.
    let slabbed = run(&mut engine, "stack.slabbed", r#"{ "slabs": { "method": "depth-map" }, "final": { "method": "average" }, "slabSize": 4 }"#);
    println!("{slabbed}");
    assert_eq!((slabs(&slabbed).len(), &slabbed["slabSettings"]["threshold"]), (3, &json!("auto")), "{slabbed}");
    let uncovered = slabbed["uncovered"].as_u64().unwrap();
    assert!(uncovered > 0, "{slabbed}");
    assert_eq!(uncovered, average["uncovered"].as_u64().unwrap(), "the slabs cover what their frames cover");
    let image = output_image(&engine, slabbed["output"].as_u64().unwrap());
    assert_eq!(image.channels(), 4);
    assert_eq!(transparent(&image), uncovered);
    // Each slab keeps its own coverage, as its alpha: the slab at the narrow end, of its frames alone, covers less.
    let covers: Vec<u64> = slabs(&slabbed).iter().map(|s| transparent(&output_image(&engine, s["output"].as_u64().unwrap()))).collect();
    println!("slabs' uncovered pixels: {covers:?}, the stack's {uncovered}");
    assert!(covers.iter().all(|&c| c >= uncovered) && covers.iter().any(|&c| c > uncovered), "{covers:?}");
}

#[test]
fn what_cant_be_slabbed_is_refused_and_cancelling_changes_nothing() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = TempDir::new("slab-refused");
    let files = synthetic_frames(dir.path(), "stack", 160, 120, &breathing(6, 0.01, None, 74));
    let mut engine = aligned_project(dir.path(), &files[..2], "{}");
    let e = engine.dispatch_json("stack.slabbed", "{}").unwrap_err();
    assert_eq!(e.code(), "unavailable", "{e}");
    assert!(e.to_string().contains("at least 3 frames"), "{e}");
    run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(&files[2..])));
    let e = engine.dispatch_json("stack.slabbed", "{}").unwrap_err();
    assert!(e.to_string().contains("align.run"), "{e}");
    run(&mut engine, "align.run", "{}");
    // Six frames: slabs of 3 sharing 1 by default.
    for (bad, field) in [
        (r#"{ "slabSize": 6 }"#, "slabSize"),
        (r#"{ "overlap": 3 }"#, "overlap"),
        (r#"{ "slabSize": 2, "overlap": 2 }"#, "overlap"),
        (r#"{ "final": { "threshold": "auto" } }"#, "final.threshold"),
        (r#"{ "slabs": { "method": "average", "threshold": "auto" } }"#, "slabs.threshold"),
    ] {
        match engine.dispatch_json("stack.slabbed", bad).unwrap_err() {
            CommandError::Invalid { source, .. } => assert_eq!(source.field, field, "{bad}: {source}"),
            other => panic!("{bad}: {other}"),
        }
    }
    let before = state(&engine);
    let job = common::job_of(engine.dispatch_json("stack.slabbed", "{}").unwrap());
    assert!(engine.cancel(job));
    assert!(matches!(engine.wait(job), Err(CommandError::Cancelled)));
    assert_eq!(state(&engine), before);
}
