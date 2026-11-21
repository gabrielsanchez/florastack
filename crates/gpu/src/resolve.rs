//! The depth map's resolve step on the GPU, between the depth-map method's passes: the whole-canvas filtering of
//! florastack-stack's `depth::fill` (its CPU reference, with `depth::filter` for the filters), which the engine hands to
//! `FocusStats::resolve_with`. The choice of the confident pixels and the confidence map stay on the CPU, which holds
//! the statistics; this takes the depth, the confident pixels' weights and the guide, and gives the depth smoothed,
//! filled and held within the frames, and each pixel's blend width.
//!
//! - **The domain** (`domain`): the distances between neighbors along rows and columns, `1 + ratio |guide difference|`,
//!   on the guide's cube root.
//! - **The recursive filter** (`recursive`): each of its three passes forth and back along every row, then every
//!   column, one invocation per line, as the CPU's threads do: a line is a sequential recursion.
//! - **The support** (`support`): the confident pixels whose smoothed neighborhood is mostly not confident are left
//!   out, and the rest counted.
//! - **The normalized smoothing** (`smooth_pack`, the filter, then `fill_base` or `smooth_unpack`), **push-pull**
//!   (`pp_reduce` down to one pixel, `pp_pull` back up) and **the end** (`fill_finish`: the depth clamped, the blend
//!   width and whether any pixel blends wider than 1).
//!
//! Every plane is the whole canvas: a canvas whose planes exceed the device's binding limit is an error, and the
//! engine resolves on the CPU then.

use florastack_core::{ColorSpace, Image};

use crate::batch::{Batch, Bind, Groups, Params};
use crate::device::Gpu;
use crate::error::GpuError;
use crate::plane::Plane;
use crate::shaders::Kernel;

/// The recursive filter's passes along each axis (florastack-stack's `PASSES`).
const PASSES: i32 = 3;

/// What the filling takes besides its planes: the smoothing radius and the resolve's constants, which
/// florastack-stack's `depth` module defines (`EDGE`, `MIN_SUPPORT`, `REACHED`, `FILL_BLEND`).
#[derive(Clone, Copy, Debug)]
pub struct FillSettings {
    /// The smoothing radius, in pixels.
    pub smoothing: f64,
    /// The guide's step that stops the smoothing.
    pub edge: f64,
    /// The confident share of its neighborhood a confident pixel needs.
    pub min_support: f32,
    /// The share of its neighborhood that keeps a pixel's smoothed depth whole.
    pub reached: f32,
    /// The widest blend, where the depth was filled.
    pub fill_blend: f32,
    /// The frames the depth indexes.
    pub frames: usize,
}

/// The planes the filling takes, each of the canvas's size and one channel.
pub struct FillPlanes<'a> {
    /// Each pixel's measured depth, in frames.
    pub depth: &'a Image<f32>,
    /// 1 where the depth is confident, 0 elsewhere.
    pub weights: &'a Image<f32>,
    /// How many of `weights` are 1.
    pub confident: usize,
    /// The sharpest frames' luminance (its cube root is taken here).
    pub guide: &'a Image<f32>,
    /// Where some frame covers (above 0); `None` when every pixel is covered.
    pub covered: Option<&'a Image<f32>>,
}

/// What the filling gives.
pub struct Filled {
    /// The depth, smoothed, filled and held within the frames.
    pub depth: Image<f32>,
    /// Each pixel's blend width; `None` when it is 1 everywhere.
    pub blend: Option<Image<f32>>,
    /// How many pixels stayed confident after the support test.
    pub confident: usize,
}

/// The recursive filter's feedback for each pass, from the first (florastack-stack's `Domain::feedback`).
pub fn feedback(sigma_s: f64) -> Vec<f32> {
    if sigma_s <= 0.0 {
        return Vec::new();
    }
    let n = f64::from(PASSES);
    (0..PASSES)
        .map(|pass| {
            // Gastal and Oliveira's equation 14: the passes' deviations halve, their variances summing to sigma_s^2.
            let sigma = sigma_s * 3.0_f64.sqrt() * 2.0_f64.powi(PASSES - pass - 1) / (4.0_f64.powf(n) - 1.0).sqrt();
            #[allow(clippy::cast_possible_truncation)] // Between 0 and 1.
            let a = (-(2.0_f64.sqrt()) / sigma).exp() as f32;
            a
        })
        .collect()
}

/// The domain of a guide on the GPU: its distances along rows and columns, and the passes' feedback.
struct Domain {
    across: Plane,
    down: Plane,
    feedback: Vec<f32>,
}

impl Domain {
    /// The domain of `guide` (its cube root with `cube_root`), or `None` when `sigma_s` is 0: no smoothing.
    fn new(batch: &mut Batch<'_>, guide: &Plane, sigma_s: f64, sigma_r: f64, cube_root: bool) -> Result<Option<Domain>, GpuError> {
        if sigma_s <= 0.0 {
            return Ok(None);
        }
        let (w, h) = (guide.width(), guide.height());
        let gpu = batch.gpu();
        let across = gpu.plane("the distances along rows", w, h, 1)?;
        let down = gpu.plane("the distances along columns", w, h, 1)?;
        #[allow(clippy::cast_possible_truncation)] // A ratio of lengths, finite and positive: as the CPU's.
        let ratio = (sigma_s / sigma_r.max(1e-6)) as f32;
        let params = Params::new().u(w).u(h).f(ratio).flag(cube_root);
        batch.run(Kernel::Domain, &params, &[guide.into(), (&across).into(), (&down).into()], Groups::Plane { width: w, height: h })?;
        Ok(Some(Domain { across, down, feedback: feedback(sigma_s) }))
    }

    /// Records the recursive filter's passes over `f`, a plane of one or two channels of the guide's size, in place.
    fn filter(&self, batch: &mut Batch<'_>, f: &Plane) -> Result<(), GpuError> {
        let (w, h) = (f.width(), f.height());
        for &a in &self.feedback {
            let ln_a = a.ln();
            for (axis, distances, lines) in [(0, &self.across, h), (1, &self.down, w)] {
                let params = Params::new().u(w).u(h).u(f.channels()).u(axis).f(ln_a);
                batch.run(Kernel::Recursive, &params, &[distances.into(), f.into()], Groups::Flat(lines))?;
            }
        }
        Ok(())
    }
}

/// Records `values` weighted by `weights`, interleaved, the normalized convolution's input.
fn packed(batch: &mut Batch<'_>, values: &Plane, weights: &Plane) -> Result<Plane, GpuError> {
    let (w, h) = (values.width(), values.height());
    let out = batch.gpu().plane("the weighted values and weights", w, h, 2)?;
    let n = w * h;
    batch.run(Kernel::SmoothPack, &Params::new().u(n), &[values.into(), weights.into(), (&out).into()], Groups::Flat(n))?;
    Ok(out)
}

/// Records push-pull from its finest level, `base` (premultiplied values and weights from 0 to 1, interleaved): the
/// values filled from coarser scales.
fn push_pull_planes(batch: &mut Batch<'_>, base: &Plane) -> Result<Plane, GpuError> {
    let gpu = batch.gpu();
    let mut coarser: Vec<Plane> = Vec::new();
    loop {
        let last = coarser.last().unwrap_or(base);
        if last.width() <= 1 && last.height() <= 1 {
            break;
        }
        let (w, h) = (last.width().div_ceil(2), last.height().div_ceil(2));
        let next = gpu.plane("a push-pull level", w, h, 2)?;
        let params = Params::new().u(w).u(h).u(last.width()).u(last.height());
        batch.run(Kernel::PpReduce, &params, &[last.into(), (&next).into()], Groups::Plane { width: w, height: h })?;
        coarser.push(next);
    }
    let mut coarse: Option<Plane> = None;
    for level in std::iter::once(base).chain(&coarser).rev() {
        let (w, h) = (level.width(), level.height());
        let fine = gpu.plane("a pulled level", w, h, 1)?;
        let (from, (cw, ch), top) = match &coarse {
            Some(c) => (Bind::Plane(c), (c.width(), c.height()), false),
            None => (Bind::Buffer(gpu.dummy()), (1, 1), true),
        };
        let params = Params::new().u(w).u(h).u(cw).u(ch).flag(top);
        batch.run(Kernel::PpPull, &params, &[level.into(), from, (&fine).into()], Groups::Plane { width: w, height: h })?;
        coarse = Some(fine);
    }
    coarse.ok_or_else(|| GpuError::shape("push-pull's levels", 0, "at least one"))
}

/// A counter on the GPU, for the kernels that count with atomics.
fn counter(gpu: &Gpu, what: &'static str) -> Result<Plane, GpuError> {
    gpu.plane(what, 1, 1, 1)
}

/// The count a counter holds, once the GPU has done everything submitted before.
fn count(gpu: &Gpu, plane: &Plane, what: &'static str) -> Result<usize, GpuError> {
    Ok(gpu.read_words(plane, what)?.first().map_or(0, |&n| n as usize))
}

/// An error unless `image` is a plane of one channel of `width` x `height`.
fn check(what: &'static str, image: &Image<f32>, width: usize, height: usize) -> Result<(), GpuError> {
    if (image.width(), image.height(), image.channels()) == (width, height, 1) {
        return Ok(());
    }
    Err(GpuError::shape(what, format!("{}x{}x{}", image.width(), image.height(), image.channels()), format!("{width}x{height}x1")))
}

/// The whole-canvas filling of the depth map's resolve (florastack-stack's `depth::fill`): see the module
/// documentation.
pub fn fill(gpu: &Gpu, planes: &FillPlanes<'_>, settings: &FillSettings) -> Result<Filled, GpuError> {
    let (w, h) = (planes.depth.width(), planes.depth.height());
    check("the depth to fill", planes.depth, w, h)?;
    check("the confident pixels", planes.weights, w, h)?;
    check("the guide", planes.guide, w, h)?;
    if let Some(covered) = planes.covered {
        check("the coverage", covered, w, h)?;
    }
    let n = w * h;
    let depth = gpu.upload(planes.depth)?;
    let weights = gpu.upload(planes.weights)?;
    let guide = gpu.upload(planes.guide)?;
    let covered = planes.covered.map(|c| gpu.upload(c)).transpose()?;
    let confident_count = counter(gpu, "the confident pixels' count")?;
    let wide_count = counter(gpu, "the count of wider blends")?;
    let mut batch = Batch::new(gpu);
    let domain = Domain::new(&mut batch, &guide, settings.smoothing, settings.edge, true)?;
    drop(guide);
    let supported = settings.smoothing > 0.0 && planes.confident > 0;
    if let (Some(domain), true) = (&domain, supported) {
        let support = gpu.plane("the support", w, h, 1)?;
        batch.copy(&weights, &support)?;
        domain.filter(&mut batch, &support)?;
        let params = Params::new().u(n).f(settings.min_support);
        batch.run(Kernel::Support, &params, &[(&support).into(), (&weights).into(), (&confident_count).into()], Groups::Flat(n))?;
    }
    let smoothed = packed(&mut batch, &depth, &weights)?;
    drop(weights);
    if let Some(domain) = &domain {
        domain.filter(&mut batch, &smoothed)?;
    }
    drop(domain);
    let base = gpu.plane("push-pull's finest level", w, h, 2)?;
    batch.run(Kernel::FillBase, &Params::new().u(n).f(settings.reached), &[(&smoothed).into(), (&base).into()], Groups::Flat(n))?;
    drop(smoothed);
    let result = push_pull_planes(&mut batch, &base)?;
    let blend = gpu.plane("the blend width", w, h, 1)?;
    #[allow(clippy::cast_precision_loss)] // At most florastack-stack's MAX_FRAMES.
    let last = settings.frames.saturating_sub(1) as f32;
    let coverage = covered.as_ref().map_or(Bind::Buffer(gpu.dummy()), Bind::Plane);
    let params = Params::new().u(n).f(last).f(settings.fill_blend).flag(covered.is_some());
    batch.run(Kernel::FillFinish, &params, &[(&base).into(), coverage, (&result).into(), (&blend).into(), (&wide_count).into()], Groups::Flat(n))?;
    batch.finish()?;
    let confident = if supported { count(gpu, &confident_count, "the confident pixels' count")? } else { planes.confident };
    let wide = count(gpu, &wide_count, "the count of wider blends")?;
    let depth = gpu.download(&result)?;
    let blend = if wide > 0 { Some(gpu.download(&blend)?) } else { None };
    Ok(Filled { depth: one_channel(depth)?, blend: blend.map(one_channel).transpose()?, confident })
}

/// `image` as a linear plane of one channel (downloads keep the plane's shape).
fn one_channel(image: Image<f32>) -> Result<Image<f32>, GpuError> {
    if image.channels() == 1 {
        return Ok(image);
    }
    Err(GpuError::shape("a resolved plane", image.channels(), "one channel"))
}

/// A guide's distances to the pixel before, along rows and along columns.
pub type Distances = (Image<f32>, Image<f32>);

/// The guide's distances along rows and columns on the GPU (florastack-stack's `Domain::new`, on `guide` as given),
/// `None` when `sigma_s` is 0.
pub fn distances(gpu: &Gpu, guide: &Image<f32>, sigma_s: f64, sigma_r: f64) -> Result<Option<Distances>, GpuError> {
    check("the guide", guide, guide.width(), guide.height())?;
    let plane = gpu.upload(guide)?;
    let mut batch = Batch::new(gpu);
    let domain = Domain::new(&mut batch, &plane, sigma_s, sigma_r, false)?;
    batch.finish()?;
    domain.map(|d| Ok((gpu.download(&d.across)?, gpu.download(&d.down)?))).transpose()
}

/// Edge-aware normalized smoothing on the GPU (florastack-stack's `depth::filter::smooth`, with the domain of `guide`
/// as given): the smoothed values and the smoothed weights.
pub fn smooth(
    gpu: &Gpu,
    values: &Image<f32>,
    weights: &Image<f32>,
    guide: &Image<f32>,
    sigma_s: f64,
    sigma_r: f64,
) -> Result<(Image<f32>, Image<f32>), GpuError> {
    let (w, h) = (guide.width(), guide.height());
    check("the guide", guide, w, h)?;
    check("the values", values, w, h)?;
    check("the weights", weights, w, h)?;
    let (values, weights, guide) = (gpu.upload(values)?, gpu.upload(weights)?, gpu.upload(guide)?);
    let mut batch = Batch::new(gpu);
    let domain = Domain::new(&mut batch, &guide, sigma_s, sigma_r, false)?;
    let f = packed(&mut batch, &values, &weights)?;
    if let Some(domain) = &domain {
        domain.filter(&mut batch, &f)?;
    }
    let (smoothed, weight) = (gpu.plane("the smoothed values", w, h, 1)?, gpu.plane("the smoothed weights", w, h, 1)?);
    let n = w * h;
    batch.run(Kernel::SmoothUnpack, &Params::new().u(n), &[(&f).into(), (&smoothed).into(), (&weight).into()], Groups::Flat(n))?;
    batch.finish()?;
    Ok((gpu.download(&smoothed)?, gpu.download(&weight)?))
}

/// Push-pull on the GPU (florastack-stack's `depth::filter::push_pull`): `values` filled where `weights` (clamped to
/// [0, 1], 0 where not finite) is below 1.
pub fn push_pull(gpu: &Gpu, values: &Image<f32>, weights: &Image<f32>) -> Result<Image<f32>, GpuError> {
    let (w, h) = (values.width(), values.height());
    check("the values", values, w, h)?;
    check("the weights", weights, w, h)?;
    let clamped = Image::from_fn(w, h, 1, ColorSpace::LINEAR, |x, y, _| {
        weights.get(x, y, 0).map_or(0.0, |k| if k.is_finite() { k.clamp(0.0, 1.0) } else { 0.0 })
    })?;
    let (values, weights) = (gpu.upload(values)?, gpu.upload(&clamped)?);
    let mut batch = Batch::new(gpu);
    let base = packed(&mut batch, &values, &weights)?;
    let filled = push_pull_planes(&mut batch, &base)?;
    batch.finish()?;
    gpu.download(&filled)
}
