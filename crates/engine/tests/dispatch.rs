//! Dispatching commands from JSON: every registered command runs from JSON text, as the CLI's `run --cmd`, the
//! control channel and the MCP server send it, and does what its help says.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::{TempDir, config, fixture_project, frame_files, run};
use florastack_engine::{CommandError, Engine, Event, Registry, Value, json};

/// Runs commands and remembers which.
struct Calls<'a> {
    engine: &'a mut Engine,
    names: BTreeSet<String>,
}

impl Calls<'_> {
    fn run(&mut self, name: &str, params: &str) -> Value {
        self.names.insert(name.to_owned());
        run(self.engine, name, params)
    }
}

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

#[test]
fn every_command_dispatches_from_json() {
    let dir = TempDir::new("every");
    let frames_dir = dir.path().join("frames");
    frame_files(dir.path(), 3);
    let fixture = fixture_project(&dir.path().join("fixture"));
    let mut engine = Engine::new(config(dir.path()));
    let mut calls = Calls { engine: &mut engine, names: BTreeSet::new() };

    assert_eq!(calls.run("project.findOrphans", "{}")["orphans"], json!([]));
    let created = calls.run("project.new", &format!(r#"{{ "name": "Wasp", "folder": {} }}"#, quoted(dir.path())));
    assert_eq!(created["name"], "Wasp");
    assert_eq!(calls.run("project.info", "")["frames"], 0, "empty parameters are none");
    calls.run("project.rename", r#"{ "name": "Wasp, side" }"#);
    let added = calls.run("project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, quoted(&frames_dir)));
    let ids: Vec<u64> = added["added"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert_eq!(ids.len(), 3, "{added}");
    let listed = calls.run("project.frames", "{}");
    assert!(listed["frames"][0]["path"].as_str().unwrap().ends_with("frame-00.tif"), "{listed}");
    assert!(listed["frames"][0]["hash"].as_str().unwrap().starts_with("sha256:"));
    let order = calls.run("project.moveFrames", &format!(r#"{{ "frames": [{}], "to": 0 }}"#, ids[2]));
    assert_eq!(order["order"], json!([ids[2], ids[0], ids[1]]));
    assert_eq!(calls.run("project.setFramesEnabled", &format!(r#"{{ "frames": [{}], "enabled": false }}"#, ids[0]))["changed"], 1);
    assert_eq!(calls.run("project.removeFrames", &format!(r#"{{ "frames": [{}] }}"#, ids[1]))["changed"], 1);
    assert_eq!(calls.run("develop.get", "{}")["settings"]["exposure"], 0.0);
    assert_eq!(calls.run("develop.set", r#"{ "exposure": 1.0, "noise_reduction": { "color": 0.25 } }"#)["settings"]["exposure"], 1.0);
    let preset = dir.path().join("mine.toml");
    let saved = calls.run("preset.save", &format!(r#"{{ "path": {}, "name": "Mine", "groups": ["develop"] }}"#, quoted(&preset)));
    assert_eq!(saved["groups"], json!(["develop"]));
    let read = calls.run("preset.read", &format!(r#"{{ "path": {} }}"#, quoted(&preset)));
    assert_eq!(read["name"], "Mine");
    assert_eq!(read["preferences"]["develop"]["exposure"], 1.0);
    calls.run("develop.reset", "{}");
    assert_eq!(calls.run("preset.apply", &format!(r#"{{ "path": {} }}"#, quoted(&preset)))["groups"], json!(["develop"]));
    assert_eq!(calls.run("develop.get", "{}")["settings"]["noise_reduction"]["color"], 0.25);
    let history = calls.run("edit.history", "{}");
    assert_eq!(history["undo"][0]["label"], "Apply preset");
    assert_eq!(history["undo"][0]["command"], "preset.apply");
    assert_eq!(calls.run("edit.undo", "{}")["label"], "Apply preset");
    assert_eq!(calls.run("edit.redo", "{}")["label"], "Apply preset");
    let jobs = calls.run("job.list", "{}");
    assert_eq!(jobs["jobs"][0]["command"], "project.addFrames");
    assert_eq!(jobs["jobs"][0]["state"], "done");
    assert_eq!(calls.run("job.cancel", r#"{ "job": 999 }"#)["cancelled"], false);
    calls.run("project.save", "{}");
    calls.run("project.close", "{}");

    // Outputs, on a project that has one.
    calls.run("project.open", &format!(r#"{{ "path": {} }}"#, quoted(&fixture.path)));
    let outputs = calls.run("output.list", "{}");
    assert_eq!(outputs["outputs"][0]["name"], "Pyramid");
    assert_eq!(outputs["outputs"][0]["layers"][0]["name"], "Bristles");
    let output = fixture.output.get();
    calls.run("output.rename", &format!(r#"{{ "output": {output}, "name": "Pyramid, final" }}"#));
    let file = dir.path().join("out.tif");
    let spec = r#"{ "format": { "format": "tiff", "compression": "lzw" }, "depth": "u16" }"#;
    let written = calls.run("output.save", &format!(r#"{{ "output": {output}, "path": {}, "spec": {spec} }}"#, quoted(&file)));
    assert_eq!(written["bytes"].as_u64().unwrap(), std::fs::metadata(&file).unwrap().len());
    calls.run("output.remove", &format!(r#"{{ "output": {output} }}"#));
    calls.run("project.close", r#"{ "discardChanges": true }"#);

    // A temporary project, old enough at once with `days: 0`, goes to the trash.
    let temporary = calls.run("project.new", "{}");
    assert_eq!(temporary["temporary"], true);
    calls.run("project.close", "{}");
    let orphans = calls.run("project.findOrphans", r#"{ "days": 0 }"#);
    let path = orphans["orphans"][0]["path"].as_str().unwrap().to_owned();
    let trashed = calls.run("project.trashOrphans", &format!(r#"{{ "paths": [{}], "days": 0 }}"#, serde_json::to_string(&path).unwrap()));
    assert_eq!(trashed["trashed"], json!([path]));
    assert!(dir.path().join("trash").join(Path::new(&path).file_name().unwrap()).is_dir(), "in the trash, not deleted");

    let all: BTreeSet<String> = Registry::standard().names().into_iter().map(str::to_owned).collect();
    let missed: Vec<&String> = all.difference(&calls.names).collect();
    assert!(missed.is_empty(), "commands this test never dispatched: {missed:?}");
}

#[test]
fn adding_frames_reports_what_it_skips() {
    let dir = TempDir::new("skips");
    let files = frame_files(dir.path(), 2);
    let bogus = dir.path().join("frames").join("notes.tif");
    std::fs::write(&bogus, b"not an image").unwrap();
    std::fs::write(dir.path().join("frames").join("readme.txt"), b"ignored: not a frame file name").unwrap();
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Skips", "folder": {} }}"#, quoted(dir.path())));
    let first = run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, quoted(&files[0])));
    assert_eq!(first["added"].as_array().unwrap().len(), 1);
    let copy = dir.path().join("copy.tif");
    std::fs::copy(&files[0], &copy).unwrap();
    let paths = [&dir.path().join("frames"), &files[1], &copy, &dir.path().join("missing.tif")];
    let list = paths.iter().map(|p| quoted(p)).collect::<Vec<_>>().join(", ");
    let result = run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{list}] }}"#));
    assert_eq!(result["added"].as_array().unwrap().len(), 1, "{result}");
    let skipped: Vec<(String, String)> = result["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["path"].as_str().unwrap().to_owned(), s["reason"].as_str().unwrap().to_owned()))
        .collect();
    let reason = |name: &str| skipped.iter().find(|(p, _)| p.ends_with(name)).map_or_else(|| panic!("{name}: {skipped:?}"), |(_, r)| r.as_str());
    assert!(reason("frame-00.tif").contains("in the project already"), "{skipped:?}");
    assert!(reason("notes.tif").contains("format"), "{skipped:?}");
    assert!(reason("frame-01.tif").contains("listed twice"), "{skipped:?}");
    assert!(reason("copy.tif").contains("in the project already"), "the same contents: {skipped:?}");
    assert!(reason("missing.tif").contains("cannot read"), "{skipped:?}");
    assert!(!skipped.iter().any(|(p, _)| p.ends_with("readme.txt")), "a folder adds frame files only");
    assert_eq!(engine.project().unwrap().frames.len(), 2);
}

#[test]
fn outputs_are_saved_with_metadata_and_never_overwritten_by_surprise() {
    let dir = TempDir::new("save");
    let fixture = fixture_project(dir.path());
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&fixture.path)));
    let output = fixture.output.get();
    let file = dir.path().join("bee.jpg");
    let params = |extra: &str| {
        format!(
            r#"{{ "output": {output}, "path": {}, "spec": {{ "format": {{ "format": "jpeg", "quality": 90, "subsampling": "s444" }}, "depth": "u8" }}{extra} }}"#,
            quoted(&file)
        )
    };
    run(&mut engine, "output.save", &params(""));
    let decoded = florastack_codecs::read_file(&file, &florastack_codecs::ReadOptions::default()).unwrap();
    assert_eq!((decoded.image.width(), decoded.image.height()), (12, 8));
    let metadata = florastack_codecs::read_metadata(&std::fs::read(&file).unwrap());
    assert!(metadata.summary().software.as_deref().unwrap().starts_with("FloraStack"), "{:?}", metadata.summary());
    let e = engine.dispatch_json("output.save", &params("")).unwrap_err();
    assert!(e.to_string().contains("overwrite"), "{e}");
    run(&mut engine, "output.save", &params(r#", "overwrite": true"#));
    let e = engine.dispatch_json("output.save", &params(r#", "metadataFrom": 99"#)).unwrap_err();
    assert!(matches!(e, CommandError::NotFound { .. }), "{e}");
    let png16 = format!(
        r#"{{ "output": {output}, "path": {}, "spec": {{ "format": {{ "format": "jpeg", "quality": 90, "subsampling": "s444" }}, "depth": "u16" }} }}"#,
        quoted(&dir.path().join("x.jpg"))
    );
    assert_eq!(engine.dispatch_json("output.save", &png16).unwrap_err().code(), "params", "JPEG has no 16-bit");
    let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(!files.iter().any(|f| Path::new(f).extension().is_some_and(|e| e == "part")), "no temporary file is left: {files:?}");
}

#[test]
fn develop_settings_merge_and_are_checked() {
    let dir = TempDir::new("develop");
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Develop", "folder": {} }}"#, quoted(dir.path())));
    run(&mut engine, "develop.set", r#"{ "noise_reduction": { "luminance": 0.1, "color": 0.2 }, "white_balance": "auto" }"#);
    let s = run(&mut engine, "develop.set", r#"{ "noise_reduction": { "color": 0.5 }, "exposure": -1 }"#)["settings"].clone();
    assert_eq!(s["noise_reduction"], json!({ "luminance": f64::from(0.1_f32), "color": 0.5 }), "nested tables merge");
    assert_eq!(s["white_balance"], "auto", "other keys stay");
    let steps = run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len();
    run(&mut engine, "develop.set", r#"{ "exposure": -1 }"#);
    assert_eq!(run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len(), steps, "no change, no step");
    for (bad, field) in [
        (r#"{ "exposure": 11 }"#, "exposure"),
        (r#"{ "exposure": 1e300 }"#, "exposure"),
        (r#"{ "white_balance": { "custom": { "temperature": 100, "tint": 0 } } }"#, "temperature"),
        (r#"{ "noise_reduction": { "color": 2 } }"#, "noise_reduction.color"),
    ] {
        let e = engine.dispatch_json("develop.set", bad).unwrap_err();
        assert_eq!(e.code(), "params", "{bad}: {e}");
        assert!(e.info().message.contains(field), "{bad}: {}", e.info().message);
    }
    for bad in [r#"{ "expsure": 1 }"#, r#"{ "white_balance": { "custom": { "temperature": 5000, "tint": 0, "tnt": 1 } } }"#, r#"{ "exposure": "1" }"#]
    {
        let e = engine.dispatch_json("develop.set", bad).unwrap_err();
        assert!(matches!(e, CommandError::Params { .. }), "{bad}: {e}");
    }
    assert_eq!(run(&mut engine, "develop.get", "{}")["settings"]["exposure"], -1.0, "refused changes changed nothing");
    assert_eq!(run(&mut engine, "develop.reset", "{}")["settings"]["exposure"], 0.0);
}

#[test]
fn presets_with_groups_that_cant_be_checked_are_refused() {
    let dir = TempDir::new("presets");
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Presets", "folder": {} }}"#, quoted(dir.path())));
    let preset = dir.path().join("rail.toml");
    std::fs::write(&preset, "[preset]\nname = \"Rail\"\n\n[develop]\nexposure = 0.5\n\n[align]\nrotation = false\n").unwrap();
    let e = engine.dispatch_json("preset.apply", &format!(r#"{{ "path": {} }}"#, quoted(&preset))).unwrap_err();
    assert_eq!(e.code(), "params");
    assert!(e.info().message.contains("`align` can't be checked"), "{}", e.info().message);
    assert!(engine.project().unwrap().preferences.is_empty(), "nothing applied");
    let applied = run(&mut engine, "preset.apply", &format!(r#"{{ "path": {}, "groups": ["develop"] }}"#, quoted(&preset)));
    assert_eq!(applied, json!({ "name": "Rail", "groups": ["develop"] }));
    std::fs::write(&preset, "[develop]\nexposure = 50.0\n").unwrap();
    assert_eq!(engine.dispatch_json("preset.apply", &format!(r#"{{ "path": {} }}"#, quoted(&preset))).unwrap_err().code(), "params");
    std::fs::write(&preset, "[develop]\nexposre = 1.0\n").unwrap();
    assert!(engine.dispatch_json("preset.apply", &format!(r#"{{ "path": {} }}"#, quoted(&preset))).is_err(), "a misspelled key");
    let e = engine.dispatch_json("preset.save", &format!(r#"{{ "path": {} }}"#, quoted(&preset))).unwrap_err();
    assert_eq!(e.code(), "unavailable", "{e}: the file exists");
}

#[test]
fn commands_say_what_they_need() {
    let dir = TempDir::new("needs");
    let mut engine = Engine::new(config(dir.path()));
    for name in ["project.info", "project.save", "project.addFrames", "develop.set", "edit.undo", "output.list"] {
        let params = if name == "project.addFrames" { r#"{ "paths": ["a.tif"] }"# } else { "{}" };
        let e = engine.dispatch_json(name, params).unwrap_err();
        assert_eq!(e.code(), "unavailable", "{name}: {e}");
        assert!(e.to_string().contains("no project is open"), "{name}: {e}");
    }
    assert_eq!(run(&mut engine, "project.close", "{}"), json!({ "path": null, "cleaned": 0 }), "closing nothing is fine");
    let fixture = fixture_project(dir.path());
    run(&mut engine, "project.open", &format!(r#"{{ "path": {}, "readOnly": true }}"#, quoted(&fixture.path)));
    let e = engine.dispatch_json("project.rename", r#"{ "name": "x" }"#).unwrap_err();
    assert!(e.to_string().contains("read-only"), "{e}");
    assert_eq!(run(&mut engine, "project.info", "{}")["readOnly"], true);
    let e = engine.dispatch_json("edit.undo", "{}").unwrap_err();
    assert!(e.to_string().contains("read-only"), "{e}");
}

#[test]
fn unsaved_changes_are_never_thrown_away_silently() {
    let dir = TempDir::new("dirty");
    let fixture = fixture_project(dir.path());
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&fixture.path)));
    assert!(!engine.is_dirty());
    run(&mut engine, "project.rename", r#"{ "name": "Changed" }"#);
    assert!(engine.is_dirty());
    for (name, params) in [
        ("project.close", "{}".to_owned()),
        ("project.open", format!(r#"{{ "path": {} }}"#, quoted(&fixture.path))),
        ("project.new", "{}".to_owned()),
    ] {
        let e = engine.dispatch_json(name, &params).unwrap_err();
        assert!(e.to_string().contains("unsaved changes"), "{name}: {e}");
        assert_eq!(engine.project().unwrap().name(), Some("Changed"), "{name} left the project open");
    }
    run(&mut engine, "edit.undo", "{}");
    assert!(!engine.is_dirty(), "undone back to the saved state");
    run(&mut engine, "edit.redo", "{}");
    run(&mut engine, "project.save", "{}");
    assert!(!engine.is_dirty());
    run(&mut engine, "edit.undo", "{}");
    assert!(engine.is_dirty(), "undoing past a save changes the project");
    // Reopening the project that is open closes it first.
    run(&mut engine, "project.open", &format!(r#"{{ "path": {}, "discardChanges": true }}"#, quoted(&fixture.path)));
    assert_eq!(engine.project().unwrap().name(), Some("Changed"), "as saved");
}

#[test]
fn what_commands_do_is_saved() {
    let dir = TempDir::new("saved");
    let fixture = fixture_project(dir.path());
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&fixture.path)));
    run(&mut engine, "project.setFramesEnabled", &format!(r#"{{ "frames": [{}], "enabled": false }}"#, fixture.frames[1].get()));
    run(&mut engine, "develop.set", r#"{ "exposure": 0.75 }"#);
    run(&mut engine, "output.rename", &format!(r#"{{ "output": {}, "name": "Final" }}"#, fixture.output.get()));
    run(&mut engine, "project.save", "{}");
    let before = common::state(&engine);
    run(&mut engine, "project.close", "{}");
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&fixture.path)));
    assert_eq!(common::state(&engine), before);
    assert_eq!(run(&mut engine, "edit.history", "{}")["undo"], json!([]), "the history isn't saved");
}

#[test]
fn events_tell_clients_what_happened() {
    let dir = TempDir::new("events");
    let files = frame_files(dir.path(), 2);
    let mut engine = Engine::new(config(dir.path()));
    let events = engine.subscribe();
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Events", "folder": {} }}"#, quoted(dir.path())));
    let list = files.iter().map(|p| quoted(p)).collect::<Vec<_>>().join(", ");
    run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{list}] }}"#));
    run(&mut engine, "develop.set", r#"{ "exposure": 0.5 }"#);
    let got: Vec<Event> = events.try_iter().collect();
    let lines: Vec<String> = got.iter().map(|e| serde_json::to_string(e).unwrap()).collect();
    assert!(matches!(got.first(), Some(Event::Project { .. })), "{lines:#?}");
    let job_states: Vec<String> = got
        .iter()
        .filter_map(|e| if let Event::Job { job } = e { Some(serde_json::to_value(job.state).unwrap().as_str().unwrap().to_owned()) } else { None })
        .collect();
    assert_eq!(job_states.first().map(String::as_str), Some("running"), "{job_states:?}");
    assert_eq!(job_states.last().map(String::as_str), Some("done"), "{job_states:?}");
    let changed: Vec<&Event> = got.iter().filter(|e| matches!(e, Event::Changed { .. })).collect();
    assert!(changed.iter().any(|e| serde_json::to_value(e).unwrap()["aspects"] == json!(["frames"])), "{lines:#?}");
    assert!(changed.iter().any(|e| serde_json::to_value(e).unwrap()["aspects"] == json!(["develop"])), "{lines:#?}");
    let history: Vec<Value> = got.iter().filter(|e| matches!(e, Event::History { .. })).map(|e| serde_json::to_value(e).unwrap()).collect();
    assert_eq!(history.last().unwrap(), &json!({ "event": "history", "undo": "Set develop settings", "dirty": true }));
    for line in &lines {
        let value: Value = serde_json::from_str(line).unwrap();
        assert!(value["event"].is_string(), "every event says what it is: {line}");
    }
}
