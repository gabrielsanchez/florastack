//! What can go wrong aligning a stack ([`AlignError`]). A frame that can't be matched is no error: it is flagged in
//! its [`FrameAlignment`](crate::FrameAlignment), so that one bad frame doesn't stop the stack.

use florastack_core::ops::OpError;
use florastack_core::{Cancelled, ImageError};

/// Why a stack can't be aligned.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum AlignError {
    /// An alignment setting outside its range, or not a number.
    #[error("the alignment setting {name} is {value}, but it must be {expected}")]
    Setting { name: &'static str, value: f64, expected: &'static str },
    #[error("a stack to align needs at least one frame")]
    NoFrames,
    #[error("the master frame is number {master}, but the stack has {frames} frames")]
    Master { master: usize, frames: usize },
    #[error("frame {index} is {width}x{height}, but the stack's frames are {expected_width}x{expected_height}: frames must all have the same size")]
    FrameSize { index: usize, width: usize, height: usize, expected_width: usize, expected_height: usize },
    #[error("a {width}x{height} frame is too small to align: frames need at least {min} pixels on each side", min = crate::MIN_SIDE)]
    TooSmall { width: usize, height: usize },
    /// A frame was added after the last one.
    #[error("every frame of the stack has been aligned already")]
    Finished,
    /// The results were asked for before every frame was added.
    #[error("only {added} of the stack's {frames} frames have been aligned")]
    Incomplete { added: usize, frames: usize },
    #[error("alignment was cancelled")]
    Cancelled(#[from] Cancelled),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Op(#[from] OpError),
}
