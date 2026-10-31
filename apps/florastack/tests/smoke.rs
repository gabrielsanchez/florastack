//! The desktop app, headless (`egui_kittest`, through AccessKit as a screen reader would see it): frames dropped on
//! the window, aligned from the command palette, stacked with the Stack tab's button, the output shown, undone and
//! redone from the keyboard, and the project saved elsewhere. Every step is an engine command through the registry;
//! the test checks what the engine holds afterwards. Stacking runs on the CPU, the viewer without a GPU renderer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{app, drop_files, frames, until};
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use florastack_app::app::Selection;

#[test]
fn frames_dropped_aligned_from_the_palette_and_stacked_by_the_button() {
    let (dir, mut harness) = app("smoke");
    let files = frames(&dir.join("photos"));
    assert!(!harness.state().has_project());

    // Dropping the frames makes a temporary project with them, listed by name.
    drop_files(&mut harness, &files);
    until(&mut harness, "the frames to be listed", |app| app.shell.frames.len() == 4);
    assert!(harness.state().has_project());
    harness.get_by_label("focus 2.png");

    // The command palette finds `align.run` by its name and runs it with Enter.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::K);
    harness.step();
    assert!(harness.state().palette.is_open());
    harness.event(egui::Event::Text("align.run".to_owned()));
    harness.step();
    harness.key_press(Key::Enter);
    harness.step();
    assert!(!harness.state().palette.is_open(), "the palette closes once a command is picked");
    until(&mut harness, "the frames to be aligned", |app| app.aligned() && !app.shell.busy());

    // The Stack tab's button stacks by the method picked (the pyramid), and the viewer shows the new output.
    harness.get_by_label("Stack frames").click();
    until(&mut harness, "the stack", |app| app.shell.outputs.len() == 1 && !app.shell.busy());
    let output = harness.state().shell.outputs[0].clone();
    assert!(output.image.is_file(), "the output's image is in the project's cache");
    assert_eq!(harness.state().selection, Selection::Output(output.id));
    until(&mut harness, "the output to be shown", |app| !app.viewer.is_empty());

    // The Edit menu names what Undo undoes; Ctrl+Z undoes it and Ctrl+Shift+Z redoes it.
    let undo = harness.state().shell.engine().undo_label().map(str::to_owned).expect("stacking is undoable");
    harness.get_by_label("Edit").click();
    harness.run_steps(3);
    harness.get_by_label_contains(&format!("Undo {undo}"));
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    until(&mut harness, "the undo", |app| app.shell.outputs.is_empty());
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    until(&mut harness, "the redo", |app| app.shell.outputs.len() == 1);

    // Save As names the temporary project and moves it out of the temporary projects' folder.
    let saved = dir.join("projects");
    harness.state_mut().save_as("Smoke test", saved.to_str().unwrap());
    harness.step();
    let folder = saved.join(format!("Smoke test{}", florastack_engine::branding::PROJECT_EXTENSION));
    assert_eq!(harness.state().shell.engine().project_path(), Some(folder.as_path()));
    assert!(folder.join("project.json").is_file() && !harness.state().shell.engine().is_dirty());
    assert_eq!(harness.state().project_title().as_deref(), Some("Smoke test"));
    assert_eq!(harness.state().prefs.recent, vec![folder.clone()], "a recent project, unlike the temporary one it was");
    assert!(harness.state().shell.notices.iter().all(|n| !n.error), "{:?}", harness.state().shell.notices);

    drop(harness);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn importing_a_folder_with_one_stack_adds_its_frames() {
    let (dir, mut harness) = app("import");
    let folder = dir.join("photos");
    frames(&folder);
    // What File > Import stacks runs on the folder picked.
    harness.state_mut().shell.run("project.findStacks", florastack_engine::json!({ "paths": [folder] }));
    until(&mut harness, "the stack's frames", |app| app.shell.frames.len() == 4);
    assert!(harness.state().dialog.is_none(), "one stack is added without asking");
    drop(harness);
    let _ = std::fs::remove_dir_all(&dir);
}
