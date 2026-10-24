//! The streaming interface every stacking method implements: [`Stacker`] (`begin`, `add_frame`, `finish`), the
//! settings that make stackers ([`Method`]), what a stack gives ([`Stacked`]), and [`stack`], which drives a stacker
//! over frames in memory.

use florastack_core::{Cancelled, Image, Monitor};

use crate::error::StackError;
use crate::source::FrameSource;
use crate::tiled::Tiling;

/// The canvas a stack is made on: the output's size and the frames' channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackShape {
    pub width: usize,
    pub height: usize,
    /// 1 to 4; every channel is stacked alike.
    pub channels: usize,
}

impl StackShape {
    /// The shape of `frame`'s canvas.
    pub fn of(frame: &dyn FrameSource) -> StackShape {
        StackShape { width: frame.width(), height: frame.height(), channels: frame.channels() }
    }

    pub(crate) fn check(self) -> Result<(), StackError> {
        if self.width == 0 || self.height == 0 || !(1..=4).contains(&self.channels) {
            return Err(StackError::Shape { width: self.width, height: self.height, channels: self.channels });
        }
        Ok(())
    }

    /// Checks that `frame` has this shape.
    pub(crate) fn check_frame(self, frame: &dyn FrameSource) -> Result<(), StackError> {
        if StackShape::of(frame) != self {
            return Err(StackError::FrameShape {
                width: frame.width(),
                height: frame.height(),
                channels: frame.channels(),
                expected_width: self.width,
                expected_height: self.height,
                expected_channels: self.channels,
            });
        }
        Ok(())
    }
}

/// What a stack gives.
#[derive(Clone, Debug, PartialEq)]
pub struct Stacked {
    /// The stacked image, linear, with the frames' channels. A pixel that no frame covers is 0.
    pub image: Image<f32>,
    /// 1 where some frame covers the pixel, 0 where none does; `None` when frames cover every pixel, as they do but in
    /// wide framing.
    pub alpha: Option<Image<f32>>,
    /// How many frames went in.
    pub frames: usize,
    /// How many pixels no frame covers.
    pub uncovered: usize,
}

/// A stacking method, fed one frame at a time. See the crate documentation.
///
/// A stacker holds the accumulators of one stack at a time, whose memory depends on the canvas's size and not on the
/// number of frames. Frames must be added in the same order for the same result: methods are deterministic for a
/// given input order and settings, whatever the number of threads. A frame that fails or is cancelled part way ends
/// the stack (its accumulators are dropped, and the next `add_frame` or `finish` is [`StackError::NotStarted`]): its
/// caller starts again or gives up, and nothing is left to clean up.
pub trait Stacker: Send {
    /// The method's name, as outputs record it, such as `average`.
    fn method(&self) -> &'static str;

    /// Starts a stack on a canvas of `shape`, dropping any stack in progress.
    fn begin(&mut self, shape: StackShape) -> Result<(), StackError>;

    /// Folds the next frame into the stack. It must have the canvas's size and channels. `monitor` hears the progress
    /// within the frame (in tiles), and cancels at the next tile.
    fn add_frame(&mut self, frame: &dyn FrameSource, monitor: &dyn Monitor) -> Result<(), StackError>;

    /// The stack of the frames added since `begin`, which ends it: the stacker is then free for another `begin`.
    /// `monitor` hears the progress (in tiles).
    fn finish(&mut self, monitor: &dyn Monitor) -> Result<Stacked, StackError>;
}

impl<S: Stacker + ?Sized> Stacker for Box<S> {
    fn method(&self) -> &'static str {
        (**self).method()
    }

    fn begin(&mut self, shape: StackShape) -> Result<(), StackError> {
        (**self).begin(shape)
    }

    fn add_frame(&mut self, frame: &dyn FrameSource, monitor: &dyn Monitor) -> Result<(), StackError> {
        (**self).add_frame(frame, monitor)
    }

    fn finish(&mut self, monitor: &dyn Monitor) -> Result<Stacked, StackError> {
        (**self).finish(monitor)
    }
}

/// A stacking method's settings, which make its stackers, at full scale or for the reduced frames of a preview.
pub trait Method {
    /// The method's name, as outputs record it.
    fn name(&self) -> &'static str;

    /// Checks the settings.
    fn validate(&self) -> Result<(), StackError>;

    /// A stacker with these settings, tiled as `tiling` says.
    fn stacker(&self, tiling: &Tiling) -> Result<Box<dyn Stacker>, StackError>;

    /// The same settings for frames reduced by `factor` (4 for a quarter-scale preview): every length in pixels
    /// divided by it, so that a preview looks like the full stack, only smaller.
    #[must_use]
    fn reduced(&self, factor: f64) -> Self
    where
        Self: Sized;
}

/// The progress of the `frame`-th of `frames` frames (from 0) as a share of the whole stack's: a stacker reports the
/// tiles done in the frame it is adding, and this reports them to `inner` as tiles done in the whole stack, so that
/// the progress never goes back from one frame to the next.
pub struct FrameProgress<'a> {
    pub inner: &'a dyn Monitor,
    pub frame: u64,
    pub frames: u64,
}

impl Monitor for FrameProgress<'_> {
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn progress(&self, done: u64, total: u64) {
        let frames = self.frames.max(self.frame + 1);
        self.inner.progress(self.frame.saturating_mul(total).saturating_add(done.min(total)), frames.saturating_mul(total));
    }
}

/// Stacks `frames`, in order, with `stacker`: begins a stack of the first frame's shape, adds each frame (reporting
/// the progress over all of them to `monitor`) and finishes. The finishing step's own progress isn't reported, but it
/// is cancelled like the rest.
pub fn stack<S: Stacker + ?Sized, F: FrameSource>(stacker: &mut S, frames: &[F], monitor: &dyn Monitor) -> Result<Stacked, StackError> {
    let first = frames.first().ok_or(StackError::NoFrames)?;
    stacker.begin(StackShape::of(first))?;
    let n = frames.len() as u64;
    for (i, frame) in frames.iter().enumerate() {
        stacker.add_frame(frame, &FrameProgress { inner: monitor, frame: i as u64, frames: n })?;
    }
    stacker.finish(&Quiet(monitor))
}

/// A monitor that passes cancellation on but not progress.
struct Quiet<'a>(&'a dyn Monitor);

impl Monitor for Quiet<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    fn progress(&self, _: u64, _: u64) {}

    fn checkpoint(&self) -> Result<(), Cancelled> {
        self.0.checkpoint()
    }
}
