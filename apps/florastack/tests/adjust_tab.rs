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

#[test]
fn an_ai_mask_asks_for_the_model_it_needs() {
    let (dir, mut harness) = app("adjust-tab-ai");
    drop_files(&mut harness, &frames(&dir.join("photos")));
    until(&mut harness, "the frames", |app| app.shell.frames.len() == 4);
    harness.get_by_label("Align").click();
    until(&mut harness, "the alignment", |app| app.aligned() && !app.shell.busy());
    harness.get_by_label("Stack frames").click();
    until(&mut harness, "the output", |app| app.shell.outputs.len() == 1 && !app.shell.busy());
    harness.get_by_label("Adjust").click();
    until(&mut harness, "the output's preview", |app| app.adjust.is_shown() && !app.shell.busy());

    // Subject (AI): a mask waiting for its first click, and, without the model, how to get it.
    harness.get_by_label("Add mask").click();
    harness.run_steps(2);
    harness.get_by_label("Subject (AI)").click();
    harness.run_steps(2);
    until(&mut harness, "the AI mask", |app| masks(app).len() == 1);
    assert!(matches!(harness.state().adjust.ai_pending, Some(florastack_app::adjust_tab::AiPending::Clicks(_))));
    harness.get_by_label("Choose the model's folder…");
    harness.get_by_label("Download page");
    // Background (AI) is the same selection, inverted.
    harness.get_by_label("Add mask").click();
    harness.run_steps(2);
    harness.get_by_label("Background (AI)").click();
    harness.run_steps(2);
    until(&mut harness, "the second AI mask", |app| masks(app).len() == 2);
    assert!(masks(harness.state())[1].invert);
    assert!(harness.state().shell.notices.iter().all(|n| !n.error), "{:?}", harness.state().shell.notices);
    drop(harness);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_export_tab_shows_the_output_with_its_adjustments() {
    let (dir, mut harness) = app("adjust-tab-export");
    drop_files(&mut harness, &frames(&dir.join("photos")));
    until(&mut harness, "the frames", |app| app.shell.frames.len() == 4);
    harness.get_by_label("Align").click();
    until(&mut harness, "the alignment", |app| app.aligned() && !app.shell.busy());
    harness.get_by_label("Stack frames").click();
    until(&mut harness, "the output", |app| app.shell.outputs.len() == 1 && !app.shell.busy());
    let id = harness.state().shell.outputs[0].id;
    let shown = |app: &FloraStackApp| app.loading.as_ref().map(|(path, ..)| path.clone());
    // Without edits, the output's own image.
    harness.get_by_label("Export").click();
    harness.state_mut().select(florastack_app::app::Selection::Output(id));
    until(&mut harness, "the plain output", |app| app.loading.is_none() && app.rendering.is_none() && !app.viewer.is_empty());
    // Adjusted: the render, made and loaded.
    harness.state_mut().shell.run("adjust.set", florastack_engine::json!({ "output": id, "adjustments": { "global": { "exposure": 1.0 } } }));
    until(&mut harness, "the render asked for", |app| {
        app.rendering.is_some() || shown(app).is_some_and(|p| p.display().to_string().contains("result-"))
    });
    until(&mut harness, "the render shown", |app| app.rendering.is_none() && app.loading.is_none() && !app.shell.busy());
    let rendered = harness.state().shell.engine().project().unwrap().outputs[0].maps.keys().any(|k| k.as_str() == "result");
    assert!(rendered, "kept as the output's result map");
    let adjusted = harness.state().viewer.images()[0].unwrap().rgba.clone();
    // Compared with another output, first or second, it shows rendered too.
    harness.state_mut().shell.run("stack.pyramid", florastack_engine::json!({}));
    until(&mut harness, "the second output", |app| app.shell.outputs.len() == 2 && !app.shell.busy());
    let other = harness.state().shell.outputs[1].id;
    for (pair, slot) in [((id, other), 0), ((other, id), 1)] {
        harness.state_mut().select(florastack_app::app::Selection::Compare(pair.0, pair.1));
        // (Without the render, the plain output would show there, and this would time out.)
        until(&mut harness, "the adjusted output compared as rendered", |app| {
            app.viewer.is_comparing() && app.viewer.images()[slot].is_some_and(|image| image.rgba == adjusted)
        });
    }
    assert!(harness.state().shell.notices.iter().all(|n| !n.error), "{:?}", harness.state().shell.notices);
    drop(harness);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_view_zooms_into_a_window_of_the_output_and_pans() {
    let (dir, mut harness) = app("adjust-tab-zoom");
    drop_files(&mut harness, &frames(&dir.join("photos")));
    until(&mut harness, "the frames", |app| app.shell.frames.len() == 4);
    harness.get_by_label("Align").click();
    until(&mut harness, "the alignment", |app| app.aligned() && !app.shell.busy());
    harness.get_by_label("Stack frames").click();
    until(&mut harness, "the output", |app| app.shell.outputs.len() == 1 && !app.shell.busy());
    harness.get_by_label("Adjust").click();
    until(&mut harness, "the output's preview", |app| app.adjust.is_shown() && !app.shell.busy());
    let (width, height) = harness.state().shell.engine().adjust().unwrap().size();
    let (fitted, _) = harness.state().adjust.view();
    let image = harness.state().adjust.image_rect.unwrap();

    // Zoomed in about the image's center, past the fitted size: the engine renders a window of a larger preview.
    harness.event(Event::PointerMoved(image.center()));
    for _ in 0..12 {
        harness.event(Event::Zoom(1.25));
        harness.step();
    }
    until(&mut harness, "the zoomed preview", |app| app.adjust.zoomed().is_some() && !app.shell.busy());
    let (zoom, center) = harness.state().adjust.view();
    assert!(zoom > 4.0 * fitted, "{zoom} against {fitted}");
    let placement = harness.state().adjust.zoomed().unwrap();
    assert!(placement.full.0 > width / 2 && placement.full.0 <= width, "{placement:?} of {width}");
    assert!((center.x - width as f32 / 2.0).abs() < 2.0 && (center.y - height as f32 / 2.0).abs() < 2.0, "{center:?}");

    // Panned with the right button: the view's center moves the other way.
    let pane = harness.state().adjust.image_rect.unwrap().intersect(image);
    let from = pane.center();
    harness.event(Event::PointerMoved(from));
    harness.step();
    harness.event(Event::PointerButton { pos: from, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE });
    harness.step();
    for i in 1..=5 {
        harness.event(Event::PointerMoved(from + egui::vec2(-8.0 * i as f32, 0.0)));
        harness.step();
    }
    harness.event(Event::PointerButton {
        pos: from - egui::vec2(40.0, 0.0),
        button: PointerButton::Secondary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    let (zoom, panned) = harness.state().adjust.view();
    // 40 points to the left, less what egui takes before a drag starts: the center moves right by most of 40 points
    // in output pixels.
    let ppp = harness.ctx.pixels_per_point();
    assert!(panned.x - center.x > 0.5 * 40.0 * ppp / zoom, "{panned:?} after {center:?}, at {zoom}");
    assert!((panned.y - center.y).abs() < 1e-3);

    // A brush stroke painted at the view's center lands at the output's point there.
    harness.get_by_label("Add mask").click();
    harness.run_steps(2);
    harness.get_by_label("Brush").click();
    harness.run_steps(2);
    until(&mut harness, "the brush mask", |app| masks(app).len() == 1 && !app.shell.busy());
    let middle = harness.state().adjust.image_rect.unwrap().intersect(image).center();
    let (_, at) = harness.state().adjust.view();
    drag(&mut harness, middle, middle + egui::vec2(4.0, 0.0), 2);
    until(&mut harness, "the stroke", |app| masks(app).first().is_some_and(|m| !m.components.is_empty()) && !app.shell.busy());
    let Shape::Brush { strokes } = &masks(harness.state())[0].components[0].shape else { panic!("a brush") };
    let first = strokes[0].points[0];
    let expected = [f64::from(at.x) / width as f64, f64::from(at.y) / height as f64];
    // Within a few output pixels: the pointer is a point or two off the pane's center.
    assert!((first[0] - expected[0]).abs() < 0.02 && (first[1] - expected[1]).abs() < 0.02, "{first:?} against {expected:?}");

    // A double click fits the output in the view again (once the stroke's click, a short drag, is past the time two
    // clicks make a double click in).
    harness.event(Event::PointerMoved(middle));
    for _ in 0..12 {
        harness.step();
    }
    // The test's clock moves a quarter of a second a frame, and its clicks come a frame or two apart: more than the
    // 0.3 seconds two clicks make a double click in, which this test lengthens.
    harness.ctx.options_mut(|o| o.input_options.max_double_click_delay = 2.0);
    for _ in 0..2 {
        harness.event(Event::PointerButton { pos: middle, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        harness.event(Event::PointerButton { pos: middle, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        harness.step();
    }
    until(&mut harness, "the fitted view", |app| app.adjust.zoomed().is_none() && !app.shell.busy());
    assert!((harness.state().adjust.view().0 - fitted).abs() < 1e-3);
    assert!(harness.state().shell.notices.iter().all(|n| !n.error), "{:?}", harness.state().shell.notices);
    drop(harness);
    let _ = std::fs::remove_dir_all(dir);
}
