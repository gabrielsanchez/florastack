//! The Stack tab, headless (`egui_kittest`, through AccessKit): a synthetic stack added, aligned and stacked with the
//! tab's buttons into an output; the depth map's contrast-threshold step and its Continue; both methods at once and
//! their outputs compared; settings changed through their controls; and the filmstrip played. Stacking runs on the CPU.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::PathBuf;

use common::{app, drop_files, frames, until};
use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use florastack_app::FloraStackApp;
use florastack_app::app::Selection;
use florastack_app::prefs::{Compare, Method};

/// The app for test `name` with the synthetic stack dropped on it and aligned with the Stack tab's Align button, its
/// filmstrip previews made.
fn aligned(name: &str) -> (PathBuf, Harness<'static, FloraStackApp>) {
    let (dir, mut harness) = app(name);
    drop_files(&mut harness, &frames(&dir.join("photos")));
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
    let automatic = harness.state().shell.threshold.clone().unwrap();
    assert_eq!(automatic["measured"], true, "the first picture measures the frames' focus");
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
    harness.step();
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
    done(dir, harness);
}
