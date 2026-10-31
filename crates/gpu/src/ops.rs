//! The basic operations on the GPU, each the port of a CPU reference in florastack-core or florastack-align: sample
//! conversions ([`to_linear_f32`], [`srgb_transfer`]), the Gaussian blur ([`gaussian_blur`], by the same
//! [`BlurPlan`] as the CPU's), the Laplacian ([`laplacian`]) and the alignment warp with its coverage
//! ([`warp_to_canvas`]). The functions here take and give images, for the agreement tests and the self-test; the
//! stackers use the same kernels on planes that stay on the GPU.
//!
//! The CPU computes the warp's coordinates and weights and the box passes in `f64`, which WGSL has no portable form
//! of; the GPU computes them in `f32`. The warp keeps its coordinates small (relative to the region and to the block
//! of the frame it reads, offsets folded in on the host in `f64`), so they are off by about 1e-4 pixels at most.

use florastack_core::ops::{BlurMethod, BlurPlan, OpError, blur_plan};
use florastack_core::{Affine, ColorSpace, Encoding, Image, Rect, Sample};
use wgpu::util::DeviceExt;

use crate::batch::{Batch, Bind, Groups, Params};
use crate::device::Gpu;
use crate::error::GpuError;
use crate::plane::Plane;
use crate::shaders::Kernel;

/// Records `plane` filled with `value`.
pub(crate) fn fill(batch: &mut Batch<'_>, plane: &Plane, value: f32) -> Result<(), GpuError> {
    fill_pattern(batch, plane, &[value])
}

/// Records `plane` filled with `pattern` (1 to 8 values) repeated: sample `i` takes `pattern[i % pattern.len()]`.
pub(crate) fn fill_pattern(batch: &mut Batch<'_>, plane: &Plane, pattern: &[f32]) -> Result<(), GpuError> {
    if pattern.is_empty() || pattern.len() > 8 {
        return Err(GpuError::shape("a fill pattern", pattern.len(), "1 to 8 values"));
    }
    let mut params = Params::new().u(plane.len()).u(pattern.len()).u(0).u(0);
    for i in 0..8 {
        params = params.f(pattern.get(i).copied().unwrap_or(0.0));
    }
    batch.run(Kernel::Fill, &params, &[plane.into()], Groups::Flat(plane.len()))
}

/// Records `plane`'s words set to `value`.
pub(crate) fn fill_words(batch: &mut Batch<'_>, plane: &Plane, value: u32) -> Result<(), GpuError> {
    let params = Params::new().u(plane.len()).bits(value);
    batch.run(Kernel::FillU32, &params, &[plane.into()], Groups::Flat(plane.len()))
}

/// Records the copy of the `width` x `height` pixels at `from` in `src` to `to` in `dst` (same channels).
pub(crate) fn copy_rect(
    batch: &mut Batch<'_>,
    src: &Plane,
    from: (usize, usize),
    dst: &Plane,
    to: (usize, usize),
    size: (usize, usize),
) -> Result<(), GpuError> {
    let (width, height) = size;
    let fits = |p: &Plane, (x, y): (usize, usize)| x + width <= p.width() && y + height <= p.height();
    if src.channels() != dst.channels() || !fits(src, from) || !fits(dst, to) {
        return Err(GpuError::shape("a block copied between planes", format!("{width}x{height}"), "inside both planes"));
    }
    let params = Params::new().u(width).u(height).u(src.channels()).u(src.width()).u(from.0).u(from.1).u(dst.width()).u(to.0).u(to.1);
    batch.run(Kernel::CopyRect, &params, &[src.into(), dst.into()], Groups::Plane { width, height })
}

/// Records `dst = a + b` or, `subtract`, `a - b`, for planes of the same shape.
pub(crate) fn combine(batch: &mut Batch<'_>, a: &Plane, b: &Plane, dst: &Plane, subtract: bool) -> Result<(), GpuError> {
    if a.len() != b.len() || a.len() != dst.len() {
        return Err(GpuError::shape("planes combined", b.len(), format!("{} samples", a.len())));
    }
    let params = Params::new().u(a.len()).flag(subtract);
    batch.run(Kernel::Combine, &params, &[a.into(), b.into(), dst.into()], Groups::Flat(a.len()))
}

/// A blur's plan, checked: its kernel's weights on the GPU when it has one.
pub(crate) struct Blur {
    plan: BlurPlan,
    weights: Option<wgpu::Buffer>,
}

impl Blur {
    pub(crate) fn new(gpu: &Gpu, sigma: f64, method: BlurMethod) -> Result<Blur, GpuError> {
        let plan = blur_plan(sigma, method)?;
        let weights = match &plan {
            BlurPlan::Exact(kernel) => Some(gpu.device().create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blur kernel"),
                contents: bytemuck::cast_slice(kernel),
                usage: wgpu::BufferUsages::STORAGE,
            })),
            _ => None,
        };
        Ok(Blur { plan, weights })
    }

    /// Records the blur of `plane` in place, through `scratch` (the same shape): rows, then columns, as the CPU does.
    pub(crate) fn record(&self, batch: &mut Batch<'_>, plane: &Plane, scratch: &Plane) -> Result<(), GpuError> {
        if (plane.width(), plane.height(), plane.channels()) != (scratch.width(), scratch.height(), scratch.channels()) {
            return Err(GpuError::shape("a blur's scratch plane", scratch.len(), format!("{} samples", plane.len())));
        }
        let groups = Groups::Plane { width: plane.width(), height: plane.height() };
        match (&self.plan, &self.weights) {
            (BlurPlan::Identity, _) => Ok(()),
            (BlurPlan::Exact(kernel), Some(weights)) => {
                let radius = kernel.len() / 2;
                for (axis, (src, dst)) in [(plane, scratch), (scratch, plane)].into_iter().enumerate() {
                    let params = Params::new().u(plane.width()).u(plane.height()).u(plane.channels()).u(radius).u(kernel.len()).u(axis).u(0);
                    batch.run(Kernel::Blur, &params.f(0.0).f(0.0), &[src.into(), dst.into(), Bind::Buffer(weights)], groups)?;
                }
                Ok(())
            }
            (BlurPlan::Exact(_), None) => Err(GpuError::Unsupported { what: "a blur without its kernel" }),
            (&BlurPlan::Boxes { radius, alpha, norm, passes }, _) => {
                // Every pass flips the plane between `plane` and `scratch`; 2 passes a box, so it ends in `plane`.
                let dummy = batch.gpu().dummy();
                let mut from_plane = true;
                for axis in 0..2 {
                    for _ in 0..passes {
                        let (src, dst) = if from_plane { (plane, scratch) } else { (scratch, plane) };
                        let params = Params::new().u(plane.width()).u(plane.height()).u(plane.channels()).u(radius).u(0).u(axis).u(1);
                        batch.run(Kernel::Blur, &params.f(to_f32(alpha)).f(to_f32(norm)), &[src.into(), dst.into(), Bind::Buffer(dummy)], groups)?;
                        from_plane = !from_plane;
                    }
                }
                if !from_plane {
                    batch.copy(scratch, plane)?;
                }
                Ok(())
            }
        }
    }
}

/// `v` rounded to `f32`.
#[allow(clippy::cast_possible_truncation)] // Rounding to the GPU's precision is the point.
pub(crate) fn to_f32(v: f64) -> f32 {
    v as f32
}

/// Records the five-point Laplacian of `src` into `dst`.
pub(crate) fn record_laplacian(batch: &mut Batch<'_>, src: &Plane, dst: &Plane) -> Result<(), GpuError> {
    let params = Params::new().u(src.width()).u(src.height()).u(src.channels());
    batch.run(Kernel::Laplacian, &params, &[src.into(), dst.into()], Groups::Plane { width: src.width(), height: src.height() })
}

/// How a frame lies on the canvas: what [`warp_to_canvas`] and the stackers warp it with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WarpGeometry {
    /// From canvas pixels to the frame's pixels (florastack-align's `Canvas::to_frame` of the frame's alignment).
    pub to_frame: Affine,
    pub frame_width: usize,
    pub frame_height: usize,
    /// The width, in frame pixels, of the coverage's ramp inside the frame's edge.
    pub feather: f64,
    /// The frame's brightness gain, multiplied into its samples.
    pub gain: f32,
}

/// `v` as a signed integer (sizes are far below `i64::MAX`).
fn signed(v: usize) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// `v` rounded down to a whole number of pixels, saturating.
#[allow(clippy::cast_possible_truncation)] // Saturating float-to-integer casts on finite values.
fn floor(v: f64) -> i64 {
    v.floor() as i64
}

impl WarpGeometry {
    /// Checks the transform and the frame's size.
    pub(crate) fn check(&self) -> Result<(), GpuError> {
        if let Some(&value) = self.to_frame.m.iter().flatten().find(|v| !v.is_finite()) {
            return Err(OpError::Param { name: "transform", value, expected: "finite in every entry" }.into());
        }
        if self.frame_width == 0 || self.frame_height == 0 {
            return Err(GpuError::shape("a frame", format!("{}x{}", self.frame_width, self.frame_height), "at least 1x1"));
        }
        Ok(())
    }

    /// The block of the frame that warping `region` of the canvas reads: the bicubic taps of the region's corners'
    /// points, one more pixel on every side for the GPU's rounding, clipped to the frame.
    pub(crate) fn block(&self, region: Rect) -> Rect {
        let (x0, y0) = (region.x as f64, region.y as f64);
        let (x1, y1) = (x0 + region.width.saturating_sub(1) as f64, y0 + region.height.saturating_sub(1) as f64);
        let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| self.to_frame.apply(x, y));
        let (mut u0, mut u1, mut v0, mut v1) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for (u, v) in corners {
            (u0, u1, v0, v1) = (u0.min(u), u1.max(u), v0.min(v), v1.max(v));
        }
        let clip = |v: i64, n: usize| usize::try_from(v.max(0)).unwrap_or(0).min(n - 1);
        let (bx0, bx1) = (clip(floor(u0) - 2, self.frame_width), clip(floor(u1) + 3, self.frame_width));
        let (by0, by1) = (clip(floor(v0) - 2, self.frame_height), clip(floor(v1) + 3, self.frame_height));
        Rect::new(bx0, by0, bx1 - bx0 + 1, by1 - by0 + 1)
    }

    /// Records the warp of `block` (the frame's pixels on `block_rect`) onto `region` of the canvas, into `dst` and
    /// its coverage `coverage` (both the region's size).
    pub(crate) fn record(
        &self,
        batch: &mut Batch<'_>,
        block: &Plane,
        block_rect: Rect,
        region: Rect,
        dst: &Plane,
        coverage: &Plane,
    ) -> Result<(), GpuError> {
        let m = Affine::translation(region.x as f64, region.y as f64)
            .then(&self.to_frame)
            .then(&Affine::translation(-(block_rect.x as f64), -(block_rect.y as f64)))
            .m;
        let (bx, by) = (block_rect.x as f64, block_rect.y as f64);
        let (fw, fh) = (self.frame_width as f64, self.frame_height as f64);
        let feather = if self.feather.is_finite() { self.feather.max(0.0) } else { 0.0 };
        let params = Params::new()
            .u(region.width)
            .u(region.height)
            .u(block.channels())
            .u(block.width())
            .u(block.height())
            .i(signed(block_rect.x))
            .i(signed(block_rect.y))
            .i(signed(self.frame_width))
            .i(signed(self.frame_height))
            .u(0)
            .u(0)
            .u(0)
            .f(to_f32(m[0][0]))
            .f(to_f32(m[0][1]))
            .f(to_f32(m[0][2]))
            .f(to_f32(m[1][0]))
            .f(to_f32(m[1][1]))
            .f(to_f32(m[1][2]))
            .f(to_f32(bx + 0.5))
            .f(to_f32(fw - 0.5 - bx))
            .f(to_f32(by + 0.5))
            .f(to_f32(fh - 0.5 - by))
            .f(to_f32(feather))
            .f(self.gain);
        batch.run(Kernel::Warp, &params, &[block.into(), dst.into(), coverage.into()], Groups::Plane { width: region.width, height: region.height })
    }
}

/// `frame` warped onto `rect` of the canvas as `geometry` says, with its coverage: florastack-align's
/// `warp_to_canvas`, the gain applied as florastack-stack's `Gained` applies it.
pub fn warp_to_canvas(gpu: &Gpu, frame: &Image<f32>, geometry: &WarpGeometry, rect: Rect) -> Result<(Image<f32>, Image<f32>), GpuError> {
    let geometry = WarpGeometry { frame_width: frame.width(), frame_height: frame.height(), ..*geometry };
    geometry.check()?;
    let block_rect = geometry.block(rect);
    let block = gpu.upload_rect(frame, block_rect)?;
    let dst = gpu.plane("warped", rect.width, rect.height, frame.channels())?;
    let coverage = gpu.plane("coverage", rect.width, rect.height, 1)?;
    let mut batch = Batch::new(gpu);
    geometry.record(&mut batch, &block, block_rect, rect, &dst, &coverage)?;
    batch.finish()?;
    let mut image = gpu.download(&dst)?;
    image.set_color_space(frame.color_space().clone());
    Ok((image, gpu.download(&coverage)?))
}

/// `image` blurred with a Gaussian of standard deviation `sigma` by `method`: florastack-core's
/// `gaussian_blur_in_place`.
pub fn gaussian_blur(gpu: &Gpu, image: &Image<f32>, sigma: f64, method: BlurMethod) -> Result<Image<f32>, GpuError> {
    let blur = Blur::new(gpu, sigma, method)?;
    let plane = gpu.upload(image)?;
    let scratch = gpu.plane("blur scratch", image.width(), image.height(), image.channels())?;
    let mut batch = Batch::new(gpu);
    blur.record(&mut batch, &plane, &scratch)?;
    batch.finish()?;
    let mut out = gpu.download(&plane)?;
    out.set_color_space(image.color_space().clone());
    Ok(out)
}

/// The five-point Laplacian of `image`: florastack-core's `laplacian`.
pub fn laplacian(gpu: &Gpu, image: &Image<f32>) -> Result<Image<f32>, GpuError> {
    let src = gpu.upload(image)?;
    let dst = gpu.plane("laplacian", image.width(), image.height(), image.channels())?;
    let mut batch = Batch::new(gpu);
    record_laplacian(&mut batch, &src, &dst)?;
    batch.finish()?;
    let mut out = gpu.download(&dst)?;
    out.set_color_space(image.color_space().clone());
    Ok(out)
}

/// The index of the alpha channel among `channels`, or one no channel has.
fn alpha(channels: usize) -> usize {
    match channels {
        2 => 1,
        4 => 3,
        _ => usize::MAX >> 1,
    }
}

/// Integer sample types the GPU unpacks: 8 and 16 bits, packed into 32-bit words.
pub trait Packed: Sample {
    /// The bits of a sample.
    const BITS: usize;
    /// The sample's value.
    fn value(self) -> u32;
}

impl Packed for u8 {
    const BITS: usize = 8;
    fn value(self) -> u32 {
        u32::from(self)
    }
}

impl Packed for u16 {
    const BITS: usize = 16;
    fn value(self) -> u32 {
        u32::from(self)
    }
}

/// An 8- or 16-bit `image` as linear `f32`: florastack-core's `Image::to_linear_f32` (sRGB-encoded samples decoded,
/// linear ones normalized; alpha only normalized).
pub fn to_linear_f32<T: Packed>(gpu: &Gpu, image: &Image<T>) -> Result<Image<f32>, GpuError> {
    let bits = T::BITS;
    let per_word = 32 / bits;
    let samples: Vec<u32> = image.rows().flatten().map(|&s| s.value()).collect();
    let words: Vec<u32> = samples.chunks(per_word).map(|chunk| chunk.iter().enumerate().fold(0, |w, (i, &s)| w | (s << (i * bits)))).collect();
    let (w, h, c) = (image.width(), image.height(), image.channels());
    let packed = gpu.plane("packed samples", words.len(), 1, 1)?;
    gpu.write_words(&packed, &words);
    let dst = gpu.plane("unpacked", w, h, c)?;
    let decode = image.color_space().encoding == Encoding::Srgb;
    let params = Params::new().u(dst.len()).u(c).u(alpha(c)).u(bits).flag(decode);
    let mut batch = Batch::new(gpu);
    batch.run(Kernel::Unpack, &params, &[(&packed).into(), (&dst).into()], Groups::Flat(dst.len()))?;
    batch.finish()?;
    let mut out = gpu.download(&dst)?;
    out.set_color_space(ColorSpace { encoding: Encoding::Linear, icc: image.color_space().icc.clone() });
    Ok(out)
}

/// `image`'s samples through the sRGB curve: encoded from linear (`encode`) or decoded to it, alpha left alone, as
/// florastack-core's `srgb_to_linear` and `linear_to_srgb` do each sample.
pub fn srgb_transfer(gpu: &Gpu, image: &Image<f32>, encode: bool) -> Result<Image<f32>, GpuError> {
    let c = image.channels();
    let src = gpu.upload(image)?;
    let dst = gpu.plane("transferred", image.width(), image.height(), c)?;
    let params = Params::new().u(dst.len()).u(c).u(alpha(c)).flag(encode);
    let mut batch = Batch::new(gpu);
    batch.run(Kernel::Transfer, &params, &[(&src).into(), (&dst).into()], Groups::Flat(dst.len()))?;
    batch.finish()?;
    let mut out = gpu.download(&dst)?;
    let encoding = if encode { Encoding::Srgb } else { Encoding::Linear };
    out.set_color_space(ColorSpace { encoding, icc: image.color_space().icc.clone() });
    Ok(out)
}
