//! Stacking algorithms (weighted average, pyramid and depth map), fed one aligned linear frame at a time so memory depends
//! on image size rather than stack depth, and deterministic for a given input order and settings. Its CPU code is the
//! reference that the GPU kernels are tested against. It may depend only on `florastack-core`.
//!
//! # The streaming framework
//!
//! Every method is a [`Stacker`]: [`Stacker::begin`] starts a stack on a canvas ([`StackShape`]: the output's size and
//! the frames' channels), [`Stacker::add_frame`] folds the next frame into the method's accumulators, and
//! [`Stacker::finish`] turns them into the result ([`Stacked`]). Frames arrive aligned, linear and `f32`, already on the
//! output's canvas, as [`FrameSource`]s that hand out any rectangle of themselves with their coverage ([`Patch`]): an
//! image in memory, a [`CoveredFrame`], or the engine's frame, which it warps onto each rectangle as it is read. The
//! caller reads one frame, adds it and drops it, so a stack of any depth holds one frame and the accumulators.
//! [`stack`] drives a stacker over frames in memory. Each method's settings make its stackers ([`Method`]).
//!
//! The methods are built on [`Tiled`] (`tiled.rs`), which keeps a method's accumulators tile by tile, in florastack-core's
//! tiled view ([`florastack_core::TileGrid`]) and a [`florastack_core::TileCache`] with a memory budget ([`Tiling`]):
//! a frame is read and folded a tile at a time, the tiles of a frame in parallel, and the tiles are stitched back at
//! the end, with an overlap wide enough for the method's filters that the result is the one the method gives on the
//! whole canvas. Beyond the budget the accumulators spill to temporary files, so a canvas larger than memory can be
//! stacked. A method is a [`TileKernel`]: its per-tile state, how a frame's patch folds into it and the result.
//!
//! - **Progress and cancellation**: `add_frame` and `finish` report the tiles done to their
//!   [`florastack_core::Monitor`] and stop at the next batch of tiles when it is cancelled; [`FrameProgress`] turns the
//!   tiles of one frame into the progress of the whole stack. A cancelled or failed frame ends the stack.
//! - **Determinism**: a tile's fold reads only that tile, and every per-pixel sum runs over the frames in the order they
//!   came, so the result is the same bit for bit whatever the number of threads, and the same for the same frames in the
//!   same order and the same settings.
//! - **Coverage**: each frame's weights are multiplied by its coverage, so that in wide framing a frame adds nothing where
//!   it has no pixels, and nothing of its edge is smeared across them. A pixel that no frame covers has no weight: it is
//!   0, marked 0 in [`Stacked::alpha`], which is there only when there are such pixels.
//!
//! # Methods
//!
//! - **Weighted average** ([`AverageSettings`], [`WeightedAverage`], `average.rs`): each frame weighted per pixel by its
//!   sharpness there, the Laplacian's energy averaged over `radius` and raised to `power`, the weights smoothed over
//!   `smoothing` and normalized. Fast, with no halos of its own but some softness where near and far detail overlap:
//!   the quick preview method, and a fallback for simple subjects.
//!
//! # Previews
//!
//! [`Preview`] runs any method on frames reduced to a quarter of their size ([`PREVIEW_FACTOR`]), with the method's
//! settings scaled to match ([`Method::reduced`]), so that the user sees a result in seconds and again after every
//! change of a setting, while the full stack runs (`preview.rs`).
//!
//! The stack's settings, as a project's preferences hold them, are [`StackSettings`].

mod average;
mod error;
mod method;
mod preview;
mod settings;
mod source;
mod tiled;

pub use average::{
    AVERAGE, AverageSettings, ENERGY_CAP, ENERGY_FLOOR, ENERGY_SCALE, MAX_RADIUS, MAX_SAMPLE, MIN_COVERAGE, POWER_RANGE, WeightedAverage,
};
pub use error::StackError;
pub use method::{FrameProgress, Method, StackShape, Stacked, Stacker, stack};
pub use preview::{PREVIEW_FACTOR, Preview, preview, preview_size, reduce, reduce_patch};
pub use settings::StackSettings;
pub use source::{CoveredFrame, FrameSource, Gained, Patch};
pub use tiled::{TileKernel, Tiled, Tiling};
