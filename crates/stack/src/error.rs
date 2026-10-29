//! What can go wrong stacking ([`StackError`]).

use florastack_core::ops::OpError;
use florastack_core::{Cancelled, ImageError, StoreError, TileError};

/// Why a stack can't be made.
#[derive(Debug, thiserror::Error)]
pub enum StackError {
    /// A stacking setting outside its range, or not a number.
    #[error("the stacking setting {name} is {value}, but it must be {expected}")]
    Setting { name: &'static str, value: f64, expected: &'static str },
    /// A frame was added, or the result asked for, before [`crate::Stacker::begin`], or after a failure or a
    /// cancellation, which leave the stack incomplete.
    #[error("no stack is in progress: begin one first (a failed or cancelled frame ends the stack it was added to)")]
    NotStarted,
    #[error("a stack needs at least one frame")]
    NoFrames,
    #[error("a stack of {width}x{height} pixels with {channels} channels can't be made: it needs at least one pixel and 1 to 4 channels")]
    Shape { width: usize, height: usize, channels: usize },
    #[error(
        "the frame is {width}x{height} with {channels} channels, but the stack is {expected_width}x{expected_height} with {expected_channels}: frames must have the canvas's size and channels"
    )]
    FrameShape { width: usize, height: usize, channels: usize, expected_width: usize, expected_height: usize, expected_channels: usize },
    /// A frame source was asked for pixels outside the frame.
    #[error("the region {x},{y} {width}x{height} isn't inside the {frame_width}x{frame_height} frame")]
    Region { x: usize, y: usize, width: usize, height: usize, frame_width: usize, frame_height: usize },
    /// A frame source returned a patch of another size than the region asked for.
    #[error("a frame source returned {got_width}x{got_height} pixels with {got_channels} channels for a {width}x{height} region with {channels}")]
    Patch { width: usize, height: usize, channels: usize, got_width: usize, got_height: usize, got_channels: usize },
    /// A depth map's pass 2 was given another number of frames than its pass 1 measured, or a stack has more frames
    /// than a depth map can index.
    #[error("the depth-map method composites the {expected} frames pass 1 measured, in the same order, but got {got}")]
    FrameCount { expected: usize, got: usize },
    #[error("a depth map indexes at most {max} frames")]
    TooManyFrames { max: usize },
    /// A mask or a depth map that isn't one plane of the canvas's size.
    #[error(
        "the {what} is {width}x{height} with {channels} channels, but it must be one channel of {expected_width}x{expected_height}, the canvas's size"
    )]
    Plane { what: &'static str, width: usize, height: usize, channels: usize, expected_width: usize, expected_height: usize },
    #[error("stacking was cancelled")]
    Cancelled(#[from] Cancelled),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Op(#[from] OpError),
    #[error(transparent)]
    Tile(#[from] TileError),
    /// The accumulators' tiles couldn't be spilled to disk or read back.
    #[error(transparent)]
    Store(#[from] StoreError),
}
