//! The project with the other crates' types: develop settings and their resolution, EXIF summaries and cache images
//! survive a save and a reopen exactly, so that a reopened project is retouched with no recomputation; and the JSON
//! shapes those types take in project files are pinned.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use florastack_align::{AlignSettings, End, Flag, Framing, MotionRegion, Status, Term};
use florastack_codecs::{ExifSummary, Format};
use florastack_core::{AnyImage, ColorSpace, Encoding, Image};
use florastack_engine::model::FrameId;
use florastack_engine::project::{
    FrameMotion, FrameReport, MaskSource, MaskSummary, PREVIEW_SIDE, RunReport, add_frame, develop, develop_settings, exif, read_image,
    set_develop_settings, store_develop, write_image, write_preview,
};
use florastack_project::{Area, Input, MapName, Maps, NewOutput, ProjectFolder, to_data};
use florastack_raw::{ChromaticAberration, Chromaticity, DevelopSettings, NoiseReduction, RawDevelop, Resolved, WhiteBalance};
use serde_json::json;

/// A folder of its own in the system's temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!("florastack-engine-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn settings() -> DevelopSettings {
    DevelopSettings {
        white_balance: WhiteBalance::Custom { temperature: 5200.0, tint: 3.5 },
        exposure: 0.333,
        chromatic_aberration: ChromaticAberration::Manual { red: 0.000_25, blue: -0.000_125 },
        noise_reduction: NoiseReduction { luminance: 0.1, color: 0.3 },
        ..DevelopSettings::default()
    }
}

#[test]
fn the_development_comes_back_from_a_reopened_project() {
    let dir = TempDir::new("develop");
    let (mut folder, mut project) = ProjectFolder::create(dir.path(), "Develop").unwrap();
    assert_eq!(develop_settings(&project).unwrap(), DevelopSettings::default(), "nothing set: the defaults");
    let frame = project.add_frame(
        florastack_project::FrameFile { path: "/a.dng".into(), size: 1, modified: None, hash: florastack_project::ContentHash::of(b"a") },
        None,
    );
    set_develop_settings(&mut project, &settings()).unwrap();
    let resolved = Resolved { white: Chromaticity { x: 0.322_123_456_789, y: 0.337_987_654_321 }, chromatic_aberration: [0.000_25, -0.000_125] };
    let development = RawDevelop::from_resolved(&settings(), resolved).unwrap();
    store_develop(&mut project, frame, &development).unwrap();
    folder.save(&mut project).unwrap();
    let root = folder.path().to_path_buf();
    drop(folder);

    let (_folder, mut reopened) = ProjectFolder::open(&root).unwrap();
    assert_eq!(develop_settings(&reopened).unwrap(), settings());
    let back = develop(&reopened).unwrap().expect("still current");
    assert_eq!(back, development);
    assert_eq!(back.fingerprint(florastack_raw::Quality::Output), development.fingerprint(florastack_raw::Quality::Output), "develops identically");
    // Other settings now: the stored resolution no longer holds.
    set_develop_settings(&mut reopened, &DevelopSettings { exposure: 1.0, ..settings() }).unwrap();
    assert_eq!(develop(&reopened).unwrap(), None);
    assert!(set_develop_settings(&mut reopened, &DevelopSettings { exposure: f32::NAN, ..settings() }).is_err());
}

#[test]
fn frames_keep_their_metadata_summary() {
    let dir = TempDir::new("exif");
    let (_folder, mut project) = ProjectFolder::create(dir.path(), "Exif").unwrap();
    // A TIFF written by FloraStack carries its software tag.
    let image = AnyImage::U16(Image::from_fn(8, 6, 3, ColorSpace::SRGB, |x, y, c| u16::try_from(x * 1000 + y * 100 + c).unwrap()).unwrap());
    let spec = florastack_codecs::OutputSpec::new(Format::Tiff).unwrap();
    let bytes = florastack_codecs::write(&image, &spec, &florastack_codecs::OutputMetadata::default()).unwrap().bytes;
    let path = dir.path().join("frame.tif");
    std::fs::write(&path, &bytes).unwrap();
    let id = add_frame(&mut project, &path).unwrap();
    let frame = project.frame(id).unwrap();
    assert_eq!((frame.size, frame.hash), (bytes.len() as u64, florastack_project::ContentHash::of(&bytes)));
    let summary = exif(frame).unwrap().expect("a summary");
    assert!(summary.software.as_deref().unwrap().starts_with("FloraStack"), "{summary:?}");
    // A full summary goes through the project unchanged (but for the orientation, which reading the frame applies).
    let full = ExifSummary {
        make: Some("Canon".into()),
        model: Some("Canon EOS R5".into()),
        lens: Some("RF100mm F2.8 L MACRO IS USM".into()),
        capture_time: Some("2026-10-20T09:15:00.120+02:00".into()),
        exposure_time: Some(1.0 / 250.0),
        f_number: Some(5.6),
        iso: Some(200),
        focal_length: Some(100.0),
        keywords: vec!["bee".into(), "macro".into()],
        gps: Some((47.366_666, 8.533_333, Some(408.5))),
        ..ExifSummary::default()
    };
    project.frame_mut(id).unwrap().exif = Some(to_data("EXIF summary", &full).unwrap());
    assert_eq!(exif(project.frame(id).unwrap()).unwrap(), Some(full));
}

/// Images of every sample type, with values that only a lossless store keeps.
fn images() -> Vec<AnyImage> {
    let rec2020 = ColorSpace { encoding: Encoding::Linear, icc: Some(florastack_raw::working_space_icc()) };
    vec![
        AnyImage::U8(Image::from_fn(33, 17, 3, ColorSpace::SRGB, |x, y, c| u8::try_from((x * 7 + y * 31 + c * 85) % 256).unwrap()).unwrap()),
        AnyImage::U16(Image::from_fn(33, 17, 1, ColorSpace::SRGB, |x, y, _| u16::try_from((x * 2_654_435 + y * 40_503) % 65_536).unwrap()).unwrap()),
        AnyImage::F32(Image::from_fn(33, 17, 3, rec2020, |x, y, c| ((x * 13 + y * 7 + c * 3) % 97) as f32 / 31.0 - 0.5).unwrap()),
        AnyImage::F32(
            Image::from_fn(33, 17, 4, ColorSpace::LINEAR, |x, y, c| if c == 3 { 0.75 } else { (x + y) as f32 * 1.0e-3 + 1.0e6 * (c as f32) })
                .unwrap(),
        ),
    ]
}

#[test]
fn a_reopened_project_is_retouched_with_no_recomputation() {
    let dir = TempDir::new("retouch");
    let (mut folder, mut project) = ProjectFolder::create(dir.path(), "Retouch").unwrap();
    let images = images();
    let frames: Vec<_> = (0..2u8)
        .map(|n| {
            project.add_frame(
                florastack_project::FrameFile {
                    path: format!("/f{n}.tif"),
                    size: 1,
                    modified: None,
                    hash: florastack_project::ContentHash::of(&[n]),
                },
                None,
            )
        })
        .collect();
    // The aligned frames, the output and its depth map, in the cache.
    for (id, image) in frames.iter().zip(&images) {
        let reference = write_image(&folder, &format!("frame-{}-aligned", id.get()), image, Some("align 1".into())).unwrap();
        project.frame_mut(*id).unwrap().aligned = Some(reference);
    }
    let output = write_image(&folder, "output-3", &images[2], None).unwrap();
    let depth = write_image(&folder, "output-3-depth", &images[1], None).unwrap();
    let Some(AnyImage::F32(linear)) = images.get(2).cloned() else { panic!() };
    let preview = write_preview(&folder, "output-3", &linear, None).unwrap();
    let id = project.add_output(NewOutput {
        name: "Pyramid".into(),
        method: "pyramid".into(),
        parameters: json!({ "grit": 0.5 }),
        inputs: frames.iter().map(|&f| Input::Frame(f)).collect(),
        image: output,
        preview: Some(preview),
        maps: [(MapName::new("depth").unwrap(), depth)].into_iter().collect::<Maps>(),
    });
    let layer = project.add_layer(id, "Bristles").unwrap();
    project.output_mut(id).unwrap().layer_mut(layer).unwrap().strokes.push(json!({ "brush": "details", "source": { "frame": frames[1].get() } }));
    folder.save(&mut project).unwrap();
    let root = folder.path().to_path_buf();
    drop(folder);

    let (folder, reopened) = ProjectFolder::open(&root).unwrap();
    let output = reopened.output(id).unwrap();
    assert_eq!(read_image(&folder, &output.image).unwrap(), images[2], "the target, bit for bit, with its profile");
    assert_eq!(read_image(&folder, &output.maps[&MapName::new("depth").unwrap()]).unwrap(), images[1]);
    for (frame, image) in frames.iter().zip(&images) {
        assert_eq!(&read_image(&folder, reopened.frame(*frame).unwrap().aligned.as_ref().unwrap()).unwrap(), image, "a source frame");
    }
    assert_eq!(output.layer(layer).unwrap().strokes.len(), 1);
}

#[test]
fn cache_images_read_back_exactly_at_every_sample_type() {
    let dir = TempDir::new("lossless");
    let (folder, _) = ProjectFolder::create(dir.path(), "Lossless").unwrap();
    for (n, image) in images().into_iter().enumerate() {
        let reference = write_image(&folder, &format!("image-{n}"), &image, None).unwrap();
        assert_eq!(reference.path.area(), Area::Cache);
        let back = read_image(&folder, &reference).unwrap();
        let bits = |i: &AnyImage| match i {
            AnyImage::F32(f) => f.as_raw().iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        assert_eq!(back, image, "image {n}");
        assert_eq!(bits(&back), bits(&image), "image {n}, bit for bit");
    }
}

#[test]
fn the_v0_fixtures_output_is_read_from_its_cache() {
    let dir = TempDir::new("v0");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../project/tests/fixtures/v0/bee-head.florastack");
    let copy = dir.path().join("bee-head.florastack");
    std::fs::create_dir_all(copy.join("cache")).unwrap();
    std::fs::copy(fixture.join("project.json"), copy.join("project.json")).unwrap();
    std::fs::copy(fixture.join("cache/output-1.tiff"), copy.join("cache/output-1.tiff")).unwrap();
    let (folder, project) = ProjectFolder::open(&copy).unwrap();
    let image = read_image(&folder, &project.outputs[0].image).unwrap();
    let AnyImage::U16(gray) = image else { panic!("{image:?}") };
    assert_eq!((gray.width(), gray.height(), gray.channels()), (2, 2, 1));
    assert_eq!(gray.as_raw(), [0, 21_845, 43_690, 65_535]);
    assert_eq!(
        develop_settings(&project).unwrap(),
        DevelopSettings { white_balance: WhiteBalance::Auto, exposure: 0.5, ..DevelopSettings::default() }
    );
    assert_eq!(exif(&project.frames[0]).unwrap().unwrap().model.as_deref(), Some("Canon EOS R5"));
}

#[test]
fn previews_are_small_srgb_jpegs() {
    let dir = TempDir::new("preview");
    let (folder, _) = ProjectFolder::create(dir.path(), "Preview").unwrap();
    let image = Image::from_fn(1200, 800, 3, ColorSpace::LINEAR, |x, y, c| ((x + 2 * y + c) % 255) as f32 / 255.0).unwrap();
    let reference = write_preview(&folder, "frame-1", &image, None).unwrap();
    assert_eq!(reference.path.area(), Area::Previews);
    let bytes = folder.read(&reference).unwrap();
    let decoded = florastack_codecs::read(&bytes, &florastack_codecs::ReadOptions::default()).unwrap();
    assert_eq!(decoded.info.format, Format::Jpeg);
    assert_eq!((decoded.image.width(), decoded.image.height()), (PREVIEW_SIDE, 341));
    let tiny = Image::from_fn(40, 30, 1, ColorSpace::LINEAR, |_, _, _| 0.5).unwrap();
    let reference = write_preview(&folder, "frame-2", &tiny, None).unwrap();
    let decoded = florastack_codecs::read(&folder.read(&reference).unwrap(), &florastack_codecs::ReadOptions::default()).unwrap();
    assert_eq!((decoded.image.width(), decoded.image.height()), (40, 30), "small images aren't enlarged");
}

/// The JSON shapes of the other crates' types in project files. If one of these fails, a type that projects store
/// changed its serialized form: raise `florastack_project::schema::VERSION`, add a migration that rewrites the old
/// form, and then update the expectation here.
#[test]
fn the_pyramid_s_coarse_limit_is_stored_only_when_it_is_on() {
    let limited = florastack_engine::StackSettings {
        pyramid: florastack_engine::PyramidSettings { coarse_limit: Some(2), ..florastack_engine::PyramidSettings::default() },
        ..florastack_engine::StackSettings::default()
    };
    assert_eq!(to_data("stack settings", &limited).unwrap()["pyramid"]["coarse_limit"], 2);
    assert!(to_data("stack settings", &florastack_engine::StackSettings::default()).unwrap()["pyramid"].get("coarse_limit").is_none());
}

#[test]
fn the_shapes_stored_in_project_files_are_pinned() {
    assert_eq!(
        to_data("develop settings", &DevelopSettings::default()).unwrap(),
        json!({
            "white_balance": "as-shot",
            "demosaic": "high-quality",
            "highlights": "reconstruct",
            "exposure": 0.0,
            "chromatic_aberration": "auto",
            "noise_reduction": { "luminance": 0.0, "color": 0.0 },
            "lens_distortion": false,
        })
    );
    assert_eq!(to_data("develop settings", &settings()).unwrap()["white_balance"], json!({ "custom": { "temperature": 5200.0, "tint": 3.5 } }));
    // `f32` settings are stored as the `f64` of the same value, which reads back exactly: 0.00025f32 is written
    // 0.0002500000118743628, not 0.00025.
    assert_eq!(
        to_data("develop settings", &settings()).unwrap()["chromatic_aberration"],
        json!({ "manual": { "red": f64::from(0.000_25_f32), "blue": f64::from(-0.000_125_f32) } })
    );
    let resolved = Resolved { white: Chromaticity { x: 0.25, y: 0.5 }, chromatic_aberration: [0.125, -0.25] };
    assert_eq!(
        to_data("resolved development", &resolved).unwrap(),
        json!({ "white": { "x": 0.25, "y": 0.5 }, "chromatic_aberration": [0.125, -0.25] })
    );
    let summary = ExifSummary {
        make: Some("Canon".into()),
        iso: Some(200),
        gps: Some((1.5, -2.5, None)),
        keywords: vec!["bee".into()],
        ..ExifSummary::default()
    };
    // What alignment stores: its settings, a frame's report and a run's.
    assert_eq!(
        to_data("alignment settings", &AlignSettings::default()).unwrap(),
        json!({
            "shift_x": true, "shift_y": true, "scale": true, "rotation": true, "brightness": true, "refine": false,
            "max_shift": 10.0, "max_scale": 15.0, "max_rotation": 5.0, "min_score": 0.3, "reject": true, "resolution": 2048,
            "automatic_order": true, "reverse_order": false, "framing": "narrow", "min_sharp_fraction": 0.002, "min_motion": 2.0,
            "detect_defects": false,
        })
    );
    // What stacking stores: its settings, one table per method.
    assert_eq!(
        to_data("stack settings", &florastack_engine::StackSettings::default()).unwrap(),
        json!({
            "average": { "radius": 3.0, "smoothing": 8.0, "power": 3.0 },
            "pyramid": { "levels": 0, "grit": 0.5, "decisions": "luminance", "range_limit": 1.0, "tone": "compress" },
        })
    );
    let id = |n: u64| serde_json::from_value::<FrameId>(json!(n)).unwrap();
    let frame = FrameReport {
        status: Status::Rejected,
        flags: vec![Flag::Limit(Term::ShiftX), Flag::OutOfFocus],
        reference: Some(id(3)),
        sharp_fraction: Some(0.25),
        motion: None,
    };
    assert_eq!(
        to_data("frame alignment", &frame).unwrap(),
        json!({ "status": "rejected", "flags": [{ "limit": "shift_x" }, "out-of-focus"], "reference": 3, "sharp_fraction": 0.25 })
    );
    let region = MotionRegion { x: 120, y: 80, width: 80, height: 64, cells: 63, dx: 3.0, dy: -2.25, peak: 3.75 };
    let moved = FrameReport {
        status: Status::Aligned,
        flags: vec![Flag::Moved],
        reference: Some(id(2)),
        sharp_fraction: None,
        motion: Some(FrameMotion { score: 0.0625, peak: 3.75, regions: vec![region] }),
    };
    assert_eq!(
        to_data("frame alignment", &moved).unwrap(),
        json!({
            "status": "aligned", "flags": ["moved"], "reference": 2, "sharp_fraction": null,
            "motion": {
                "score": 0.0625, "peak": 3.75,
                "regions": [{ "x": 120, "y": 80, "width": 80, "height": 64, "cells": 63, "dx": 3.0, "dy": -2.25, "peak": 3.75 }],
            },
        })
    );
    // A defect mask's summary, its cache key.
    let mask = MaskSummary { source: MaskSource::Detected, width: 6000, height: 4000, pixels: 1234, spots: 7 };
    assert_eq!(serde_json::to_value(&mask).unwrap(), json!({ "source": "detected", "width": 6000, "height": 4000, "pixels": 1234, "spots": 7 }));
    let run =
        RunReport { width: 6000, height: 4000, narrow_end: Some(End::Last), start: End::First, order: vec![id(1), id(3)], narrow_frame: Some(id(3)) };
    assert_eq!(
        to_data("alignment run", &run).unwrap(),
        json!({ "width": 6000, "height": 4000, "narrow_end": "last", "start": "first", "order": [1, 3], "narrow_frame": 3 })
    );
    assert_eq!(serde_json::to_value(Framing::ALL).unwrap(), json!(["narrow", "wide", "common"]));
    assert_eq!(
        serde_json::to_value([Flag::LowScore, Flag::RepeatingPattern, Flag::NoMatch, Flag::NotRefined, Flag::Moved]).unwrap(),
        json!(["low-score", "repeating-pattern", "no-match", "not-refined", "moved"])
    );
    assert_eq!(serde_json::to_value([Status::Master, Status::Aligned]).unwrap(), json!(["master", "aligned"]));
    assert_eq!(
        to_data("EXIF summary", &summary).unwrap(),
        json!({
            "make": "Canon", "model": null, "lens": null, "software": null, "capture_time": null, "exposure_time": null,
            "f_number": null, "iso": 200, "focal_length": null, "focal_length_35mm": null, "artist": null, "copyright": null,
            "title": null, "caption": null, "keywords": ["bee"], "gps": [1.5, -2.5, null],
        })
    );
}
