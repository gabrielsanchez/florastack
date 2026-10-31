//! The weighted average on the GPU: florastack-stack's `WeightedAverage` (`average.rs`), whose module documentation
//! describes the weights. Per frame and tile: luminance and the coverage that counts (`luma`), the Laplacian's
//! energy where its taps are covered (`energy`), the blur over `radius`, the power (`sharpen`), the blur over
//! `smoothing`, and the weighted samples and weights added to the tile's two accumulators (`accumulate`); the result
//! divides them (`normalize`).

use florastack_core::ops::{AUTO_EXACT_MAX_SIGMA, BlurMethod};

use super::{Patch, Shape, TileMethod, View};
use crate::batch::{Batch, Bind, Groups, Params};
use crate::device::Gpu;
use crate::error::GpuError;
use crate::ops::{Blur, to_f32};
use crate::plane::Plane;
use crate::shaders::Kernel;

/// The largest `radius` and `smoothing` (florastack-stack's `MAX_RADIUS`), and the range of `power`.
const MAX_RADIUS: f64 = 64.0;
const POWER_RANGE: (f64, f64) = (0.5, 4.0);

/// How far a blur of `sigma` reaches by `BlurMethod::Auto` (florastack-stack's `average::reach`).
pub(crate) fn reach(sigma: f64) -> usize {
    let r = if sigma <= 0.0 {
        0.0
    } else if sigma <= AUTO_EXACT_MAX_SIGMA {
        (3.0 * sigma).ceil()
    } else {
        (3.5 * sigma).ceil() + 4.0
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // At most 3.5 * 256 + 4.
    let r = r as usize;
    r
}

pub(crate) struct Average {
    radius: Blur,
    smoothing: Blur,
    power: f32,
    context: usize,
}

impl Average {
    pub(crate) fn new(gpu: &Gpu, radius: f64, smoothing: f64, power: f64) -> Result<Average, GpuError> {
        for (what, v) in [("the weighted average's radius", radius), ("the weighted average's smoothing", smoothing)] {
            if !(v.is_finite() && (0.0..=MAX_RADIUS).contains(&v)) {
                return Err(GpuError::shape(what, v, "from 0 to 64 pixels"));
            }
        }
        if !(power.is_finite() && (POWER_RANGE.0..=POWER_RANGE.1).contains(&power)) {
            return Err(GpuError::shape("the weighted average's power", power, "from 0.5 to 4"));
        }
        Ok(Average {
            radius: Blur::new(gpu, radius, BlurMethod::Auto)?,
            smoothing: Blur::new(gpu, smoothing, BlurMethod::Auto)?,
            power: to_f32(power),
            context: 1 + reach(radius) + reach(smoothing),
        })
    }
}

impl TileMethod for Average {
    fn context(&self) -> usize {
        self.context
    }

    fn state(&self, batch: &mut Batch<'_>, view: &View, shape: Shape) -> Result<Vec<Plane>, GpuError> {
        let gpu = batch.gpu();
        let (w, h) = (view.inner.width, view.inner.height);
        Ok(vec![gpu.plane("the weighted sum", w, h, shape.channels)?, gpu.plane("the sum of weights", w, h, 1)?])
    }

    fn fold(&self, batch: &mut Batch<'_>, state: &[Plane], patch: &Patch, view: &View, shape: Shape) -> Result<(), GpuError> {
        let [sum, total] = state else { return Err(GpuError::shape("the weighted average's state", state.len(), "2 planes")) };
        let gpu = batch.gpu();
        let (w, h) = (view.region.width, view.region.height);
        let n = w * h;
        let luma = gpu.plane("luminance", w, h, 1)?;
        let coverage = gpu.plane("coverage", w, h, 1)?;
        let given = patch.coverage.as_ref().map_or(Bind::Buffer(gpu.dummy()), Bind::Plane);
        let params = Params::new().u(n).u(shape.channels).flag(patch.coverage.is_some()).u(0);
        batch.run(Kernel::Luma, &params, &[(&patch.image).into(), given, (&luma).into(), (&coverage).into()], Groups::Flat(n))?;
        let weights = gpu.plane("weights", w, h, 1)?;
        let params = Params::new().u(w).u(h).u(0);
        batch.run(Kernel::Energy, &params, &[(&luma).into(), (&coverage).into(), (&weights).into()], Groups::Plane { width: w, height: h })?;
        // The luminance is no longer needed: it is the blurs' scratch plane.
        self.radius.record(batch, &weights, &luma)?;
        let params = Params::new().u(n).f(self.power);
        batch.run(Kernel::Sharpen, &params, &[(&weights).into()], Groups::Flat(n))?;
        self.smoothing.record(batch, &weights, &luma)?;
        let (ox, oy) = view.offset();
        let (iw, ih) = (view.inner.width, view.inner.height);
        let params = Params::new().u(iw).u(ih).u(shape.channels).u(w).u(ox).u(oy);
        let binds = [(&patch.image).into(), (&coverage).into(), (&weights).into(), sum.into(), total.into()];
        batch.run(Kernel::Accumulate, &params, &binds, Groups::Plane { width: iw, height: ih })
    }

    fn result(&self, batch: &mut Batch<'_>, state: &[Plane], view: &View, shape: Shape) -> Result<(Plane, Plane), GpuError> {
        let [sum, total] = state else { return Err(GpuError::shape("the weighted average's state", state.len(), "2 planes")) };
        let gpu = batch.gpu();
        let (w, h) = (view.inner.width, view.inner.height);
        let out = gpu.plane("the stacked tile", w, h, shape.channels)?;
        let covered = gpu.plane("the tile's coverage", w, h, 1)?;
        let params = Params::new().u(w * h).u(shape.channels);
        batch.run(Kernel::Normalize, &params, &[sum.into(), total.into(), (&out).into(), (&covered).into()], Groups::Flat(w * h))?;
        Ok((out, covered))
    }
}
