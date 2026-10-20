//! Basic operations on linear `Image<f32>`, the building blocks of alignment and stacking: Gaussian blur, Lanczos3
//! resize, Sobel and Laplacian, and bilinear and bicubic affine warps.
//!
//! - **Borders**: outside the image the edge pixels repeat (clamp to edge), except where a [`Border`] says otherwise.
//! - **Coordinates**: pixel `(x, y)` is the point `(x, y)`, as in [`crate::geometry`].
//! - **Channels**: every channel is processed alike, alpha included (blur and resample premultiplied data if alpha
//!   matters).
//! - **Determinism**: rows, or vertical strips of rows, are processed in parallel with rayon, and every output sample
//!   is computed by the same arithmetic in the same order whatever the thread count, so results are the same bit for
//!   bit on every run. These CPU versions are the reference that the GPU kernels of step 4.3 are checked against.
//!
//! Parameters are checked: a NaN, negative or absurd sigma, a non-finite transform or a mismatched output returns an
//! [`OpError`], never a panic.

mod blur;
mod gradient;
mod resize;
mod warp;

pub use blur::{
    AUTO_EXACT_MAX_SIGMA, BlurMethod, MAX_BOX_PASSES, MAX_EXACT_SIGMA, MAX_SIGMA, gaussian_blur, gaussian_blur_in_place, gaussian_kernel,
};
pub use gradient::{Gradients, laplacian, sobel, sobel_magnitude};
pub use resize::{lanczos3, resize};
pub use warp::{Border, Interpolation, warp, warp_into};

use crate::image::ImageError;

/// Why an operation can't run.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum OpError {
    #[error("{name} is {value}, but it must be {expected}")]
    Param { name: &'static str, value: f64, expected: &'static str },
    #[error("the output has {output} channels but the input has {input}")]
    Channels { input: usize, output: usize },
    #[error("cannot allocate the {0} for this operation")]
    Alloc(&'static str),
    #[error(transparent)]
    Image(#[from] ImageError),
}

/// `v` as an index: NaN and negative values give 0, and values beyond `usize` saturate.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Saturating float-to-int casts, on clamped values.
fn to_index(v: f64) -> usize {
    v as usize
}

/// `v` rounded to the nearest `f32`.
#[allow(clippy::cast_possible_truncation)] // f64 to f32 rounds; that is the intent.
fn to_f32(v: f64) -> f32 {
    v as f32
}
