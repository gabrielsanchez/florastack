//! The Adjust tab, headless (`egui_kittest`): an output opened from the tab and previewed, a radial mask added from
//! the menu and its center dragged on the image, a brush mask painted by dragging and the stroke undone from the
//! keyboard, the output compared with itself before its adjustments, and everything reset. Stacking runs on the CPU,
//! the view without a GPU renderer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{app, drop_files, frames, until};
use egui::{Event, Key, Modifiers, PointerButton};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use florastack_app::FloraStackApp;
use florastack_engine::adjust::Shape;

/// The masks of the output open for adjusting, as the project holds them.
fn masks(app: &FloraStackApp) -> Vec<florastack_engine::adjust::Mask> {
    let id = app.adjust.output.unwrap();
    let project = app.shell.engine().project().unwrap();
    let output = project.outputs.iter().find(|o| o.id().get() == id).unwrap();
    output.adjustments.as_ref().map(|a| serde_json::from_value::<florastack_engine::adjust::Settings>(a.clone()).unwrap().masks).unwrap_or_default()
}

/// Drags the pointer from `from` to `to` with the left button, in `steps` moves.
fn drag(harness: &mut Harness<'_, FloraStackApp>, from: egui::Pos2, to: egui::Pos2, steps: usize) {
    harness.event(Event::PointerMoved(from));
    harness.step();
    harness.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    harness.step();
    for i in 1..=steps {
        #[allow(clippy::cast_precision_loss)] // A few steps.
        let t = i as f32 / steps as f32;
        harness.event(Event::PointerMoved(from + (to - from) * t));
        harness.step();
    }
    harness.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    harness.step();
}

#[test]
fn masks_are_added_moved_painted_and_undone_from_the_tab() {
    let (dir, mut harness) = app("adjust-tab");
    drop_files(&mut harness, &frames(&dir.join("photos")));
    until(&mut harness, "the frames", |app| app.shell.frames.len() == 4);
    harness.get_by_label("Align").click();
    until(&mut harness, "the alignment", |app| app.aligned() && !app.shell.busy());
    harness.get_by_label("Stack frames").click();
    until(&mut harness, "the output", |app| app.shell.outputs.len() == 1 && !app.shell.busy());

    // The tab opens the output on its own and shows its preview.
    harness.get_by_label("Adjust").click();
    until(&mut harness, "the output's preview", |app| app.adjust.is_shown() && !app.shell.busy());

    // A radial gradient from the menu, selected; its center dragged to the left.
    harness.get_by_label("Add mask").click();
    harness.run_steps(2);
    harness.get_by_label("Radial gradient").click();
    harness.run_steps(2);
    until(&mut harness, "the mask", |app| masks(app).len() == 1 && !app.shell.busy());
    assert_eq!(harness.state().adjust.selected, Some(masks(harness.state())[0].id));
    let image = harness.state().adjust.image_rect.unwrap();
    drag(&mut harness, image.center(), image.center() - egui::vec2(image.width() * 0.25, 0.0), 6);
    until(&mut harness, "the center moved", |app| {
        matches!(masks(app)[0].components[0].shape, Shape::Radial { center, .. } if center[0] < 0.4) && !app.shell.busy()
    });
    assert_eq!(harness.state().shell.engine().undo_label(), Some("Set mask"), "the whole drag is one step");

    // A brush mask, painted by dragging across the image; undone from the keyboard.
    harness.get_by_label("Add mask").click();
    harness.run_steps(2);
    harness.get_by_label("Brush").click();
    harness.run_steps(2);
    until(&mut harness, "the brush mask", |app| masks(app).len() == 2 && !app.shell.busy());
    let image = harness.state().adjust.image_rect.unwrap();
    let start = image.left_center() + egui::vec2(image.width() * 0.2, 0.0);
    drag(&mut harness, start, start + egui::vec2(image.width() * 0.5, 10.0), 8);
    let strokes = |app: &FloraStackApp| {
        masks(app).iter().flat_map(|m| &m.components).map(|c| if let Shape::Brush { strokes } = &c.shape { strokes.len() } else { 0 }).sum::<usize>()
    };
    until(&mut harness, "the stroke", |app| strokes(app) == 1 && !app.shell.busy());
    assert_eq!(harness.state().shell.engine().undo_label(), Some("Brush stroke"));
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    until(&mut harness, "the undo", |app| strokes(app) == 0);

    // Before: the output without its adjustments, at the preview's size.
    harness.get_by_label("Before").click();
    until(&mut harness, "the before picture", |app| app.shell.engine().adjust().is_some_and(|v| v.before().is_some()) && !app.shell.busy());

    // Reset all: no masks, one step that undo takes back.
    harness.get_by_label("Reset all").click();
    until(&mut harness, "the reset", |app| masks(app).is_empty());
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    until(&mut harness, "the reset undone", |app| masks(app).len() == 2);
    assert!(harness.state().shell.notices.iter().all(|n| !n.error), "{:?}", harness.state().shell.notices);
    drop(harness);
    let _ = std::fs::remove_dir_all(dir);
}
