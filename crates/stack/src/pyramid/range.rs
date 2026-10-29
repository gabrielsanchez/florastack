//! Range: the range limit, which holds the pyramid's result within the range the frames span at each pixel, and the
//! tone compression that brings a result back into `[0, 1]`.
//!
//! # The range limit
//!
//! Selecting each band's coefficients from whichever frame has the most energy there can put a pixel beyond every
//! frame: darker than the darkest frame next to a bright edge, or with a color that no frame has (an inversion halo,
//! or the blue cast a black subject took on in the camera stacks the method was checked on). The true sharp value of a
//! pixel is the one the frame focused at its depth shows, so it lies within the frames' values there. The stacker
//! keeps, per pixel and channel, the least and the largest value of the frames that cover it, and moves each channel
//! of the result into that range; `range_limit` is the share of the excess removed (1 clamps). The idea is
//! focus-stack's, which limits a gray stack to its inputs' range in the same way (`src/task_reassign.cc`,
//! `Task_Reassign_Map::build_gray` and `Task_Reassign::reassign_gray`, at commit 0cd289e,
//! <https://github.com/PetteriAimonen/focus-stack>, MIT, Copyright (c) 2019 Petteri Aimonen); here it is per channel
//! and streamed, and written anew.
//!
//! The bounds are kept as the top 16 bits of their `f32` (bfloat16), rounded outward (the least value down, the
//! largest up), so that they take 2 bytes a channel each instead of 4: a bound is at most 0.8% wider than the frames'
//! true range, far below the halos it stops.
//!
//! # Tone compression
//!
//! [`compress_range`] brings a result into `[0, 1]` as [`Tone`] says. `Compress` changes only pixels near values above
//! 1: the image is cut into blocks of [`BLOCK`] pixels, each block's largest sample (at least 1) is taken over its
//! neighbors too and interpolated bilinearly between block centers into a smooth local maximum `M`, at least every
//! pixel's own largest sample. Where `M` exceeds 1, a pixel's largest sample `x` above the knee `k = max(0.5, 2 - M)` is
//! mapped by the shoulder `k + (1 - k) (1 - (1 - t)^a)` with `t = (x - k) / (M - k)` and `a = (M - k) / (1 - k)`, which
//! leaves `[0, k]` alone, meets it with slope 1 and takes `M` to 1, and the pixel's color channels are scaled alike, so
//! that hue is kept. Negative samples are set to 0. A pixel's alpha channel, if it has one, is only clipped.

use florastack_core::Image;
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::StackError;

/// How [`compress_range`] brings a result into `[0, 1]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// A local shoulder for values above 1, and 0 for values below 0 (see the module documentation).
    #[default]
    Compress,
    /// Every sample clipped to `[0, 1]`.
    Clip,
    /// Nothing changed: values outside `[0, 1]` stay, for those who tone-map the result themselves.
    Keep,
}

/// The side, in pixels, of the blocks over which [`Tone::Compress`] finds the local maximum.
pub const BLOCK: usize = 32;

/// The bfloat16 bits of the largest bfloat16 at most `v`.
pub fn bound_down(v: f32) -> u16 {
    let bits = v.to_bits();
    let top = bits >> 16;
    let exact = bits.trailing_zeros() >= 16;
    // Truncating the low bits moves a value toward 0: down for positive values, up for negative ones.
    let top = if v.is_sign_negative() && !exact { top + 1 } else { top };
    u16::try_from(top).unwrap_or(u16::MAX)
}

/// The bfloat16 bits of the smallest bfloat16 at least `v`.
pub fn bound_up(v: f32) -> u16 {
    let bits = v.to_bits();
    let top = bits >> 16;
    let exact = bits.trailing_zeros() >= 16;
    let top = if !v.is_sign_negative() && !exact { top + 1 } else { top };
    u16::try_from(top).unwrap_or(u16::MAX)
}

/// The value of bfloat16 bits.
pub fn bound_value(bits: u16) -> f32 {
    f32::from_bits(u32::from(bits) << 16)
}

/// The bits a least-value bound starts at: no frame yet (`+inf`).
pub const EMPTY_LOW: u16 = 0x7f80;
/// The bits a largest-value bound starts at: no frame yet (`-inf`).
pub const EMPTY_HIGH: u16 = 0xff80;

/// `v` moved toward `[low, high]` by `limit` of its excess (1 clamps); unchanged when the range is empty.
pub fn limit_to(v: f32, low: f32, high: f32, limit: f32) -> f32 {
    if low.is_nan() || high.is_nan() || low > high {
        return v;
    }
    let c = v.clamp(low, high);
    c + (1.0 - limit) * (v - c)
}

/// The color channels of an image of `c` channels (alpha, the second of two and the fourth of four, is not one).
fn color_channels(c: usize) -> usize {
    if c >= 3 { 3 } else { 1 }
}

/// How many pixels of `image` have a sample outside `[0, 1]`.
pub fn out_of_range(image: &Image<f32>) -> usize {
    let c = image.channels();
    image.par_rows().map(|row| row.chunks_exact(c).filter(|p| p.iter().any(|v| !(0.0..=1.0).contains(v))).count()).sum()
}

/// Brings `image` into `[0, 1]` as `tone` says, and returns how many pixels changed. See the module documentation.
pub fn compress_range(image: &mut Image<f32>, tone: Tone) -> Result<usize, StackError> {
    let c = image.channels();
    let colors = color_channels(c);
    match tone {
        Tone::Keep => Ok(0),
        Tone::Clip => Ok(image
            .par_rows_mut()
            .map(|row| {
                let mut changed = 0;
                for pixel in row.chunks_exact_mut(c) {
                    let before = pixel.to_vec();
                    for v in pixel.iter_mut() {
                        *v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
                    }
                    changed += usize::from(before.as_slice() != &*pixel);
                }
                changed
            })
            .sum()),
        Tone::Compress => {
            let maxima = local_maxima(image, colors)?;
            let (bw, bh) = (maxima.width(), maxima.height());
            Ok(image
                .par_rows_mut()
                .enumerate()
                .map(|(y, row)| {
                    let mut changed = 0;
                    for (x, pixel) in row.chunks_exact_mut(c).enumerate() {
                        let before = pixel.to_vec();
                        let m = interpolate(&maxima, bw, bh, x, y);
                        shoulder(pixel, colors, m);
                        for (i, v) in pixel.iter_mut().enumerate() {
                            *v = if v.is_nan() {
                                0.0
                            } else if i >= colors {
                                v.clamp(0.0, 1.0)
                            } else {
                                v.max(0.0)
                            };
                        }
                        changed += usize::from(before.as_slice() != &*pixel);
                    }
                    changed
                })
                .sum())
        }
    }
}

/// Each block's largest color sample, at least 1, taken over the block and its eight neighbors.
fn local_maxima(image: &Image<f32>, colors: usize) -> Result<Image<f32>, StackError> {
    let (w, h, c) = (image.width(), image.height(), image.channels());
    let (bw, bh) = (w.div_ceil(BLOCK).max(1), h.div_ceil(BLOCK).max(1));
    let mut blocks = vec![1.0_f32; bw * bh];
    for (y, row) in image.rows().enumerate() {
        let by = y / BLOCK;
        for (x, pixel) in row.chunks_exact(c).enumerate() {
            let m = pixel.iter().take(colors).copied().filter(|v| v.is_finite()).fold(f32::NEG_INFINITY, f32::max);
            if let Some(b) = blocks.get_mut(by * bw + x / BLOCK)
                && m > *b
            {
                *b = m;
            }
        }
    }
    Ok(Image::from_fn(bw, bh, 1, florastack_core::ColorSpace::LINEAR, |bx, by, _| {
        let mut m = 1.0_f32;
        for ny in by.saturating_sub(1)..=(by + 1).min(bh - 1) {
            for nx in bx.saturating_sub(1)..=(bx + 1).min(bw - 1) {
                m = m.max(blocks.get(ny * bw + nx).copied().unwrap_or(1.0));
            }
        }
        m
    })?)
}

/// The local maximum at pixel `(x, y)`: bilinear between the centers of the blocks around it.
fn interpolate(maxima: &Image<f32>, bw: usize, bh: usize, x: usize, y: usize) -> f32 {
    let half = (BLOCK as f32 - 1.0) / 2.0;
    let fx = ((x as f32 - half) / BLOCK as f32).clamp(0.0, (bw - 1) as f32);
    let fy = ((y as f32 - half) / BLOCK as f32).clamp(0.0, (bh - 1) as f32);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped to the block grid above.
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(bw - 1), (y0 + 1).min(bh - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let at = |i: usize, j: usize| maxima.get(i, j, 0).unwrap_or(1.0);
    let top = at(x0, y0) + tx * (at(x1, y0) - at(x0, y0));
    let bottom = at(x0, y1) + tx * (at(x1, y1) - at(x0, y1));
    // Interpolation can't fall below any of the four, but rounding could take it a hair under the pixel's own maximum.
    top + ty * (bottom - top)
}

/// Maps the pixel's largest color sample through the shoulder for a local maximum `m`, scaling its colors alike.
fn shoulder(pixel: &mut [f32], colors: usize, m: f32) {
    if m <= 1.0 {
        return;
    }
    let x = pixel.iter().take(colors).copied().fold(f32::NEG_INFINITY, f32::max);
    let knee = (2.0 - m).max(0.5);
    if x.is_nan() || x <= knee {
        return;
    }
    let m = m.max(x);
    let a = (m - knee) / (1.0 - knee);
    let t = ((x - knee) / (m - knee)).clamp(0.0, 1.0);
    let mapped = knee + (1.0 - knee) * (1.0 - (1.0 - t).powf(a));
    let gain = mapped / x;
    for v in pixel.iter_mut().take(colors) {
        *v *= gain;
    }
}

#[cfg(test)]
mod tests {
    use florastack_core::ColorSpace;

    use super::*;

    #[test]
    #[allow(clippy::float_cmp)] // bfloat16 values are exact f32 values.
    fn bounds_round_outward() {
        for v in [0.3_f32, -0.3, 1.0, 0.0, -0.0, 1e-30, -2.5e5, 0.999_999] {
            let (low, high) = (bound_value(bound_down(v)), bound_value(bound_up(v)));
            assert!(low <= v && v <= high, "{v}: {low} {high}");
            assert!((high - low).abs() <= v.abs() * 0.008 + 1e-38, "{v}: {low} {high}");
        }
        assert_eq!(bound_value(bound_down(0.5)), 0.5);
        assert_eq!(bound_value(EMPTY_LOW), f32::INFINITY);
        assert_eq!(bound_value(EMPTY_HIGH), f32::NEG_INFINITY);
        assert_eq!(limit_to(1.5, 0.2, 0.8, 1.0), 0.8);
        assert_eq!(limit_to(1.0, 0.2, 0.8, 0.5), 0.9);
        assert_eq!(limit_to(0.5, f32::INFINITY, f32::NEG_INFINITY, 1.0), 0.5, "no frame: no limit");
    }

    #[test]
    fn compression_changes_only_what_is_near_values_beyond_1() {
        // Gray 0.5 everywhere, with a bright patch up to 1.6 in one corner.
        let mut image = Image::from_fn(160, 128, 3, ColorSpace::LINEAR, |x, y, c| if x < 20 && y < 20 { 1.6 - 0.2 * c as f32 } else { 0.5 }).unwrap();
        let far = image.pixel(150, 120).unwrap().to_vec();
        let changed = compress_range(&mut image, Tone::Compress).unwrap();
        assert!(changed > 0 && changed < 160 * 128 / 4, "{changed}");
        assert!(image.as_raw().iter().all(|v| (0.0..=1.0).contains(v)));
        assert_eq!(image.pixel(150, 120).unwrap(), &far[..], "far from the patch nothing changes");
        // Hue is kept: the channels keep their ratios.
        let p = image.pixel(5, 5).unwrap();
        assert!((p[1] / p[0] - 1.4 / 1.6).abs() < 1e-5, "{p:?}");
        // In range already: nothing to do.
        let mut fine = Image::from_fn(40, 40, 1, ColorSpace::LINEAR, |x, _, _| x as f32 / 40.0).unwrap();
        let copy = fine.clone();
        assert_eq!(compress_range(&mut fine, Tone::Compress).unwrap(), 0);
        assert_eq!(fine, copy);
        let mut negative = Image::from_fn(8, 8, 2, ColorSpace::LINEAR, |x, _, c| if c == 1 { 2.0 } else { x as f32 - 2.0 }).unwrap();
        compress_range(&mut negative, Tone::Compress).unwrap();
        assert!(negative.as_raw().iter().all(|v| (0.0..=1.0).contains(v)), "negatives to 0, alpha clipped");
        let mut clip = Image::from_fn(4, 4, 1, ColorSpace::LINEAR, |x, _, _| x as f32 * 0.5 - 0.2).unwrap();
        assert_eq!(compress_range(&mut clip, Tone::Clip).unwrap(), 2 * 4);
        assert_eq!(out_of_range(&clip), 0);
        let mut keep = Image::from_fn(4, 4, 1, ColorSpace::LINEAR, |_, _, _| 3.0).unwrap();
        assert_eq!((compress_range(&mut keep, Tone::Keep).unwrap(), out_of_range(&keep)), (0, 16));
    }

    #[test]
    fn the_shoulder_is_continuous_and_monotone() {
        let mut last = 0.0;
        for i in 0..=400 {
            let x = i as f32 / 200.0;
            let mut p = [x];
            shoulder(&mut p, 1, 2.0);
            assert!(p[0] >= last - 1e-6 && p[0] <= 1.0 + 1e-6, "{x}: {}", p[0]);
            last = p[0];
        }
        let mut p = [0.4];
        shoulder(&mut p, 1, 1.3);
        assert!((p[0] - 0.4).abs() < 1e-7, "below the knee nothing changes");
    }
}
