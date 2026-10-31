//! What the app's UI tests share: a synthetic stack written as PNG files, the app headless on a temporary folder
//! (`egui_kittest`; stacking on the CPU, the viewer without a GPU renderer), files dropped on its window, and waiting
//! for what the engine does in the background.

// Each test binary uses some of these.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use florastack_app::FloraStackApp;
use florastack_app::app::Options;
use florastack_app::prefs::Prefs;
use florastack_engine::{Compute, EngineConfig};

/// A file dropped on the window.
#[derive(Debug)]
pub struct Dropped(pub PathBuf);

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

/// A synthetic stack of four frames: four depth bands with a common low-frequency texture, and fine detail that comes
/// into focus one band at a time (the CLI's tests' frames).
pub fn frames(folder: &Path) -> Vec<PathBuf> {
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

/// A folder of its own for test `name`, emptied, and the app on it.
pub fn app(name: &str) -> (PathBuf, Harness<'static, FloraStackApp>) {
    let dir = std::env::temp_dir().join(format!("florastack-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = EngineConfig { temporary_projects: Some(dir.join("temporary")), compute: Compute::CPU, ..EngineConfig::default() };
    let options = Options { engine, files: Vec::new() };
    let mut harness =
        Harness::builder().with_size([1400.0, 900.0]).build_eframe(|cc| FloraStackApp::with_context(&cc.egui_ctx, options, Prefs::default(), None));
    harness.step();
    (dir, harness)
}

/// Drops `files` on the window.
pub fn drop_files(harness: &mut Harness<'_, FloraStackApp>, files: &[PathBuf]) {
    for file in files {
        harness.input_mut().dropped_files.push(Arc::new(Dropped(file.clone())));
    }
}

/// Runs frames until `done` holds, failing after a minute or at the first error the app shows.
pub fn until(harness: &mut Harness<'_, FloraStackApp>, what: &str, done: impl Fn(&FloraStackApp) -> bool) {
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
