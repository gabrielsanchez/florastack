//! Exporting and presets, end to end through the commands: exported files reopen with the bit depth, color profile
//! and metadata the export settings asked for, with the maps and aligned frames alongside; presets, full and partial,
//! saved and applied again, give back the same preferences; and every built-in preset is valid.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use common::{TempDir, config, run};
use florastack_codecs::{Format, Metadata, NamedSpace, OutputMetadata, OutputProfile, OutputSpec, ReadOptions, WideSpace};
use florastack_core::{AnyImage, Encoding, Image};
use florastack_engine::{Engine, Value, json};

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

/// XMP naming a camera and a title, the title telling which frame it is.
fn xmp(title: &str) -> String {
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" tiff:Make="Acme" tiff:Model="Bellows 3"><dc:title><rdf:Alt><rdf:li xml:lang="x-default">{title}</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta>"#
    )
}

/// A synthetic stack of `n` frames as 16-bit ProPhoto TIFF files, each with XMP metadata titled "Frame <k>" (from 1).
fn tagged_frames(dir: &Path, n: usize) -> Vec<PathBuf> {
    let params = common::breathing(n, 0.02, None, 5);
    let (sharp, depth) = florastack_core::testutil::scene::textured(96 + 2 * common::MARGIN, 72 + 2 * common::MARGIN, 3).unwrap();
    let stack = florastack_core::testutil::generate(&sharp, &depth, &params).unwrap();
    let folder = dir.join("frames");
    std::fs::create_dir_all(&folder).unwrap();
    let spec = OutputSpec { profile: OutputProfile::Wide(WideSpace::ProPhoto), ..OutputSpec::new(Format::Tiff).unwrap() };
    stack
        .frames
        .iter()
        .enumerate()
        .map(|(i, frame)| {
            let frame: Image<f32> = frame.crop(florastack_core::Rect::new(common::MARGIN, common::MARGIN, 96, 72)).unwrap();
            let encoded = frame.to_encoding::<u16>(Encoding::Srgb).unwrap();
            let metadata = OutputMetadata {
                source: Some(Metadata::from_parts(None, Some(xmp(&format!("Frame {}", i + 1))), None)),
                ..OutputMetadata::default()
            };
            let path = folder.join(format!("frame-{i:02}.tif"));
            std::fs::write(&path, florastack_codecs::write(&AnyImage::U16(encoded), &spec, &metadata).unwrap().bytes).unwrap();
            path
        })
        .collect()
}

/// A project with `n` tagged frames, aligned, and stacked by the pyramid and the depth map: the two outputs' ids.
fn stacked(engine: &mut Engine, dir: &Path, n: usize) -> (Value, Value) {
    let frames = tagged_frames(dir, n);
    run(engine, "project.new", &format!(r#"{{ "name": "Weevil", "folder": {} }}"#, quoted(dir)));
    run(engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, common::path_list(&frames)));
    run(engine, "align.run", "{}");
    let pyramid = run(engine, "stack.pyramid", "{}")["output"].clone();
    let depth = run(engine, "stack.depthMap", r#"{ "threshold": "percentile", "percentile": 40 }"#)["output"].clone();
    (pyramid, depth)
}

/// Exports `output` with `settings` into `folder`: the files written.
fn export(engine: &mut Engine, output: &Value, folder: &Path, settings: &Value) -> Vec<(PathBuf, String)> {
    let params = json!({ "output": output, "folder": folder.display().to_string(), "settings": settings, "overwrite": true });
    let exported = run(engine, "export.run", &params.to_string());
    exported["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (PathBuf::from(f["path"].as_str().unwrap()), f["what"].as_str().unwrap().to_owned()))
        .collect()
}

#[test]
fn exported_files_reopen_with_their_depth_profile_and_metadata() {
    let dir = TempDir::new("export");
    let mut engine = Engine::new(config(dir.path()));
    let (pyramid, _) = stacked(&mut engine, dir.path(), 5);
    let folder = dir.path().join("out");
    // Each format at each depth it holds, with a profile, and the metadata of one frame or another.
    let cases = [
        (json!({ "format": "tiff", "depth": "u8", "profile": "srgb" }), 8, false, Some(NamedSpace::Srgb)),
        (json!({ "format": "tiff", "depth": "u16", "profile": "input", "metadata_frame": 3 }), 16, false, Some(NamedSpace::ProPhoto)),
        (json!({ "format": "tiff", "depth": "f32", "profile": { "wide": "rec2020" }, "metadata_frame": 5 }), 32, true, Some(NamedSpace::Rec2020)),
        (json!({ "format": "png", "depth": "u16", "profile": { "wide": "pro-photo" }, "metadata_frame": 2 }), 16, false, Some(NamedSpace::ProPhoto)),
        (json!({ "format": "png", "depth": "u8", "profile": "srgb", "metadata_frame": 9 }), 8, false, Some(NamedSpace::Srgb)),
        (json!({ "format": "jpeg", "depth": "u8", "profile": "input", "jpeg_quality": 90 }), 8, false, Some(NamedSpace::ProPhoto)),
        (json!({ "format": "dng", "depth": "u16", "metadata_frame": 4 }), 16, false, None),
        (json!({ "format": "open-exr", "depth": "f32", "extended_range": true, "metadata_frame": 2 }), 32, true, None),
    ];
    for (i, (settings, bits, float, named)) in cases.into_iter().enumerate() {
        let mut settings = settings;
        settings["template"] = json!(format!("case {i}"));
        let files = export(&mut engine, &pyramid, &folder, &settings);
        assert_eq!(files.len(), 1, "{settings}: {files:?}");
        let (path, what) = &files[0];
        assert_eq!(what, "output");
        let format = settings["format"].as_str().unwrap();
        let extension = match format {
            "tiff" => "tif",
            "jpeg" => "jpg",
            "open-exr" => "exr",
            other => other,
        };
        assert_eq!(path.file_name().unwrap().to_string_lossy(), format!("case {i}.{extension}"));
        let bytes = std::fs::read(path).unwrap();
        if format == "dng" {
            // A DNG is a RAW file, which the RAW importer reads.
            let info = florastack_raw::info(&bytes).unwrap();
            assert_eq!((info.width, info.height, info.bits), (96, 72, 16), "{settings}");
        } else {
            let decoded = florastack_codecs::read(&bytes, &ReadOptions::default()).unwrap();
            assert_eq!((decoded.info.width, decoded.info.height), (96, 72), "{settings}: the canvas is the frames' size");
            assert_eq!((decoded.info.bits_per_sample, decoded.info.float), (bits, float), "{settings}");
            if let Some(named) = named {
                let profile = decoded.profile.as_ref().unwrap_or_else(|| panic!("{settings}: no profile"));
                assert_eq!(profile.named, Some(named), "{settings}: {profile:?}");
            }
        }
        // The metadata is the chosen frame's (from 1; the last beyond it), with FloraStack's own on top.
        let frame = settings.get("metadata_frame").and_then(Value::as_u64).unwrap_or(1).min(5);
        let metadata = florastack_codecs::read_metadata(&bytes);
        let summary = metadata.summary();
        assert_eq!(summary.make.as_deref(), Some("Acme"), "{settings}: {summary:?}");
        // A DNG names its camera as made by FloraStack, so that no RAW software applies the camera's own profile.
        assert!(summary.model.as_deref().unwrap().contains("Bellows 3"), "{settings}: {summary:?}");
        assert_eq!(summary.title.as_deref(), Some(format!("Frame {frame}").as_str()), "{settings}: {summary:?}");
        assert!(summary.software.as_deref().unwrap().starts_with("FloraStack"), "{settings}: {summary:?}");
    }
    // Metadata off: only what FloraStack writes itself.
    let files = export(&mut engine, &pyramid, &folder, &json!({ "metadata": false, "template": "bare" }));
    let summary = florastack_codecs::read_file(&files[0].0, &ReadOptions::default()).unwrap().metadata.summary().clone();
    assert_eq!((summary.make, summary.title), (None, None));
    // The project's settings are what an export uses when it gives none: set once, used by every export.
    run(&mut engine, "export.set", r#"{ "format": "png", "depth": "u8", "template": "{project}-{method}-{frames}" }"#);
    let files = export(&mut engine, &pyramid, &folder, &json!({}));
    assert_eq!(files[0].0.file_name().unwrap().to_string_lossy(), "Weevil-pyramid-5.png");
}

#[test]
fn maps_and_aligned_frames_are_exported_alongside() {
    let dir = TempDir::new("alongside");
    let mut engine = Engine::new(config(dir.path()));
    let (pyramid, depth) = stacked(&mut engine, dir.path(), 4);
    let folder = dir.path().join("out");
    let settings = json!({ "depth": "u8", "template": "{name}", "depth_map": true, "confidence_map": true, "aligned_frames": true });
    let estimate = run(&mut engine, "export.estimate", &json!({ "output": depth, "settings": settings }).to_string());
    let files = export(&mut engine, &depth, &folder, &settings);
    let kinds: Vec<&str> = files.iter().map(|(_, what)| what.as_str()).collect();
    assert_eq!(kinds, ["output", "depth", "confidence", "aligned", "aligned", "aligned", "aligned"]);
    assert_eq!(estimate["files"], 7, "{estimate}");
    assert_eq!(estimate["name"], files[0].0.file_name().unwrap().to_string_lossy().as_ref(), "{estimate}");
    for (path, what) in &files {
        let decoded = florastack_codecs::read_file(path, &ReadOptions::default()).unwrap();
        assert_eq!((decoded.info.width, decoded.info.height), (96, 72), "{}", path.display());
        let (bits, channels) = match what.as_str() {
            "output" => (8, 3),
            "depth" | "confidence" => (16, 1),
            _ => (16, 3),
        };
        assert_eq!((decoded.info.bits_per_sample, decoded.info.channels), (bits, channels), "{}", path.display());
        if what == "depth" || what == "confidence" {
            assert!(decoded.profile.is_none(), "maps are data, untagged: {}", path.display());
        }
    }
    let aligned: Vec<String> =
        files.iter().filter(|(_, w)| w == "aligned").map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(aligned, ["001 frame-00.tif", "002 frame-01.tif", "003 frame-02.tif", "004 frame-03.tif"]);
    assert!(files[3].0.parent().unwrap().ends_with("Depth map aligned"), "{}", files[3].0.display());
    // The estimate is near what was written: compressed files are guessed at (and this synthetic stack compresses
    // better than most photographs), uncompressed ones known within their headers.
    let written: u64 = files.iter().map(|(p, _)| std::fs::metadata(p).unwrap().len()).sum();
    let estimated = estimate["bytes"].as_u64().unwrap();
    assert_eq!(estimate["approximate"], true);
    assert!(written.abs_diff(estimated) * 2 < written, "estimated {estimated} bytes, wrote {written}");
    let plain = json!({ "tiff_compression": "none", "template": "plain" });
    let estimate = run(&mut engine, "export.estimate", &json!({ "output": pyramid, "settings": plain }).to_string());
    let files = export(&mut engine, &pyramid, &folder, &plain);
    let (written, estimated) = (std::fs::metadata(&files[0].0).unwrap().len(), estimate["bytes"].as_u64().unwrap());
    assert_eq!(estimate["approximate"], false);
    assert!(written.abs_diff(estimated) < 8192, "estimated {estimated} bytes, wrote {written}: only the headers differ");
    // Without the files there, an export refuses to replace them unless asked.
    let again = json!({ "output": depth, "folder": folder.display().to_string(), "settings": settings });
    let e = engine.dispatch_json("export.run", &again.to_string()).unwrap_err();
    assert!(e.to_string().contains("overwrite"), "{e}");
    // A pyramid output has no maps: asking for them writes the output alone, saying why.
    let exported = run(
        &mut engine,
        "export.run",
        &json!({ "output": pyramid, "folder": folder.display().to_string(), "settings": { "depth_map": true } }).to_string(),
    );
    assert_eq!(exported["files"].as_array().unwrap().len(), 1, "{exported}");
    assert!(exported["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("depth")), "{exported}");
}

/// The project's preferences, group by group.
fn preferences(engine: &Engine) -> Value {
    serde_json::to_value(&engine.project().unwrap().preferences).unwrap()
}

#[test]
fn presets_round_trip_whole_and_in_part() {
    let dir = TempDir::new("presets");
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Source", "folder": {} }}"#, quoted(dir.path())));
    run(&mut engine, "develop.set", r#"{ "exposure": 0.5, "noise_reduction": { "color": 0.25 } }"#);
    run(&mut engine, "align.set", r#"{ "framing": "common", "rotation": false }"#);
    run(&mut engine, "stack.set", r#"{ "pyramid": { "grit": 0.3 }, "depth_map": { "threshold": "level", "level": 0.002 } }"#);
    run(
        &mut engine,
        "export.set",
        r#"{ "format": "png", "depth": "u16", "profile": { "wide": "pro-photo" }, "metadata_frame": 2, "depth_map": true }"#,
    );
    let source = preferences(&engine);
    for group in ["develop", "align", "stack", "output"] {
        assert!(source.get(group).is_some(), "{group}: {source}");
    }
    let whole = dir.path().join("whole.toml");
    let part = dir.path().join("part.toml");
    run(&mut engine, "preset.save", &format!(r#"{{ "path": {}, "name": "Whole", "description": "Everything" }}"#, quoted(&whole)));
    run(&mut engine, "preset.save", &format!(r#"{{ "path": {}, "name": "Output only", "groups": ["output"] }}"#, quoted(&part)));
    let read = run(&mut engine, "preset.read", &format!(r#"{{ "path": {} }}"#, quoted(&whole)));
    assert_eq!((&read["name"], &read["description"]), (&json!("Whole"), &json!("Everything")));
    assert_eq!(read["preferences"], source, "the file holds the preferences as they are");
    run(&mut engine, "project.close", r#"{ "discardChanges": true }"#);

    // Applied to a new project, the whole preset gives the same preferences, and the same settings to every command.
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Whole", "folder": {} }}"#, quoted(dir.path())));
    let applied = run(&mut engine, "preset.apply", &format!(r#"{{ "path": {} }}"#, quoted(&whole)));
    assert_eq!(applied["groups"], json!(["develop", "align", "stack", "output"]));
    assert_eq!(preferences(&engine), source);
    assert_eq!(run(&mut engine, "export.get", "{}")["settings"]["metadata_frame"], 2);
    assert_eq!(run(&mut engine, "stack.get", "{}")["settings"]["pyramid"]["grit"], 0.3);
    run(&mut engine, "project.close", r#"{ "discardChanges": true }"#);

    // The partial one sets its group alone; a partial application of the whole one sets the groups asked for.
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Part", "folder": {} }}"#, quoted(dir.path())));
    run(&mut engine, "preset.apply", &format!(r#"{{ "path": {} }}"#, quoted(&part)));
    assert_eq!(preferences(&engine), json!({ "output": source["output"] }));
    run(&mut engine, "preset.apply", &format!(r#"{{ "path": {}, "groups": ["align"] }}"#, quoted(&whole)));
    assert_eq!(preferences(&engine), json!({ "align": source["align"], "output": source["output"] }));
    // Saved again, a preset is the same file.
    let again = dir.path().join("again.toml");
    run(&mut engine, "preset.save", &format!(r#"{{ "path": {}, "name": "Output only", "groups": ["output"] }}"#, quoted(&again)));
    assert_eq!(std::fs::read_to_string(&again).unwrap(), std::fs::read_to_string(&part).unwrap());
    // A complete preset holds every setting of its groups, so that it gives them whatever the project had.
    let full = dir.path().join("full.toml");
    run(&mut engine, "preset.save", &format!(r#"{{ "path": {}, "groups": ["align", "output"], "complete": true }}"#, quoted(&full)));
    let held = run(&mut engine, "preset.read", &format!(r#"{{ "path": {} }}"#, quoted(&full)))["preferences"].clone();
    assert_eq!(held["output"], run(&mut engine, "export.get", "{}")["settings"], "every output setting, the defaults too");
    assert_eq!(held["align"], run(&mut engine, "align.get", "{}")["settings"]);
    run(&mut engine, "export.set", r#"{ "format": "jpeg", "depth": "u8", "jpeg_quality": 70 }"#);
    run(&mut engine, "preset.apply", &format!(r#"{{ "path": {} }}"#, quoted(&full)));
    assert_eq!(run(&mut engine, "export.get", "{}")["settings"], held["output"], "the JPEG quality set since is set back too");
}

#[test]
fn every_built_in_preset_is_valid_and_applies() {
    let dir = TempDir::new("built-in");
    let mut engine = Engine::new(config(dir.path()));
    let listed = run(&mut engine, "preset.list", "{}");
    let names: Vec<&str> = listed["presets"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Insect macro", "Product shot", "Landscape", "Microscope"]);
    for preset in listed["presets"].as_array().unwrap() {
        let id = preset["id"].as_str().unwrap();
        assert!(preset["description"].as_str().is_some_and(|d| !d.is_empty()), "{preset}");
        run(&mut engine, "project.new", &format!(r#"{{ "name": "{id}", "folder": {} }}"#, quoted(dir.path())));
        let applied = run(&mut engine, "preset.apply", &format!(r#"{{ "builtin": "{id}" }}"#));
        assert_eq!(applied["groups"], preset["groups"], "{id}");
        assert_eq!(applied["groups"], json!(["align", "stack", "output"]), "{id}: alignment, stacking and output, not the develop step");
        // Every command reads its settings back.
        for command in ["align.get", "stack.get", "export.get", "develop.get"] {
            run(&mut engine, command, "{}");
        }
        // Saved as a file and read again, it holds the same preferences.
        let file = dir.path().join(format!("{id}.toml"));
        run(&mut engine, "preset.save", &format!(r#"{{ "path": {} }}"#, quoted(&file)));
        assert_eq!(run(&mut engine, "preset.read", &format!(r#"{{ "path": {} }}"#, quoted(&file)))["preferences"], preferences(&engine), "{id}");
        run(&mut engine, "project.close", r#"{ "discardChanges": true }"#);
    }
    run(&mut engine, "project.new", "{}");
    assert_eq!(engine.dispatch_json("preset.apply", r#"{ "builtin": "underwater" }"#).unwrap_err().code(), "params");
}
