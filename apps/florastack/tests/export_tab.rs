//! The Export tab, presets and preferences, headless (`egui_kittest`, through AccessKit): an output's export settings
//! changed through their controls, its size estimated and the output exported with its aligned frames; a built-in
//! preset applied from the Presets menu, the project's settings saved as a preset, and a built-in preset picked for a
//! batch; and the Preferences dialog, with the GPU's self-test and settings that reach the engine. Stacking runs on
//! the CPU; file dialogs are left out (the app's methods behind them are called).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::PathBuf;

use common::{app, drop_files, frames, until};
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use florastack_app::FloraStackApp;
use florastack_app::app::Dialog;
use florastack_app::prefs::Tab;
use florastack_engine::json;

/// The app for test `name` with the synthetic stack dropped on it, aligned and stacked by the pyramid, on the Export
/// tab.
fn stacked(name: &str) -> (PathBuf, Harness<'static, FloraStackApp>) {
    let (dir, mut harness) = app(name);
    drop_files(&mut harness, &frames(&dir.join("photos")));
    until(&mut harness, "the frames to be listed", |app| app.shell.frames.len() == 4);
    harness.get_by_label("Align").click();
    until(&mut harness, "the alignment", |app| app.aligned() && !app.shell.busy());
    harness.get_by_label("Stack frames").click();
    until(&mut harness, "the output", |app| app.shell.outputs.len() == 1 && !app.shell.busy());
    harness.get_by_label("Export").click();
    harness.step();
    assert_eq!(harness.state().prefs.tab, Tab::Export);
    (dir, harness)
}

fn done(dir: PathBuf, harness: Harness<'static, FloraStackApp>) {
    assert!(harness.state().shell.notices.iter().all(|n| !n.error), "{:?}", harness.state().shell.notices);
    drop(harness);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_output_is_exported_as_the_tab_sets_it() {
    let (dir, mut harness) = stacked("export-tab");
    // The settings, with their defaults: a 16-bit TIFF in the frames' profile, with a frame's metadata.
    harness.get_by_label("Keep the frames' profile");
    harness.get_by_label("Copy metadata from frame");
    // The format, picked from its list: the depth follows what PNG holds.
    harness.get_by_label("Format").click();
    harness.run_steps(2);
    harness.get_by_label("PNG").click();
    until(&mut harness, "PNG", |app| app.shell.export_settings["format"] == "png");
    assert_eq!(harness.state().shell.export_settings["depth"], "u16");
    harness.get_by_label("sRGB").click();
    until(&mut harness, "sRGB", |app| app.shell.export_settings["profile"] == "srgb");
    harness.get_by_label("Aligned frames").click();
    until(&mut harness, "the aligned frames", |app| app.shell.export_settings["aligned_frames"] == true);
    // Each change is an undo step of the project's.
    assert_eq!(harness.state().shell.engine().undo_label(), Some("Set export settings"));
    // With the output picked, the size it takes: the PNG and the frames it was made from (the stack's alignment
    // rejects one of its four frames), aligned.
    let output = harness.state().shell.outputs[0].id;
    let estimate = harness.state_mut().shell.run("export.estimate", json!({ "output": output })).unwrap();
    let files = estimate["files"].as_u64().unwrap();
    assert!(files >= 4, "{estimate}");
    harness.get_by_label_contains(&format!("in {files} files"));
    let folder = dir.join("exported");
    harness.state_mut().export_to(output, &folder);
    until(&mut harness, "the export", |app| {
        !app.shell.busy() && app.shell.notices.iter().any(|n| n.text.starts_with(&format!("Exported {files} files")))
    });
    let written: Vec<PathBuf> = std::fs::read_dir(&folder).unwrap().map(|e| e.unwrap().path()).collect();
    assert!(written.iter().any(|p| p.extension().is_some_and(|e| e == "png")), "{written:?}");
    let aligned = written.iter().find(|p| p.is_dir()).unwrap();
    assert_eq!(std::fs::read_dir(aligned).unwrap().count() as u64, files - 1);
    // Again, the files are there: the export says so, unless told to replace them.
    harness.state_mut().export_to(output, &folder);
    until(&mut harness, "the refusal", |app| !app.shell.busy() && app.shell.notices.last().is_some_and(|n| n.error));
    assert!(harness.state().shell.notices.last().unwrap().text.contains("overwrite"));
    harness.state_mut().shell.notices.clear();
    harness.get_by_label("Replace files that exist").click();
    harness.step();
    harness.state_mut().export_to(output, &folder);
    until(&mut harness, "the export again", |app| !app.shell.busy() && app.shell.notices.iter().any(|n| n.text.starts_with("Exported")));
    done(dir, harness);
}

#[test]
fn presets_are_applied_from_the_menu_saved_and_picked_for_a_batch() {
    let (dir, mut harness) = stacked("presets");
    // A built-in preset, from the Presets menu: its dialog lists the settings it holds, all picked.
    harness.get_by_label("Presets").click();
    harness.run_steps(2);
    harness.get_by_label("Microscope").click();
    harness.run_steps(2);
    let Some(Dialog::ApplyPreset(applying)) = harness.state().dialog.clone() else { panic!("the preset's dialog") };
    assert_eq!(applying.groups.iter().map(|(g, on)| (g.as_str(), *on)).collect::<Vec<_>>(), [("align", true), ("stack", true), ("output", true)]);
    // Only its output settings.
    harness.get_by_label("Alignment").click();
    harness.get_by_label("Stacking").click();
    harness.step();
    harness.get_by_label("Apply").click();
    until(&mut harness, "the preset", |app| app.dialog.is_none());
    let preferences = harness.state().shell.engine().project().unwrap().preferences.clone();
    assert!(!preferences.group(florastack_engine::model::Group::Output).is_empty());
    assert!(preferences.group(florastack_engine::model::Group::Align).is_empty(), "alignment left out");
    harness.get_by_label_contains("Applied Microscope");
    // The project's settings, saved as a preset with every setting of its groups, and read back.
    let file = dir.join("mine.toml");
    let saving = florastack_app::presets::Saving {
        name: "Mine".to_owned(),
        description: "Bees on a rail".to_owned(),
        groups: vec![("align".to_owned(), true), ("output".to_owned(), true)],
        complete: true,
    };
    assert!(harness.state_mut().save_preset(&saving, &file));
    let read = harness.state_mut().shell.run("preset.read", json!({ "path": file })).unwrap();
    assert_eq!((&read["name"], &read["description"]), (&json!("Mine"), &json!("Bees on a rail")));
    assert_eq!(read["preferences"]["output"], harness.state().shell.export_settings, "every output setting");
    assert!(read["preferences"]["align"]["framing"].is_string(), "the defaults too");
    // The Batch tab takes a built-in preset from its list.
    harness.get_by_label("Batch").click();
    harness.step();
    harness.get_by_label("Preset").click();
    harness.run_steps(2);
    harness.get_by_label("Landscape").click();
    harness.step();
    assert_eq!(harness.state().batch.preset().as_deref(), Some("builtin:landscape"));
    done(dir, harness);
}

#[test]
fn preferences_reach_the_engine_and_say_nothing_is_sent() {
    let (dir, mut harness) = app("preferences");
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    harness.run_steps(2);
    let Some(Dialog::Preferences(form)) = harness.state().dialog.clone() else { panic!("the Preferences dialog") };
    harness.get_by_label_contains("sends nothing anywhere: no telemetry");
    harness.get_by_label_contains("is suggested");
    // The GPU's self-test, on demand: what it found is shown, whatever this machine has. (The dialog's settings are in
    // a scroll area, where kittest's pointer clicks don't land, though a mouse's do: they are clicked through
    // AccessKit, as a screen reader would.)
    harness.get_by_label("Test the GPU").click_accesskit();
    until(&mut harness, "the GPU's test", |app| app.shell.tested.is_some() && !app.shell.busy());
    let tested = harness.state().shell.tested.clone().unwrap();
    let shown = if tested["device"] == "gpu" { "The GPU passed" } else { "The CPU stacks" };
    harness.get_by_label_contains(shown);
    // OK with nothing changed changes nothing remembered.
    harness.get_by_label("OK").click();
    until(&mut harness, "the dialog to close", |app| app.dialog.is_none());
    assert_eq!((harness.state().prefs.memory_mb, harness.state().prefs.cache_folder.as_ref()), (None, None));
    // A memory budget, the CPU, a cache folder and no autosave: in the engine, and remembered.
    let cache = dir.join("cache");
    let mut changed = (*form).clone();
    changed.memory_mb = 2048;
    changed.device = florastack_engine::Device::Cpu;
    changed.cache_folder = cache.display().to_string();
    changed.autosave_minutes = 0;
    let ctx = harness.ctx.clone();
    assert!(harness.state_mut().apply_preferences(&ctx, &changed));
    let info = harness.state_mut().shell.run("compute.info", json!({})).unwrap();
    assert_eq!((&info["memory"]["budgetMb"], &info["preference"]), (&json!(2048), &json!("cpu")), "{info}");
    let caches = harness.state_mut().shell.run("cache.info", json!({})).unwrap();
    assert_eq!(caches["temporaryProjects"]["folder"].as_str().map(PathBuf::from), Some(cache.join("projects")), "{caches}");
    let prefs = harness.state().prefs.clone();
    assert_eq!(
        (prefs.memory_mb, prefs.device, prefs.cache_folder.as_deref(), prefs.autosave()),
        (Some(2048), Some(florastack_engine::Device::Cpu), Some(cache.as_path()), None)
    );
    // A new project goes into the cache folder's temporary projects.
    harness.state_mut().shell.run("project.new", json!({}));
    harness.step();
    assert!(harness.state().shell.engine().project_path().unwrap().starts_with(cache.join("projects")));
    done(dir, harness);
}
