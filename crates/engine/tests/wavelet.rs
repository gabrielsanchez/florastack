//! The wavelet method through the engine: `stack.wavelet` stacks the aligned frames into a new output as sharp as the
//! pyramid's on a scene whose layers occlude one another, records its parameters and is one undo step; its settings
//! apply to one stack or to the project and are checked against the other methods'; `stack.preview` and
//! `stack.slabbed` take it; and it stacks gray and color frames read from 8- and 16-bit files, and stacks of two.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use common::{MARGIN, TempDir, config, path_list, run, state};
use florastack_codecs::{BitDepth, OutputFormat, OutputMetadata, OutputProfile, OutputSpec, TiffCompression};
use florastack_core::testutil::{Backdrop, StackParams, generate_layered, scene};
use florastack_core::{AnyImage, ColorSpace, Encoding, Image, Rect};
use florastack_engine::{Engine, json};
use florastack_metrics::ssim;

/// An engine with a new project in `dir` holding `files`, in that order, aligned.
fn aligned(dir: &Path, files: &[PathBuf]) -> Engine {
    std::fs::create_dir_all(dir).unwrap();
    let mut engine = Engine::new(config(dir));
    let folder = serde_json::to_string(&dir.display().to_string()).unwrap();
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Stack", "folder": {folder} }}"#));
    let added = run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(files)));
    assert_eq!(added["added"].as_array().unwrap().len(), files.len(), "{added}");
    run(&mut engine, "align.run", "{}");
    engine
}

/// The pixels of the project's output `id`, as linear `f32`.
fn output_image(engine: &Engine, id: &serde_json::Value) -> Image<f32> {
    let project = engine.project().unwrap();
    let output = project.outputs.iter().find(|o| o.id().get() == id.as_u64().unwrap()).unwrap();
    let path = output.image.path.resolve(engine.project_path().unwrap());
    let options = florastack_codecs::ReadOptions { orientation: florastack_codecs::OrientationPolicy::Ignore, ..Default::default() };
    match florastack_codecs::read_file(&path, &options).unwrap().image {
        AnyImage::F32(image) => image,
        other => panic!("an output is stored as f32, not {:?}", other.format()),
    }
}

/// A layered scene's frames (bristles over discs over a textured backdrop), not moved, and the sharp truth, cropped by
/// `MARGIN` like the other synthetic frames.
fn layered(width: usize, height: usize, frames: usize, seed: u64) -> (Vec<Image<f32>>, Image<f32>) {
    let scene = scene::layered(width + 2 * MARGIN, height + 2 * MARGIN, 1.0, Backdrop::Textured, seed).unwrap();
    let params =
        StackParams { focus_range: Some((0.0, 1.0)), blur_per_depth: 8.0, max_blur: 8.0, blur_step: 0.5, seed, ..StackParams::identity(frames) };
    let stack = generate_layered(&scene, &params).unwrap();
    let inner = Rect::new(MARGIN, MARGIN, width, height);
    (stack.frames.iter().map(|f| f.crop(inner).unwrap()).collect(), stack.truth.sharp.crop(inner).unwrap())
}

/// `image` as one gray channel: its channels' mean.
fn gray(image: &Image<f32>) -> Image<f32> {
    let c = image.channels();
    let values = image.as_raw().chunks_exact(c).map(|p| p.iter().sum::<f32>() / c as f32).collect();
    Image::from_vec(image.width(), image.height(), 1, ColorSpace::LINEAR, values).unwrap()
}

/// Linear frames as sRGB TIFF files of `depth` (8 or 16 bits) `dir/<name>/frame-00.tif` and on.
fn write(dir: &Path, name: &str, frames: &[Image<f32>], depth: BitDepth) -> Vec<PathBuf> {
    let folder = dir.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    let spec = OutputSpec {
        format: OutputFormat::Tiff { compression: TiffCompression::Deflate },
        depth,
        profile: OutputProfile::Input,
        extended_range: false,
    };
    frames
        .iter()
        .enumerate()
        .map(|(i, frame)| {
            let encoded = match depth {
                BitDepth::U8 => AnyImage::U8(frame.to_encoding::<u8>(Encoding::Srgb).unwrap()),
                _ => AnyImage::U16(frame.to_encoding::<u16>(Encoding::Srgb).unwrap()),
            };
            let path = folder.join(format!("frame-{i:02}.tif"));
            std::fs::write(&path, florastack_codecs::write(&encoded, &spec, &OutputMetadata::default()).unwrap().bytes).unwrap();
            path
        })
        .collect()
}

#[test]
fn a_wavelet_stack_is_as_sharp_as_the_pyramid_on_occluding_layers_and_becomes_an_output() {
    let dir = TempDir::new("wavelet-layers");
    let (frames, truth) = layered(240, 180, 10, 31);
    let files = write(dir.path(), "stack", &frames, BitDepth::U16);
    let mut engine = aligned(dir.path(), &files);
    let before = state(&engine);
    let steps = run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len();
    let average = run(&mut engine, "stack.average", r#"{ "radius": 1.0, "smoothing": 1.0 }"#);
    let pyramid = run(&mut engine, "stack.pyramid", "{}");
    let wavelet = run(&mut engine, "stack.wavelet", r#"{ "name": "Wavelet" }"#);
    println!("{wavelet}");
    let score = |summary: &serde_json::Value| ssim(&output_image(&engine, &summary["output"]), &truth).unwrap();
    let (a, p, w) = (score(&average), score(&pyramid), score(&wavelet));
    println!("SSIM: weighted average {a:.4}, pyramid {p:.4}, wavelet {w:.4}");
    assert!(w > a + 0.005, "{w:.4} against the average's {a:.4}");
    assert!(w > p - 0.01, "{w:.4} against the pyramid's {p:.4}");
    assert_eq!((wavelet["width"].as_u64(), wavelet["height"].as_u64(), wavelet["uncovered"].as_u64()), (Some(240), Some(180), Some(0)));
    assert_eq!(wavelet["levels"], 3, "log2 of the short side, less 4, as the pyramid's");
    assert_eq!(wavelet["range"]["extended"], json!(wavelet["range"]["outOfRange"].as_u64().unwrap() > 0), "{wavelet}");
    assert_eq!((&wavelet["settings"]["window"], &wavelet["settings"]["lowpass"]), (&json!(3), &json!("focus")));
    assert_eq!(wavelet["frames"].as_array().unwrap().len(), 10);

    // Listed with its method and parameters, each stack one undo step.
    let listed = run(&mut engine, "output.list", "{}");
    let entry = &listed["outputs"][2];
    assert_eq!((&entry["name"], &entry["method"]), (&json!("Wavelet"), &json!("wavelet")));
    assert_eq!(
        entry["parameters"],
        json!({
            "levels": 3, "wavelet": "bior22", "window": 3, "consistency": true, "lowpass": "focus", "grit": 0.5, "rangeLimit": 1.0,
            "tone": "compress", "framing": "narrow"
        })
    );
    assert_eq!(run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len(), steps + 3);
    assert_eq!(run(&mut engine, "edit.undo", "{}")["label"], "Stack by wavelet");
    run(&mut engine, "edit.undo", "{}");
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(state(&engine), before);

    // Settings for one stack apply to it alone; the project's apply to the next.
    let other = run(&mut engine, "stack.wavelet", r#"{ "window": 5, "lowpass": "average", "wavelet": "db2", "consistency": false, "levels": 2 }"#);
    let used = &other["settings"];
    assert_eq!(
        (&used["window"], &used["lowpass"], &used["wavelet"], &used["consistency"]),
        (&json!(5), &json!("average"), &json!("db2"), &json!(false))
    );
    assert_eq!(other["levels"], 2);
    assert_eq!(run(&mut engine, "stack.get", "{}")["settings"]["wavelet"]["window"], 3);
    run(&mut engine, "stack.set", r#"{ "wavelet": { "lowpass": "select", "tone": "keep" } }"#);
    let project = run(&mut engine, "stack.wavelet", "{}");
    assert_eq!((&project["settings"]["lowpass"], &project["settings"]["tone"]), (&json!("select"), &json!("keep")));
    assert_eq!(project["range"], json!({ "outOfRange": 0, "extended": false }), "kept: nothing is compressed");
    for bad in [
        r#"{ "window": 4 }"#,
        r#"{ "grit": 0.1 }"#,
        r#"{ "rangeLimit": 2 }"#,
        r#"{ "levels": 11 }"#,
        r#"{ "lowpass": "median" }"#,
        r#"{ "wavelet": "haar" }"#,
        r#"{ "decisions": "channels" }"#,
        r#"{ "radius": 1 }"#,
        r#"{ "name": "" }"#,
    ] {
        assert_eq!(engine.dispatch_json("stack.wavelet", bad).unwrap_err().code(), "params", "{bad}");
    }
    for bad in [r#"{ "wavelet": { "window": 7 } }"#, r#"{ "wavelet": { "grit": 2 } }"#, r#"{ "wavelet": { "decisions": "channels" } }"#] {
        assert!(engine.dispatch_json("stack.set", bad).is_err(), "{bad}");
    }
}

#[test]
fn the_preview_and_slabbing_take_the_wavelet_method_and_its_settings_alone() {
    let dir = TempDir::new("wavelet-preview");
    let (frames, _) = layered(160, 120, 8, 33);
    let files = write(dir.path(), "stack", &frames, BitDepth::U16);
    let mut engine = aligned(dir.path(), &files);
    let before = state(&engine);
    let previewed = run(&mut engine, "stack.preview", r#"{ "method": "wavelet", "window": 5, "grit": 0.25 }"#);
    assert_eq!((previewed["width"].as_u64(), previewed["height"].as_u64()), (Some(40), Some(30)), "{previewed}");
    assert_eq!(
        (&previewed["method"], &previewed["settings"]["window"], &previewed["settings"]["grit"]),
        (&json!("wavelet"), &json!(5), &json!(0.25))
    );
    assert!(Path::new(previewed["path"].as_str().unwrap()).is_file());
    assert_eq!(state(&engine), before, "a preview changes nothing");
    for bad in [
        r#"{ "method": "wavelet", "decisions": "channels" }"#,
        r#"{ "method": "wavelet", "coarseLimit": 2 }"#,
        r#"{ "method": "wavelet", "threshold": "auto" }"#,
        r#"{ "method": "wavelet", "power": 2 }"#,
        r#"{ "method": "wavelet", "window": 2 }"#,
        r#"{ "method": "pyramid", "window": 5 }"#,
        r#"{ "method": "depth-map", "lowpass": "focus" }"#,
        r#"{ "lowpass": "focus" }"#,
    ] {
        assert_eq!(engine.dispatch_json("stack.preview", bad).unwrap_err().code(), "params", "{bad}");
    }

    // Slabs by the wavelet method, stacked by the wavelet method: two passes at each stage.
    let slabbed = run(
        &mut engine,
        "stack.slabbed",
        r#"{ "slabSize": 4, "overlap": 1, "slabs": { "method": "wavelet" }, "final": { "method": "wavelet", "window": 5 } }"#,
    );
    let outputs = run(&mut engine, "output.list", "{}")["outputs"].as_array().unwrap().clone();
    let last = outputs.last().unwrap();
    assert_eq!(last["method"], "wavelet", "{slabbed}");
    assert_eq!(last["parameters"]["window"], 5);
    assert!(outputs[..outputs.len() - 1].iter().all(|o| o["method"] == "wavelet" && o["parameters"]["window"] == 3), "{outputs:?}");
}

/// A stack to try: its name, its frames, its truth and the files' depth.
type Case<'a> = (&'a str, &'a [Image<f32>], &'a Image<f32>, BitDepth);

#[test]
fn gray_and_color_frames_of_8_and_16_bits_and_two_frame_stacks_stack() {
    let dir = TempDir::new("wavelet-depths");
    let (frames, truth) = layered(160, 120, 6, 35);
    let grays: Vec<Image<f32>> = frames.iter().map(gray).collect();
    let gray_truth = gray(&truth);
    let cases: [Case<'_>; 5] = [
        ("color-16", &frames, &truth, BitDepth::U16),
        ("color-8", &frames, &truth, BitDepth::U8),
        ("gray-16", &grays, &gray_truth, BitDepth::U16),
        ("gray-8", &grays, &gray_truth, BitDepth::U8),
        ("two-frames", &frames[2..4], &truth, BitDepth::U16),
    ];
    for (name, frames, truth, depth) in cases {
        let files = write(dir.path(), name, frames, depth);
        let mut engine = aligned(&dir.path().join(format!("{name}-project")), &files);
        let pyramid = run(&mut engine, "stack.pyramid", "{}");
        let wavelet = run(&mut engine, "stack.wavelet", "{}");
        let (p, w) = (output_image(&engine, &pyramid["output"]), output_image(&engine, &wavelet["output"]));
        assert_eq!((w.width(), w.height(), w.channels()), (p.width(), p.height(), p.channels()), "{name}");
        assert_eq!(w.channels(), truth.channels(), "{name}: gray stays gray");
        assert!(w.as_raw().iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{name}: in range");
        let (sp, sw) = (ssim(&p, truth).unwrap(), ssim(&w, truth).unwrap());
        println!("{name}: SSIM pyramid {sp:.4}, wavelet {sw:.4}");
        assert!(sw > sp - 0.02, "{name}: {sw:.4} against the pyramid's {sp:.4}");
    }
}
