//! Retouching: the brushes (details, pixels, darken, lighten and eraser) that paint from a source frame or output onto a
//! stacked result, and the non-destructive layers of deterministic, replayable stroke records they produce, with undo
//! one stroke at a time. It may depend only on `florastack-core` and `florastack-project`.
//!
//! A stroke is a record ([`Stroke`]): the brush, its size, hardness and opacity, its source, and the pointer's path.
//! The project keeps the records, layer by layer, on top of the output they retouch (florastack-project's
//! `RetouchLayer`); nothing about the pixels is stored with them, so the retouched result is the output with its
//! strokes painted over it again ([`render`]). Painting is designed so that a stroke gives the same pixels painted
//! whole, in tiles, live as it is drawn ([`LiveStroke`]) or replayed over a region ([`replay`]): every pixel depends
//! only on itself before the stroke, the stroke's mask there, and two images painting never changes, the source and
//! the output before any retouching (`brush.rs` explains it). Undo is the project's: a stroke is removed and the
//! region it covered is replayed.
//!
//! The brushes and their arithmetic are FloraStack's own, designed from what each brush is for; no other stacker's
//! code or behavior was studied for them.

mod brush;
mod layer;
mod stroke;

pub use brush::Fields;
pub use layer::{LiveStroke, Painted, apply, render, replay};
pub use stroke::{Brush, Dab, MAX_DABS, MAX_SAMPLES, MAX_SIZE, MIN_SIZE, STROKE_VERSION, Source, Stroke, intersect, union};

/// Why a stroke couldn't be painted.
#[derive(Debug, thiserror::Error)]
pub enum RetouchError {
    /// The record is malformed.
    #[error("the stroke's {what} {reason}")]
    Stroke { what: &'static str, reason: String },
    /// An image doesn't fit the others.
    #[error("cannot paint from {what}")]
    Mismatch { what: String },
    #[error(transparent)]
    Image(#[from] florastack_core::ImageError),
}
