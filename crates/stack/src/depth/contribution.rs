//! Normalized contributions of the depth compositor, including feathered coverage, each pixel's blend width and the
//! nearest-frame fallback. Two streaming passes retain three scalar planes, never one plane per input frame.

use florastack_core::{ColorSpace, Image};

use super::composite::{check_blend, weight};
use crate::average::MIN_COVERAGE;
use crate::{Patch, StackError};

#[derive(Debug)]
pub struct Contributions {
    depth: Image<f32>,
    /// Each pixel's blend width, as the composite had it; 1 everywhere when `None`.
    blend: Option<Image<f32>>,
    total: Image<f32>,
    nearest: Image<f32>,
    distance: Image<f32>,
    frames: usize,
    added: usize,
}

impl Contributions {
    pub fn new(depth: Image<f32>, frames: usize) -> Result<Self, StackError> {
        if frames == 0 {
            return Err(StackError::NoFrames);
        }
        if frames > super::MAX_FRAMES {
            return Err(StackError::TooManyFrames { max: super::MAX_FRAMES });
        }
        if depth.channels() != 1 {
            return Err(StackError::Shape { width: depth.width(), height: depth.height(), channels: depth.channels() });
        }
        if depth.rows().flatten().any(|d| !d.is_finite() || *d < 0.0 || f64::from(*d) > frames.saturating_sub(1) as f64) {
            return Err(StackError::Shape { width: depth.width(), height: depth.height(), channels: depth.channels() });
        }
        let plane = || Image::new(depth.width(), depth.height(), 1, ColorSpace::LINEAR);
        let mut distance = plane()?;
        for row in distance.rows_mut() {
            row.fill(f32::INFINITY);
        }
        Ok(Self { total: plane()?, nearest: plane()?, distance, depth, blend: None, frames, added: 0 })
    }

    /// The contributions with each pixel's blend width (the depth map's `blend`, a plane of the depth's size), as the
    /// composite took them.
    pub fn with_blend(mut self, blend: Option<Image<f32>>) -> Result<Self, StackError> {
        if let Some(b) = &blend {
            check_blend(b, &self.depth)?;
        }
        self.blend = blend;
        Ok(self)
    }

    fn width(&self, x: usize, y: usize) -> f32 {
        self.blend.as_ref().and_then(|b| b.get(x, y, 0)).unwrap_or(1.0)
    }

    fn check(&self, patch: &Patch) -> Result<(), StackError> {
        let (w, h) = (self.depth.width(), self.depth.height());
        if (patch.image.width(), patch.image.height()) != (w, h)
            || patch.coverage.as_ref().is_some_and(|c| (c.width(), c.height(), c.channels()) != (w, h, 1))
        {
            return Err(StackError::Shape { width: patch.image.width(), height: patch.image.height(), channels: patch.image.channels() });
        }
        Ok(())
    }

    /// Pass 1: add each input in its compositing order.
    #[allow(clippy::cast_precision_loss)] // Same bounded frame indices as CompositeKernel.
    pub fn add_frame(&mut self, patch: &Patch) -> Result<(), StackError> {
        self.check(patch)?;
        if self.added >= self.frames {
            return Err(StackError::FrameCount { expected: self.frames, got: self.added + 1 });
        }
        let i = self.added as f32;
        let c = patch.image.channels();
        let blend = self.blend.as_ref();
        for (y, (((total, nearest), distance), depth)) in
            self.total.rows_mut().zip(self.nearest.rows_mut()).zip(self.distance.rows_mut()).zip(self.depth.rows()).enumerate()
        {
            let widths = blend.and_then(|b| b.row(y));
            let Some(row) = patch.image.row(y) else { continue };
            for (x, ((((t, n), dist), &d), pixel)) in total.iter_mut().zip(nearest).zip(distance).zip(depth).zip(row.chunks_exact(c)).enumerate() {
                let coverage = coverage(patch, x, y, pixel);
                if coverage == 0.0 {
                    continue;
                }
                let delta = (i - d).abs();
                *t += weight(delta, widths.and_then(|w| w.get(x)).copied().unwrap_or(1.0), coverage);
                if delta < *dist {
                    *dist = delta;
                    *n = i;
                }
            }
        }
        self.added += 1;
        Ok(())
    }

    /// Pass 2: one frame's fraction of the final pixel, in [0, 1]. Uncovered pixels contribute zero.
    #[allow(clippy::cast_precision_loss, clippy::float_cmp)] // Frame indices are small exact integers.
    pub fn map(&self, frame: usize, patch: &Patch) -> Result<Image<f32>, StackError> {
        self.check(patch)?;
        if self.added != self.frames || frame >= self.frames {
            return Err(StackError::FrameCount { expected: self.frames, got: self.added });
        }
        let mut out = Image::new(self.depth.width(), self.depth.height(), 1, ColorSpace::LINEAR)?;
        let i = frame as f32;
        let c = patch.image.channels();
        for (y, ((((out, depth), total), nearest), distance)) in
            out.rows_mut().zip(self.depth.rows()).zip(self.total.rows()).zip(self.nearest.rows()).zip(self.distance.rows()).enumerate()
        {
            let Some(row) = patch.image.row(y) else { continue };
            for (x, (((((dst, &d), &t), &n), &dist), pixel)) in
                out.iter_mut().zip(depth).zip(total).zip(nearest).zip(distance).zip(row.chunks_exact(c)).enumerate()
            {
                *dst = if t > 0.0 {
                    weight((i - d).abs(), self.width(x, y), coverage(patch, x, y, pixel)) / t
                } else if dist.is_finite() && i == n {
                    1.0
                } else {
                    0.0
                };
            }
        }
        Ok(out)
    }
}

fn coverage(patch: &Patch, x: usize, y: usize, pixel: &[f32]) -> f32 {
    let k = patch.coverage.as_ref().map_or(1.0, |c| c.get(x, y, 0).unwrap_or(0.0));
    if k.is_finite() && k >= MIN_COVERAGE && pixel.iter().all(|v| v.is_finite()) { k.min(1.0) } else { 0.0 }
}
