//! The browser build's test stack, for `cargo xtask web --test` (`xtask/src/web.rs`), which runs it natively around the
//! headless browser's run of the same stack:
//!
//! - `web_stack frames <dir>`: writes the stack, 20 frames of a synthetic layered scene (`florastack_core::testutil`)
//!   as 8-bit PNG files, `frame-00.png` to `frame-19.png`, which the browser test hands the page.
//! - `web_stack reference <frames> <out.png>`: stacks them natively, on the CPU, as the browser test does in the page
//!   (`align.run`, then `stack.pyramid` with the default settings), and writes the output as an 8-bit PNG.
//! - `web_stack compare <reference.png> <web.png>`: the two outputs' PSNR, which must be at least
//!   [`AGREEMENT_DB`]: the browser build stacks what the desktop one does, the same code compiled for another machine,
//!   which differs from it only in the last bits of its floating point (its own `exp` and `pow`, for one).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use florastack_core::testutil::{Backdrop, StackParams, generate_layered, scene};
use florastack_core::{AnyImage, Encoding, Image, Rect};
use florastack_engine::model::FolderTrash;
use florastack_engine::{Compute, Dispatched, Engine, EngineConfig, InlineExecutor, SystemClock, Value, json};

/// The frames, and their size.
const FRAMES: usize = 20;
const WIDTH: usize = 480;
const HEIGHT: usize = 360;
/// The generator's margin, cropped off (as the quality runner's synthetic stacks are made).
const MARGIN: usize = 8;
/// The least PSNR, in dB, between the browser's output and the native one. 8-bit outputs of the same stack that
/// differ only in the rounding of some samples score above 50 dB; a stack that went wrong (other frames, another
/// method, a misaligned frame) scores below 30.
const AGREEMENT_DB: f64 = 40.0;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["frames", dir] => frames(Path::new(dir)),
        ["reference", frames, out] => reference(Path::new(frames), Path::new(out)),
        ["compare", reference, web] => compare(Path::new(reference), Path::new(web)),
        _ => Err("usage: web_stack frames <dir> | reference <frames> <out.png> | compare <reference.png> <web.png>".to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("web_stack: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The stack: a layered scene focused from front to back, the frames in register, lightly noisy.
fn stack() -> Vec<Image<f32>> {
    const SCALE: f32 = 3.0;
    let scene = scene::layered(WIDTH + 2 * MARGIN, HEIGHT + 2 * MARGIN, f64::from(SCALE), Backdrop::Textured, 7).unwrap();
    let params = StackParams {
        focus_range: Some((0.0, 1.0)),
        blur_per_depth: 8.0 * SCALE,
        max_blur: 8.0 * SCALE,
        blur_step: 0.5 * SCALE,
        read_noise: 0.002,
        shot_noise: 0.004,
        seed: 7,
        ..StackParams::identity(FRAMES)
    };
    let made = generate_layered(&scene, &params).unwrap();
    made.frames.iter().map(|f| f.crop(Rect::new(MARGIN, MARGIN, WIDTH, HEIGHT)).unwrap()).collect()
}

fn png(image: &Image<f32>) -> Result<Vec<u8>, String> {
    let encoded = image.to_encoding::<u8>(Encoding::Srgb).map_err(|e| e.to_string())?;
    let spec = florastack_codecs::OutputSpec::new(florastack_codecs::Format::Png).map_err(|e| e.to_string())?;
    let written =
        florastack_codecs::write(&AnyImage::U8(encoded), &spec, &florastack_codecs::OutputMetadata::default()).map_err(|e| e.to_string())?;
    Ok(written.bytes)
}

fn frames(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (i, frame) in stack().iter().enumerate() {
        let path = dir.join(format!("frame-{i:02}.png"));
        std::fs::write(&path, png(frame)?).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    println!("wrote {FRAMES} frames of {WIDTH}x{HEIGHT} to {}", dir.display());
    Ok(())
}

fn command(engine: &mut Engine, name: &str, params: Value) -> Result<Value, String> {
    let result = match engine.dispatch(name, params) {
        Ok(Dispatched::Done(value)) => Ok(value),
        Ok(Dispatched::Started(job)) => engine.wait(job),
        Err(e) => Err(e),
    };
    result.map_err(|e| format!("{name}: {e}"))
}

fn reference(frames: &Path, out: &Path) -> Result<(), String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(frames)
        .map_err(|e| format!("{}: {e}", frames.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    files.sort();
    let work = std::env::temp_dir().join(format!("florastack-web-reference-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let config = EngineConfig {
        temporary_projects: Some(work.join("temp")),
        trash: Arc::new(FolderTrash::new(work.join("trash"))),
        executor: Arc::new(InlineExecutor),
        clock: Arc::new(SystemClock::new()),
        waker: None,
        max_heavy_jobs: 1,
        max_preview_jobs: 1,
        compute: Compute::CPU,
        tiling: florastack_engine::Tiling::default(),
    };
    let mut engine = Engine::new(config);
    command(&mut engine, "project.new", json!({}))?;
    command(&mut engine, "project.addFrames", json!({ "paths": files }))?;
    command(&mut engine, "align.run", json!({}))?;
    let stacked = command(&mut engine, "stack.pyramid", json!({}))?;
    let folder = work.join("export");
    let settings = json!({ "format": "png", "depth": "u8", "metadata": false, "template": "reference" });
    command(&mut engine, "export.run", json!({ "output": stacked["output"], "folder": folder, "settings": settings }))?;
    std::fs::copy(folder.join("reference.png"), out).map_err(|e| format!("{}: {e}", out.display()))?;
    let _ = std::fs::remove_dir_all(&work);
    println!("stacked {} frames natively: {}", files.len(), out.display());
    Ok(())
}

fn read(path: &Path) -> Result<Image<f32>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let decoded = florastack_codecs::read(&bytes, &florastack_codecs::ReadOptions::default()).map_err(|e| format!("{}: {e}", path.display()))?;
    let image = match decoded.image {
        AnyImage::U8(image) => image.to_linear_f32(),
        AnyImage::U16(image) => image.to_linear_f32(),
        AnyImage::F32(image) => Ok(image),
    }
    .map_err(|e| e.to_string())?;
    // Color only: an alpha channel, if the output has one, is all opaque here.
    let channels = match image.channels() {
        2 => 1,
        4 => 3,
        n => n,
    };
    Image::from_fn(image.width(), image.height(), channels, image.color_space().clone(), |x, y, c| image.get(x, y, c).unwrap_or(0.0))
        .map_err(|e| e.to_string())
}

fn compare(reference: &Path, web: &Path) -> Result<(), String> {
    let (a, b) = (read(reference)?, read(web)?);
    if (a.width(), a.height(), a.channels()) != (b.width(), b.height(), b.channels()) {
        return Err(format!(
            "the outputs differ in shape: {}x{}x{} natively, {}x{}x{} in the browser",
            a.width(),
            a.height(),
            a.channels(),
            b.width(),
            b.height(),
            b.channels()
        ));
    }
    let db = florastack_metrics::psnr(&b, &a).map_err(|e| e.to_string())?;
    println!("the browser's output against the native one: {}x{}, PSNR {db:.1} dB (at least {AGREEMENT_DB} dB)", a.width(), a.height());
    if db < AGREEMENT_DB { Err(format!("the browser's output differs from the native one: {db:.1} dB, below {AGREEMENT_DB} dB")) } else { Ok(()) }
}
