//! Affine warps: output pixel `p` takes the value of the source at `to_source(p)`, interpolated bilinearly or with
//! Keys' bicubic convolution (R. G. Keys, "Cubic convolution interpolation for digital image processing", IEEE
//! Transactions on Acoustics, Speech, and Signal Processing 29(6), 1981, with `a = -0.5`). Both reproduce a sample
//! exactly when the point lands on it, so the identity is an exact copy and whole-pixel shifts move pixels unchanged.
//!
//! Coordinates and weights are computed in `f64` and each sample is accumulated in `f64`, rows of four taps first, so
//! that the bicubic warp with [`Border::Clamp`] gives, bit for bit, what the synthetic generator's reference warp
//! gives. Rows of the output are computed in parallel.

use rayon::prelude::*;

use super::{OpError, to_f32, to_index};
use crate::geometry::Affine;
use crate::image::Image;

/// How a warp interpolates between source pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Interpolation {
    /// The four nearest pixels, weighted by distance: fast, a little soft.
    Bilinear,
    /// Keys' cubic convolution on the 16 nearest pixels: sharper, with a slight overshoot at edges.
    #[default]
    Bicubic,
}

/// What a warp reads outside the source image.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Border {
    /// The edge pixels repeat.
    #[default]
    Clamp,
    /// Every sample is this value: 0 marks pixels the source doesn't cover, such as the corners a rotation uncovers.
    Constant(f32),
}

/// The image of `src` under the transform whose inverse is `to_source`: output pixel `p` samples `src` at
/// `to_source(p)`, with `src`'s size. To align a frame with a master, pass the transform from master coordinates to
/// frame coordinates.
pub fn warp(src: &Image<f32>, to_source: &Affine, interpolation: Interpolation, border: Border) -> Result<Image<f32>, OpError> {
    let mut dst = Image::new(src.width(), src.height(), src.channels(), src.color_space().clone())?;
    warp_into(src, to_source, interpolation, border, &mut dst)?;
    Ok(dst)
}

/// [`warp`] into `dst`, whose size sets the output's (its channel count must be `src`'s). Its color space is set to
/// `src`'s.
pub fn warp_into(src: &Image<f32>, to_source: &Affine, interpolation: Interpolation, border: Border, dst: &mut Image<f32>) -> Result<(), OpError> {
    if let Some(&value) = to_source.m.iter().flatten().find(|v| !v.is_finite()) {
        return Err(OpError::Param { name: "transform", value, expected: "finite in every entry" });
    }
    if let Border::Constant(value) = border
        && !value.is_finite()
    {
        return Err(OpError::Param { name: "border value", value: f64::from(value), expected: "finite" });
    }
    let c = src.channels();
    if dst.channels() != c {
        return Err(OpError::Channels { input: c, output: dst.channels() });
    }
    dst.set_color_space(src.color_space().clone());
    let (w, h) = (src.width(), src.height());
    let rows: Vec<&[f32]> = src.rows().collect();
    let constant = match border {
        Border::Clamp => None,
        Border::Constant(v) => Some(f64::from(v)),
    };
    dst.par_rows_mut().enumerate().for_each(|(y, out)| {
        for (x, pixel) in out.chunks_exact_mut(c).enumerate() {
            let (sx, sy) = to_source.apply(x as f64, y as f64);
            match interpolation {
                Interpolation::Bicubic => {
                    let (xs, wx) = cubic_taps(sx, w, constant.is_none());
                    let (ys, wy) = cubic_taps(sy, h, constant.is_none());
                    sample(&rows, c, &xs, &wx, &ys, &wy, constant, pixel);
                }
                Interpolation::Bilinear => {
                    let (xs, wx) = linear_taps(sx, w, constant.is_none());
                    let (ys, wy) = linear_taps(sy, h, constant.is_none());
                    sample(&rows, c, &xs, &wx, &ys, &wy, constant, pixel);
                }
            }
        }
    });
    Ok(())
}

/// The value at the taps `xs` x `ys` with their weights: for each channel, the rows' weighted sums in order, each
/// weighted by its row's weight and added up from zero. A tap outside the image (`None`) reads `constant`.
#[allow(clippy::too_many_arguments)] // The taps of both axes and their weights: grouping them would only add a struct.
fn sample<const N: usize>(
    rows: &[&[f32]],
    c: usize,
    xs: &[Option<usize>; N],
    wx: &[f64; N],
    ys: &[Option<usize>; N],
    wy: &[f64; N],
    constant: Option<f64>,
    out: &mut [f32],
) {
    if let (Some(xs), Some(ys)) = (inside(xs), inside(ys)) {
        // Every tap is in the image, as always when clamping: read them directly, in the same order.
        let lines: [&[f32]; N] = ys.map(|y| rows.get(y).copied().unwrap_or(&[]));
        for (ch, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (line, wy) in lines.iter().zip(wy) {
                let across: f64 = xs.iter().zip(wx).map(|(&col, wx)| wx * line.get(col * c + ch).map_or(0.0, |v| f64::from(*v))).sum();
                acc += wy * across;
            }
            *o = to_f32(acc);
        }
        return;
    }
    let fill = constant.unwrap_or(0.0);
    for (ch, o) in out.iter_mut().enumerate() {
        let mut acc = 0.0;
        for (row, wy) in ys.iter().zip(wy) {
            let line = row.and_then(|r| rows.get(r));
            let across: f64 = xs
                .iter()
                .zip(wx)
                .map(|(col, wx)| {
                    let v = line.zip(*col).and_then(|(line, col)| line.get(col * c + ch)).map_or(fill, |v| f64::from(*v));
                    wx * v
                })
                .sum();
            acc += wy * across;
        }
        *o = to_f32(acc);
    }
}

/// The taps' indices if every one is inside the image.
fn inside<const N: usize>(taps: &[Option<usize>; N]) -> Option<[usize; N]> {
    let mut out = [0; N];
    for (o, t) in out.iter_mut().zip(taps) {
        *o = (*t)?;
    }
    Some(out)
}

/// The weights of Keys' cubic convolution (a = -0.5) for the four samples around a point a fraction `t` past the
/// second one. At `t = 0` they are exactly `[0, 1, 0, 0]`.
fn keys_weights(t: f64) -> [f64; 4] {
    const A: f64 = -0.5;
    let near = |d: f64| ((A + 2.0) * d - (A + 3.0)) * d * d + 1.0;
    let far = |d: f64| ((A * d - 5.0 * A) * d + 8.0 * A) * d - 4.0 * A;
    [far(1.0 + t), near(t), near(1.0 - t), far(2.0 - t)]
}

/// The tap at `floor + offset` on an axis of `len` samples: clamped to the edge, or `None` outside when not clamping.
fn tap(floor: f64, offset: f64, len: usize, clamp: bool) -> Option<usize> {
    let last = (len - 1) as f64;
    let i = floor + offset;
    if clamp {
        Some(to_index(i.clamp(0.0, last)))
    } else if (0.0..=last).contains(&i) {
        Some(to_index(i))
    } else {
        None
    }
}

/// The four bicubic taps around coordinate `v` and their weights.
fn cubic_taps(v: f64, len: usize, clamp: bool) -> ([Option<usize>; 4], [f64; 4]) {
    let floor = v.floor();
    let index = |offset: f64| tap(floor, offset, len, clamp);
    ([index(-1.0), index(0.0), index(1.0), index(2.0)], keys_weights(v - floor))
}

/// The two bilinear taps around coordinate `v` and their weights.
fn linear_taps(v: f64, len: usize, clamp: bool) -> ([Option<usize>; 2], [f64; 2]) {
    let floor = v.floor();
    let t = v - floor;
    ([tap(floor, 0.0, len, clamp), tap(floor, 1.0, len, clamp)], [1.0 - t, t])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ColorSpace;

    fn ramp(w: usize, h: usize) -> Image<f32> {
        Image::from_fn(w, h, 2, ColorSpace::LINEAR, |x, y, c| (x as f32) * 0.1 + (y as f32) * 0.01 + c as f32).unwrap()
    }

    #[test]
    fn the_identity_and_whole_pixel_shifts_copy_exactly() {
        let src = ramp(12, 8);
        for interpolation in [Interpolation::Bilinear, Interpolation::Bicubic] {
            assert_eq!(warp(&src, &Affine::IDENTITY, interpolation, Border::Clamp).unwrap(), src);
            let shifted = warp(&src, &Affine::translation(-2.0, 1.0), interpolation, Border::Clamp).unwrap();
            assert_eq!(shifted.pixel(5, 3), src.pixel(3, 4));
            assert_eq!(shifted.pixel(0, 3), src.pixel(0, 4), "the edge repeats");
            let framed = warp(&src, &Affine::translation(-2.0, 1.0), interpolation, Border::Constant(-1.0)).unwrap();
            assert_eq!(framed.pixel(5, 3), src.pixel(3, 4));
            assert_eq!(framed.pixel(1, 3), Some(&[-1.0, -1.0][..]), "outside reads the constant");
            assert_eq!(framed.pixel(4, 7), Some(&[-1.0, -1.0][..]));
        }
    }

    #[test]
    fn both_interpolations_reproduce_linear_ramps_between_pixels() {
        let src = ramp(12, 8);
        let t = Affine::translation(0.5, 0.25);
        for interpolation in [Interpolation::Bilinear, Interpolation::Bicubic] {
            let half = warp(&src, &t, interpolation, Border::Clamp).unwrap();
            let expected = 0.1 * 4.5 + 0.01 * 3.25;
            assert!((half.get(4, 3, 0).unwrap() - expected).abs() < 1e-5, "{interpolation:?}: {:?}", half.get(4, 3, 0));
        }
    }

    #[test]
    fn the_output_size_comes_from_the_destination() {
        let src = ramp(10, 10);
        let mut dst = Image::<f32>::new(4, 3, 2, ColorSpace::SRGB).unwrap();
        // Twice the size: output (x, y) samples (x / 2, y / 2).
        warp_into(&src, &Affine::scaling(0.5), Interpolation::Bilinear, Border::Clamp, &mut dst).unwrap();
        assert!((dst.get(3, 2, 1).unwrap() - (1.0 + 0.15 + 0.01)).abs() < 1e-6);
        assert_eq!(dst.color_space(), src.color_space());
        let mut wrong = Image::<f32>::new(4, 3, 1, ColorSpace::LINEAR).unwrap();
        assert_eq!(
            warp_into(&src, &Affine::IDENTITY, Interpolation::Bicubic, Border::Clamp, &mut wrong),
            Err(OpError::Channels { input: 2, output: 1 })
        );
    }

    #[test]
    #[allow(clippy::float_cmp)] // Every tap reads the constant, with weights that sum to exactly 1.
    fn non_finite_parameters_are_errors_and_far_points_read_the_border() {
        let src = ramp(6, 6);
        let mut bad = Affine::IDENTITY;
        bad.m[1][2] = f64::NAN;
        assert!(matches!(warp(&src, &bad, Interpolation::Bicubic, Border::Clamp), Err(OpError::Param { name: "transform", .. })));
        assert!(warp(&src, &Affine::IDENTITY, Interpolation::Bilinear, Border::Constant(f32::INFINITY)).is_err());
        let far = warp(&src, &Affine::translation(1e300, -1e300), Interpolation::Bicubic, Border::Constant(0.5)).unwrap();
        assert!(far.as_raw().iter().all(|v| *v == 0.5));
        let clamped = warp(&src, &Affine::translation(1e300, -1e300), Interpolation::Bicubic, Border::Clamp).unwrap();
        assert_eq!(clamped.pixel(3, 3), src.pixel(5, 0));
    }
}
