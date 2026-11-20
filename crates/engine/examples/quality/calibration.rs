//! The calibration workload: a fixed piece of the kind of work the runs do, timed at the start of every stack on the
//! stack's threads, so that a run's runtime divided by it (`runtime_rel`) says the same on a fast machine and a slow
//! one, and the gate can compare it with a baseline made elsewhere, as it can't the runtime itself.
//!
//! The work is what a stacking command spends its time on, in the crates the dev profile optimizes as it does the
//! stackers: a Gaussian blur, a Laplacian and a bicubic warp of a 1536x1024 RGB image (florastack-core), and the image
//! written as a 16-bit Deflate TIFF file, read back and decoded (florastack-codecs, and the disk). It takes about a
//! second on one thread; the fastest of three tries counts, so that a moment's load elsewhere doesn't.

use std::path::Path;
use std::time::Instant;

use florastack_core::ops::{Border, Interpolation, gaussian_blur, laplacian, warp};
use florastack_core::{Affine, AnyImage, ColorSpace, Image};

const WIDTH: usize = 1536;
const HEIGHT: usize = 1024;
const TRIES: usize = 3;

/// The workload's time in seconds, in `dir`.
pub fn seconds(dir: &Path) -> Result<f64, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let data: Vec<f32> = (0..WIDTH * HEIGHT * 3).map(|i| ((i.wrapping_mul(2_654_435_761) >> 7) % 1000) as f32 / 1000.0).collect();
    let image = Image::from_vec(WIDTH, HEIGHT, 3, ColorSpace::LINEAR, data).map_err(|e| e.to_string())?;
    let mut best = f64::INFINITY;
    for _ in 0..TRIES {
        let start = Instant::now();
        once(&image, dir)?;
        best = best.min(start.elapsed().as_secs_f64());
    }
    Ok(best)
}

fn once(image: &Image<f32>, dir: &Path) -> Result<(), String> {
    let e = |e: florastack_core::ops::OpError| e.to_string();
    let blurred = gaussian_blur(image, 3.0).map_err(e)?;
    let edges = laplacian(&blurred).map_err(e)?;
    let turn = Affine { m: [[0.9986, -0.0523, 30.0], [0.0523, 0.9986, -20.0]] };
    let warped = warp(&edges, &turn, Interpolation::Bicubic, Border::Clamp).map_err(e)?;
    // 16-bit, Deflate: the defaults.
    let spec = florastack_codecs::OutputSpec::new(florastack_codecs::Format::Tiff).map_err(|e| e.to_string())?;
    let written =
        florastack_codecs::write(&AnyImage::F32(warped), &spec, &florastack_codecs::OutputMetadata::default()).map_err(|e| e.to_string())?;
    let path = dir.join("calibration.tif");
    std::fs::write(&path, &written.bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let read = florastack_codecs::read_file(&path, &florastack_codecs::ReadOptions::default()).map_err(|e| e.to_string())?;
    std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    if (read.image.width(), read.image.height()) == (WIDTH, HEIGHT) { Ok(()) } else { Err("the calibration file read back wrong".to_owned()) }
}
