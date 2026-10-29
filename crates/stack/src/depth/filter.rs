//! The filters the depth map is made with: edge-aware normalized smoothing ([`smooth`]) and hole filling from coarser
//! scales ([`push_pull`]), on whole-canvas planes of one channel.
//!
//! # Edge-aware smoothing
//!
//! The recursive filter of the domain transform (E. S. L. Gastal and M. M. Oliveira, "Domain transform for edge-aware
//! image and video processing", ACM Transactions on Graphics 30(4), SIGGRAPH 2011): along each row, then each column,
//! a first-order recursive filter whose feedback between neighbors decays with their distance in the transformed
//! domain, `1 + (sigma_s / sigma_r) |guide difference|`, so that it smooths freely along the guide's flat regions and
//! stops at its edges; three passes of each, with the paper's standard deviation for each pass so that the whole has
//! the spatial standard deviation `sigma_s`. It costs the same at any radius. Run on `weights * values` and on
//! `weights` alike, and divided, it is a normalized convolution (H. Knutsson and C.-F. Westin, "Normalized and
//! differential convolution", CVPR 1993): pixels of weight 0 take the weighted average of the weighted pixels the
//! filter reaches from them.
//!
//! # Filling
//!
//! Push-pull (S. J. Gortler, R. Grzeszczuk, R. Szeliski and M. F. Cohen, "The lumigraph", SIGGRAPH 1996): the weighted
//! values are summed down a pyramid of 2x2 blocks, each level's weights clamped to 1, and pulled back up, each pixel
//! keeping its own value as much as its weight says and taking the rest from the coarser level, bilinearly
//! interpolated. A hole of any size is filled smoothly from its edges, and a pixel of weight 1 keeps its value.

use florastack_core::{ColorSpace, Image};
use rayon::prelude::*;

use crate::error::StackError;

/// The passes along each axis of the domain transform's recursive filter.
const PASSES: i32 = 3;

/// `image` (one channel or more) with rows and columns swapped.
pub(crate) fn transpose(image: &Image<f32>) -> Result<Image<f32>, StackError> {
    const BLOCK: usize = 64;
    let (w, h, c) = (image.width(), image.height(), image.channels());
    let mut out = vec![0.0_f32; w * h * c];
    let rows: Vec<&[f32]> = image.rows().collect();
    // Each chunk is `BLOCK` rows of the output: `BLOCK` columns of the input, read a block of rows at a time.
    out.par_chunks_mut(BLOCK * h * c).enumerate().for_each(|(b, chunk)| {
        let x0 = b * BLOCK;
        for y0 in (0..h).step_by(BLOCK) {
            for (y, row) in rows.iter().enumerate().skip(y0).take(BLOCK) {
                for (dx, pixel) in row.chunks_exact(c).skip(x0).take(BLOCK).enumerate() {
                    let at = (dx * h + y) * c;
                    if let Some(dst) = chunk.get_mut(at..at + c) {
                        dst.copy_from_slice(pixel);
                    }
                }
            }
        }
    });
    Ok(Image::from_vec(h, w, c, ColorSpace::LINEAR, out)?)
}

/// The domain transform's distance from each pixel to the one before it in its row: `1 + ratio |guide difference|`
/// (the first pixel's is unused).
fn distances(guide: &Image<f32>, ratio: f32) -> Result<Image<f32>, StackError> {
    let mut d = Image::new(guide.width(), guide.height(), 1, ColorSpace::LINEAR)?;
    d.par_rows_mut().zip(guide.par_rows()).for_each(|(d, g)| {
        for (x, v) in d.iter_mut().enumerate() {
            let diff = match (x.checked_sub(1).and_then(|p| g.get(p)), g.get(x)) {
                (Some(&a), Some(&b)) => (b - a).abs(),
                _ => 0.0,
            };
            *v = 1.0 + ratio * diff;
        }
    });
    Ok(d)
}

/// One recursive pass, forth and back along every row, of a plane of two channels (the weighted values and the
/// weights), with feedback `a^distance`.
fn recursive_rows(f: &mut Image<f32>, distances: &Image<f32>, a: f32) {
    let ln_a = a.ln();
    f.par_rows_mut().zip(distances.par_rows()).for_each_init(Vec::new, |feedback, (row, d)| {
        feedback.clear();
        feedback.extend(d.iter().map(|&d| (ln_a * d).exp()));
        let w = feedback.len().min(row.len() / 2);
        for x in 1..w {
            let v = feedback[x];
            let (prev, cur) = row.split_at_mut(2 * x);
            for (c, p) in cur.iter_mut().take(2).zip(&prev[2 * x - 2..]) {
                *c += v * (*p - *c);
            }
        }
        for x in (0..w.saturating_sub(1)).rev() {
            let v = feedback[x + 1];
            let (cur, next) = row.split_at_mut(2 * x + 2);
            for (c, n) in cur[2 * x..].iter_mut().zip(next.iter()) {
                *c += v * (*n - *c);
            }
        }
    });
}

/// Edge-aware normalized smoothing of `values` weighted by `weights` (both one channel, `guide`'s size), guided by
/// `guide`: the smoothed values (0 where no weight reaches) and the smoothed weights. See the module documentation.
/// `sigma_s` is in pixels, `sigma_r` in the guide's units.
pub(crate) fn smooth(
    values: &Image<f32>,
    weights: &Image<f32>,
    guide: &Image<f32>,
    sigma_s: f64,
    sigma_r: f64,
) -> Result<(Image<f32>, Image<f32>), StackError> {
    let (w, h) = (guide.width(), guide.height());
    let mut f = Image::new(w, h, 2, ColorSpace::LINEAR)?;
    f.par_rows_mut().zip(values.par_rows().zip(weights.par_rows())).for_each(|(out, (v, k))| {
        for ((o, &v), &k) in out.as_chunks_mut::<2>().0.iter_mut().zip(v).zip(k) {
            *o = [k * v, k];
        }
    });
    if sigma_s > 0.0 {
        #[allow(clippy::cast_possible_truncation)] // A ratio of lengths, finite and positive.
        let ratio = (sigma_s / sigma_r.max(1e-6)) as f32;
        let across = distances(guide, ratio)?;
        let down = distances(&transpose(guide)?, ratio)?;
        let n = f64::from(PASSES);
        for pass in 0..PASSES {
            // Gastal and Oliveira's equation 14: the passes' deviations halve, their variances summing to sigma_s^2.
            let sigma = sigma_s * 3.0_f64.sqrt() * 2.0_f64.powi(PASSES - pass - 1) / (4.0_f64.powf(n) - 1.0).sqrt();
            #[allow(clippy::cast_possible_truncation)] // Between 0 and 1.
            let a = (-(2.0_f64.sqrt()) / sigma).exp() as f32;
            recursive_rows(&mut f, &across, a);
            let mut t = transpose(&f)?;
            recursive_rows(&mut t, &down, a);
            f = transpose(&t)?;
        }
    }
    let mut smoothed = Image::new(w, h, 1, ColorSpace::LINEAR)?;
    let mut weight = Image::new(w, h, 1, ColorSpace::LINEAR)?;
    smoothed.par_rows_mut().zip(weight.par_rows_mut()).zip(f.par_rows()).for_each(|((s, k), f)| {
        for ((s, k), &[p, w]) in s.iter_mut().zip(k.iter_mut()).zip(f.as_chunks::<2>().0) {
            (*s, *k) = if w > 1e-30 { (p / w, w) } else { (0.0, 0.0) };
        }
    });
    Ok((smoothed, weight))
}

/// A level of the push-pull pyramid: premultiplied values and weights from 0 to 1, interleaved.
struct Level {
    width: usize,
    height: usize,
    data: Vec<f32>,
}

impl Level {
    fn at(&self, x: usize, y: usize) -> (f32, f32) {
        let i = 2 * (y * self.width + x);
        (self.data.get(i).copied().unwrap_or(0.0), self.data.get(i + 1).copied().unwrap_or(0.0))
    }

    /// The next coarser level: 2x2 blocks summed, normalized where their weight passes 1.
    fn reduce(&self) -> Level {
        let (width, height) = (self.width.div_ceil(2), self.height.div_ceil(2));
        let mut data = vec![0.0; 2 * width * height];
        data.par_chunks_mut(2 * width).enumerate().for_each(|(y, row)| {
            for (x, out) in row.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let (mut p, mut k) = (0.0, 0.0);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = (2 * x + dx, 2 * y + dy);
                    if sx < self.width && sy < self.height {
                        let (pv, kv) = self.at(sx, sy);
                        p += pv;
                        k += kv;
                    }
                }
                if k > 1.0 {
                    p /= k;
                    k = 1.0;
                }
                *out = [p, k];
            }
        });
        Level { width, height, data }
    }
}

/// The values bilinearly interpolated from the coarser `level` at a finer level's pixel `(x, y)`.
fn upsampled(level: &[f32], width: usize, height: usize, x: usize, y: usize) -> f32 {
    #[allow(clippy::cast_precision_loss)] // Canvas coordinates, far below 2^24.
    let (fx, fy) = ((x as f32 * 0.5 - 0.25).max(0.0), (y as f32 * 0.5 - 0.25).max(0.0));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Non-negative and within the level.
    let (x0, y0) = ((fx as usize).min(width - 1), (fy as usize).min(height - 1));
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    #[allow(clippy::cast_precision_loss)]
    let (tx, ty) = ((fx - x0 as f32).clamp(0.0, 1.0), (fy - y0 as f32).clamp(0.0, 1.0));
    let v = |x: usize, y: usize| level.get(y * width + x).copied().unwrap_or(0.0);
    let top = v(x0, y0) + tx * (v(x1, y0) - v(x0, y0));
    let bottom = v(x0, y1) + tx * (v(x1, y1) - v(x0, y1));
    top + ty * (bottom - top)
}

/// `values` filled where `weights` (from 0 to 1) is below 1, from coarser scales: see the module documentation. Where
/// no pixel has any weight, the result is 0.
pub(crate) fn push_pull(values: &Image<f32>, weights: &Image<f32>) -> Result<Image<f32>, StackError> {
    let (w, h) = (values.width(), values.height());
    let mut base = vec![0.0; 2 * w * h];
    base.par_chunks_mut(2 * w).zip(values.par_rows().zip(weights.par_rows())).for_each(|(out, (v, k))| {
        for ((o, &v), &k) in out.as_chunks_mut::<2>().0.iter_mut().zip(v).zip(k) {
            let k = if k.is_finite() { k.clamp(0.0, 1.0) } else { 0.0 };
            *o = [k * v, k];
        }
    });
    let mut levels = vec![Level { width: w, height: h, data: base }];
    while let Some(last) = levels.last()
        && (last.width > 1 || last.height > 1)
    {
        let next = last.reduce();
        levels.push(next);
    }
    // Pull: the coarsest level's value, then each finer level's own value topped up from the coarser one.
    let mut coarse: Vec<f32> =
        levels.last().map(|l| l.data.as_chunks::<2>().0.iter().map(|&[p, k]| if k > 0.0 { p / k } else { 0.0 }).collect()).unwrap_or_default();
    let (mut cw, mut ch) = levels.last().map_or((1, 1), |l| (l.width, l.height));
    for level in levels.iter().rev().skip(1) {
        let mut fine = vec![0.0; level.width * level.height];
        fine.par_chunks_mut(level.width).enumerate().for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                let (p, k) = level.at(x, y);
                *v = p + (1.0 - k) * upsampled(&coarse, cw, ch, x, y);
            }
        });
        coarse = fine;
        (cw, ch) = (level.width, level.height);
    }
    Ok(Image::from_vec(w, h, 1, ColorSpace::LINEAR, coarse)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Image<f32> {
        Image::from_fn(w, h, 1, ColorSpace::LINEAR, |x, y, _| f(x, y)).unwrap()
    }

    #[test]
    fn transposing_swaps_rows_and_columns() {
        let image = Image::from_fn(150, 70, 2, ColorSpace::LINEAR, |x, y, c| (x * 1000 + y * 10 + c) as f32).unwrap();
        let t = transpose(&image).unwrap();
        assert_eq!((t.width(), t.height(), t.channels()), (70, 150, 2));
        for (x, y) in [(0, 0), (149, 69), (64, 3), (3, 64), (130, 65)] {
            assert_eq!(t.pixel(y, x), image.pixel(x, y));
        }
        assert_eq!(transpose(&t).unwrap(), image);
    }

    #[test]
    fn smoothing_stops_at_the_guides_edges() {
        // A guide with an edge down the middle; values 0 on the left and 1 on the right, weighted everywhere but a
        // gap on each side of the edge.
        let guide = plane(80, 40, |x, _| if x < 40 { 0.2 } else { 0.8 });
        let values = plane(80, 40, |x, _| if x < 40 { 0.0 } else { 1.0 });
        let weights = plane(80, 40, |x, _| if (30..50).contains(&x) { 0.0 } else { 1.0 });
        let (smoothed, weight) = smooth(&values, &weights, &guide, 16.0, 0.1).unwrap();
        // The gap is filled from its own side of the edge.
        assert!(smoothed.get(38, 20, 0).unwrap() < 0.01, "{}", smoothed.get(38, 20, 0).unwrap());
        assert!(smoothed.get(41, 20, 0).unwrap() > 0.99, "{}", smoothed.get(41, 20, 0).unwrap());
        assert!(weight.get(35, 20, 0).unwrap() > 0.0 && weight.get(10, 20, 0).unwrap() > 0.9);
        // Without the edge, the values blend across.
        let flat = plane(80, 40, |_, _| 0.5);
        let (blended, _) = smooth(&values, &weights, &flat, 16.0, 0.1).unwrap();
        let mid = blended.get(40, 20, 0).unwrap();
        assert!((0.2..0.8).contains(&mid), "{mid}");
        // No smoothing changes no weighted value.
        let (same, _) = smooth(&values, &weights, &guide, 0.0, 0.1).unwrap();
        assert_eq!(same.get(10, 5, 0), Some(0.0));
        assert_eq!(same.get(70, 5, 0), Some(1.0));
    }

    #[test]
    fn push_pull_fills_holes_and_keeps_weighted_pixels() {
        let values = plane(37, 23, |x, y| (x + y) as f32 * 0.1);
        let weights = plane(37, 23, |x, y| if (10..25).contains(&x) && (5..18).contains(&y) { 0.0 } else { 1.0 });
        let filled = push_pull(&values, &weights).unwrap();
        assert_eq!(filled.get(3, 3, 0), values.get(3, 3, 0), "full weight keeps the value");
        let inside = filled.get(17, 11, 0).unwrap();
        let (lo, hi) = (values.as_raw().iter().copied().fold(f32::MAX, f32::min), values.as_raw().iter().copied().fold(f32::MIN, f32::max));
        assert!(inside > lo && inside < hi, "{inside}");
        // Smooth: neighbors in the hole are close.
        assert!((filled.get(17, 11, 0).unwrap() - filled.get(18, 11, 0).unwrap()).abs() < 0.5);
        assert!(push_pull(&values, &plane(37, 23, |_, _| 0.0)).unwrap().as_raw().iter().all(|&v| v == 0.0));
        let one = push_pull(&plane(1, 1, |_, _| 2.0), &plane(1, 1, |_, _| 0.5)).unwrap();
        assert_eq!(one.get(0, 0, 0), Some(2.0));
    }
}
