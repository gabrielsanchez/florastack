//! The preview mode: any method run on frames reduced to a quarter of their size, so that a result shows in seconds
//! while the full stack runs, and so that it can be made again on every change of a setting.
//!
//! [`Preview`] wraps a stacker made with the method's [`Method::reduced`] settings (every length in pixels divided by
//! [`PREVIEW_FACTOR`], so that the preview looks like the full stack, only smaller: [`preview`] makes one). It reduces
//! each frame it is given, with its coverage, before the inner stacker sees it: a sixteenth of the pixels to weigh and
//! accumulate. A caller that can make the reduced frames more cheaply, such as the engine, which reduces each frame
//! once as it is decoded and warps the small copy onto the reduced canvas, feeds the reduced stacker directly with a
//! canvas of [`preview_size`].
//!
//! Frames are reduced by Lanczos3 resampling ([`florastack_core::ops::resize`]), which averages away the detail the
//! smaller frame can't hold. A frame's coverage is reduced with it, its pixels weighted by their coverage first and
//! divided by the reduced coverage after, so that the zeros beyond its edge don't darken its reduced edge pixels.

use florastack_core::ops::resize;
use florastack_core::{Image, Monitor, Rect};
use rayon::prelude::*;

use crate::average::MIN_COVERAGE;
use crate::error::StackError;
use crate::method::{Method, StackShape, Stacked, Stacker};
use crate::source::{CoveredFrame, FrameSource, Patch, read_checked};
use crate::tiled::Tiling;

/// How much smaller a preview is on each side.
pub const PREVIEW_FACTOR: usize = 4;

/// The size of the preview of a `width` x `height` canvas: a quarter on each side, rounded up.
pub fn preview_size(width: usize, height: usize) -> (usize, usize) {
    (width.div_ceil(PREVIEW_FACTOR).max(1), height.div_ceil(PREVIEW_FACTOR).max(1))
}

/// A preview stacker for `method`: its settings reduced for quarter-size frames, which [`Preview`] makes.
pub fn preview<M: Method>(method: &M, tiling: &Tiling) -> Result<Preview<Box<dyn Stacker>>, StackError> {
    Ok(Preview::new(method.reduced(PREVIEW_FACTOR as f64).stacker(tiling)?))
}

/// A stacker that reduces each frame to a quarter of its size before handing it to the stacker it wraps, which must
/// have settings for reduced frames. See the module documentation.
pub struct Preview<S> {
    inner: S,
    shape: Option<StackShape>,
}

impl<S: Stacker> Preview<S> {
    pub fn new(inner: S) -> Preview<S> {
        Preview { inner, shape: None }
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }
}

/// `frame` reduced to [`preview_size`], every channel.
pub fn reduce(frame: &Image<f32>) -> Result<Image<f32>, StackError> {
    let (w, h) = preview_size(frame.width(), frame.height());
    Ok(resize(frame, w, h)?)
}

/// A patch of a whole frame reduced to [`preview_size`], with its coverage when it has one (see the module
/// documentation).
pub fn reduce_patch(patch: &Patch) -> Result<Patch, StackError> {
    let Some(coverage) = &patch.coverage else {
        return Ok(Patch { image: reduce(&patch.image)?, coverage: None });
    };
    let c = patch.image.channels();
    let mut weighted = patch.image.clone();
    weighted.par_rows_mut().zip(coverage.par_rows()).for_each(|(row, cov)| {
        for (pixel, &k) in row.chunks_exact_mut(c).zip(cov) {
            let k = if k.is_finite() { k.clamp(0.0, 1.0) } else { 0.0 };
            for v in pixel.iter_mut() {
                *v *= k;
            }
        }
    });
    let mut image = reduce(&weighted)?;
    drop(weighted);
    let mut reduced = reduce(coverage)?;
    image.par_rows_mut().zip(reduced.par_rows_mut()).for_each(|(row, cov)| {
        for (pixel, k) in row.chunks_exact_mut(c).zip(cov.iter_mut()) {
            // Lanczos rings a little beside a sharp edge of the coverage: clamped.
            *k = k.clamp(0.0, 1.0);
            if *k >= MIN_COVERAGE {
                let inverse = 1.0 / *k;
                for v in pixel.iter_mut() {
                    *v *= inverse;
                }
            } else {
                *k = 0.0;
                pixel.fill(0.0);
            }
        }
    });
    Ok(Patch { image, coverage: Some(reduced) })
}

impl<S: Stacker> Stacker for Preview<S> {
    fn method(&self) -> &'static str {
        self.inner.method()
    }

    fn begin(&mut self, shape: StackShape) -> Result<(), StackError> {
        self.shape = None;
        shape.check()?;
        let (width, height) = preview_size(shape.width, shape.height);
        self.inner.begin(StackShape { width, height, channels: shape.channels })?;
        self.shape = Some(shape);
        Ok(())
    }

    fn add_frame(&mut self, frame: &dyn FrameSource, monitor: &dyn Monitor) -> Result<(), StackError> {
        let shape = self.shape.ok_or(StackError::NotStarted)?;
        shape.check_frame(frame)?;
        monitor.checkpoint()?;
        let whole = read_checked(frame, Rect::new(0, 0, shape.width, shape.height))?;
        let reduced = reduce_patch(&whole)?;
        drop(whole);
        match reduced.coverage {
            Some(coverage) => self.inner.add_frame(&CoveredFrame::new(reduced.image, coverage)?, monitor),
            None => self.inner.add_frame(&reduced.image, monitor),
        }
    }

    fn finish(&mut self, monitor: &dyn Monitor) -> Result<Stacked, StackError> {
        self.shape = None;
        self.inner.finish(monitor)
    }
}
