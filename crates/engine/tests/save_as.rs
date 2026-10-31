//! Saving a project in another folder (`project.saveAs`): a temporary project moves out of the temporary-projects
//! folder, a named one is copied and its original stays, and either keeps its undo history; the folders it refuses.
//! And checking a reopened project's frames (`project.checkFrames`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use common::{TempDir, config, frame_files, path_list, run};
use florastack_engine::{Engine, Event, ProjectEvent, json};

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

#[test]
fn a_temporary_project_moves_out_of_the_temporary_folder_with_its_history() {
    let dir = TempDir::new("save-as-temporary");
    let files = frame_files(dir.path(), 2);
    let mut engine = Engine::new(config(dir.path()));
    let events = engine.subscribe();
    run(&mut engine, "project.new", "{}");
    let temporary = engine.project_path().unwrap().to_path_buf();
    assert!(temporary.starts_with(dir.path().join("temp")), "{}", temporary.display());
    run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(&files)));
    // Unnamed, it has no name to go by.
    let e = engine.dispatch("project.saveAs", json!({ "folder": dir.path().join("saved") })).unwrap_err();
    assert_eq!(e.code(), "unavailable", "{e}");
    run(&mut engine, "project.rename", r#"{ "name": "Wasp" }"#);
    let saved = run(&mut engine, "project.saveAs", &format!(r#"{{ "folder": {} }}"#, quoted(&dir.path().join("saved"))));
    let target = dir.path().join("saved").join("Wasp.florastack");
    assert_eq!((saved["path"].as_str(), saved["moved"].as_bool()), (Some(target.display().to_string().as_str()), Some(true)));
    assert!(!temporary.exists(), "moved, not copied");
    assert!(target.join("project.json").is_file());
    assert_eq!(engine.project_path(), Some(target.as_path()));
    assert!(!engine.is_dirty(), "saved first");
    let history = run(&mut engine, "edit.history", "{}");
    assert_eq!(history["undo"][0]["command"], "project.rename", "the undo history stays: {history}");
    assert_eq!(history["undo"][1]["command"], "project.addFrames");
    // It is the open project there: changes save in its new folder.
    run(&mut engine, "project.rename", r#"{ "name": "Wasp, dorsal" }"#);
    run(&mut engine, "project.save", "{}");
    assert!(std::fs::read_to_string(target.join("project.json")).unwrap().contains("Wasp, dorsal"));
    let moved = events.try_iter().any(|e| matches!(e, Event::Project { state: ProjectEvent::Moved, ref path } if Path::new(path) == target));
    assert!(moved, "the move is an event");
}

#[test]
fn a_named_project_is_copied_and_its_original_stays() {
    let dir = TempDir::new("save-as-named");
    let files = frame_files(dir.path(), 2);
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Bee", "folder": {} }}"#, quoted(dir.path())));
    run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(&files)));
    let original = engine.project_path().unwrap().to_path_buf();
    let saved = run(&mut engine, "project.saveAs", &format!(r#"{{ "folder": {} }}"#, quoted(&dir.path().join("copies"))));
    assert_eq!(saved["moved"], false);
    let copy = dir.path().join("copies").join("Bee.florastack");
    assert_eq!(engine.project_path(), Some(copy.as_path()));
    assert!(original.join("project.json").is_file(), "the original stays as it was saved");
    // Its lock went with the project: the original can be opened again, by another engine.
    let mut other = Engine::new(config(&dir.path().join("other")));
    run(&mut other, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&original)));
    assert_eq!(other.project().unwrap().frames.len(), 2);
    // Saving as the folder it is in only saves it; onto another project's folder is refused.
    let again = run(&mut engine, "project.saveAs", &format!(r#"{{ "folder": {} }}"#, quoted(&dir.path().join("copies"))));
    assert_eq!(again["path"], json!(copy.display().to_string()));
    let e = engine.dispatch("project.saveAs", json!({ "folder": dir.path() })).unwrap_err();
    assert!(e.info().message.contains("already exists"), "{e}");
    assert_eq!(engine.project_path(), Some(copy.as_path()), "a refused save leaves the project where it was");
}

#[test]
fn frames_stored_relative_to_the_project_can_t_leave_its_folder() {
    let dir = TempDir::new("save-as-relative");
    let fixture = common::fixture_project(dir.path());
    // The fixture's frames are in `<dir>/frames`: store them relative to `<dir>`, the folder holding the project.
    let file = fixture.path.join("project.json");
    let text = std::fs::read_to_string(&file).unwrap();
    let absolute = serde_json::to_string(&dir.path().join("frames").display().to_string()).unwrap();
    let relative = text.replace(absolute.trim_matches('"'), "frames");
    assert_ne!(relative, text, "the fixture stores its frames' absolute paths");
    std::fs::write(&file, relative).unwrap();
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&fixture.path)));
    assert_eq!(run(&mut engine, "project.checkFrames", "{}")["frames"], json!([]), "found from the folder holding the project");
    let e = engine.dispatch("project.saveAs", json!({ "folder": dir.path().join("elsewhere") })).unwrap_err();
    assert_eq!(e.code(), "unavailable", "{e}");
    assert!(e.info().message.contains("relative"), "{e}");
    assert!(!dir.path().join("elsewhere").exists(), "nothing was made");
}

#[test]
fn missing_and_changed_frames_are_listed() {
    let dir = TempDir::new("check-frames");
    let files = frame_files(dir.path(), 3);
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", "{}");
    run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(&files)));
    let checked = run(&mut engine, "project.checkFrames", "{}");
    assert_eq!((checked["checked"].as_u64(), &checked["frames"]), (Some(3), &json!([])), "{checked}");
    std::fs::remove_file(&files[0]).unwrap();
    common::write_tiff(&files[2], 999);
    let checked = run(&mut engine, "project.checkFrames", "{}");
    let states: Vec<(&str, &str)> =
        checked["frames"].as_array().unwrap().iter().map(|f| (f["path"].as_str().unwrap(), f["state"].as_str().unwrap())).collect();
    let name = |p: &Path| p.display().to_string();
    assert_eq!(states, [(name(&files[0]).as_str(), "missing"), (name(&files[2]).as_str(), "changed")]);
}
