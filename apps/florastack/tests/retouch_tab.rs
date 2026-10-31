//! The Retouch tab, headless (`egui_kittest`): an output opened from the tab, its sharpest frames mapped and one picked
//! as the source with Ctrl+click, a stroke painted by dragging on the output, undone and redone from the keyboard,
//! and the brush resized with its keys. Stacking runs on the CPU, the views without a GPU renderer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{app, drop_files, frames, until};
use egui::{Event, Key, Modifiers, PointerButton};
use egui_kittest::kittest::Queryable;
use florastack_app::FloraStackApp;

/// The output's strokes, in all its layers.
fn strokes(app: &FloraStackApp) -> usize {
    let id = app.retouch.output.unwrap();
    let project = app.shell.engine().project().unwrap();
    project.outputs.iter().find(|o| o.id().get() == id).map_or(0, |o| o.retouch.iter().map(|l| l.strokes.len()).sum())
}

#[test]
fn a_stroke_painted_on_the_output_from_its_sharpest_frame_undoes_and_redoes() {
    let (dir, mut harness) = app("retouch-tab");
    drop_files(&mut harness, &frames(&dir.join("photos")));
    until(&mut harness, "the frames", |app| app.shell.frames.len() == 4);
    harness.get_by_label("Align").click();
    until(&mut harness, "the alignment", |app| app.aligned() && !app.shell.busy());
    harness.get_by_label("Stack frames").click();
    until(&mut harness, "the output", |app| app.shell.outputs.len() == 1 && !app.shell.busy());

    // The tab opens the output on its own and shows it beside its source.
    harness.get_by_label("Retouch").click();
    until(&mut harness, "the output opened for retouching", |app| app.retouch.is_shown());
    harness.get_by_label("Map sharpest frames").click();
    until(&mut harness, "the map", |app| app.shell.engine().retouch().is_some_and(|v| v.has_sharpest()));
    let (_, target) = harness.state().retouch.panes.unwrap();
    // Ctrl+click on the output: its sharpest frame there becomes the source.
    let at = target.center();
    harness.event(Event::PointerMoved(at));
    harness.event_modifiers(
        Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::COMMAND },
        Modifiers::COMMAND,
    );
    harness.event_modifiers(
        Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::COMMAND },
        Modifiers::COMMAND,
    );
    until(&mut harness, "the source", |app| app.shell.engine().retouch().is_some_and(|v| v.source().is_some()) && !app.shell.busy());
    harness.get_by_label_contains("Frame: focus");

    // A drag across the output paints a stroke, committed when the button is let go.
    let start = target.center() - egui::vec2(target.width() * 0.25, 0.0);
    harness.event(Event::PointerMoved(start));
    harness.step();
    harness.event(Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    harness.step();
    for i in 1..=10 {
        let p = start + egui::vec2(target.width() * 0.05 * i as f32, 4.0 * i as f32);
        harness.event(Event::PointerMoved(p));
        harness.step();
    }
    assert!(harness.state().retouch.drawing(), "the stroke is drawn live");
    let end = start + egui::vec2(target.width() * 0.5, 40.0);
    harness.event(Event::PointerButton { pos: end, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    harness.step();
    until(&mut harness, "the stroke", |app| strokes(app) == 1);
    assert!(!harness.state().retouch.drawing());
    assert_eq!(harness.state().shell.engine().undo_label(), Some("Retouch stroke"));

    // Undo and redo, from the keyboard.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    until(&mut harness, "the undo", |app| strokes(app) == 0);
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    until(&mut harness, "the redo", |app| strokes(app) == 1);

    // The brush's keys.
    let size = harness.state().retouch.size;
    harness.key_press(Key::CloseBracket);
    harness.step();
    assert!(harness.state().retouch.size > size);
    assert!(harness.state().shell.notices.iter().all(|n| !n.error), "{:?}", harness.state().shell.notices);
    drop(harness);
    let _ = std::fs::remove_dir_all(dir);
}
