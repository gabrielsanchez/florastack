//! The depth map's two passes on the GPU: its focus statistics ([`Focus`], florastack-stack's `FocusKernel` in
//! `depth/focus.rs`) and its composite ([`Composite`], `CompositeKernel` in `depth/composite.rs`), whose module
//! documentation describes them. The depth map between them (`FocusStats::resolve`: the edge-aware smoothing and the
//! fill) stays on the CPU: it reads no frame, and runs once per threshold.
//!
//! Pass 1, per frame and tile: luminance and coverage (`luma`), the luminance blurred over the radius by normalized
//! convolution (`blur`, `focus_normalize`), its modified Laplacian squared where its taps are covered (`energy`),
//! blurred over the radius and marked -1 where uncovered (`mark`), and folded into each pixel's running record
//! (`record`); the result turns records into the statistics (`stats`). Pass 2 folds each frame into the composite's
//! sums by its distance from each pixel's depth (`composite`) and divides them (`composite_result`).

use std::sync::Arc;

use florastack_core::ops::BlurMethod;
use florastack_core::{Image, Rect};

use super::average::reach;
use super::{Patch, Shape, TileMethod, View};
use crate::batch::{Batch, Bind, Groups, Params};
use crate::device::Gpu;
use crate::error::GpuError;
use crate::ops::{Blur, fill, fill_pattern};
use crate::plane::Plane;
use crate::shaders::Kernel;

/// The most frames a depth map can index (florastack-stack's `MAX_FRAMES`): frame indices are `f32`.
const MAX_FRAMES: usize = 1 << 24;
/// A record with no frame yet: [best, peak, before, after, last, runner, floor, guide], the floor at BIG (the WGSL
/// kernels' finite infinity).
const EMPTY: [f32; 8] = [-1.0, -1.0, -1.0, -1.0, -1.0, -1.0, 3.0e38, 0.0];

/// A frame's place in the stack as the kernels take it.
#[allow(clippy::cast_precision_loss)] // Below MAX_FRAMES, exact.
fn index(frame: usize) -> f32 {
    frame as f32
}

pub(crate) struct Focus {
    blur: Blur,
    context: usize,
}

impl Focus {
    pub(crate) fn new(gpu: &Gpu, radius: f64) -> Result<Focus, GpuError> {
        if !(radius.is_finite() && (0.0..=64.0).contains(&radius)) {
            return Err(GpuError::shape("the depth map's radius", radius, "from 0 to 64 pixels"));
        }
        Ok(Focus { blur: Blur::new(gpu, radius, BlurMethod::Auto)?, context: 2 * reach(radius) + 1 })
    }
}

impl TileMethod for Focus {
    fn context(&self) -> usize {
        self.context
    }

    fn state(&self, batch: &mut Batch<'_>, view: &View, _shape: Shape) -> Result<Vec<Plane>, GpuError> {
        let record = batch.gpu().plane("the focus records", view.inner.width, view.inner.height, 8)?;
        fill_pattern(batch, &record, &EMPTY)?;
        Ok(vec![record])
    }

    fn fold(&self, batch: &mut Batch<'_>, state: &[Plane], patch: &Patch, view: &View, shape: Shape) -> Result<(), GpuError> {
        let [record] = state else { return Err(GpuError::shape("the focus state", state.len(), "1 plane")) };
        if view.frame >= MAX_FRAMES {
            return Err(GpuError::shape("the frames of a depth map", view.frame + 1, format!("at most {MAX_FRAMES}")));
        }
        let gpu = batch.gpu();
        let (w, h) = (view.region.width, view.region.height);
        let n = w * h;
        let luma = gpu.plane("luminance", w, h, 1)?;
        let covered = gpu.plane("coverage", w, h, 1)?;
        let given = patch.coverage.as_ref().map_or(Bind::Buffer(gpu.dummy()), Bind::Plane);
        let params = Params::new().u(n).u(shape.channels).flag(patch.coverage.is_some()).u(1);
        batch.run(Kernel::Luma, &params, &[(&patch.image).into(), given, (&luma).into(), (&covered).into()], Groups::Flat(n))?;
        // The blurred luminance by normalized convolution, the blurred coverage dividing it.
        let scratch = gpu.plane("blur scratch", w, h, 1)?;
        let smooth = gpu.plane("smoothed luminance", w, h, 1)?;
        batch.copy(&luma, &smooth)?;
        self.blur.record(batch, &smooth, &scratch)?;
        let weight = gpu.plane("smoothed coverage", w, h, 1)?;
        batch.copy(&covered, &weight)?;
        self.blur.record(batch, &weight, &scratch)?;
        batch.run(Kernel::FocusNormalize, &Params::new().u(n), &[(&smooth).into(), (&weight).into()], Groups::Flat(n))?;
        let focus = gpu.plane("focus", w, h, 1)?;
        let params = Params::new().u(w).u(h).u(1);
        batch.run(Kernel::Energy, &params, &[(&smooth).into(), (&covered).into(), (&focus).into()], Groups::Plane { width: w, height: h })?;
        self.blur.record(batch, &focus, &scratch)?;
        batch.run(Kernel::Mark, &Params::new().u(n), &[(&focus).into(), (&covered).into()], Groups::Flat(n))?;
        let (ox, oy) = view.offset();
        let (iw, ih) = (view.inner.width, view.inner.height);
        let params = Params::new().u(iw).u(ih).u(w).u(ox).u(oy).f(index(view.frame));
        batch.run(Kernel::Record, &params, &[(&focus).into(), (&luma).into(), record.into()], Groups::Plane { width: iw, height: ih })
    }

    fn result(&self, batch: &mut Batch<'_>, state: &[Plane], view: &View, _shape: Shape) -> Result<(Plane, Plane), GpuError> {
        let [record] = state else { return Err(GpuError::shape("the focus state", state.len(), "1 plane")) };
        let gpu = batch.gpu();
        let (w, h) = (view.inner.width, view.inner.height);
        let statistics = gpu.plane("focus statistics", w, h, 3)?;
        let covered = gpu.plane("the tile's coverage", w, h, 1)?;
        batch.run(Kernel::Stats, &Params::new().u(w * h), &[record.into(), (&statistics).into(), (&covered).into()], Groups::Flat(w * h))?;
        Ok((statistics, covered))
    }
}

pub(crate) struct Composite {
    depth: Arc<Image<f32>>,
    frames: usize,
    added: usize,
}

impl Composite {
    pub(crate) fn new(depth: Arc<Image<f32>>, frames: usize) -> Result<Composite, GpuError> {
        if frames == 0 || frames > MAX_FRAMES {
            return Err(GpuError::shape("the frames of a depth map", frames, format!("from 1 to {MAX_FRAMES}")));
        }
        if depth.channels() != 1 {
            return Err(GpuError::shape("a depth map", format!("{} channels", depth.channels()), "1 channel"));
        }
        Ok(Composite { depth, frames, added: 0 })
    }
}

impl TileMethod for Composite {
    fn context(&self) -> usize {
        0
    }

    fn start(&mut self, _gpu: &Gpu, shape: Shape) -> Result<(), GpuError> {
        if (shape.width, shape.height) != (self.depth.width(), self.depth.height()) {
            return Err(GpuError::shape(
                "a depth map",
                format!("{}x{}", self.depth.width(), self.depth.height()),
                format!("the canvas's {}x{}", shape.width, shape.height),
            ));
        }
        self.added = 0;
        Ok(())
    }

    fn state(&self, batch: &mut Batch<'_>, view: &View, shape: Shape) -> Result<Vec<Plane>, GpuError> {
        let gpu = batch.gpu();
        let (w, h, c) = (view.inner.width, view.inner.height, shape.channels);
        let distance = gpu.plane("the nearest frame's distance", w, h, 1)?;
        fill(batch, &distance, 3.0e38)?;
        let depth = gpu.upload_rect(&self.depth, Rect::new(view.inner.x, view.inner.y, w, h))?;
        Ok(vec![
            gpu.plane("the weighted sum", w, h, c)?,
            gpu.plane("the sum of weights", w, h, 1)?,
            gpu.plane("the nearest frame's pixels", w, h, c)?,
            distance,
            depth,
        ])
    }

    fn fold(&self, batch: &mut Batch<'_>, state: &[Plane], patch: &Patch, view: &View, shape: Shape) -> Result<(), GpuError> {
        let [sum, total, nearest, distance, depth] = state else { return Err(GpuError::shape("the composite's state", state.len(), "5 planes")) };
        if view.frame >= self.frames {
            return Err(GpuError::shape("the frames composited", view.frame + 1, format!("the depth map's {}", self.frames)));
        }
        let gpu = batch.gpu();
        let given = patch.coverage.as_ref().map_or(Bind::Buffer(gpu.dummy()), Bind::Plane);
        let (ox, oy) = view.offset();
        let (iw, ih) = (view.inner.width, view.inner.height);
        let params = Params::new().u(iw).u(ih).u(shape.channels).u(view.region.width).u(ox).u(oy).flag(patch.coverage.is_some()).f(index(view.frame));
        let binds = [(&patch.image).into(), given, depth.into(), sum.into(), total.into(), nearest.into(), distance.into()];
        batch.run(Kernel::Composite, &params, &binds, Groups::Plane { width: iw, height: ih })
    }

    fn end_frame(&mut self, _batch: &mut Batch<'_>) -> Result<(), GpuError> {
        self.added += 1;
        Ok(())
    }

    fn prepare(&mut self, _batch: &mut Batch<'_>) -> Result<(), GpuError> {
        if self.added != self.frames {
            return Err(GpuError::shape("the frames composited", self.added, format!("the depth map's {}", self.frames)));
        }
        Ok(())
    }

    fn result(&self, batch: &mut Batch<'_>, state: &[Plane], view: &View, shape: Shape) -> Result<(Plane, Plane), GpuError> {
        let [sum, total, nearest, distance, _] = state else { return Err(GpuError::shape("the composite's state", state.len(), "5 planes")) };
        let gpu = batch.gpu();
        let (w, h) = (view.inner.width, view.inner.height);
        let out = gpu.plane("the composited tile", w, h, shape.channels)?;
        let covered = gpu.plane("the tile's coverage", w, h, 1)?;
        let params = Params::new().u(w * h).u(shape.channels);
        let binds = [sum.into(), total.into(), nearest.into(), distance.into(), (&out).into(), (&covered).into()];
        batch.run(Kernel::CompositeResult, &params, &binds, Groups::Flat(w * h))?;
        Ok((out, covered))
    }
}
