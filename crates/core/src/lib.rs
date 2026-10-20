//! Foundation types shared by every FloraStack crate: image buffers, pixel formats (`u8`, `u16` and `f32` samples with
//! one to four channels), geometry and error types, plus the product identity in [`branding`]. It may depend on no other
//! FloraStack crate, so that every crate can depend on it; `cargo xtask check-layers` enforces this.
//!
//! [`Image`] is the image buffer every crate passes around and [`Affine`] the 2x3 transform that alignment estimates
//! and warps apply. Behind the `testutil` feature, which only `[dev-dependencies]` may enable, the [`testutil`]
//! module generates synthetic focus stacks with known ground truth for the alignment and stacking tests.

pub mod geometry;
pub mod image;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

pub use geometry::Affine;
pub use image::{ColorSpace, Encoding, Image, ImageError, Sample};

/// FloraStack's product identity: display name, binary names, project-folder extension and app ID.
///
/// Generated at build time from `branding.toml` at the repository root, the one place these values are defined.
pub mod branding {
    include!(concat!(env!("OUT_DIR"), "/branding.rs"));
}
