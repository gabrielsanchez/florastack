//! The pyramid's building blocks on the GPU, ports of florastack-stack's `pyramid::levels`: REDUCE with the 5-tap
//! binomial kernel (plain, and by normalized convolution for frames that cover part of the canvas), EXPAND, the local
//! energy of a band, and the Laplacian pyramid of an image with its collapse.
//!
//! A [`Span`] is a rectangle of samples of one level with the level's whole size, as on the CPU: every operation is
//! defined on the whole level and evaluated on the span it is given, mirroring at the level's edges and clamping to the
//! input span. Filters are separable, the horizontal pass first, and every output sums its taps in order from the
//! first, as the CPU does; the kernel's weights are exact in `f32`, so the GPU differs from the CPU only where the
//! hardware rounds a multiply-add differently (contracted to one fused operation, say).

use florastack_core::Image;

use crate::batch::{Batch, Groups, Params};
use crate::device::Gpu;
use crate::error::GpuError;
use crate::plane::Plane;
use crate::shaders::Kernel;

/// A rectangle of samples of one level, and the level's whole size (florastack-stack's `levels::Span`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub full_width: usize,
    pub full_height: usize,
}

/// `v` as a signed index.
fn signed(v: usize) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl Span {
    /// The whole of a level of `width` x `height` samples.
    pub fn whole(width: usize, height: usize) -> Span {
        Span { x: 0, y: 0, width, height, full_width: width, full_height: height }
    }

    /// The span of the next coarser level that this one's samples reduce to.
    #[must_use]
    pub fn coarser(self) -> Span {
        let (fw, fh) = (self.full_width.div_ceil(2), self.full_height.div_ceil(2));
        let (x, y) = (self.x / 2, self.y / 2);
        let end_x = (self.x + self.width).div_ceil(2).min(fw);
        let end_y = (self.y + self.height).div_ceil(2).min(fh);
        Span { x, y, width: end_x.saturating_sub(x).max(1), height: end_y.saturating_sub(y).max(1), full_width: fw, full_height: fh }
    }

    /// The samples `x0 .. x1`, `y0 .. y1` of this span's level, clipped to it.
    #[must_use]
    pub fn of_level(self, x0: i64, y0: i64, x1: i64, y1: i64) -> Span {
        let clip = |v: i64, n: usize| usize::try_from(v.max(0)).unwrap_or(0).min(n);
        let (x0, x1) = (clip(x0, self.full_width), clip(x1, self.full_width));
        let (y0, y1) = (clip(y0, self.full_height), clip(y1, self.full_height));
        Span {
            x: x0.min(self.full_width.saturating_sub(1)),
            y: y0.min(self.full_height.saturating_sub(1)),
            width: x1.saturating_sub(x0).max(1),
            height: y1.saturating_sub(y0).max(1),
            full_width: self.full_width,
            full_height: self.full_height,
        }
    }

    fn check(self, plane: &Plane, what: &'static str) -> Result<(), GpuError> {
        if (plane.width(), plane.height()) == (self.width, self.height) {
            Ok(())
        } else {
            Err(GpuError::shape(what, format!("{}x{}", plane.width(), plane.height()), format!("its span's {}x{}", self.width, self.height)))
        }
    }
}

/// Records one pass of the 5-tap kernel along `axis` (0 rows, 1 columns) from `src` (input span `along` the axis:
/// start, length, level size) into a new plane of `width` x `height`, its outputs starting at `out_start` along the
/// axis, centered on input sample `stride` times theirs.
#[allow(clippy::too_many_arguments)] // One pass's geometry: grouping it would only add a struct used here alone.
fn filter5(
    batch: &mut Batch<'_>,
    src: &Plane,
    axis: usize,
    stride: i64,
    along: (usize, usize, usize),
    out_start: usize,
    width: usize,
    height: usize,
) -> Result<Plane, GpuError> {
    let dst = batch.gpu().plane("filtered level", width, height, src.channels())?;
    let (start, len, full) = along;
    let params = Params::new()
        .u(width)
        .u(height)
        .u(src.channels())
        .u(axis)
        .i(stride)
        .i(signed(start))
        .i(signed(len))
        .i(signed(full))
        .i(signed(out_start))
        .u(src.width());
    batch.run(Kernel::Filter5, &params, &[src.into(), (&dst).into()], Groups::Plane { width, height })?;
    Ok(dst)
}

/// Records one pass of EXPAND along `axis`, as [`filter5`] records REDUCE's.
fn expand_pass(
    batch: &mut Batch<'_>,
    src: &Plane,
    axis: usize,
    along: (usize, usize, usize),
    out_start: usize,
    width: usize,
    height: usize,
) -> Result<Plane, GpuError> {
    let dst = batch.gpu().plane("expanded level", width, height, src.channels())?;
    let (start, len, full) = along;
    let params = Params::new()
        .u(width)
        .u(height)
        .u(src.channels())
        .u(axis)
        .i(signed(start))
        .i(signed(len))
        .i(signed(full))
        .i(signed(out_start))
        .u(src.width());
    batch.run(Kernel::Expand, &params, &[src.into(), (&dst).into()], Groups::Plane { width, height })?;
    Ok(dst)
}

/// Records REDUCE of `src` (a plane of `span`) into the samples of `span.coarser()`.
pub(crate) fn reduce(batch: &mut Batch<'_>, src: &Plane, span: Span) -> Result<(Plane, Span), GpuError> {
    span.check(src, "a level reduced")?;
    let out = span.coarser();
    let rows = filter5(batch, src, 0, 2, (span.x, span.width, span.full_width), out.x, out.width, span.height)?;
    let dst = filter5(batch, &rows, 1, 2, (span.y, span.height, span.full_height), out.y, out.width, out.height)?;
    Ok((dst, out))
}

/// Records EXPAND of `src` (a plane of `span`) onto the samples `out` of the next finer level.
pub(crate) fn expand(batch: &mut Batch<'_>, src: &Plane, span: Span, out: Span) -> Result<Plane, GpuError> {
    span.check(src, "a level expanded")?;
    let rows = expand_pass(batch, src, 0, (span.x, span.width, span.full_width), out.x, out.width, span.height)?;
    expand_pass(batch, &rows, 1, (span.y, span.height, span.full_height), out.y, out.width, out.height)
}

/// Records REDUCE by normalized convolution: `reduce(coverage * src) / reduce(coverage)`, and the reduced coverage.
pub(crate) fn reduce_covered(batch: &mut Batch<'_>, src: &Plane, coverage: &Plane, span: Span) -> Result<(Plane, Plane, Span), GpuError> {
    let gpu = batch.gpu();
    let c = src.channels();
    let weighted = gpu.plane("weighted level", src.width(), src.height(), c)?;
    let count = src.width() * src.height();
    let params = Params::new().u(count).u(c).u(0);
    batch.run(Kernel::Coverage, &params, &[src.into(), coverage.into(), (&weighted).into()], Groups::Flat(count))?;
    let (values, out) = reduce(batch, &weighted, span)?;
    let (reduced, _) = reduce(batch, coverage, span)?;
    let n = out.width * out.height;
    let params = Params::new().u(n).u(c).u(1);
    let dummy = crate::batch::Bind::Buffer(gpu.dummy());
    batch.run(Kernel::Coverage, &params, &[dummy, (&reduced).into(), (&values).into()], Groups::Flat(n))?;
    Ok((values, reduced, out))
}

/// Records the local energy of `band` (a plane of `span`): the 5x5 binomial window of its luminance's squares
/// (`luminance`), or of each channel's.
pub(crate) fn local_energy(batch: &mut Batch<'_>, band: &Plane, span: Span, luminance: bool) -> Result<Plane, GpuError> {
    span.check(band, "a band")?;
    let channels = if luminance { 1 } else { band.channels() };
    let squares = batch.gpu().plane("squares", span.width, span.height, channels)?;
    let count = span.width * span.height;
    let params = Params::new().u(count).u(band.channels()).flag(luminance);
    batch.run(Kernel::BandEnergy, &params, &[band.into(), (&squares).into()], Groups::Flat(count))?;
    let rows = filter5(batch, &squares, 0, 1, (span.x, span.width, span.full_width), span.x, span.width, span.height)?;
    filter5(batch, &rows, 1, 1, (span.y, span.height, span.full_height), span.y, span.width, span.height)
}

/// A plane `a - b` or `a + b` of the same shape, recorded.
pub(crate) fn combined(batch: &mut Batch<'_>, a: &Plane, b: &Plane, subtract: bool) -> Result<Plane, GpuError> {
    let dst = batch.gpu().plane("combined level", a.width(), a.height(), a.channels())?;
    crate::ops::combine(batch, a, b, &dst, subtract)?;
    Ok(dst)
}

/// The span of a whole image.
fn whole(image: &Image<f32>) -> Span {
    Span::whole(image.width(), image.height())
}

/// `src`, an image of `span`, reduced on the GPU: florastack-stack's `levels::reduce`.
pub fn reduce_image(gpu: &Gpu, src: &Image<f32>, span: Span) -> Result<(Image<f32>, Span), GpuError> {
    let plane = gpu.upload(src)?;
    let mut batch = Batch::new(gpu);
    let (dst, out) = reduce(&mut batch, &plane, span)?;
    batch.finish()?;
    Ok((gpu.download(&dst)?, out))
}

/// `src`, an image of `span`, expanded onto `out` on the GPU: florastack-stack's `levels::expand`.
pub fn expand_image(gpu: &Gpu, src: &Image<f32>, span: Span, out: Span) -> Result<Image<f32>, GpuError> {
    let plane = gpu.upload(src)?;
    let mut batch = Batch::new(gpu);
    let dst = expand(&mut batch, &plane, span, out)?;
    batch.finish()?;
    gpu.download(&dst)
}

/// `src` reduced by normalized convolution with `coverage` on the GPU: florastack-stack's `levels::reduce_covered`.
pub fn reduce_covered_image(gpu: &Gpu, src: &Image<f32>, coverage: &Image<f32>, span: Span) -> Result<(Image<f32>, Image<f32>, Span), GpuError> {
    let (plane, cov) = (gpu.upload(src)?, gpu.upload(coverage)?);
    let mut batch = Batch::new(gpu);
    let (values, reduced, out) = reduce_covered(&mut batch, &plane, &cov, span)?;
    batch.finish()?;
    Ok((gpu.download(&values)?, gpu.download(&reduced)?, out))
}

/// The local energy of `band` on the GPU: florastack-stack's `levels::local_energy` of its `luminance` (decisions on
/// luminance) or of the band itself.
pub fn local_energy_image(gpu: &Gpu, band: &Image<f32>, span: Span, luminance: bool) -> Result<Image<f32>, GpuError> {
    let plane = gpu.upload(band)?;
    let mut batch = Batch::new(gpu);
    let energy = local_energy(&mut batch, &plane, span, luminance)?;
    batch.finish()?;
    gpu.download(&energy)
}

/// The Laplacian pyramid of `image` on the GPU: florastack-stack's `levels::laplacian_pyramid` (`levels` bands, finest
/// first, then the residual).
pub fn laplacian_pyramid(gpu: &Gpu, image: &Image<f32>, levels: usize) -> Result<Vec<Image<f32>>, GpuError> {
    let mut span = whole(image);
    let mut g = gpu.upload(image)?;
    let mut batch = Batch::new(gpu);
    let mut bands = Vec::with_capacity(levels + 1);
    for _ in 0..levels {
        let (next, coarse) = reduce(&mut batch, &g, span)?;
        let up = expand(&mut batch, &next, coarse, span)?;
        bands.push(combined(&mut batch, &g, &up, true)?);
        g = next;
        span = coarse;
    }
    bands.push(g);
    batch.finish()?;
    bands.iter().map(|b| gpu.download(b)).collect()
}

/// The image whose Laplacian pyramid is `pyramid`, on the GPU: florastack-stack's `levels::collapse`.
pub fn collapse(gpu: &Gpu, pyramid: &[Image<f32>]) -> Result<Image<f32>, GpuError> {
    let Some((top, bands)) = pyramid.split_last() else {
        return Err(GpuError::shape("a pyramid", 0, "at least one level"));
    };
    let mut g = gpu.upload(top)?;
    let mut span = whole(top);
    let mut batch = Batch::new(gpu);
    for band in bands.iter().rev() {
        let fine = whole(band);
        let up = expand(&mut batch, &g, span, fine)?;
        let b = gpu.upload(band)?;
        g = combined(&mut batch, &b, &up, false)?;
        span = fine;
    }
    batch.finish()?;
    gpu.download(&g)
}
