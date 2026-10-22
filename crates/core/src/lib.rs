//! Foundation types shared by every FloraStack crate: image buffers, pixel formats (`u8`, `u16` and `f32` samples with
//! one to four channels), geometry and error types, the basic image operations, and the product identity in
//! [`branding`]. It may depend on no other FloraStack crate, so that every crate can depend on it; `cargo xtask
//! check-layers` enforces this.
//!
//! - [`image`]: [`Image`], the buffer every crate passes around, with its [`ColorSpace`] tag, and [`AnyImage`] for a
//!   sample type known only at run time.
//! - [`convert`]: conversions between sample types and between sRGB-encoded and linear samples. Stacking math runs on
//!   linear `Image<f32>`.
//! - [`geometry`]: [`Affine`], the 2x3 transform that alignment estimates and warps apply, and [`Rect`].
//! - [`orientation`]: [`Orientation`], the EXIF and TIFF orientation tag, and [`Image::oriented`], which turns stored
//!   pixels upright.
//! - [`ops`]: Gaussian blur, Lanczos3 resize, Sobel and Laplacian, and bilinear and bicubic affine warps, the CPU
//!   reference for the GPU kernels to come.
//! - [`tile`]: [`TileGrid`], overlapping tiles stitched back with feathered seams, so that processing can bound its
//!   memory.
//! - [`store`]: [`TileCache`], tiles in memory up to a budget and spilled to a [`TileStore`] (temporary files) beyond it.
//! - [`lock`]: [`LockFile`], an exclusive lock that the operating system releases when its process ends, so that a
//!   folder in use (a tile store's, an open project's) can be told from one that a crashed run left behind.
//! - [`progress`]: [`Monitor`], what long operations report their progress to and check for cancellation at their
//!   checkpoints, so that the engine can run them as cancellable jobs.
//!
//! Behind the `testutil` feature, which only `[dev-dependencies]` may enable, the [`testutil`] module generates
//! synthetic focus stacks with known ground truth for the alignment and stacking tests.

pub mod convert;
pub mod geometry;
pub mod image;
pub mod lock;
pub mod ops;
pub mod orientation;
pub mod progress;
pub mod store;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;
pub mod tile;

pub use geometry::{Affine, Rect};
pub use image::{AnyImage, ColorSpace, Encoding, Image, ImageError, Sample, SampleFormat};
pub use lock::{LockFile, LockState};
pub use orientation::Orientation;
pub use progress::{Cancelled, Monitor, Unmonitored};
pub use store::{CacheStats, MemoryStore, StoreError, TempFileStore, TileCache, TileKey, TileStore};
pub use tile::{Tile, TileError, TileGrid};

/// FloraStack's product identity: display name, binary names, project-folder extension, app ID and the XMP namespace of
/// its output files.
///
/// Generated at build time from `branding.toml` at the repository root, the one place these values are defined.
pub mod branding {
    include!(concat!(env!("OUT_DIR"), "/branding.rs"));
}
