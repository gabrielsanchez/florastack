//! Undo and redo: every undoable command is one step that undo takes back exactly and redo makes again; retouch strokes
//! undo one at a time; the calls of one slider drag are one step.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeSet;

use common::{Gate, TempDir, open_fixture, run, state, test_registry};
use florastack_engine::{DispatchOptions, Engine, Registry, json};

/// An undoable command, how to set the project up for it, and its parameters.
struct Case {
    name: &'static str,
    setup: Vec<(&'static str, String)>,
    params: String,
}

fn cases(fixture: &common::Fixture, dir: &std::path::Path) -> Vec<Case> {
    let [a, b, c] = [fixture.frames[0].get(), fixture.frames[1].get(), fixture.frames[2].get()];
    let (output, layer) = (fixture.output.get(), fixture.layer.get());
    let extra = dir.join("extra.tif");
    common::write_tiff(&extra, 99);
    let preset = dir.join("p.toml");
    std::fs::write(&preset, "[develop]\nexposure = 1.25\nwhite_balance = \"auto\"\n").unwrap();
    let quoted = |p: &std::path::Path| serde_json::to_string(&p.display().to_string()).unwrap();
    let none = Vec::new;
    // The fixture's frames are too small to align: the cases that align disable them and add these, in a shuffled order.
    let stack = common::synthetic_frames(dir, "synthetic", 160, 120, &common::breathing(5, 0.02, None, 9));
    let shuffled = [3, 0, 4, 1, 2].map(|i| stack[i].clone());
    // A defect mask the size of the synthetic frames: a black spot on white.
    let mask = dir.join("mask.png");
    let drawn =
        florastack_core::Image::from_fn(
            160,
            120,
            1,
            florastack_core::ColorSpace::SRGB,
            |x, y, _| if x.abs_diff(40) + y.abs_diff(30) < 4 { 0_u8 } else { 255 },
        )
        .unwrap();
    let png = florastack_codecs::OutputSpec::new(florastack_codecs::Format::Png).unwrap();
    std::fs::write(
        &mask,
        florastack_codecs::write(&florastack_core::AnyImage::U8(drawn), &png, &florastack_codecs::OutputMetadata::default()).unwrap().bytes,
    )
    .unwrap();
    let aligning = || {
        vec![
            ("project.setFramesEnabled", format!(r#"{{ "frames": [{a}, {b}, {c}], "enabled": false }}"#)),
            ("project.addFrames", format!(r#"{{ "paths": [{}] }}"#, common::path_list(&shuffled))),
        ]
    };
    vec![
        Case { name: "project.rename", setup: none(), params: r#"{ "name": "Renamed" }"#.into() },
        Case { name: "project.addFrames", setup: none(), params: format!(r#"{{ "paths": [{}] }}"#, quoted(&extra)) },
        Case { name: "project.removeFrames", setup: none(), params: format!(r#"{{ "frames": [{a}, {c}] }}"#) },
        Case { name: "project.moveFrames", setup: none(), params: format!(r#"{{ "frames": [{c}, {a}], "to": 9 }}"#) },
        Case { name: "project.setFramesEnabled", setup: none(), params: format!(r#"{{ "frames": [{b}], "enabled": false }}"#) },
        Case { name: "develop.set", setup: none(), params: r#"{ "exposure": 2.0, "highlights": "off" }"#.into() },
        Case {
            name: "develop.reset",
            setup: vec![("develop.set", r#"{ "exposure": 2.0 }"#.into()), ("project.save", "{}".into())],
            params: "{}".into(),
        },
        Case { name: "preset.apply", setup: none(), params: format!(r#"{{ "path": {} }}"#, quoted(&preset)) },
        Case { name: "align.set", setup: none(), params: r#"{ "framing": "common", "rotation": false }"#.into() },
        Case {
            name: "align.reset",
            setup: vec![("align.set", r#"{ "framing": "wide" }"#.into()), ("project.save", "{}".into())],
            params: "{}".into(),
        },
        Case { name: "align.run", setup: aligning(), params: "{}".into() },
        Case { name: "align.sortByScale", setup: aligning(), params: "{}".into() },
        Case { name: "defects.detect", setup: aligning(), params: "{}".into() },
        Case { name: "defects.import", setup: none(), params: format!(r#"{{ "path": {} }}"#, quoted(&mask)) },
        Case {
            name: "defects.clear",
            setup: vec![("defects.import", format!(r#"{{ "path": {} }}"#, quoted(&mask))), ("project.save", "{}".into())],
            params: "{}".into(),
        },
        Case { name: "output.rename", setup: none(), params: format!(r#"{{ "output": {output}, "name": "Renamed" }}"#) },
        Case { name: "output.remove", setup: none(), params: format!(r#"{{ "output": {output} }}"#) },
        Case {
            name: "test.stroke",
            setup: none(),
            params: format!(r#"{{ "output": {output}, "layer": {layer}, "stroke": {{ "brush": "details", "points": [[1, 2]] }} }}"#),
        },
    ]
}

#[test]
fn every_undoable_command_is_undone_and_redone_exactly() {
    let gate = Gate::new();
    let mut covered = BTreeSet::new();
    let names: Vec<&str> = {
        let dir = TempDir::new("undo-cases");
        let fixture = common::fixture_project(dir.path());
        cases(&fixture, dir.path()).iter().map(|c| c.name).collect()
    };
    for name in names {
        let dir = TempDir::new("undo");
        let (mut engine, fixture) = open_fixture(dir.path(), test_registry(&gate));
        let case = cases(&fixture, dir.path()).into_iter().find(|c| c.name == name).unwrap();
        for (setup, params) in &case.setup {
            run(&mut engine, setup, params);
        }
        let (before, dirty) = (state(&engine), engine.is_dirty());
        let steps = run(&mut engine, "edit.history", "{}")["undo"].as_array().unwrap().len();
        run(&mut engine, case.name, &case.params);
        let after = state(&engine);
        assert_ne!(after, before, "{name} changed nothing");
        assert!(engine.is_dirty(), "{name}");
        let history = run(&mut engine, "edit.history", "{}");
        assert_eq!(history["undo"].as_array().unwrap().len(), steps + 1, "{name} is one step: {history}");
        assert_eq!(history["undo"][0]["command"], case.name);
        let title = engine.registry().get(case.name).unwrap().title;
        assert_eq!(run(&mut engine, "edit.undo", "{}")["label"], title);
        assert_eq!(state(&engine), before, "undoing {name} restores the project");
        assert_eq!(engine.is_dirty(), dirty, "{name}: back to the saved state");
        assert_eq!(run(&mut engine, "edit.redo", "{}")["label"], title);
        assert_eq!(state(&engine), after, "redoing {name} makes it again, with the same ids");
        covered.insert(case.name);
    }
    let undoable: BTreeSet<&str> = Registry::standard().list().into_iter().filter(|c| c.undoable).map(|c| c.name).collect();
    let missed: Vec<&&str> = undoable.difference(&covered).collect();
    assert!(missed.is_empty(), "undoable commands this test doesn't undo: {missed:?}");
    for name in Registry::standard().names() {
        let info = Registry::standard().get(name).cloned().unwrap();
        assert!(covered.contains(name) || !info.undoable, "{name}");
    }
}

#[test]
fn retouch_strokes_undo_one_at_a_time() {
    let dir = TempDir::new("strokes");
    let (mut engine, fixture) = open_fixture(dir.path(), test_registry(&Gate::new()));
    let (output, layer) = (fixture.output, fixture.layer);
    let strokes = |engine: &Engine| engine.project().unwrap().output(output).unwrap().layer(layer).unwrap().strokes.clone();
    for n in 0..3 {
        run(&mut engine, "test.stroke", &format!(r#"{{ "output": {}, "layer": {}, "stroke": {{ "n": {n} }} }}"#, output.get(), layer.get()));
    }
    assert_eq!(strokes(&engine), [json!({ "n": 0 }), json!({ "n": 1 }), json!({ "n": 2 })]);
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(strokes(&engine), [json!({ "n": 0 }), json!({ "n": 1 })], "the last stroke only");
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(strokes(&engine), [json!({ "n": 0 })]);
    run(&mut engine, "edit.redo", "{}");
    assert_eq!(strokes(&engine).len(), 2);
    // A new stroke after an undo drops the redo step.
    run(&mut engine, "test.stroke", &format!(r#"{{ "output": {}, "layer": {}, "stroke": {{ "n": 9 }} }}"#, output.get(), layer.get()));
    assert_eq!(run(&mut engine, "edit.history", "{}")["redo"], json!([]));
    assert_eq!(strokes(&engine).last(), Some(&json!({ "n": 9 })));
    // Strokes are saved, and come back with the project.
    run(&mut engine, "project.save", "{}");
    run(&mut engine, "project.close", "{}");
    run(&mut engine, "project.open", &format!(r#"{{ "path": {} }}"#, serde_json::to_string(&fixture.path).unwrap()));
    assert_eq!(strokes(&engine).len(), 3);
}

#[test]
fn a_slider_drag_is_one_step() {
    let dir = TempDir::new("drag");
    let (mut engine, _) = open_fixture(dir.path(), Registry::standard());
    run(&mut engine, "develop.set", r#"{ "exposure": 0.25 }"#);
    let before = state(&engine);
    for i in 1..=20 {
        let options = DispatchOptions { coalesce: Some("drag-exposure".into()) };
        engine.dispatch_with("develop.set", json!({ "exposure": f64::from(i) / 10.0 }), options).unwrap();
    }
    assert_eq!(engine.project().unwrap().preferences.group(florastack_engine::model::Group::Develop)["exposure"], 2.0);
    let history = run(&mut engine, "edit.history", "{}");
    assert_eq!(history["undo"].as_array().unwrap().len(), 2, "the first set, then the drag: {history}");
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(state(&engine), before, "one undo takes the whole drag back");
    let e = engine.dispatch("edit.redo", json!({ "bogus": 1 })).unwrap_err();
    assert_eq!(e.code(), "params");
    run(&mut engine, "edit.undo", "{}");
    let e = engine.dispatch("edit.undo", json!({})).unwrap_err();
    assert!(e.to_string().contains("nothing to undo"), "{e}");
}
