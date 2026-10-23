//! The order a stack is processed in: from the end with the narrower field of view, by default, so that the frames
//! are aligned to that end's frame and, cropped to its field of view, every output pixel is covered by every frame.
//!
//! Focus breathing, and a rail that moves the camera, change the magnification steadily from one end of a stack to
//! the other. The frame with the largest magnification sees the least of the scene: the stack's *narrow end*. Every
//! other frame sees more, so an output framed on the narrow end has pixels of every frame everywhere, and no frame's
//! edge has to be stretched across it (the edge streaks of stacking from the wide end).
//!
//! - [`narrow_end`] compares the scales of the first and the last frame that aligned, once the stack is aligned: the
//!   end whose frame shows the scene larger is the narrow one, when the two differ by enough to move the frame's corner
//!   by half a pixel ([`NARROW_MIN`]).
//! - [`ProcessingOrder::new`] starts from the narrow end ([`AlignSettings::automatic_order`]), or from the first frame
//!   when that is off or the ends can't be told apart, and from the other end with [`AlignSettings::reverse_order`].
//!   Its master is the first frame in that order that aligned: the narrow end's, by default.
//! - [`order_stack`] does both and returns the alignment rebased onto that master ([`StackAlignment::rebased`]), and
//!   [`align_ordered`] aligns frames in memory that way. The stack is aligned from its middle frame, which shares the
//!   most detail with the others, before it is rebased: rebasing composes transforms, it doesn't align again.

use florastack_core::{Image, Monitor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::AlignError;
use crate::model::center;
use crate::settings::AlignSettings;
use crate::stack::{StackAlignment, Status, align_frames};

/// The ends' scales must differ by enough to move a frame's corner by this many pixels to tell the narrow end.
pub const NARROW_MIN: f64 = 0.5;

/// One end of a stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum End {
    First,
    Last,
}

impl End {
    #[must_use]
    pub fn other(self) -> End {
        match self {
            End::First => End::Last,
            End::Last => End::First,
        }
    }
}

/// The frames of `alignment` that were aligned (not rejected), in stack order.
fn accepted(alignment: &StackAlignment) -> impl DoubleEndedIterator<Item = usize> + '_ {
    alignment.frames.iter().enumerate().filter(|(_, f)| f.status != Status::Rejected).map(|(i, _)| i)
}

/// The end of the stack with the narrower field of view: of the first and the last frame that aligned, the one that
/// shows the scene larger. `None` when their scales differ by less than [`NARROW_MIN`] pixels at the frame's corner, or
/// fewer than two frames aligned.
pub fn narrow_end(alignment: &StackAlignment) -> Option<End> {
    let (first, last) = (accepted(alignment).next()?, accepted(alignment).next_back()?);
    if first == last {
        return None;
    }
    let scale = |i: usize| alignment.frames.get(i).map(|f| f.similarity.scale).filter(|s| s.is_finite());
    let (a, b) = (scale(first)?, scale(last)?);
    let c = center(alignment.width, alignment.height);
    let radius = c.0.hypot(c.1).max(1.0);
    if (b - a).abs() * radius < NARROW_MIN {
        return None;
    }
    Some(if b > a { End::Last } else { End::First })
}

/// The order a stack is processed in. See the module documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessingOrder {
    /// The frames by their index in the stack, in the order they are processed: from `start` to the other end.
    pub frames: Vec<usize>,
    /// The end processing starts from.
    pub start: End,
    /// The end with the narrower field of view, if the frames' scales tell.
    pub narrow: Option<End>,
    /// The frame the stack is aligned to: the first in processing order that aligned (or the first, if none did).
    /// Its sharpness doesn't matter: once the stack is aligned, the master is only the frame whose pixels the
    /// transforms start from.
    pub master: usize,
    /// The frame at the narrow end whose field of view narrow framing keeps: the first that aligned, from that end.
    /// The master's, when processing starts there; `None` when the narrow end isn't known.
    pub narrow_frame: Option<usize>,
}

impl ProcessingOrder {
    /// The order `settings` choose for a stack aligned as `alignment`.
    pub fn new(alignment: &StackAlignment, settings: &AlignSettings) -> ProcessingOrder {
        let n = alignment.frames.len();
        let narrow = narrow_end(alignment);
        let start = match (settings.automatic_order, narrow) {
            (true, Some(end)) => end,
            _ => End::First,
        };
        let start = if settings.reverse_order { start.other() } else { start };
        let from = |end: End| -> Vec<usize> { if end == End::First { (0..n).collect() } else { (0..n).rev().collect() } };
        let frames = from(start);
        let aligned = |i: &&usize| alignment.frames.get(**i).is_some_and(|f| f.status != Status::Rejected);
        let master = frames.iter().find(aligned).or(frames.first()).copied().unwrap_or(0);
        let narrow_frame = narrow.and_then(|end| from(end).iter().find(aligned).copied());
        ProcessingOrder { frames, start, narrow, master, narrow_frame }
    }
}

/// Orders an aligned stack as `settings` say (see the module documentation): the processing order, and the
/// alignment rebased onto its master.
pub fn order_stack(alignment: &StackAlignment, settings: &AlignSettings) -> Result<(StackAlignment, ProcessingOrder), AlignError> {
    let order = ProcessingOrder::new(alignment, settings);
    Ok((alignment.rebased(order.master)?, order))
}

/// Aligns frames in memory from their middle frame, and orders them as `settings` say ([`order_stack`]).
pub fn align_ordered(
    frames: &[Image<f32>],
    settings: &AlignSettings,
    monitor: &dyn Monitor,
) -> Result<(StackAlignment, ProcessingOrder), AlignError> {
    let alignment = align_frames(frames, frames.len() / 2, settings, monitor)?;
    order_stack(&alignment, settings)
}
