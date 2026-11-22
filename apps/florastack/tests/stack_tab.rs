//! The Stack tab, headless (`egui_kittest`, through AccessKit): a synthetic stack added, aligned and stacked with the
//! tab's buttons into an output; the depth map's contrast-threshold step and its Continue; both methods at once and
//! their outputs compared; settings changed through their controls; and the filmstrip played. Stacking runs on the CPU.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::PathBuf;

use common::{app, drop_files, frames, shifted_frames, until};
use egui::{Event, Key, Modifiers, PointerButton};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use florastack_app::FloraStackApp;
use florastack_app::app::Selection;
use florastack_app::prefs::{Compare, Method};

/// The app for test `name` with the synthetic stack dropped on it and aligned with the Stack tab's Align button, its
/// filmstrip previews made.
fn aligned(name: &str) -> (PathBuf, Harness<'static, FloraStackApp>) {
    aligned_shifted(name, 0)
}

/// [`aligned`], each frame `shift` pixels right of the one before (`shifted_frames`).
fn aligned_shifted(name: &str, shift: u32) -> (PathBuf, Harness<'static, FloraStackApp>) {
    let (dir, mut harness) = app(name);
    let photos = dir.join("photos");
    drop_files(&mut harness, &if shift == 0 { frames(&photos) } else { shifted_frames(&photos, shift) });
    until(&mut harness, "the frames to be listed", |app| app.shell.frames.len() == 4);
    harness.get_by_label("Align").click();
    until(&mut harness, "the alignment and the previews", |app| app.aligned() && !app.shell.busy());
    (dir, harness)
}

fn done(dir: PathBuf, harness: Harness<'static, FloraStackApp>) {
    assert!(harness.state().shell.notices.iter().all(|n| !n.error), "{:?}", harness.state().shell.notices);
    drop(harness);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn adding_aligning_and_stacking_makes_an_output() {
    let (dir, mut harness) = aligned("stack-tab");
    // Each frame shows how its alignment went, and the tab sums it up.
    // Each frame shows how its alignment went (on this stack, one frame's alignment is rejected), and the tab sums it
    // up.
    let status = |s: &str| harness.state().shell.frames.iter().filter(|f| f.alignment.as_ref().is_some_and(|a| a["status"] == s)).count();
    let (aligned, rejected) = (status("aligned"), status("rejected"));
    assert_eq!((status("master"), aligned + rejected), (1, 3));
    harness.get_by_label("master");
    assert_eq!(harness.query_all_by_label_contains("match ").count(), aligned, "each frame aligned to the master shows its score");
    assert_eq!(harness.query_all_by_label("rejected").count(), rejected);
    harness.get_by_label_contains(&format!("4 of 4 frames aligned, {rejected} flagged, {rejected} rejected"));
    harness.get_by_label("Stack frames").click();
    until(&mut harness, "the output", |app| app.shell.outputs.len() == 1 && !app.shell.busy());
    let output = harness.state().shell.outputs[0].clone();
    assert_eq!(output.method, "pyramid");
    assert_eq!(harness.state().selection, Selection::Output(output.id), "the new output is shown");
    // The outputs list says what it was made with.
    harness.get_by_label_contains("Pyramid · grit 0.5");
    done(dir, harness);
}

#[test]
fn the_depth_map_asks_for_its_contrast_threshold_first() {
    let (dir, mut harness) = aligned("threshold");
    harness.get_by_label("Depth map").click();
    harness.step();
    assert_eq!(harness.state().prefs.method, Method::DepthMap);
    harness.get_by_label("Stack frames").click();
    harness.step();
    assert!(harness.state().threshold_step && harness.state().selection == Selection::Threshold);
    until(&mut harness, "the threshold's picture", |app| app.shell.threshold.is_some() && !app.viewer.is_empty());
    harness.get_by_label("Contrast threshold");
    // The first picture measures the frames' focus, unless the depth-map preview that choosing the method started has
    // measured it first: the session keeps it for both.
    let automatic = harness.state().shell.threshold.clone().unwrap();
    assert!(automatic["measured"].is_boolean(), "{automatic}");
    // Percentile starts where the automatic threshold was; its picture reuses the focus measured.
    harness.get_by_label("Percentile").click();
    until(&mut harness, "the percentile's picture", |app| {
        app.shell.stack_settings["depth_map"]["threshold"] == "percentile"
            && !app.shell.running(&["stack.threshold"])
            && app.shell.threshold.as_ref().is_some_and(|t| t["measured"] == false)
    });
    let percentile = harness.state().shell.threshold.clone().unwrap();
    assert!((percentile["below"].as_f64().unwrap() - automatic["below"].as_f64().unwrap()).abs() < 0.02, "{percentile} {automatic}");
    harness.get_by_label_contains("of the image is below the threshold");
    harness.get_by_label("Continue").click();
    until(&mut harness, "the depth map", |app| app.shell.outputs.len() == 1 && !app.shell.busy());
    let output = harness.state().shell.outputs[0].clone();
    assert_eq!((output.method.as_str(), &output.parameters["threshold"]), ("depth-map", &florastack_engine::json!("percentile")));
    assert!(!harness.state().threshold_step, "Continue closes the step");
    done(dir, harness);
}

#[test]
fn both_methods_make_two_outputs_to_compare() {
    let (dir, mut harness) = aligned("both");
    harness.get_by_label("Both (pyramid and depth map)").click();
    // Choosing it previews both methods, shown compared.
    until(&mut harness, "both previews compared", |app| app.shell.previews.len() == 2 && !app.shell.busy() && app.viewer.is_comparing());
    assert_eq!(harness.state().selection, Selection::Preview);
    assert_eq!(harness.state().shell.previews.keys().collect::<Vec<_>>(), ["depth-map", "pyramid"]);
    harness.get_by_label("Stack frames").click();
    until(&mut harness, "the threshold's picture", |app| app.shell.threshold.is_some());
    harness.get_by_label("Continue").click();
    until(&mut harness, "the two outputs", |app| app.shell.outputs.len() == 2 && !app.shell.busy());
    let methods: Vec<String> = harness.state().shell.outputs.iter().map(|o| o.method.clone()).collect();
    assert_eq!(methods, ["pyramid", "depth-map"]);
    // Ticking Compare on both shows them split, then side by side.
    for checkbox in harness.get_all_by_label("Compare") {
        checkbox.click();
    }
    until(&mut harness, "the comparison", |app| app.viewer.is_comparing());
    let ids: Vec<u64> = harness.state().shell.outputs.iter().map(|o| o.id).collect();
    assert_eq!(harness.state().selection, Selection::Compare(ids[0], ids[1]));
    harness.get_by_label("Side by side").click();
    harness.step();
    assert_eq!(harness.state().prefs.compare, Compare::SideBySide);
    done(dir, harness);
}

#[test]
#[allow(clippy::cast_possible_truncation)] // Pixel positions, well within f32.
fn outputs_of_different_framings_compared_line_up_by_their_canvases() {
    let (dir, mut harness) = aligned_shifted("framings", 2);
    harness.get_by_label("Stack frames").click();
    until(&mut harness, "the narrow output", |app| app.shell.outputs.len() == 1 && !app.shell.busy());
    harness.get_by_label("Wide").click();
    until(&mut harness, "the wide framing", |app| app.shell.align_settings["framing"] == "wide" && !app.shell.busy());
    harness.get_by_label("Stack frames").click();
    until(&mut harness, "the wide output", |app| app.shell.outputs.len() == 2 && !app.shell.busy());
    let canvases: Vec<_> = harness.state().shell.outputs.iter().map(|o| o.canvas.unwrap()).collect();
    assert_ne!(canvases[0].size, canvases[1].size, "the wide canvas is larger");
    for checkbox in harness.get_all_by_label("Compare") {
        checkbox.click();
    }
    until(&mut harness, "the comparison", |app| app.viewer.is_comparing());
    // The narrow output's pixels are found in the wide one through the master frame, not at the same coordinates.
    let map = harness.state().viewer.pair_map().expect("the outputs' canvases place the second image");
    let (zoom, center) = map.view(1.0, egui::Vec2::new(10.0, 10.0));
    let [[a, _, c], [_, e, f]] = canvases[0].to_master;
    let [[_, _, wc], [_, _, wf]] = canvases[1].to_master;
    let want = egui::Vec2::new((a * 9.5 + c - wc + 0.5) as f32, (e * 9.5 + f - wf + 0.5) as f32);
    assert!((zoom - 1.0).abs() < 0.05 && (center - want).length() < 0.5, "{zoom} {center:?} against {want:?}");
    assert!((center - egui::Vec2::new(10.0, 10.0)).length() > 1.0, "the wide canvas starts elsewhere: {center:?}");
    done(dir, harness);
}

#[test]
fn a_settings_question_mark_opens_its_section_of_the_guide_in_the_app() {
    let (dir, mut harness) = app("help");
    drop_files(&mut harness, &frames(&dir.join("photos")));
    until(&mut harness, "the frames to be listed", |app| app.shell.frames.len() == 4 && !app.shell.stack_settings.is_null());
    // The first "?" is the method's: its section, from the guide the app carries.
    harness.get_all_by_label("?").next().unwrap().click();
    // A window shows from its second frame, the first measuring it.
    harness.step();
    harness.step();
    assert_eq!(florastack_app::help::shown(&harness.ctx).as_deref(), Some("method"));
    harness.get_by_label_contains("picks the sharpest detail scale by scale. It is the best all-rounder");
    harness.get_by_label("Open the whole guide online");
    // A link to another section of the page opens it in the window.
    // (Through AccessKit: egui_kittest's pointer clicks miss widgets in a scroll area inside a window, as the roadmap
    // notes for the Preferences dialog.)
    harness.get_by_label("contrast threshold").click_accesskit();
    harness.step();
    harness.step();
    assert_eq!(florastack_app::help::shown(&harness.ctx).as_deref(), Some("contrast-threshold"));
    harness.get_by_label_contains("follows each pixel's sharpest frame only where that frame stands out clearly");
    done(dir, harness);
}

#[test]
fn settings_change_through_their_controls() {
    let (dir, mut harness) = aligned("settings");
    // A framing applies when stacking, so a preview follows it at once.
    harness.get_by_label("Wide").click();
    until(&mut harness, "the preview", |app| app.shell.align_settings["framing"] == "wide" && app.shell.preview.is_some() && !app.shell.busy());
    assert_eq!(harness.state().selection, Selection::Preview, "the preview takes the viewer");
    // A term of alignment needs a new alignment, which the tab asks for.
    harness.get_by_label("Rotation").click();
    until(&mut harness, "the rotation off", |app| app.shell.align_settings["rotation"] == false);
    harness.get_by_label_contains("The alignment settings changed");
    // The Advanced section has the rest.
    harness.get_by_label("Advanced").click();
    harness.step();
    harness.get_by_label("Refine against the master");
    assert!(harness.state().shell.engine().undo_label().is_some());
    done(dir, harness);
}

#[test]
fn the_filmstrip_plays_the_aligned_frames() {
    let (dir, mut harness) = aligned("film");
    assert!(harness.state().shell.frames.iter().all(|f| f.aligned_preview.is_some()), "the aligned previews are made");
    harness.get_by_label("Play").click();
    harness.step();
    assert!(harness.state().film.as_ref().is_some_and(|f| f.playing));
    until(&mut harness, "the next frame", |app| app.film.as_ref().is_some_and(|f| f.index > 0));
    until(&mut harness, "the frames' pictures", |app| app.thumbnails.len() >= 8);
    harness.get_by_label_contains("Frame ");
    harness.get_by_label("Stop").click();
    harness.step();
    assert!(harness.state().film.as_ref().is_some_and(|f| !f.playing));
    harness.key_press(Key::Escape);
    harness.step();
    assert!(harness.state().film.is_none(), "Escape returns to the viewer");
    // Dragging down over the thumbnails plays the stack frame by frame, and back up returns; letting go keeps the
    // frame shown.
    let names: Vec<String> = harness.state().shell.frames.iter().map(|f| f.name.clone()).collect();
    let thumb = |harness: &Harness<'static, FloraStackApp>, i: usize| harness.get_by_label(&format!("Preview of {}", names[i])).rect().center();
    let (first, last) = (thumb(&harness, 0), thumb(&harness, names.len() - 1));
    let mut seen = Vec::new();
    harness.event(Event::PointerMoved(first));
    harness.step();
    harness.event(Event::PointerButton { pos: first, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    harness.step();
    for at in (0..=12).map(|i| first + (last - first) * (i as f32 / 12.0)).chain([first + (last - first) * 0.4]) {
        harness.event(Event::PointerMoved(at));
        harness.step();
        let film = harness.state().film.clone().expect("scrubbing shows the filmstrip");
        assert!(!film.playing);
        if seen.last() != Some(&film.index) {
            seen.push(film.index);
        }
    }
    harness.event(Event::PointerButton { pos: first, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    harness.step();
    assert_eq!(&seen[..names.len()], (0..names.len()).collect::<Vec<_>>(), "each frame in turn, down: {seen:?}");
    assert!(seen.len() > names.len() && seen.last() < Some(&(names.len() - 1)), "and back up: {seen:?}");
    assert_eq!(harness.state().film.as_ref().map(|f| f.index), seen.last().copied(), "the frame stays shown");
    // Crops at 100% of what the viewer shows, one for each frame stacked (the rejected one has none), played in place of
    // the previews.
    harness.key_press(Key::Escape);
    harness.step();
    harness.get_by_label("Crop at 100%").click();
    let stacked = harness.state().shell.frames.iter().filter(|f| f.alignment.as_ref().is_some_and(|a| a["status"] != "rejected")).count();
    until(&mut harness, "the crops", |app| app.shell.film_crops.len() == stacked && !app.shell.busy());
    assert!(harness.state().film_crop && harness.state().film.is_some(), "the filmstrip plays the crops");
    let frames = harness.state().shell.frames.clone();
    for (i, frame) in frames.iter().enumerate() {
        let shown = harness.state().film_frame(i).map(|(path, _)| path);
        assert_eq!(shown.as_ref(), harness.state().shell.film_crops.get(&frame.id), "frame {i} plays its crop, or nothing");
    }
    harness.get_by_label_contains("(100% crop)");
    // Unticked, the previews play again.
    harness.get_by_label("100% crop").click();
    harness.step();
    assert_eq!(harness.state().film_frame(0).map(|(path, _)| path), frames[0].aligned_preview.clone());
    done(dir, harness);
}
