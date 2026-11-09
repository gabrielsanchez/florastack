//! Why adjusting an image failed.

/// Why adjusting an image failed.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// The adjustments are out of range or too large.
    #[error("invalid adjustments: {reason}")]
    Invalid { reason: String },
    /// The image isn't one the renderer takes.
    #[error("cannot adjust this image: {reason}")]
    Image { reason: String },
    /// A mask needs the stack's focus, which wasn't given.
    #[error("the {shape} mask needs the stack's focus map, which this output doesn't have")]
    NoFocus { shape: &'static str },
    /// A focus map has another shape than the image's.
    #[error("the focus map is {width}x{height}: it must have one channel and the image's aspect ratio")]
    Guide { width: usize, height: usize },
    #[error(transparent)]
    Core(#[from] florastack_core::ImageError),
}

impl EditError {
    pub(crate) fn invalid(reason: impl Into<String>) -> EditError {
        EditError::Invalid { reason: reason.into() }
    }

    pub(crate) fn image(reason: impl Into<String>) -> EditError {
        EditError::Image { reason: reason.into() }
    }
}
