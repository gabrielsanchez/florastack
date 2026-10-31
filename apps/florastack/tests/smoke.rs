//! The desktop app, headless (`egui_kittest`, through AccessKit as a screen reader would see it): frames dropped on
//! the window, aligned from the command palette, stacked with the Stack tab's button, the output shown, undone and
//! redone from the keyboard, and the project saved elsewhere. Every step is an engine command through the registry;
//! the test checks what the engine holds afterwards. Stacking runs on the CPU, the viewer without a GPU renderer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use florastack_app::FloraStackApp;
use florastack_app::app::{Options, Selection};
use florastack_app::prefs::Prefs;
use florastack_engine::{Compute, EngineConfig};

/// A file dropped on the window.
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

fn png_file(path: &Path, width: u32, height: u32, pixels: &[u8]) {
    let mut encoder = png::Encoder::new(std::fs::File::create(path).unwrap(), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    encoder.write_header().unwrap().write_image_data(pixels).unwrap();
}

/// Four depth bands with a common low-frequency texture, and fine detail that comes into focus one band at a time
/// (the CLI's tests' frames).
fn frames(folder: &Path) -> Vec<PathBuf> {
    std::fs::create_dir_all(folder).unwrap();
    (0..4)
        .map(|focus| {
            let mut pixels = Vec::new();
            for y in 0..72_u32 {
                for x in 0..96_u32 {
                    let texture = (x * 7 + y * 13 + (x * y) % 29) % 60;
                    let detail = if x / 24 == focus { (x + y) % 2 * 65 } else { 32 };
                    let sample = u8::try_from(60 + texture + detail).unwrap();
                    pixels.extend_from_slice(&[sample, sample.saturating_add(7), sample.saturating_sub(9)]);
                }
            }
            let path = folder.join(format!("focus {focus}.png"));
            png_file(&path, 96, 72, &pixels);
            path
        })
        .collect()
}

/// Runs frames until `done` holds, failing after a minute.
fn until(harness: &mut Harness<'_, FloraStackApp>, what: &str, done: impl Fn(&FloraStackApp) -> bool) {
    let start = Instant::now();
    while !done(harness.state()) {
        let errors: Vec<&str> = harness.state().shell.notices.iter().filter(|n| n.error).map(|n| n.text.as_str()).collect();
        assert!(errors.is_empty(), "while waiting for {what}: {errors:?}");
        assert!(start.elapsed() < Duration::from_secs(60), "{what} never happened");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.step();
}

#[test]
fn frames_dropped_aligned_from_the_palette_and_stacked_by_the_button() {
    let dir = std::env::temp_dir().join(format!("florastack-app-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let files = frames(&dir.join("photos"));
    let engine = EngineConfig { temporary_projects: Some(dir.join("temporary")), compute: Compute::CPU, ..EngineConfig::default() };
    let options = Options { engine, files: Vec::new() };
    let mut harness =
        Harness::builder().with_size([1400.0, 900.0]).build_eframe(|cc| FloraStackApp::with_context(&cc.egui_ctx, options, Prefs::default(), None));
    harness.step();
    assert!(!harness.state().has_project());

    // Dropping the frames makes a temporary project with them, listed by name.
    for file in &files {
        harness.input_mut().dropped_files.push(Arc::new(Dropped(file.clone())));
    }
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
    let dir = std::env::temp_dir().join(format!("florastack-app-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let folder = dir.join("photos");
    frames(&folder);
    let engine = EngineConfig { temporary_projects: Some(dir.join("temporary")), compute: Compute::CPU, ..EngineConfig::default() };
    let options = Options { engine, files: Vec::new() };
    let mut harness =
        Harness::builder().with_size([1400.0, 900.0]).build_eframe(|cc| FloraStackApp::with_context(&cc.egui_ctx, options, Prefs::default(), None));
    harness.step();
    // What File > Import stacks runs on the folder picked.
    harness.state_mut().shell.run("project.findStacks", florastack_engine::json!({ "paths": [folder] }));
    until(&mut harness, "the stack's frames", |app| app.shell.frames.len() == 4);
    assert!(harness.state().dialog.is_none(), "one stack is added without asking");
    drop(harness);
    let _ = std::fs::remove_dir_all(&dir);
}
