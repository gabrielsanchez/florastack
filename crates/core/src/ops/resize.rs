//! Resampling to a new size with the Lanczos3 filter, `sinc(x) sinc(x / 3)` on `|x| < 3` (C. E. Duchon, "Lanczos
//! filtering in one and two dimensions", Journal of Applied Meteorology 18(8), 1979; K. Turkowski, "Filters for common
//! resampling tasks", Graphics Gems, 1990), applied along the rows and then along the columns.
//!
//! The image's extent maps onto itself: output pixel `i` of `n` samples the input at `(i + 0.5) s - 0.5`, with
//! `s = input / output`, so the corners and the center line up. When shrinking, the filter is stretched by `s` so that
//! it averages away the detail the output can't hold (no aliasing); when enlarging it interpolates. The weights of each
//! output pixel are normalized to sum 1, taps beyond the border fall on the edge pixel, and an axis whose size doesn't
//! change is copied unchanged. Lanczos rings a little at sharp edges, and the result is not clipped: an `f32` image
//! keeps the overshoot.

use rayon::prelude::*;

use super::{OpError, to_f32, to_index};
use crate::image::Image;

/// The Lanczos3 kernel: `sinc(x) sinc(x / 3)` for `|x| < 3`, 0 beyond.
pub fn lanczos3(x: f64) -> f64 {
    let sinc = |t: f64| {
        let pt = std::f64::consts::PI * t;
        if t.abs() < 1e-12 { 1.0 } else { pt.sin() / pt }
    };
    if x.abs() < 3.0 { sinc(x) * sinc(x / 3.0) } else { 0.0 }
}

/// The taps of one output position: input samples `start ..`, one per weight.
struct Taps {
    starts: Vec<usize>,
    /// `len` weights per output position, zero-padded.
    weights: Vec<f32>,
    len: usize,
}

impl Taps {
    /// The taps for resampling an axis of `from` samples to `to`.
    fn new(from: usize, to: usize) -> Result<Taps, OpError> {
        let scale = from as f64 / to as f64;
        let stretch = scale.max(1.0);
        let support = 3.0 * stretch;
        let len = to_index(2.0 * support.ceil()) + 1;
        let mut starts = Vec::new();
        let mut weights = Vec::new();
        starts.try_reserve_exact(to).map_err(|_| OpError::Alloc("resampling weights"))?;
        weights.try_reserve_exact(to.saturating_mul(len)).map_err(|_| OpError::Alloc("resampling weights"))?;
        let last = from - 1;
        let mut row: Vec<f64> = Vec::new();
        row.try_reserve_exact(len).map_err(|_| OpError::Alloc("resampling weights"))?;
        for i in 0..to {
            let center = (i as f64 + 0.5) * scale - 0.5;
            let first = (center - support).ceil();
            let start = to_index(first.max(0.0)).min(last);
            row.clear();
            row.resize(len, 0.0);
            // Taps beyond the edges fall on the edge pixels; the indices only grow, so they stay within `len`.
            for k in 0..len {
                let j = first + k as f64;
                if j > center + support {
                    break;
                }
                let weight = lanczos3((j - center) / stretch);
                let index = to_index(j.clamp(0.0, last as f64));
                if let Some(slot) = row.get_mut(index - start) {
                    *slot += weight;
                }
            }
            let sum: f64 = row.iter().sum();
            let norm = if sum.abs() > 1e-12 { 1.0 / sum } else { 0.0 };
            starts.push(start);
            weights.extend(row.iter().map(|w| to_f32(w * norm)));
        }
        Ok(Taps { starts, weights, len })
    }

    /// The input start and the weights of output position `i`.
    fn get(&self, i: usize) -> (usize, &[f32]) {
        let start = self.starts.get(i).copied().unwrap_or(0);
        (start, self.weights.get(i * self.len..(i + 1) * self.len).unwrap_or(&[]))
    }
}

/// `src` resampled to `width` x `height` with Lanczos3.
pub fn resize(src: &Image<f32>, width: usize, height: usize) -> Result<Image<f32>, OpError> {
    let c = src.channels();
    let color = src.color_space().clone();
    // The output first: an impossible size fails here, before any weights are computed.
    let mut out = Image::new(width, height, c, color.clone())?;
    let (w, h) = (src.width(), src.height());
    let mid = if width == w {
        None
    } else {
        let taps = Taps::new(w, width)?;
        let mut mid = Image::new(width, h, c, color)?;
        mid.par_rows_mut().zip(src.par_rows()).for_each(|(dst, line)| {
            for (x, out) in dst.chunks_exact_mut(c).enumerate() {
                let (start, weights) = taps.get(x);
                out.fill(0.0);
                for (t, &weight) in weights.iter().enumerate() {
                    if let Some(pixel) = line.get((start + t) * c..(start + t + 1) * c) {
                        for (o, s) in out.iter_mut().zip(pixel) {
                            *o += weight * s;
                        }
                    }
                }
            }
        });
        Some(mid)
    };
    let mid = mid.as_ref().unwrap_or(src);
    if height == h {
        out.par_rows_mut().zip(mid.par_rows()).for_each(|(dst, line)| dst.copy_from_slice(line));
        return Ok(out);
    }
    let taps = Taps::new(h, height)?;
    let rows: Vec<&[f32]> = mid.rows().collect();
    out.par_rows_mut().enumerate().for_each(|(y, dst)| {
        let (start, weights) = taps.get(y);
        dst.fill(0.0);
        for (t, &weight) in weights.iter().enumerate() {
            if weight == 0.0 {
                continue;
            }
            if let Some(line) = rows.get(start + t) {
                for (o, s) in dst.iter_mut().zip(line.iter()) {
                    *o += weight * s;
                }
            }
        }
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ColorSpace;

    #[test]
    fn the_kernel_is_one_at_zero_and_zero_at_other_integers() {
        assert_eq!(lanczos3(0.0), 1.0);
        for x in [1.0, 2.0, -1.0, -2.0] {
            assert!(lanczos3(x).abs() < 1e-15);
        }
        assert_eq!(lanczos3(3.0), 0.0);
        assert!(lanczos3(0.5) > 0.6 && lanczos3(1.5) < 0.0);
    }

    #[test]
    fn the_same_size_is_an_exact_copy_and_flat_images_stay_flat() {
        let image = Image::from_fn(17, 9, 3, ColorSpace::SRGB, |x, y, c| ((x * 5 + y * 3 + c) % 7) as f32).unwrap();
        assert_eq!(resize(&image, 17, 9).unwrap(), image);
        let flat = Image::from_fn(40, 30, 2, ColorSpace::LINEAR, |_, _, c| 0.3 + c as f32 * 0.2).unwrap();
        for (w, h) in [(10, 7), (13, 30), (40, 61), (97, 113), (1, 1)] {
            let out = resize(&flat, w, h).unwrap();
            assert_eq!((out.width(), out.height(), out.channels()), (w, h, 2));
            assert!(out.as_raw().iter().zip(flat.as_raw().iter().cycle()).all(|(a, b)| (a - b).abs() < 1e-5), "{w}x{h}");
        }
    }

    #[test]
    fn ramps_survive_and_extents_line_up() {
        // A linear ramp in x: away from the borders, Lanczos3 reproduces it (to its small ripple).
        let ramp = Image::from_fn(64, 4, 1, ColorSpace::LINEAR, |x, _, _| x as f32).unwrap();
        let half = resize(&ramp, 32, 4).unwrap();
        for x in 4..28 {
            let expected = (x as f32 + 0.5) * 2.0 - 0.5;
            assert!((half.get(x, 1, 0).unwrap() - expected).abs() < 0.02, "{x}: {:?}", half.get(x, 1, 0));
        }
        let double = resize(&ramp, 128, 4).unwrap();
        for x in 8..120 {
            let expected = x as f32 / 2.0 - 0.25;
            assert!((double.get(x, 2, 0).unwrap() - expected).abs() < 0.02, "{x}");
        }
    }

    #[test]
    fn shrinking_averages_away_detail_the_output_cannot_hold() {
        // A one-pixel checkerboard is above the output's Nyquist limit: it becomes flat gray, not a moire.
        let checker = Image::from_fn(64, 64, 1, ColorSpace::LINEAR, |x, y, _| ((x + y) % 2) as f32).unwrap();
        let small = resize(&checker, 16, 16).unwrap();
        assert!(small.as_raw().iter().all(|v| (v - 0.5).abs() < 0.02), "{:?}", &small.as_raw()[..8]);
    }

    #[test]
    fn impossible_sizes_are_errors() {
        let image = Image::<f32>::new(4, 4, 1, ColorSpace::LINEAR).unwrap();
        assert!(matches!(resize(&image, 0, 4), Err(OpError::Image(_))));
        assert!(resize(&image, usize::MAX, 2).is_err());
    }
}
