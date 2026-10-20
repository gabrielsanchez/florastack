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
//!   edges for the halo metric and thin near structures over a far background.
//! - [`rng`] is the small seeded generator behind both, so a test sees the same stack on every platform and run.
//!
//! The blur and the warp the generator uses are private, exact and unoptimized reference implementations; the public
//! image operations arrive with roadmap step 1.1.

mod ops;
pub mod rng;
pub mod scene;
pub mod synth;

pub use rng::Rng;
pub use synth::{DustSpot, FrameTruth, GroundTruth, StackParams, SynthError, SyntheticStack, generate, transform_error};
