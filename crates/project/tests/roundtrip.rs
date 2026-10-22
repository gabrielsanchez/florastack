//! A project saved and opened again is the same project, with its cache intact, so that retouching can resume at
//! once; and what the project folder does around that: its lock, atomic saves, cache integrity, unreferenced files
//! and changed frames.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use common::TempDir;
use florastack_core::Affine;
use florastack_project::{
    Alignment, AlignmentRun, Area, CacheProblem, Develop, FrameFile, FrameState, Group, Input, MapName, Maps, NewOutput, Preset, ProjectError,
    ProjectFolder, Timestamp, Value, schema,
};
use serde_json::json;

fn maps(entries: Vec<(&str, florastack_project::CacheRef)>) -> Maps {
    entries.into_iter().map(|(name, r)| (MapName::new(name).unwrap(), r)).collect()
}

/// Writes a frame file into `dir` and reads it as a frame.
fn frame_file(dir: &Path, name: &str, bytes: &[u8]) -> FrameFile {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    FrameFile::read(&path).unwrap()
}

#[test]
fn a_saved_project_reopens_exactly_and_can_be_retouched_at_once() {
    let dir = TempDir::new("roundtrip");
    let photos = dir.path().join("photos");
    std::fs::create_dir(&photos).unwrap();
    let (mut folder, mut project) = ProjectFolder::create(dir.path(), "Bee head").unwrap();
    let root = folder.path().to_path_buf();
    assert_eq!(root.file_name().unwrap(), "Bee head.florastack");

    // Three frames, with what alignment and the cache add to them.
    let exif = json!({ "make": "Canon", "model": "Canon EOS R5", "iso": 200, "f_number": 5.6, "keywords": ["bee", "macro"] });
    let a = project.add_frame(frame_file(&photos, "IMG_0001.CR3", b"first raw file"), Some(exif.clone()));
    let b = project.add_frame(frame_file(&photos, "IMG_0002.CR3", b"second raw file"), None);
    let c = project.add_frame(frame_file(&photos, "IMG_0003.CR3", b"third raw file"), Some(exif));
    project.frame_mut(b).unwrap().enabled = false;
    // Values whose decimal forms are long, to check that floats come back bit for bit.
    let transform = Affine::similarity((2999.5, 1999.5), 1.0 + 1.0 / 3.0e3, 0.1_f64.to_radians(), (-1.25e-3, 7.0 / 9.0));
    for (id, gain, quality) in [(a, 1.0, 1.0), (c, 0.985_123_456_789_012_3, 0.1 + 0.2)] {
        let aligned = folder
            .store(
                Area::Cache,
                &format!("frame-{}-aligned", id.get()),
                "tiff",
                format!("aligned {id}").as_bytes(),
                Some("develop 1f2e align 3d4c".into()),
            )
            .unwrap();
        let preview = folder.store(Area::Previews, &format!("frame-{}", id.get()), "jpg", b"small jpeg", None).unwrap();
        let motion = folder.store(Area::Cache, &format!("frame-{}-motion", id.get()), "tiff", b"motion map", None).unwrap();
        let frame = project.frame_mut(id).unwrap();
        frame.alignment = Some(Alignment { transform: if id == a { Affine::IDENTITY } else { transform }, brightness_gain: gain, quality });
        frame.aligned = Some(aligned);
        frame.preview = Some(preview);
        frame.maps = maps(vec![("motion", motion)]);
    }

    // The stack's development, alignment run and preferences, as the engine stores them.
    project.develop = Some(Develop {
        reference: a,
        settings: json!({ "white_balance": { "custom": { "temperature": 5200.0, "tint": 3.0 } }, "exposure": 0.25 }),
        resolved: json!({ "white": { "x": 0.312_727, "y": 0.329_023 }, "chromatic_aberration": [0.000_12, -0.000_08] }),
    });
    project.alignment = Some(AlignmentRun { master: c, settings: json!({ "rotation": false }) });
    let preset = Preset::parse("[align]\nrotation = false\n[stack]\nmethod = \"pyramid\"\n[stack.pyramid]\ngrit = 0.5\n").unwrap();
    project.preferences.layer(&preset.preferences);
    project.preferences.set(Group::Develop, &json!({ "white_balance": "auto", "exposure": 0.25 })).unwrap();
    let dust = folder.store(Area::Cache, "dust", "tiff", b"dust mask", None).unwrap();
    project.maps = maps(vec![("dust", dust)]);

    // Two outputs, the second made from the first (a slabbed stack), and retouch strokes on the first.
    let image = folder.store(Area::Cache, "output-1", "tiff", b"stacked pixels", None).unwrap();
    let preview = folder.store(Area::Previews, "output-1", "jpg", b"stack preview", None).unwrap();
    let depth = folder.store(Area::Cache, "output-1-depth", "tiff", b"depth map", None).unwrap();
    let confidence = folder.store(Area::Cache, "output-1-confidence", "tiff", b"confidence map", None).unwrap();
    let pyramid = project.add_output(NewOutput {
        name: "Pyramid".into(),
        method: "pyramid".into(),
        parameters: json!({ "grit": 0.5, "levels": 7 }),
        inputs: vec![Input::Frame(a), Input::Frame(c)],
        image,
        preview: Some(preview),
        maps: maps(vec![("depth", depth), ("confidence", confidence)]),
    });
    let layer = project.add_layer(pyramid, "Bristles").unwrap();
    let strokes = vec![
        json!({ "brush": "details", "size": 40.0, "source": { "frame": c.get() }, "points": [[100.5, 120.25, 1.0], [104.0, 122.5, 0.875]] }),
        json!({ "brush": "darken", "size": 12.0, "source": { "frame": a.get() }, "points": [[50.0, 60.0, 0.5]] }),
    ];
    project.output_mut(pyramid).unwrap().layer_mut(layer).unwrap().strokes.clone_from(&strokes);
    let final_image = folder.store(Area::Cache, "output-2", "tiff", b"final pixels", None).unwrap();
    let slabbed = project.add_output(NewOutput {
        name: "Slabbed depth map".into(),
        method: "depth-map".into(),
        parameters: json!({ "threshold": { "percentile": 12.5 } }),
        inputs: vec![Input::Output(pyramid)],
        image: final_image,
        preview: None,
        maps: Maps::new(),
    });
    folder.save(&mut project).unwrap();
    assert_eq!(project.software(), Some(format!("FloraStack {}", env!("CARGO_PKG_VERSION")).as_str()));
    drop(folder);

    let (folder, reopened) = ProjectFolder::open(&root).unwrap();
    assert_eq!(reopened, project, "everything comes back");
    assert_eq!(reopened.frame(c).unwrap().alignment.unwrap().transform, transform, "bit for bit");
    assert_eq!(reopened.frame(c).unwrap().alignment.unwrap().quality.to_bits(), (0.1_f64 + 0.2).to_bits());
    assert_eq!(folder.migrated_from(), None);

    // Retouching resumes without computing anything: the target's pixels, its depth map (to find the sharpest
    // frame), the aligned source frames and the strokes are all there, and intact.
    let output = reopened.output(pyramid).unwrap();
    assert_eq!(folder.read(&output.image).unwrap(), b"stacked pixels");
    assert_eq!(folder.read(&output.maps[&MapName::new("depth").unwrap()]).unwrap(), b"depth map");
    assert_eq!(folder.read(reopened.frame(c).unwrap().aligned.as_ref().unwrap()).unwrap(), format!("aligned {c}").as_bytes());
    assert_eq!(output.layer(layer).unwrap().strokes, strokes);
    assert!(folder.check_cache(&reopened, true).is_empty(), "every cache file is present and intact");
    assert_eq!(reopened.output(slabbed).unwrap().inputs, [Input::Output(pyramid)]);
    assert_eq!(reopened.preferences.group(Group::Stack)["pyramid"]["grit"], json!(0.5));

    // The file is readable JSON that starts with its format and version.
    let text = std::fs::read_to_string(folder.project_file()).unwrap();
    assert!(text.starts_with("{\n  \"format\": \"florastack-project\",\n  \"version\": 1,\n"), "{}", &text[..80]);
    assert_eq!(schema::parse(text.as_bytes()).unwrap(), reopened);
}

#[test]
fn an_open_project_is_locked_against_a_second_editor() {
    let dir = TempDir::new("lock");
    let (folder, _) = ProjectFolder::create(dir.path(), "Coin").unwrap();
    let root = folder.path().to_path_buf();
    assert!(folder.is_locked());
    assert!(matches!(ProjectFolder::open(&root), Err(ProjectError::InUse { .. })));
    // Reading it is fine, but not saving through a read-only handle.
    let (mut reader, mut project) = ProjectFolder::open_read_only(&root.join("project.json")).unwrap();
    assert_eq!(reader.path(), root);
    assert!(matches!(reader.save(&mut project), Err(ProjectError::ReadOnly { .. })));
    assert!(matches!(reader.store(Area::Cache, "x", "bin", b"x", None), Err(ProjectError::ReadOnly { .. })));
    drop(folder);
    let (again, _) = ProjectFolder::open(&root).unwrap();
    assert!(again.is_locked());
    assert!(matches!(ProjectFolder::create(dir.path(), "Coin"), Err(ProjectError::Exists { .. })));
    assert!(matches!(ProjectFolder::open(dir.path()), Err(ProjectError::NotAProject { .. })));
}

#[test]
fn saves_are_atomic_and_refuse_an_inconsistent_project() {
    let dir = TempDir::new("atomic");
    let (mut folder, mut project) = ProjectFolder::create(dir.path(), "Flower").unwrap();
    let root = folder.path().to_path_buf();
    let mut names: Vec<String> = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    assert_eq!(names, [".lock", "cache", "previews", "project.json"], "no temporary file is left");
    let before = std::fs::read(folder.project_file()).unwrap();

    let id = project.add_frame(FrameFile { path: "/a.tif".into(), size: 1, modified: None, hash: florastack_project::ContentHash::of(b"a") }, None);
    project.frame_mut(id).unwrap().alignment =
        Some(Alignment { transform: Affine::translation(f64::INFINITY, 0.0), brightness_gain: 1.0, quality: 1.0 });
    let e = folder.save(&mut project).unwrap_err();
    assert!(matches!(e, ProjectError::Invalid { .. }), "{e}");
    assert_eq!(std::fs::read(folder.project_file()).unwrap(), before, "the last good save stays");

    project.frame_mut(id).unwrap().alignment = None;
    project.preferences.set(Group::Output, &json!({ "format": "tiff" })).unwrap();
    folder.save(&mut project).unwrap();
    assert_ne!(std::fs::read(folder.project_file()).unwrap(), before);
}

#[test]
fn a_damaged_cache_file_is_reported_and_never_used() {
    let dir = TempDir::new("integrity");
    let (folder, _) = ProjectFolder::create(dir.path(), "Damaged").unwrap();
    let reference = folder.store(Area::Cache, "output-1", "tiff", b"0123456789", None).unwrap();
    assert_eq!(reference.size, 10);
    assert!(reference.path.file_name().starts_with("output-1-") && reference.path.area() == Area::Cache);
    assert_eq!(Path::new(reference.path.file_name()).extension().unwrap(), "tiff");
    let file = reference.path.resolve(folder.path());
    // Same size, other bytes.
    std::fs::write(&file, b"0123456780").unwrap();
    let e = folder.read(&reference).unwrap_err();
    assert!(matches!(e, ProjectError::Cache { source: CacheProblem::Hash { .. }, .. }), "{e}");
    assert!(matches!(folder.verify(&reference), Err(ProjectError::Cache { source: CacheProblem::Hash { .. }, .. })));
    std::fs::write(&file, b"012").unwrap();
    assert!(matches!(folder.read(&reference), Err(ProjectError::Cache { source: CacheProblem::Size { expected: 10, found: 3 }, .. })));
    std::fs::remove_file(&file).unwrap();
    assert!(matches!(folder.read(&reference), Err(ProjectError::Cache { source: CacheProblem::Missing, .. })));
    assert!(e.to_string().contains("computed again"), "{e}");
}

#[test]
fn unreferenced_cache_files_are_listed_and_removed() {
    let dir = TempDir::new("unreferenced");
    let (mut folder, mut project) = ProjectFolder::create(dir.path(), "Garbage").unwrap();
    let keep = folder.store(Area::Cache, "output-1", "tiff", b"kept", None).unwrap();
    let gone = folder.store(Area::Cache, "output-2", "tiff", b"removed output", None).unwrap();
    let gone_preview = folder.store(Area::Previews, "output-2", "jpg", b"its preview", None).unwrap();
    let new = |image| NewOutput {
        name: "Output".into(),
        method: "pyramid".into(),
        parameters: Value::Null,
        inputs: Vec::new(),
        image,
        preview: None,
        maps: Maps::new(),
    };
    let first = project.add_output(new(keep.clone()));
    let mut second = new(gone.clone());
    second.preview = Some(gone_preview.clone());
    let second = project.add_output(second);
    // A writer that is dropped without a commit leaves nothing; a tile folder inside the cache isn't the cache's.
    drop(folder.writer(Area::Cache, "abandoned", "tiff").unwrap());
    std::fs::create_dir(folder.path().join("cache").join("florastack-tiles-1-0-0")).unwrap();
    folder.save(&mut project).unwrap();
    assert_eq!(folder.unreferenced(&project).unwrap(), Vec::<std::path::PathBuf>::new());

    project.remove_output(second);
    let expected = [gone.path.resolve(folder.path()), gone_preview.path.resolve(folder.path())];
    assert_eq!(folder.unreferenced(&project).unwrap(), expected);
    assert_eq!(folder.remove_unreferenced(&project).unwrap(), expected);
    assert!(expected.iter().all(|p| !p.exists()));
    assert_eq!(folder.read(&project.output(first).unwrap().image).unwrap(), b"kept");
}

#[test]
fn frame_files_are_checked_for_changes() {
    let dir = TempDir::new("frames");
    let (folder, mut project) = ProjectFolder::create(dir.path(), "Frames").unwrap();
    let photo = dir.path().join("IMG_1.tif");
    std::fs::write(&photo, b"pixels").unwrap();
    let id = project.add_frame(FrameFile::read(&photo).unwrap(), None);
    assert!(Path::new(&project.frame(id).unwrap().path).is_absolute());
    assert_eq!(folder.check_frame(project.frame(id).unwrap()).unwrap(), FrameState::Unchanged);
    // Same size and contents, a later time: hashed again, still the same file.
    let file = std::fs::File::options().write(true).open(&photo).unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600)).unwrap();
    drop(file);
    assert_eq!(folder.check_frame(project.frame(id).unwrap()).unwrap(), FrameState::Unchanged);
    std::fs::write(&photo, b"PIXELS").unwrap();
    assert_eq!(folder.check_frame(project.frame(id).unwrap()).unwrap(), FrameState::Changed);
    std::fs::remove_file(&photo).unwrap();
    assert_eq!(folder.check_frame(project.frame(id).unwrap()).unwrap(), FrameState::Missing);
    // A relative path is relative to the folder holding the project folder.
    project.frame_mut(id).unwrap().path = "IMG_1.tif".into();
    assert_eq!(folder.frame_path(project.frame(id).unwrap()), dir.path().join("IMG_1.tif"));
}

/// Regression test (macOS CI): a file rewritten with the same size right after it was added kept the same modification
/// time to the millisecond, so the quick check took it for unchanged. FAT memory cards, with their 2-second times, make
/// that likely too. A just-written file keeps no modification time, so every check hashes it.
#[test]
fn a_file_rewritten_right_after_it_was_added_is_seen_as_changed() {
    let dir = TempDir::new("racy");
    let (folder, mut project) = ProjectFolder::create(dir.path(), "Racy").unwrap();
    let photo = dir.path().join("IMG_2.tif");
    std::fs::write(&photo, b"pixels").unwrap();
    let written = std::fs::metadata(&photo).unwrap().modified().unwrap();
    let file = FrameFile::read(&photo).unwrap();
    assert_eq!(file.modified, None, "too recent to be trusted");
    let id = project.add_frame(file, None);
    // The worst case, made certain: the same size and the same modification time as when it was added.
    std::fs::write(&photo, b"PIXELS").unwrap();
    std::fs::File::options().write(true).open(&photo).unwrap().set_modified(written).unwrap();
    assert_eq!(folder.check_frame(project.frame(id).unwrap()).unwrap(), FrameState::Changed);
}

#[test]
fn an_older_file_keeps_its_modification_time_for_quick_checks() {
    let dir = TempDir::new("settled");
    let (folder, mut project) = ProjectFolder::create(dir.path(), "Settled").unwrap();
    let photo = dir.path().join("IMG_3.tif");
    std::fs::write(&photo, b"pixels").unwrap();
    let an_hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options().write(true).open(&photo).unwrap().set_modified(an_hour_ago).unwrap();
    let file = FrameFile::read(&photo).unwrap();
    assert_eq!(file.modified, Some(Timestamp::from_system_time(an_hour_ago)));
    let id = project.add_frame(file, None);
    assert_eq!(folder.check_frame(project.frame(id).unwrap()).unwrap(), FrameState::Unchanged);
    std::fs::write(&photo, b"PIXELS").unwrap();
    assert_eq!(folder.check_frame(project.frame(id).unwrap()).unwrap(), FrameState::Changed, "a new time: hashed again");
}
