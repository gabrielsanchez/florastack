//! Image-quality metrics for tests and benchmarks (PSNR, SSIM, halo energy around edges, noise in flat regions and
//! color shift), scored against ground truth. It may depend only on `florastack-core`, and other crates may use it only
//! as a dev-dependency, so none of it ships in the apps.
//!
//! Every metric compares a result with its ground truth, both [`Image<f32>`] of the same size and channel count, with
//! samples nominally in `[0, 1]`:
//!
//! | Metric | Function | Better | What it catches |
//! |---|---|---|---|
//! | PSNR | [`psnr`] (and [`mse`]) | higher | overall error |
//! | SSIM | [`ssim`] | higher, at most 1 | lost or false structure (detail, grit, blur) |
//! | Halo | [`halo`] | lower | fringes and smears in a ring around the truth's high-contrast edges |
//! | Noise | [`noise`] | lower | noise built up in flat regions |
//! | Color shift | [`color_shift`] | lower | mean and largest CIEDE2000 difference |
//!
//! [`color_shift`] converts both images to CIELAB from their [`Encoding`] and assumes sRGB
//! primaries and a D65 white; the others work on the samples as they are, so both images must have the same encoding.
//! An alpha channel (the second of two, the fourth of four) is ignored. Sums are taken in `f64`, in a fixed order, so
//! every metric is deterministic.

mod color;
mod halo;
mod noise;
mod plane;
mod psnr;
mod ssim;

pub use color::{DeltaE, Lab, color_shift, delta_e2000, linear_rgb_to_lab, srgb_to_linear};
pub use halo::{Halo, HaloParams, halo};
pub use noise::{Noise, NoiseParams, noise};
pub use psnr::{mse, psnr};
pub use ssim::ssim;

use florastack_core::{Encoding, Image};

/// Why a metric can't be computed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum MetricsError {
    #[error("the result is {}x{} with {} channels but the ground truth is {}x{} with {}", .result[0], .result[1], .result[2], .truth[0], .truth[1], .truth[2])]
    Shape { result: [usize; 3], truth: [usize; 3] },
    #[error("the result is {result:?}-encoded but the ground truth is {truth:?}-encoded: convert one of them first")]
    Encoding { result: Encoding, truth: Encoding },
    #[error("{metric} needs images of at least {min}x{min} pixels, not {width}x{height}")]
    TooSmall { metric: &'static str, min: usize, width: usize, height: usize },
    #[error("a sample is NaN or infinite")]
    NonFinite,
    #[error("the ground truth has no edge with a contrast of at least {0}, so there is no halo to measure")]
    NoEdges(f32),
    #[error("no flat region: no pixel's 3x3 neighborhood in the reference varies by less than {0}")]
    NoFlatRegion(f32),
}

/// Checks that `result` and `truth` can be compared sample by sample.
fn check_pair(result: &Image<f32>, truth: &Image<f32>) -> Result<(), MetricsError> {
    if !result.same_shape(truth) {
        return Err(MetricsError::Shape {
            result: [result.width(), result.height(), result.channels()],
            truth: [truth.width(), truth.height(), truth.channels()],
        });
    }
    let (r, t) = (result.color_space().encoding, truth.color_space().encoding);
    if r != t {
        return Err(MetricsError::Encoding { result: r, truth: t });
    }
    Ok(())
}

/// The channels that carry color: the first of gray (with or without alpha), the first three of RGB(A).
fn color_channels(image: &Image<f32>) -> usize {
    if image.channels() >= 3 { 3 } else { 1 }
}
