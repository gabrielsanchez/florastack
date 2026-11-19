//! Pass 2 of the depth-map method: each frame read again, and each pixel of the output taken from the frames at its
//! depth ([`CompositeKernel`], a [`TileKernel`] that [`super::DepthMap::compositor`] runs tile by tile). See the
//! module documentation of [`super`].
//!
//! Frame `i` weighs `max(0, 1 - |i - depth| / width)` at a pixel, times its coverage there, with the pixel's blend width
//! from the depth map (1 where its depth was measured, up to [`super::FILL_BLEND`] where it was filled): at a width of
//! 1, the two frames on either side of a depth between frames, in proportion to how close it lies to each, and the
//! frame itself at a whole depth; wider, the frames around it, tapering. Each output pixel is the weighted mean of those
//! frames' pixels, so its color and contrast are theirs. A tile keeps the
//! weighted sum and the sum of the weights, and, for wide framing, where neither frame covers the pixel, the pixel of
//! the covering frame nearest the depth and its distance: `8 C + 8` bytes a pixel (32 for RGB).

use std::sync::Arc;

use florastack_core::{ColorSpace, Image, Monitor, Rect, Tile};
use rayon::prelude::*;

use super::{DEPTH_MAP, MAX_FRAMES};
use crate::average::{MAX_SAMPLE, MIN_COVERAGE};
use crate::error::StackError;
use crate::method::StackShape;
use crate::source::Patch;
use crate::tiled::{Share, StatePlane, TileKernel, TileView};

/// Pass 2 as a [`TileKernel`]. See the module documentation.
#[derive(Clone, Debug)]
pub struct CompositeKernel {
    /// The depth of every pixel of the canvas, in frames.
    depth: Arc<Image<f32>>,
    /// The frames the depth indexes.
    frames: usize,
    /// The frames added to the stack in progress.
    added: usize,
    /// Each pixel's blend width, in frames (the depth map's `blend`); 1 everywhere when `None`.
    blend: Option<Arc<Image<f32>>>,
}

impl CompositeKernel {
    /// A kernel that composites `frames` frames from `depth` (one channel, the canvas's size).
    pub fn new(depth: Arc<Image<f32>>, frames: usize) -> Result<CompositeKernel, StackError> {
        if frames == 0 {
            return Err(StackError::NoFrames);
        }
        if frames > MAX_FRAMES {
            return Err(StackError::TooManyFrames { max: MAX_FRAMES });
        }
        if depth.channels() != 1 {
            return Err(StackError::Plane {
                what: "depth map",
                width: depth.width(),
                height: depth.height(),
                channels: depth.channels(),
                expected_width: depth.width(),
                expected_height: depth.height(),
            });
        }
        Ok(CompositeKernel { depth, frames, added: 0, blend: None })
    }

    /// The kernel with each pixel's blend width, a plane of the depth's size (`None`: 1 everywhere).
    pub fn with_blend(mut self, blend: Option<Arc<Image<f32>>>) -> Result<CompositeKernel, StackError> {
        if let Some(b) = &blend {
            check_blend(b, &self.depth)?;
        }
        self.blend = blend;
        Ok(self)
    }

    pub fn depth(&self) -> &Image<f32> {
        &self.depth
    }

    pub fn blend(&self) -> Option<&Image<f32>> {
        self.blend.as_deref()
    }
}

/// An error unless `blend` is a plane of one channel of `depth`'s size.
pub(crate) fn check_blend(blend: &Image<f32>, depth: &Image<f32>) -> Result<(), StackError> {
    if (blend.width(), blend.height(), blend.channels()) == (depth.width(), depth.height(), 1) {
        return Ok(());
    }
    Err(StackError::Plane {
        what: "blend width",
        width: blend.width(),
        height: blend.height(),
        channels: blend.channels(),
        expected_width: depth.width(),
        expected_height: depth.height(),
    })
}

/// The weight of a frame `distance` frames from a pixel's depth, with the pixel's blend `width` and coverage `k`. A
/// width of 1 (or less, or not a number) divides nothing, so that it is `max(0, 1 - distance)` exactly.
#[inline]
pub(crate) fn weight(distance: f32, width: f32, k: f32) -> f32 {
    let reach = if width > 1.0 { distance / width } else { distance };
    (1.0 - reach).max(0.0) * k.min(1.0)
}

/// A tile's four planes: the weighted sum of the frames' pixels (`C` channels), the sum of the weights, the nearest
/// covering frame's pixel (`C`) and its distance from the depth (infinite where no frame covers).
fn planes(state: &[StatePlane]) -> Result<[&Image<f32>; 4], StackError> {
    match state {
        [StatePlane::F32(s), StatePlane::F32(w), StatePlane::F32(n), StatePlane::F32(d)] => Ok([s, w, n, d]),
        _ => Err(StackError::Shape { width: 0, height: 0, channels: state.len() }),
    }
}

impl TileKernel for CompositeKernel {
    fn name(&self) -> &'static str {
        DEPTH_MAP
    }

    fn context(&self) -> usize {
        // Every pixel is its own: no filter reaches beyond it.
        0
    }

    fn state(&self, inner: Rect, channels: usize) -> Result<Vec<StatePlane>, StackError> {
        let (w, h) = (inner.width, inner.height);
        let mut distance = Image::new(w, h, 1, ColorSpace::LINEAR)?;
        distance.par_rows_mut().for_each(|row| row.fill(f32::INFINITY));
        Ok(vec![
            StatePlane::F32(Image::new(w, h, channels, ColorSpace::LINEAR)?),
            StatePlane::F32(Image::new(w, h, 1, ColorSpace::LINEAR)?),
            StatePlane::F32(Image::new(w, h, channels, ColorSpace::LINEAR)?),
            StatePlane::F32(distance),
        ])
    }

    fn fold(&self, state: &mut [StatePlane], patch: &Patch, view: &TileView) -> Result<Option<Share>, StackError> {
        if view.frame >= self.frames {
            return Err(StackError::FrameCount { expected: self.frames, got: view.frame + 1 });
        }
        let [StatePlane::F32(sum), StatePlane::F32(total), StatePlane::F32(nearest), StatePlane::F32(nearest_distance)] = state else {
            return Err(StackError::Shape { width: 0, height: 0, channels: state.len() });
        };
        let c = patch.image.channels();
        let inner = view.inner_in_patch();
        #[allow(clippy::cast_precision_loss)] // Below MAX_FRAMES, exact.
        let i = view.frame as f32;
        let coverage = patch.coverage.as_ref();
        let depth = &self.depth;
        let rows = sum.par_rows_mut().zip(total.par_rows_mut()).zip(nearest.par_rows_mut()).zip(nearest_distance.par_rows_mut());
        rows.enumerate().for_each(|(j, (((sum, total), nearest), nearest_distance))| {
            let (Some(pixels), Some(depth)) = (patch.image.row(inner.y + j), depth.row(view.inner.y + j)) else { return };
            let cov = coverage.and_then(|k| k.row(inner.y + j));
            let pixels = pixels.get(inner.x * c..(inner.x + inner.width) * c).unwrap_or(&[]);
            let depth = depth.get(view.inner.x..view.inner.x + view.inner.width).unwrap_or(&[]);
            let blend = self.blend.as_ref().and_then(|b| b.row(view.inner.y + j)).and_then(|r| r.get(view.inner.x..view.inner.x + view.inner.width));
            let samples = sum.chunks_exact_mut(c).zip(total.iter_mut()).zip(nearest.chunks_exact_mut(c)).zip(nearest_distance.iter_mut());
            for (x, ((((sum, total), nearest), nearest_distance), (pixel, &d))) in samples.zip(pixels.chunks_exact(c).zip(depth)).enumerate() {
                let k = cov.map_or(1.0, |r| r.get(inner.x + x).copied().unwrap_or(0.0));
                if !(k.is_finite() && k >= MIN_COVERAGE && pixel.iter().all(|v| v.is_finite())) {
                    continue;
                }
                let distance = (i - d).abs();
                let weight = weight(distance, blend.and_then(|b| b.get(x)).copied().unwrap_or(1.0), k);
                if weight > 0.0 {
                    for (s, &v) in sum.iter_mut().zip(pixel) {
                        *s += weight * v.clamp(-MAX_SAMPLE, MAX_SAMPLE);
                    }
                    *total += weight;
                }
                if distance < *nearest_distance {
                    for (o, &v) in nearest.iter_mut().zip(pixel) {
                        *o = v.clamp(-MAX_SAMPLE, MAX_SAMPLE);
                    }
                    *nearest_distance = distance;
                }
            }
        });
        Ok(None)
    }

    fn start(&mut self, shape: StackShape) -> Result<(), StackError> {
        if (shape.width, shape.height) != (self.depth.width(), self.depth.height()) {
            return Err(StackError::Plane {
                what: "depth map",
                width: self.depth.width(),
                height: self.depth.height(),
                channels: 1,
                expected_width: shape.width,
                expected_height: shape.height,
            });
        }
        self.added = 0;
        Ok(())
    }

    fn absorb(&mut self, _shares: Vec<(Tile, Share)>) -> Result<(), StackError> {
        self.added += 1;
        Ok(())
    }

    fn prepare(&mut self, _monitor: &dyn Monitor) -> Result<(), StackError> {
        if self.added != self.frames {
            return Err(StackError::FrameCount { expected: self.frames, got: self.added });
        }
        Ok(())
    }

    fn covered(&self, state: &[StatePlane]) -> Result<Image<f32>, StackError> {
        let [_, _, _, distance] = planes(state)?;
        let mut covered = distance.clone();
        covered.par_rows_mut().for_each(|row| row.iter_mut().for_each(|d| *d = if d.is_finite() { 1.0 } else { 0.0 }));
        Ok(covered)
    }

    fn result(&self, state: &[StatePlane], _inner: Rect, channels: usize) -> Result<Image<f32>, StackError> {
        let [sum, total, nearest, distance] = planes(state)?;
        let c = channels.max(1);
        let mut out = sum.clone();
        let rows = out.par_rows_mut().zip(total.par_rows()).zip(nearest.par_rows()).zip(distance.par_rows());
        rows.for_each(|(((out, total), nearest), distance)| {
            for (((o, &t), n), &d) in out.chunks_exact_mut(c).zip(total).zip(nearest.chunks_exact(c)).zip(distance) {
                if t > 0.0 {
                    let inverse = 1.0 / t;
                    for v in o.iter_mut() {
                        *v *= inverse;
                    }
                } else if d.is_finite() {
                    o.copy_from_slice(n);
                } else {
                    o.fill(0.0);
                }
            }
        });
        Ok(out)
    }
}
