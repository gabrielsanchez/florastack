//! Frames as stacking reads them: [`FrameSource`], an aligned frame on the output's canvas that hands out any
//! rectangle of itself with its coverage, so that a stacker can read a frame tile by tile and the frame never has to
//! exist whole on the canvas.
//!
//! - An [`Image<f32>`] is a frame that covers the whole canvas.
//! - [`CoveredFrame`] is a frame already on the canvas, with its coverage weights: what `florastack-align`'s
//!   `warp_to_canvas` makes, or a reduced frame of a preview.
//! - The engine's own source warps a decoded frame onto each rectangle as it is asked for it, with the frame's coverage
//!   (`florastack_align::Coverage`): the warped frame then never exists whole.
//!
//! A [`Patch`] holds the pixels of a rectangle and, unless the frame covers all of it, a weight per pixel from 0 to 1
//! ([`Patch::coverage`]): 0 where the frame has nothing (its pixels are 0 there, not its edge pixels repeated), ramping
//! up inside the frame's edge. Stacking multiplies each frame's weights by it, so that a frame is left out where it
//! doesn't cover the canvas.

use florastack_core::{Image, Rect};
use rayon::prelude::*;

use crate::error::StackError;

/// The pixels of a rectangle of a frame, and how much of each the frame covers. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct Patch {
    /// The frame's pixels, linear.
    pub image: Image<f32>,
    /// One channel of weights from 0 to 1, the image's size; `None` when the frame covers every pixel.
    pub coverage: Option<Image<f32>>,
}

/// An aligned frame on the canvas, read a rectangle at a time. See the module documentation.
///
/// `Sync`, so that a stacker can read the tiles of a frame in parallel.
pub trait FrameSource: Sync {
    /// The canvas's width, in pixels.
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn channels(&self) -> usize;
    /// The frame's pixels on `rect`, which lies inside the frame, and its coverage there.
    fn read(&self, rect: Rect) -> Result<Patch, StackError>;
}

/// Checks that `rect` lies inside a `width` x `height` frame and isn't empty.
pub(crate) fn check_region(rect: Rect, width: usize, height: usize) -> Result<(), StackError> {
    if rect.is_empty() || !rect.fits_in(width, height) {
        return Err(StackError::Region { x: rect.x, y: rect.y, width: rect.width, height: rect.height, frame_width: width, frame_height: height });
    }
    Ok(())
}

impl FrameSource for Image<f32> {
    fn width(&self) -> usize {
        Image::width(self)
    }

    fn height(&self) -> usize {
        Image::height(self)
    }

    fn channels(&self) -> usize {
        Image::channels(self)
    }

    fn read(&self, rect: Rect) -> Result<Patch, StackError> {
        check_region(rect, Image::width(self), Image::height(self))?;
        Ok(Patch { image: self.crop(rect)?, coverage: None })
    }
}

/// A frame on the canvas with its coverage weights (see the module documentation).
#[derive(Clone, Debug, PartialEq)]
pub struct CoveredFrame {
    pub image: Image<f32>,
    /// One channel, the image's size.
    pub coverage: Image<f32>,
}

impl CoveredFrame {
    /// `image` with `coverage`, which must have its size and one channel.
    pub fn new(image: Image<f32>, coverage: Image<f32>) -> Result<CoveredFrame, StackError> {
        let (w, h) = (image.width(), image.height());
        if (coverage.width(), coverage.height(), coverage.channels()) != (w, h, 1) {
            return Err(StackError::Patch {
                width: w,
                height: h,
                channels: 1,
                got_width: coverage.width(),
                got_height: coverage.height(),
                got_channels: coverage.channels(),
            });
        }
        Ok(CoveredFrame { image, coverage })
    }
}

impl FrameSource for CoveredFrame {
    fn width(&self) -> usize {
        self.image.width()
    }

    fn height(&self) -> usize {
        self.image.height()
    }

    fn channels(&self) -> usize {
        self.image.channels()
    }

    fn read(&self, rect: Rect) -> Result<Patch, StackError> {
        check_region(rect, self.image.width(), self.image.height())?;
        Ok(Patch { image: self.image.crop(rect)?, coverage: Some(self.coverage.crop(rect)?) })
    }
}

impl<S: FrameSource + ?Sized> FrameSource for &S {
    fn width(&self) -> usize {
        (**self).width()
    }

    fn height(&self) -> usize {
        (**self).height()
    }

    fn channels(&self) -> usize {
        (**self).channels()
    }

    fn read(&self, rect: Rect) -> Result<Patch, StackError> {
        (**self).read(rect)
    }
}

/// A frame whose samples are multiplied by a gain as they are read: a frame's brightness matched to the master's with
/// the gain alignment measured.
pub struct Gained<S> {
    pub frame: S,
    pub gain: f32,
}

impl<S: FrameSource> FrameSource for Gained<S> {
    fn width(&self) -> usize {
        self.frame.width()
    }

    fn height(&self) -> usize {
        self.frame.height()
    }

    fn channels(&self) -> usize {
        self.frame.channels()
    }

    fn read(&self, rect: Rect) -> Result<Patch, StackError> {
        let mut patch = self.frame.read(rect)?;
        // A gain of exactly 1 changes nothing, and is the common case (brightness correction off, or the master).
        #[allow(clippy::float_cmp)]
        if self.gain != 1.0 {
            let gain = self.gain;
            patch.image.par_rows_mut().for_each(|row| row.iter_mut().for_each(|v| *v *= gain));
        }
        Ok(patch)
    }
}

/// Reads `rect` from `frame` and checks that the patch has the rectangle's size and the frame's channels, and a
/// coverage of the same size with one channel.
pub(crate) fn read_checked(frame: &dyn FrameSource, rect: Rect) -> Result<Patch, StackError> {
    let patch = frame.read(rect)?;
    let c = frame.channels();
    let bad = |image: &Image<f32>, channels: usize| StackError::Patch {
        width: rect.width,
        height: rect.height,
        channels,
        got_width: image.width(),
        got_height: image.height(),
        got_channels: image.channels(),
    };
    if (patch.image.width(), patch.image.height(), patch.image.channels()) != (rect.width, rect.height, c) {
        return Err(bad(&patch.image, c));
    }
    if let Some(coverage) = &patch.coverage
        && (coverage.width(), coverage.height(), coverage.channels()) != (rect.width, rect.height, 1)
    {
        return Err(bad(coverage, 1));
    }
    Ok(patch)
}
