//! Preparing a frame for alignment, once: what the solver compares at each level of its pyramid.
//!
//! 1. **Luminance**: Rec. 709 weights over RGB (the gray channel of a one- or two-channel image; alpha is ignored),
//!    on linear values: a frame tagged sRGB is linearized sample by sample. Samples that aren't finite count as 0.
//! 2. **Working resolution**: a frame whose longer side exceeds [`AlignSettings::resolution`] is reduced to it, first
//!    by averaging blocks of 2 x 2 (or 4 x 4...) pixels while its luminance is computed, then with Lanczos3
//!    ([`resize`]); both average away the detail the smaller image can't hold. Alignment is sub-pixel at that level, so
//!    finer levels would cost far more time than they gain in accuracy.
//! 3. **Gaussian pyramid**: each level blurred with a Gaussian of one pixel and every second pixel kept (P. J. Burt and
//!    E. H. Adelson, "The Laplacian pyramid as a compact image code", IEEE Transactions on Communications 31(4), 1983),
//!    down to a level of about 32 pixels on its shorter side.
//! 4. **Band-pass** at each level: a difference of Gaussians of one and four pixels, which keeps edges and texture and
//!    drops the slow shading that lighting and vignetting change, with its derivatives and its Laplacian for the
//!    solver. It is linear in the light, on purpose: two copies of a scene blurred differently by a symmetric defocus
//!    correlate best where they line up, because their cross-correlation is the scene's autocorrelation blurred by both
//!    kernels, which is symmetric. A nonlinear measure of detail, such as the gradient magnitude or anything computed
//!    after a square root or a logarithm, loses that symmetry: its ridges merge, split and shift as the blur changes,
//!    and frames focused at different depths seem to move against each other: about twice as much as with the
//!    band-pass, on the synthetic stacks.
//!
//! Each level knows how its pixels map to the frame's ([`Mapping`]), so that the solver's parameters stay in frame
//! pixels on every level. Phase correlation runs on a coarser level's band-pass, a strongly low-passed linear
//! luminance of about 64 pixels feeds the brightness gain, and the finest level's band-pass gives the frame's focus map
//! (`focus.rs`).
//!
//! [`AlignSettings::resolution`]: crate::AlignSettings::resolution

use florastack_core::convert::srgb_to_linear;
use florastack_core::ops::{BlurMethod, Gradients, gaussian_blur_in_place, laplacian, resize, sobel};
use florastack_core::{Affine, ColorSpace, Encoding, Image};
use rayon::prelude::*;

use crate::error::AlignError;
use crate::focus::FocusMap;

/// The smallest side, in pixels, of a frame that can be aligned.
pub const MIN_SIDE: usize = 16;
/// The pyramid stops before a level whose shorter side would be below this.
const MIN_LEVEL_SIDE: usize = 32;
/// The most levels a pyramid has.
const MAX_LEVELS: usize = 12;
/// Phase correlation runs on the finest level whose longer side is at most this.
pub(crate) const CORRELATION_SIDE: usize = 256;
/// The longer side of the brightness plane, and its extra blur in its own pixels.
const BRIGHTNESS_SIDE: usize = 64;
const BRIGHTNESS_SIGMA: f64 = 2.0;
/// The blur before each halving of the pyramid, in pixels of the finer level.
const PYRAMID_SIGMA: f64 = 1.0;
/// The band-pass: the difference of Gaussians of these standard deviations, in pixels of the level.
const FINE_SIGMA: f64 = 1.0;
const COARSE_SIGMA: f64 = 4.0;
/// Rec. 709 luminance weights.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// How the pixels of a level map to the frame's: `X = scale.0 x + offset.0` and `Y = scale.1 y + offset.1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Mapping {
    pub scale: (f64, f64),
    pub offset: (f64, f64),
}

impl Mapping {
    pub const IDENTITY: Mapping = Mapping { scale: (1.0, 1.0), offset: (0.0, 0.0) };

    /// The mapping of an image resampled by [`resize`] from `from` to `to` pixels, whose own pixels this maps: the
    /// extents line up, so `x_from = (x_to + 0.5) s - 0.5` with `s = from / to`.
    #[must_use]
    pub fn resized(self, from: (usize, usize), to: (usize, usize)) -> Mapping {
        let axis = |scale: f64, offset: f64, from: usize, to: usize| {
            let s = from as f64 / to as f64;
            (scale * s, scale * (0.5 * s - 0.5) + offset)
        };
        let (sx, ox) = axis(self.scale.0, self.offset.0, from.0, to.0);
        let (sy, oy) = axis(self.scale.1, self.offset.1, from.1, to.1);
        Mapping { scale: (sx, sy), offset: (ox, oy) }
    }

    /// The mapping of an image whose pixel `x` averages pixels `k x` to `k x + k - 1` of this one's.
    #[must_use]
    pub fn blocks(self, k: usize) -> Mapping {
        let k = k as f64;
        let offset = (k - 1.0) / 2.0;
        Mapping {
            scale: (self.scale.0 * k, self.scale.1 * k),
            offset: (self.scale.0 * offset + self.offset.0, self.scale.1 * offset + self.offset.1),
        }
    }

    /// The mapping of the next pyramid level, which keeps every second pixel: `x_fine = 2 x_coarse`.
    #[must_use]
    pub fn halved(self) -> Mapping {
        Mapping { scale: (2.0 * self.scale.0, 2.0 * self.scale.1), offset: self.offset }
    }

    /// From level pixels to frame pixels, as an affine transform.
    pub fn to_frame(self) -> Affine {
        Affine { m: [[self.scale.0, 0.0, self.offset.0], [0.0, self.scale.1, self.offset.1]] }
    }

    /// The frame transform `transform` (frame pixels to frame pixels) in this level's pixels.
    pub fn level_transform(&self, transform: &Affine) -> Affine {
        let to_frame = self.to_frame();
        // The scales are positive and finite, so the mapping inverts.
        let from_frame = to_frame.inverse().unwrap_or(Affine::IDENTITY);
        to_frame.then(transform).then(&from_frame)
    }
}

/// One pyramid level of a prepared frame.
pub(crate) struct Level {
    pub mapping: Mapping,
    /// The band-pass, its x and y derivatives (per level pixel) and its Laplacian, interleaved in four channels.
    pub field: Image<f32>,
}

/// A frame prepared for alignment. See the module documentation.
pub(crate) struct Prepared {
    /// The pyramid, finest level first; never empty.
    pub levels: Vec<Level>,
    /// The level phase correlation runs on.
    pub correlation_level: usize,
    /// The low-passed linear luminance for the brightness gain, when the gain is estimated.
    pub brightness: Option<(Mapping, Image<f32>)>,
    /// The frame's focus map, from its finest level (`focus.rs`).
    pub focus: FocusMap,
}

impl Prepared {
    /// Prepares `frame`, reducing it to at most `resolution` pixels on its longer side, with the brightness plane
    /// when `brightness` is asked for.
    pub fn new(frame: &Image<f32>, resolution: usize, brightness: bool) -> Result<Prepared, AlignError> {
        let (w, h) = (frame.width(), frame.height());
        if w < MIN_SIDE || h < MIN_SIDE {
            return Err(AlignError::TooSmall { width: w, height: h });
        }
        let (working, mapping) = working_luminance(frame, resolution)?;
        let mut planes = vec![(working, mapping)];
        while planes.len() < MAX_LEVELS {
            let Some((last, mapping)) = planes.last() else { break };
            let next = (last.width().div_ceil(2), last.height().div_ceil(2));
            if next.0.min(next.1) < MIN_LEVEL_SIDE {
                break;
            }
            let halved = halve(last, next)?;
            let mapping = mapping.halved();
            planes.push((halved, mapping));
        }
        let brightness = if brightness { Some(brightness_plane(&planes)?) } else { None };
        let correlation_level = planes.iter().position(|(p, _)| p.width().max(p.height()) <= CORRELATION_SIDE).unwrap_or(planes.len() - 1);
        let levels =
            planes.iter().map(|(plane, mapping)| Ok(Level { mapping: *mapping, field: field(plane)? })).collect::<Result<Vec<_>, AlignError>>()?;
        let finest = levels.first().ok_or(AlignError::NoFrames)?;
        let focus = FocusMap::new(&finest.field, finest.mapping)?;
        Ok(Prepared { levels, correlation_level, brightness, focus })
    }
}

/// The size a `w` x `h` frame is reduced to so that its longer side is at most `resolution` pixels (but its shorter
/// side not below [`MIN_SIDE`]), or `None` when it fits already.
fn working_size(w: usize, h: usize, resolution: usize) -> Option<(usize, usize)> {
    let (long, short) = (w.max(h), w.min(h));
    if long <= resolution {
        return None;
    }
    let factor = (resolution as f64 / long as f64).max(MIN_SIDE as f64 / short as f64).min(1.0);
    let scaled = |n: usize| to_size(n as f64 * factor).clamp(1, n);
    let size = (scaled(w), scaled(h));
    (size != (w, h)).then_some(size)
}

/// `v` rounded to a whole number of pixels, at least 1.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A rounded, positive size no larger than the frame's.
fn to_size(v: f64) -> usize {
    v.round().max(1.0) as usize
}

/// The linear luminance of `frame` at the working resolution (see [`working_size`]), and how its pixels map to the
/// frame's. A frame at least twice the working size is first reduced by averaging blocks of `k` x `k` pixels, `k` a
/// power of two, as its luminance is computed: that is exact in linear light, reads the frame once, and leaves Lanczos
/// a reduction of less than two, with short kernels.
fn working_luminance(frame: &Image<f32>, resolution: usize) -> Result<(Image<f32>, Mapping), AlignError> {
    let (w, h) = (frame.width(), frame.height());
    let Some(size) = working_size(w, h, resolution) else {
        return Ok((luminance(frame, 1)?, Mapping::IDENTITY));
    };
    let mut k = 1;
    while k < MAX_BLOCK && w / (2 * k) >= size.0 && h / (2 * k) >= size.1 {
        k *= 2;
    }
    let reduced = luminance(frame, k)?;
    let (mapping, from) = (Mapping::IDENTITY.blocks(k), (reduced.width(), reduced.height()));
    if from == size {
        return Ok((reduced, mapping));
    }
    Ok((resize(&reduced, size.0, size.1)?, mapping.resized(from, size)))
}

/// The largest block [`working_luminance`] averages.
const MAX_BLOCK: usize = 64;

/// The linear luminance of `frame`, averaged over blocks of `k` x `k` pixels (`k` at least 1): `width / k` x
/// `height / k` pixels, the last columns and rows dropped when `k` doesn't divide the size. See the module
/// documentation for the luminance itself.
pub(crate) fn luminance(frame: &Image<f32>, k: usize) -> Result<Image<f32>, AlignError> {
    let k = k.max(1);
    let mut out = Image::new((frame.width() / k).max(1), (frame.height() / k).max(1), 1, ColorSpace::LINEAR)?;
    let c = frame.channels();
    let srgb = frame.color_space().encoding == Encoding::Srgb;
    let linear = |v: f32| {
        let v = if v.is_finite() { v } else { 0.0 };
        if srgb { srgb_to_linear(v) } else { v }
    };
    let luma = |pixel: &[f32]| match *pixel {
        [r, g, b, ..] => LUMA[0] * linear(r) + LUMA[1] * linear(g) + LUMA[2] * linear(b),
        [v, ..] => linear(v),
        [] => 0.0,
    };
    let scale = 1.0 / (k * k) as f32;
    out.par_rows_mut().enumerate().for_each(|(y, dst)| {
        dst.fill(0.0);
        for row in (y * k..y * k + k).filter_map(|r| frame.row(r)) {
            for (d, block) in dst.iter_mut().zip(row.chunks_exact(c * k)) {
                *d += block.chunks_exact(c).map(luma).sum::<f32>();
            }
        }
        for d in dst.iter_mut() {
            *d = if d.is_finite() { *d * scale } else { 0.0 };
        }
    });
    Ok(out)
}

/// The next pyramid level of `plane`, `size` pixels: blurred, then every second pixel.
fn halve(plane: &Image<f32>, size: (usize, usize)) -> Result<Image<f32>, AlignError> {
    let mut blurred = plane.clone();
    gaussian_blur_in_place(&mut blurred, PYRAMID_SIGMA, BlurMethod::Exact)?;
    let mut out = Image::new(size.0, size.1, 1, ColorSpace::LINEAR)?;
    out.par_rows_mut().enumerate().for_each(|(y, dst)| {
        if let Some(src) = blurred.row(2 * y) {
            for (d, &v) in dst.iter_mut().zip(src.iter().step_by(2)) {
                *d = v;
            }
        }
    });
    Ok(out)
}

/// The field of a level: its band-pass, the band-pass's x and y derivatives and its Laplacian, interleaved.
fn field(plane: &Image<f32>) -> Result<Image<f32>, AlignError> {
    let mut band = plane.clone();
    gaussian_blur_in_place(&mut band, FINE_SIGMA, BlurMethod::Exact)?;
    let mut wide = plane.clone();
    gaussian_blur_in_place(&mut wide, COARSE_SIGMA, BlurMethod::Exact)?;
    band.par_rows_mut().zip(wide.par_rows()).for_each(|(b, w)| {
        for (b, w) in b.iter_mut().zip(w) {
            *b -= w;
        }
    });
    drop(wide);
    let Gradients { x, y } = sobel(&band)?;
    let curvature = laplacian(&band)?;
    let mut field = Image::new(plane.width(), plane.height(), 4, ColorSpace::LINEAR)?;
    field.par_rows_mut().zip(band.par_rows()).zip(x.par_rows()).zip(y.par_rows()).zip(curvature.par_rows()).for_each(|((((dst, b), gx), gy), c)| {
        for ((((pixel, &b), &gx), &gy), &c) in dst.as_chunks_mut::<4>().0.iter_mut().zip(b).zip(gx).zip(gy).zip(c) {
            *pixel = [b, gx, gy, c];
        }
    });
    Ok(field)
}

/// The brightness plane: the coarsest pyramid level at least [`BRIGHTNESS_SIDE`] pixels long (or the finest, for a
/// smaller frame), reduced to that size and blurred.
fn brightness_plane(planes: &[(Image<f32>, Mapping)]) -> Result<(Mapping, Image<f32>), AlignError> {
    let long = |p: &Image<f32>| p.width().max(p.height());
    let Some((source, mapping)) = planes.iter().rev().find(|(p, _)| long(p) >= BRIGHTNESS_SIDE).or(planes.first()) else {
        return Err(AlignError::NoFrames);
    };
    let (w, h) = (source.width(), source.height());
    let factor = BRIGHTNESS_SIDE as f64 / long(source) as f64;
    let (mut plane, mapping) = if factor < 1.0 {
        let size = (to_size(w as f64 * factor), to_size(h as f64 * factor));
        (resize(source, size.0, size.1)?, mapping.resized((w, h), size))
    } else {
        (source.clone(), *mapping)
    };
    gaussian_blur_in_place(&mut plane, BRIGHTNESS_SIGMA, BlurMethod::Exact)?;
    Ok((mapping, plane))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings_follow_resizing_and_halving() {
        // 300 pixels resized to 100: output pixel 0 covers input pixels -0.5 to 2.5, centered on 1.
        let m = Mapping::IDENTITY.resized((300, 200), (100, 50));
        assert_eq!(m.to_frame().apply(0.0, 0.0), (1.0, 1.5));
        assert_eq!(m.to_frame().apply(99.0, 49.0), (298.0, 197.5));
        let half = m.halved();
        assert_eq!(half.to_frame().apply(1.0, 1.0), m.to_frame().apply(2.0, 2.0));
        // A block of 4 x 4 pixels is centered between its second and third pixels.
        assert_eq!(Mapping::IDENTITY.blocks(4).to_frame().apply(0.0, 2.0), (1.5, 9.5));
        assert_eq!(Mapping::IDENTITY.blocks(1), Mapping::IDENTITY);
        // A shift of 3 frame pixels is a shift of 1 pixel in the resized level, 0.5 in the halved one.
        let shift = Affine::translation(3.0, 0.0);
        assert!((m.level_transform(&shift).apply(10.0, 10.0).0 - 11.0).abs() < 1e-12);
        assert!((half.level_transform(&shift).apply(10.0, 10.0).0 - 10.5).abs() < 1e-12);
    }

    #[test]
    fn working_sizes_keep_the_aspect_and_a_minimum_side() {
        assert_eq!(working_size(6000, 4000, 2048), Some((2048, 1365)));
        assert_eq!(working_size(4000, 6000, 2048), Some((1365, 2048)));
        assert_eq!(working_size(2048, 1000, 2048), None);
        assert_eq!(working_size(20_000, 20, 2048), Some((16_000, 16)), "the shorter side stays at least 16 pixels");
    }

    #[test]
    fn luminance_weighs_linear_rgb_and_zeroes_bad_samples() {
        let rgb = Image::from_vec(2, 1, 3, ColorSpace::LINEAR, vec![1.0, 0.0, 0.0, f32::NAN, 1.0, f32::INFINITY]).unwrap();
        let y = luminance(&rgb, 1).unwrap();
        assert!((y.get(0, 0, 0).unwrap() - 0.2126).abs() < 1e-6 && (y.get(1, 0, 0).unwrap() - 0.7152).abs() < 1e-6);
        let gray_alpha = Image::from_vec(1, 1, 2, ColorSpace::LINEAR, vec![0.25, 1.0]).unwrap();
        assert_eq!(luminance(&gray_alpha, 1).unwrap().get(0, 0, 0), Some(0.25));
        let srgb = Image::from_vec(1, 1, 1, ColorSpace::SRGB, vec![0.5]).unwrap();
        assert!((luminance(&srgb, 1).unwrap().get(0, 0, 0).unwrap() - srgb_to_linear(0.5)).abs() < 1e-7);
        // Blocks of 2 x 2 average; the odd last column is dropped.
        let gray = Image::from_vec(5, 2, 1, ColorSpace::LINEAR, vec![1.0, 3.0, 5.0, 7.0, 100.0, 2.0, 4.0, 6.0, 8.0, 100.0]).unwrap();
        let blocks = luminance(&gray, 2).unwrap();
        assert_eq!((blocks.width(), blocks.height(), blocks.as_raw()), (2, 1, &[2.5, 6.5][..]));
    }

    #[test]
    fn pyramids_halve_down_to_the_smallest_level() {
        let frame = Image::from_fn(300, 140, 3, ColorSpace::LINEAR, |x, y, c| ((x * 7 + y * 3 + c) % 11) as f32 / 10.0).unwrap();
        let p = Prepared::new(&frame, 2048, true).unwrap();
        let sizes: Vec<(usize, usize)> = p.levels.iter().map(|l| (l.field.width(), l.field.height())).collect();
        assert_eq!(sizes, [(300, 140), (150, 70), (75, 35)]);
        assert_eq!(p.correlation_level, 1, "the first level at most 256 pixels long");
        assert!(p.levels.iter().all(|l| l.field.channels() == 4));
        let (mapping, plane) = p.brightness.as_ref().unwrap();
        assert_eq!((plane.width(), plane.height()), (64, 30));
        // From the 75 x 35 level (four frame pixels per pixel), resized to 64 x 30.
        assert_eq!(mapping.scale, (4.0 * 75.0 / 64.0, 4.0 * 35.0 / 30.0));
        let reduced = Prepared::new(&frame, 100, false).unwrap();
        assert_eq!((reduced.levels[0].field.width(), reduced.levels[0].field.height()), (100, 47));
        assert!(reduced.brightness.is_none());
        let tiny = Image::<f32>::new(15, 40, 1, ColorSpace::LINEAR).unwrap();
        assert!(matches!(Prepared::new(&tiny, 2048, true), Err(AlignError::TooSmall { width: 15, height: 40 })));
    }
}
