//! Test utilities, behind the `testutil` feature: synthetic focus stacks with known ground truth, so that alignment
//! and stacking can be scored objectively, with `florastack-metrics`.
//!
//! Enable the feature from `[dev-dependencies]` only (`cargo xtask check-layers` rejects it anywhere else):
//!
//! ```toml
//! [dev-dependencies]
//! florastack-core = { workspace = true, features = ["testutil"] }
//! ```
//!
//! - [`synth`] turns a sharp image and a depth map into a stack of frames: depth-dependent Gaussian blur, focus
//!   breathing, sub-pixel shift, rotation, brightness jitter, sensor noise and fixed dust spots, with the true
//!   transforms and gains recorded as ground truth.
//! - [`scene`] makes a procedural sharp image and depth map to feed it: texture everywhere for alignment, high-contrast
//!   edges for the halo metric and thin near structures over a far background; and a scene of occluding layers
//!   ([`scene::layered`]) for [`layered`].
//! - [`layered`] makes stacks of such a scene whose out-of-focus edges spread over what lies behind them, as a lens's
//!   do, so that the veils and halos stacking methods leave around a subject's edges can be scored.
//! - [`rng`] is the small seeded generator behind both, so a test sees the same stack on every platform and run.
//!
//! The generator blurs with the exact Gaussian kernel and warps with the bicubic filter of [`crate::ops`]. The plain,
//! unoptimized versions of both that it used before step 1.1 remain as the tests' oracle, which the public operations
//! match bit for bit.

pub mod layered;
mod ops;
pub mod rng;
pub mod scene;
pub mod synth;

pub use layered::{Layer, LayeredScene, generate_layered};
pub use rng::Rng;
pub use scene::Backdrop;
pub use synth::{DustSpot, FrameTruth, GroundTruth, StackParams, SynthError, SyntheticStack, generate, transform_error};
