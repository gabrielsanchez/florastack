//! A project file in format version 0, committed in `tests/fixtures/v0/`, opens upgraded to the current version,
//! keeps everything it held, can be retouched at once (its output's pixels are in its cache, intact), and is
//! written in the current version at the next save, with the original kept beside it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{TempDir, copy_dir, fixture};
use florastack_core::Affine;
use florastack_project::{ContentHash, Group, Input, ProjectError, ProjectFolder, Timestamp, schema};
use serde_json::json;

fn copy_of_fixture(dir: &TempDir) -> std::path::PathBuf {
    let copy = dir.path().join("bee-head.florastack");
    copy_dir(&fixture("v0/bee-head.florastack"), &copy);
    copy
}

#[test]
fn a_v0_project_is_upgraded_and_reopens_without_recomputation() {
    let dir = TempDir::new("migration");
    let root = copy_of_fixture(&dir);
    let original = std::fs::read(root.join("project.json")).unwrap();
    assert_eq!(schema::header(&original).unwrap().version, 0, "the fixture is a version 0 file");

    let (mut folder, project) = ProjectFolder::open(&root).unwrap();
    assert_eq!(folder.migrated_from(), Some(0));
    assert_eq!(project.name(), Some("Bee head"));
    assert_eq!(project.created(), "2026-10-20T12:40:00Z".parse::<Timestamp>().unwrap());
    assert_eq!(project.modified(), project.created());

    // Frames: ids in order, `enabled` for `skip`, hashes in their prefixed, lower-case form, the matrix as two rows.
    let ids: Vec<u64> = project.frames.iter().map(|f| f.id().get()).collect();
    assert_eq!(ids, [1, 2, 3]);
    let enabled: Vec<bool> = project.frames.iter().map(|f| f.enabled).collect();
    assert_eq!(enabled, [true, false, true]);
    let first = &project.frames[0];
    assert_eq!((first.path.as_str(), first.size), ("/photos/bee/IMG_0001.CR3", 25_165_824));
    assert_eq!(first.hash, "sha256:0f3c9a6d1e2b4f5a8c7d9e0f1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d".parse::<ContentHash>().unwrap());
    assert_eq!(first.exif.as_ref().unwrap()["model"], json!("Canon EOS R5"));
    assert_eq!(first.alignment.unwrap().transform, Affine::IDENTITY);
    assert_eq!(project.frames[1].alignment, None);
    assert_eq!(project.frames[1].exif, None);
    let third = project.frames[2].alignment.unwrap();
    assert_eq!(third.transform, Affine { m: [[1.0021, -0.0004, -1.25], [0.0004, 1.0021, 0.75]] });
    assert_eq!((third.brightness_gain, third.quality), (0.985, 0.931));

    // The develop settings are now a preference group.
    assert_eq!(serde_json::Value::Object(project.preferences.group(Group::Develop).clone()), json!({ "white_balance": "auto", "exposure": 0.5 }));
    assert!(project.develop.is_none() && project.alignment.is_none());

    // The output: an id after the frames', a name, its inputs (the frames that weren't skipped), its image as a cache
    // reference, and its strokes as one layer.
    assert_eq!(project.outputs.len(), 1);
    let output = &project.outputs[0];
    assert_eq!((output.id().get(), output.name.as_str(), output.method.as_str()), (4, "Output 1", "pyramid"));
    assert_eq!(output.parameters, json!({ "grit_suppression": 0.5 }));
    assert_eq!(output.created(), "2026-10-20T12:45:00Z".parse::<Timestamp>().unwrap());
    assert_eq!(output.inputs, [Input::Frame(project.frames[0].id()), Input::Frame(project.frames[2].id())]);
    assert_eq!(output.image.path.to_string(), "cache/output-1.tiff");
    assert_eq!(output.retouch.len(), 1);
    let layer = &output.retouch[0];
    assert_eq!((layer.id().get(), layer.name.as_str(), layer.visible), (5, "Retouching", true));
    assert_eq!(
        layer.strokes,
        [json!({ "brush": "details", "size": 40.0, "source": { "frame": 3 }, "points": [[100.0, 120.0, 1.0], [104.0, 122.5, 0.9]] })]
    );

    // Retouching can start at once: the output's pixels are in the cache and match their reference.
    let pixels = folder.read(&output.image).unwrap();
    assert_eq!(&pixels[..4], b"II*\0", "the fixture's output is a TIFF");
    assert!(folder.check_cache(&project, true).is_empty());

    // Opening changed nothing on disk; the first save writes the current version and keeps the original.
    assert_eq!(std::fs::read(root.join("project.json")).unwrap(), original);
    let mut saved = project.clone();
    folder.save(&mut saved).unwrap();
    assert_eq!(folder.migrated_from(), None);
    assert_eq!(std::fs::read(root.join("project.v0.json")).unwrap(), original);
    let written = std::fs::read(root.join("project.json")).unwrap();
    assert_eq!(schema::header(&written).unwrap().version, u64::from(schema::VERSION));
    drop(folder);
    let (folder, reopened) = ProjectFolder::open(&root).unwrap();
    assert_eq!(folder.migrated_from(), None);
    assert_eq!(reopened, saved);
    // New ids continue after the migrated ones.
    let mut reopened = reopened;
    assert_eq!(reopened.add_layer(reopened.outputs[0].id(), "More").unwrap().get(), 6);
}

#[test]
fn a_damaged_v0_file_says_where_it_is_wrong() {
    let dir = TempDir::new("migration-damaged");
    let root = copy_of_fixture(&dir);
    let file = root.join("project.json");
    let text = std::fs::read_to_string(&file).unwrap().replace("\"bytes\": 25165312,", "");
    std::fs::write(&file, text).unwrap();
    let e = ProjectFolder::open(&root).unwrap_err();
    let ProjectError::Migration { from: 0, source, .. } = &e else { panic!("{e:?}") };
    assert_eq!(source.at, "/frames/1/bytes");
    assert!(e.to_string().contains("cannot upgrade"), "{e}");
}

#[test]
fn every_version_has_a_migration() {
    assert_eq!(schema::MIGRATIONS.len(), schema::VERSION as usize);
}
