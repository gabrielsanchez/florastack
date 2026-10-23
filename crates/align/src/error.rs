//! What can go wrong aligning, ordering and framing a stack ([`AlignError`]). A frame that can't be matched, or that
//! is out of focus, is no error: it is flagged in its [`FrameAlignment`](crate::FrameAlignment), so that one bad frame
//! doesn't stop the stack.

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
    /// A frame's transform isn't finite or can't be inverted, so its field of view can't be placed.
    #[error("the alignment of frame {index} can't be used for framing: it isn't finite or can't be inverted")]
    Transform { index: usize },
    #[error("the aligned frames have no area in common, so the output can't be cropped to it: a frame may be misaligned, or from another scene")]
    NoCommonArea,
    /// A framing would make an output far larger than the frames, which only wrong alignments do.
    #[error(
        "framing the output this way would make it {width}x{height} pixels, more than four times the frames' {frame_width}x{frame_height} on a side: a frame is misaligned"
    )]
    CanvasSize { width: usize, height: usize, frame_width: usize, frame_height: usize },
    /// A rectangle that isn't inside the canvas it is asked of.
    #[error("the rectangle {x},{y} {width}x{height} isn't inside the {canvas_width}x{canvas_height} output")]
    Rect { x: usize, y: usize, width: usize, height: usize, canvas_width: usize, canvas_height: usize },
    /// The frames' scales are too alike to sort them by.
    #[error(
        "the frames' scales differ by {spread:.2} pixels at the frame's corner at most, too little to sort them by: the stack has no focus breathing or magnification change to order it with"
    )]
    NoScaleChange { spread: f64 },
    /// A defect mask that doesn't have the frames' size.
    #[error("the defect mask is {width}x{height} pixels, but the frames are {frame_width}x{frame_height}: a mask must have the frames' size")]
    MaskSize { width: usize, height: usize, frame_width: usize, frame_height: usize },
    /// A defect mask of too many separate spots, such as a grid of dots.
    #[error("the defect mask has more than {max} separate spots: a mask marks dust and hot pixels, not a pattern")]
    MaskSpots { max: usize },
    /// Defect detection counts at most this many frames.
    #[error("defect detection counts at most {max} frames")]
    DetectionFrames { max: usize },
    #[error("defect detection needs at least {min} frames, not {frames}: with fewer, noise could pass for dust")]
    FewFrames { frames: usize, min: usize },
    #[error("alignment was cancelled")]
    Cancelled(#[from] Cancelled),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Op(#[from] OpError),
}
