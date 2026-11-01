//! Headless Capture tab, watch-folder dispatch and calculator query.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
use common::{app, frames, until};
use egui_kittest::kittest::Queryable;
use florastack_app::prefs::Tab;

#[test]
fn capture_tab_calculates_physical_spacing_and_watches_real_files() {
    let (dir, mut harness) = app("capture-tab");
    harness.get_by_label("Capture").click();
    harness.step();
    assert_eq!(harness.state().prefs.tab, Tab::Capture);
    #[cfg(target_os = "windows")]
    harness.get_by_label_contains("on Windows use the camera maker");
    harness.get_by_label("Diffraction step calculator").click_accesskit();
    harness.run_steps(15); // Let the collapsing section's opening animation expose its controls to AccessKit.
    harness.get_by_label("Calculate step").click();
    harness.step();
    let result = harness.state().capture.recommendation.as_ref().unwrap();
    assert!((result["stepMicrometers"].as_f64().unwrap() - 70.4).abs() < 1e-10);
    harness.get_by_label("Use recommended rail spacing").click();
    harness.step();
    assert!((harness.state().capture.step - 70.4).abs() < 1e-10);
    harness.get_by_label("Diffraction step calculator").click_accesskit();
    harness.step();
    let input = dir.join("input");
    let originals = frames(&dir.join("source"));
    std::fs::create_dir(&input).unwrap();
    std::fs::copy(&originals[0], input.join("first.png")).unwrap();
    harness.state_mut().capture.input = input.display().to_string();
    harness.state_mut().capture.output = dir.join("results").display().to_string();
    harness.get_by_label("Watch folder and stack").click();
    until(&mut harness, "a watched preview", |app| app.shell.preview.is_some());
    let first_revision = harness.state().shell.preview_revision;
    let first_path = harness.state().shell.preview.clone();
    std::fs::copy(&originals[1], input.join("second.png")).unwrap();
    until(&mut harness, "a replacement preview at the same path", |app| app.shell.preview_revision > first_revision);
    assert_eq!(harness.state().shell.preview, first_path);
    harness.get_by_label("Stop capture").click();
    until(&mut harness, "capture cancellation", |app| !app.shell.busy());
    assert!(harness.state().shell.notices.iter().all(|n| !n.error), "{:?}", harness.state().shell.notices);
    drop(harness);
    let _ = std::fs::remove_dir_all(dir);
}
