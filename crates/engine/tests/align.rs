//! The alignment commands on synthetic stacks written as frame files: `align.run` orders the stack from its narrow
//! end, flags the frames focused off the scene, and records the run in the project, as one undo step that survives a
//! save; `align.info` frames the stored alignment three ways; `align.sortByScale` restores a shuffled stack and is
//! undone whole; and stacks that can't be aligned change nothing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use common::{TempDir, breathing, config, path_list, run, state, synthetic_frames};
use florastack_engine::{CommandError, Engine, Value, json};

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

/// An engine with a new project in `dir` holding `files`, in that order; and the frames' ids.
fn project_with(dir: &Path, files: &[PathBuf]) -> (Engine, Vec<u64>) {
    std::fs::create_dir_all(dir).unwrap();
    let mut engine = Engine::new(config(dir));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Stack", "folder": {} }}"#, quoted(dir)));
    let added = run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(files)));
    let ids: Vec<u64> = added["added"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert_eq!(ids.len(), files.len(), "{added}");
    (engine, ids)
}

fn ids_of(value: &Value) -> Vec<u64> {
    value.as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect()
}

#[test]
fn aligning_orders_the_stack_from_its_narrow_end_and_flags_frames_out_of_focus() {
    let dir = TempDir::new("align-run");
    // Nine frames focused every quarter from 0.75 in front of the scene to 0.25 behind it, each 1% larger than the one
    // before: the last is the narrow end, and the first two, blurred by 4 pixels or more everywhere, have nothing in
    // focus.
    let files = synthetic_frames(dir.path(), "stack", 160, 120, &breathing(9, 0.01, Some((-0.75, 1.25)), 3));
    let (mut engine, ids) = project_with(dir.path(), &files);
    let before = state(&engine);
    let aligned = run(&mut engine, "align.run", "{}");
    println!("{aligned}");
    assert_eq!((aligned["master"].as_u64(), &aligned["narrowEnd"], &aligned["start"]), (Some(ids[8]), &json!("last"), &json!("last")));
    assert_eq!(ids_of(&aligned["order"]), ids.iter().rev().copied().collect::<Vec<_>>(), "processed from the narrow end");
    let out_of_focus = ids_of(&aligned["outOfFocus"]);
    assert!(out_of_focus.contains(&ids[0]) && out_of_focus.contains(&ids[1]), "{aligned}");
    assert!(out_of_focus.iter().all(|id| [ids[0], ids[1], ids[2], ids[8]].contains(id)), "{aligned}");
    assert_eq!(aligned["rejected"], json!([]));

    // The frame list shows the flags, for the user to disable the frames.
    let listed = run(&mut engine, "project.frames", "{}");
    let frames = listed["frames"].as_array().unwrap();
    assert!(frames.iter().all(|f| f["aligned"] == true), "{listed}");
    assert!(frames[0]["flags"].as_array().unwrap().contains(&json!("out-of-focus")), "{listed}");
    assert_eq!(frames[4]["flags"], json!([]));
    assert!(frames[4]["sharpFraction"].as_f64().unwrap() > 0.01);

    // The stored alignment, framed three ways.
    let info = run(&mut engine, "align.info", "{}");
    let r = &info["run"];
    assert_eq!(
        (&info["aligned"], &info["framing"], r["master"].as_u64(), &r["width"], &r["height"]),
        (&json!(true), &json!("narrow"), Some(ids[8]), &json!(160), &json!(120))
    );
    let framing = |name: &str| r["framings"].as_array().unwrap().iter().find(|f| f["framing"] == name).unwrap().clone();
    assert_eq!(framing("narrow"), json!({ "framing": "narrow", "width": 160, "height": 120, "toMaster": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] }));
    let (wide, common) = (framing("wide"), framing("common"));
    assert!(wide["width"].as_u64().unwrap() > 170 && wide["height"].as_u64().unwrap() > 125, "{wide}");
    assert!(common["width"].as_u64().unwrap() <= 160 && common["width"].as_u64().unwrap() > 150, "{common}");
    let infos = r["frames"].as_array().unwrap();
    assert_eq!(infos.len(), 9);
    assert_eq!((&infos[8]["status"], &infos[8]["scale"]), (&json!("master"), &json!(1.0)));
    assert!(infos[..8].iter().all(|f| f["scale"].as_f64().unwrap() < 1.0 && f["status"] == "aligned"), "every other frame sees more: {info}");

    // One undo step; saved and reopened, it is all there.
    let after = state(&engine);
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(state(&engine), before);
    assert_eq!(run(&mut engine, "align.info", "{}")["aligned"], false);
    run(&mut engine, "edit.redo", "{}");
    assert_eq!(state(&engine), after);
    run(&mut engine, "project.save", "{}");
    let path = engine.project_path().unwrap().to_path_buf();
    run(&mut engine, "project.close", "{}");
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&path)));
    assert_eq!(run(&mut engine, "align.info", "{}"), info);

    // The framing follows the settings at once, with no new alignment.
    run(&mut engine, "align.set", r#"{ "framing": "wide" }"#);
    assert_eq!(run(&mut engine, "align.info", "{}")["framing"], "wide");

    // The flagged frames disabled and the stack aligned again: they lose their alignment.
    run(&mut engine, "project.setFramesEnabled", &format!(r#"{{ "frames": {}, "enabled": false }}"#, json!(out_of_focus)));
    let again = run(&mut engine, "align.run", "{}");
    assert!(ids_of(&again["order"]).iter().all(|id| !out_of_focus.contains(id)), "{again}");
    let listed = run(&mut engine, "project.frames", "{}");
    for f in listed["frames"].as_array().unwrap() {
        let disabled = out_of_focus.contains(&f["id"].as_u64().unwrap());
        assert_eq!(f["aligned"] == false, disabled, "{f}");
    }
}

#[test]
fn sorting_by_scale_restores_a_shuffled_stack_in_one_undo_step() {
    let dir = TempDir::new("align-sort");
    let files = synthetic_frames(dir.path(), "stack", 160, 120, &breathing(8, 0.02, None, 4));
    let shuffled = [5, 2, 7, 0, 3, 6, 1, 4];
    let (mut engine, ids) = project_with(dir.path(), &shuffled.map(|i| files[i].clone()));
    // A frame that isn't enabled keeps its place.
    run(&mut engine, "project.setFramesEnabled", &format!(r#"{{ "frames": [{}], "enabled": false }}"#, ids[4]));
    let before = state(&engine);
    let sorted = run(&mut engine, "align.sortByScale", "{}");
    println!("{sorted}");
    let order = ids_of(&sorted["frameOrder"]);
    let names: Vec<usize> = order.iter().map(|id| shuffled[ids.iter().position(|i| i == id).unwrap()]).collect();
    assert_eq!(names[4], 3, "the disabled frame stays where it was: {names:?}");
    let enabled: Vec<usize> = names.iter().copied().filter(|&n| n != 3).collect();
    let forward = [0, 1, 2, 4, 5, 6, 7];
    assert!(enabled == forward || enabled.iter().rev().copied().eq(forward), "{names:?}");
    let project_order: Vec<u64> = engine.project().unwrap().frames.iter().map(|f| f.id().get()).collect();
    assert_eq!(project_order, order);
    let info = run(&mut engine, "align.info", "{}");
    assert_eq!(info["run"]["frames"].as_array().unwrap().len(), 7, "the enabled frames are aligned");
    assert_eq!(sorted["rejected"], json!([]));

    // The new order and the alignment are one step.
    let after = state(&engine);
    let history = run(&mut engine, "edit.history", "{}");
    assert_eq!(history["undo"][0]["command"], "align.sortByScale");
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(state(&engine), before, "the shuffled order, unaligned");
    run(&mut engine, "edit.redo", "{}");
    assert_eq!(state(&engine), after);
}

#[test]
fn stacks_that_cant_be_aligned_or_sorted_change_nothing() {
    let dir = TempDir::new("align-errors");
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Empty", "folder": {} }}"#, quoted(dir.path())));
    for name in ["align.run", "align.sortByScale"] {
        let e = engine.dispatch_json(name, "{}").unwrap_err();
        assert_eq!(e.code(), "unavailable", "{name}: {e}");
        assert!(e.to_string().contains("enabled frames"), "{e}");
    }
    run(&mut engine, "project.close", r#"{ "discardChanges": true }"#);

    // Frames too small to align, and a frame of another size.
    let fixture = common::fixture_project(&dir.path().join("fixture"));
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, quoted(&fixture.path)));
    let before = state(&engine);
    let job = common::job_of(engine.dispatch_json("align.run", "{}").unwrap());
    let e = engine.wait(job).unwrap_err();
    assert_eq!(e.code(), "align", "{e}");
    assert!(e.to_string().contains("too small"), "{e}");
    assert_eq!(state(&engine), before);
    run(&mut engine, "project.close", r#"{ "discardChanges": true }"#);

    let mut files = synthetic_frames(dir.path(), "stack", 160, 120, &breathing(4, 0.0, None, 5));
    files.extend(synthetic_frames(dir.path(), "other", 100, 80, &breathing(1, 0.0, None, 6)));
    let (mut engine, ids) = project_with(&dir.path().join("sizes"), &files);
    let job = common::job_of(engine.dispatch_json("align.run", "{}").unwrap());
    let e = engine.wait(job).unwrap_err();
    assert_eq!(e.code(), "unavailable", "{e}");
    assert!(e.to_string().contains(&format!("frame {}", ids[4])) && e.to_string().contains("100x80"), "{e}");
    assert!(engine.project().unwrap().alignment.is_none());

    // Frames that don't breathe can't be sorted by scale; one frame can't be sorted at all.
    run(&mut engine, "project.setFramesEnabled", &format!(r#"{{ "frames": [{}], "enabled": false }}"#, ids[4]));
    let before = state(&engine);
    let steps = run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len();
    let job = common::job_of(engine.dispatch_json("align.sortByScale", "{}").unwrap());
    let e = engine.wait(job).unwrap_err();
    assert_eq!(e.code(), "align", "{e}");
    assert!(e.to_string().contains("too little to sort"), "{e}");
    assert_eq!(state(&engine), before);
    assert_eq!(run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len(), steps);
    run(&mut engine, "project.setFramesEnabled", &format!(r#"{{ "frames": [{}, {}, {}], "enabled": false }}"#, ids[1], ids[2], ids[3]));
    assert_eq!(engine.dispatch_json("align.sortByScale", "{}").unwrap_err().code(), "unavailable");
}

#[test]
fn a_cancelled_alignment_changes_nothing() {
    let dir = TempDir::new("align-cancel");
    let files = synthetic_frames(dir.path(), "stack", 160, 120, &breathing(6, 0.01, None, 7));
    let (mut engine, _) = project_with(dir.path(), &files);
    let before = state(&engine);
    let steps = run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len();
    for name in ["align.run", "align.sortByScale"] {
        let job = common::job_of(engine.dispatch_json(name, "{}").unwrap());
        // Cancelling wins over a job that finishes anyway: its commit is dropped.
        assert!(engine.cancel(job));
        assert!(matches!(engine.wait(job), Err(CommandError::Cancelled)), "{name}");
        assert_eq!(state(&engine), before, "{name}");
        assert_eq!(run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len(), steps);
    }
}

#[test]
fn settings_merge_and_are_checked() {
    let dir = TempDir::new("align-settings");
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Settings", "folder": {} }}"#, quoted(dir.path())));
    assert_eq!(run(&mut engine, "align.get", "{}")["settings"]["framing"], "narrow");
    run(&mut engine, "align.set", r#"{ "rotation": false, "framing": "common" }"#);
    let s = run(&mut engine, "align.set", r#"{ "reverse_order": true }"#)["settings"].clone();
    assert_eq!(
        (&s["rotation"], &s["framing"], &s["reverse_order"], &s["automatic_order"]),
        (&json!(false), &json!("common"), &json!(true), &json!(true))
    );
    for bad in [r#"{ "framing": "widest" }"#, r#"{ "min_sharp_fraction": 2 }"#, r#"{ "max_shift": 500 }"#, r#"{ "rotaton": false }"#] {
        assert_eq!(engine.dispatch_json("align.set", bad).unwrap_err().code(), "params", "{bad}");
    }
    assert_eq!(run(&mut engine, "align.reset", "{}")["settings"], serde_json::to_value(florastack_engine::AlignSettings::default()).unwrap());
    assert!(engine.project().unwrap().preferences.group(florastack_engine::model::Group::Align).is_empty());
}
