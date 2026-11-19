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

/// The columns of a strip that [`recursive_columns`] filters on one thread.
const STRIP: usize = 64;

/// What the domain transform's recursive filter needs of a guide, made once for every plane smoothed with it: the
/// spatial standard deviation and, when it is above 0, the distance from each pixel to the one before it in its row
/// (`across`) and in its column (`down`), `1 + (sigma_s / sigma_r) |guide difference|` (the first's is unused).
pub(crate) struct Domain {
    width: usize,
    height: usize,
    sigma_s: f64,
    distances: Option<(Image<f32>, Image<f32>)>,
}

impl Domain {
    /// The domain of `guide` for a smoothing of `sigma_s` pixels that stops at edges of `sigma_r` in the guide's units.
    pub(crate) fn new(guide: &Image<f32>, sigma_s: f64, sigma_r: f64) -> Result<Domain, StackError> {
        let (width, height) = (guide.width(), guide.height());
        if sigma_s <= 0.0 {
            return Ok(Domain { width, height, sigma_s, distances: None });
        }
        #[allow(clippy::cast_possible_truncation)] // A ratio of lengths, finite and positive.
        let ratio = (sigma_s / sigma_r.max(1e-6)) as f32;
        let mut across = Image::new(width, height, 1, ColorSpace::LINEAR)?;
        across.par_rows_mut().zip(guide.par_rows()).for_each(|(d, g)| {
            for (x, v) in d.iter_mut().enumerate() {
                let diff = match (x.checked_sub(1).and_then(|p| g.get(p)), g.get(x)) {
                    (Some(&a), Some(&b)) => (b - a).abs(),
                    _ => 0.0,
                };
                *v = 1.0 + ratio * diff;
            }
        });
        let mut down = Image::new(width, height, 1, ColorSpace::LINEAR)?;
        down.par_rows_mut().enumerate().for_each(|(y, d)| {
            let (above, row) = (y.checked_sub(1).and_then(|p| guide.row(p)), guide.row(y));
            for (x, v) in d.iter_mut().enumerate() {
                let diff = match (above.and_then(|r| r.get(x)), row.and_then(|r| r.get(x))) {
                    (Some(&a), Some(&b)) => (b - a).abs(),
                    _ => 0.0,
                };
                *v = 1.0 + ratio * diff;
            }
        });
        Ok(Domain { width, height, sigma_s, distances: Some((across, down)) })
    }

    /// An error unless `plane` is a plane of one channel of the guide's size.
    fn check(&self, plane: &Image<f32>) -> Result<(), StackError> {
        if (plane.width(), plane.height(), plane.channels()) == (self.width, self.height, 1) {
            return Ok(());
        }
        Err(StackError::Plane {
            what: "plane to smooth",
            width: plane.width(),
            height: plane.height(),
            channels: plane.channels(),
            expected_width: self.width,
            expected_height: self.height,
        })
    }

    /// The recursive filter's passes over `f`, a plane of the guide's size, in place.
    fn filter(&self, f: &mut Image<f32>) {
        let Some((across, down)) = &self.distances else { return };
        let n = f64::from(PASSES);
        for pass in 0..PASSES {
            // Gastal and Oliveira's equation 14: the passes' deviations halve, their variances summing to sigma_s^2.
            let sigma = self.sigma_s * 3.0_f64.sqrt() * 2.0_f64.powi(PASSES - pass - 1) / (4.0_f64.powf(n) - 1.0).sqrt();
            #[allow(clippy::cast_possible_truncation)] // Between 0 and 1.
            let a = (-(2.0_f64.sqrt()) / sigma).exp() as f32;
            recursive_rows(f, across, a);
            recursive_columns(f, down, a);
        }
    }
}

/// One recursive pass, forth and back along every row, of a plane of one or two channels (the weighted values and the
/// weights, or the weights alone), with feedback `a^distance`. Each channel's recursion is its own.
fn recursive_rows(f: &mut Image<f32>, distances: &Image<f32>, a: f32) {
    let ln_a = a.ln();
    let ch = f.channels().max(1);
    f.par_rows_mut().zip(distances.par_rows()).for_each_init(Vec::new, |feedback, (row, d)| {
        feedback.clear();
        feedback.extend(d.iter().map(|&d| (ln_a * d).exp()));
        let w = feedback.len().min(row.len() / ch);
        for x in 1..w {
            let v = feedback[x];
            let (prev, cur) = row.split_at_mut(ch * x);
            for (c, p) in cur.iter_mut().take(ch).zip(&prev[ch * x - ch..]) {
                *c += v * (*p - *c);
            }
        }
        for x in (0..w.saturating_sub(1)).rev() {
            let v = feedback[x + 1];
            let (cur, next) = row.split_at_mut(ch * x + ch);
            for (c, n) in cur[ch * x..].iter_mut().zip(next.iter()) {
                *c += v * (*n - *c);
            }
        }
    });
}

/// One recursive pass, down and back up every column, of a plane of one or two channels, with feedback `a^distance` from `down`: the same arithmetic as [`recursive_rows`] on the plane transposed, in place.
/// Each thread filters a strip of [`STRIP`] columns from the top row to the bottom one.
fn recursive_columns(f: &mut Image<f32>, down: &Image<f32>, a: f32) {
    let ln_a = a.ln();
    let ch = f.channels().max(1);
    let strips = f.width().div_ceil(STRIP);
    let mut columns: Vec<Vec<&mut [f32]>> = (0..strips).map(|_| Vec::with_capacity(f.height())).collect();
    for row in f.rows_mut() {
        for (strip, segment) in columns.iter_mut().zip(row.chunks_mut(ch * STRIP)) {
            strip.push(segment);
        }
    }
    let down: Vec<&[f32]> = down.rows().collect();
    columns.into_par_iter().enumerate().for_each_init(Vec::new, |feedback: &mut Vec<f32>, (s, mut rows)| {
        let x0 = s * STRIP;
        let width = rows.first().map_or(0, |r| r.len() / ch);
        feedback.clear();
        for d in &down {
            feedback.extend(d.get(x0..x0 + width).unwrap_or_default().iter().map(|&d| (ln_a * d).exp()));
        }
        if feedback.len() != width * rows.len() {
            return;
        }
        for y in 1..rows.len() {
            let (above, below) = rows.split_at_mut(y);
            let (Some(p), Some(c), Some(v)) = (above.last(), below.first_mut(), feedback.get(y * width..(y + 1) * width)) else { continue };
            for ((c, p), &v) in c.chunks_exact_mut(ch).zip(p.chunks_exact(ch)).zip(v) {
                for (c, &p) in c.iter_mut().zip(p) {
                    *c += v * (p - *c);
                }
            }
        }
        for y in (0..rows.len().saturating_sub(1)).rev() {
            let (above, below) = rows.split_at_mut(y + 1);
            let (Some(c), Some(n), Some(v)) = (above.last_mut(), below.first(), feedback.get((y + 1) * width..(y + 2) * width)) else { continue };
            for ((c, n), &v) in c.chunks_exact_mut(ch).zip(n.chunks_exact(ch)).zip(v) {
                for (c, &n) in c.iter_mut().zip(n) {
                    *c += v * (n - *c);
                }
            }
        }
    });
}

/// Edge-aware normalized smoothing of `values` weighted by `weights` (both one channel, the size of the `domain`'s
/// guide), guided by that guide: the smoothed values (0 where no weight reaches) and the smoothed weights. See the
/// module documentation.
pub(crate) fn smooth(values: &Image<f32>, weights: &Image<f32>, domain: &Domain) -> Result<(Image<f32>, Image<f32>), StackError> {
    let (w, h) = (domain.width, domain.height);
    domain.check(values)?;
    domain.check(weights)?;
    let mut f = Image::new(w, h, 2, ColorSpace::LINEAR)?;
    f.par_rows_mut().zip(values.par_rows().zip(weights.par_rows())).for_each(|(out, (v, k))| {
        for ((o, &v), &k) in out.as_chunks_mut::<2>().0.iter_mut().zip(v).zip(k) {
            *o = [k * v, k];
        }
    });
    domain.filter(&mut f);
    let mut smoothed = Image::new(w, h, 1, ColorSpace::LINEAR)?;
    let mut weight = Image::new(w, h, 1, ColorSpace::LINEAR)?;
    smoothed.par_rows_mut().zip(weight.par_rows_mut()).zip(f.par_rows()).for_each(|((s, k), f)| {
        for ((s, k), &[p, w]) in s.iter_mut().zip(k.iter_mut()).zip(f.as_chunks::<2>().0) {
            (*s, *k) = if w > 1e-30 { (p / w, w) } else { (0.0, 0.0) };
        }
    });
    Ok((smoothed, weight))
}

/// The weights [`smooth`] gives for `weights`, alone: each channel's recursion is its own, so the weights smoothed
/// without values are the same, bit for bit, for half the work.
pub(crate) fn smooth_weights(weights: &Image<f32>, domain: &Domain) -> Result<Image<f32>, StackError> {
    domain.check(weights)?;
    let mut f = weights.clone();
    domain.filter(&mut f);
    f.par_rows_mut().for_each(|row| row.iter_mut().for_each(|w| *w = if *w > 1e-30 { *w } else { 0.0 }));
    Ok(f)
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

    fn transpose(image: &Image<f32>) -> Image<f32> {
        Image::from_fn(image.height(), image.width(), image.channels(), ColorSpace::LINEAR, |x, y, c| image.get(y, x, c).unwrap()).unwrap()
    }

    #[test]
    fn the_column_pass_is_the_row_pass_on_the_transposed_plane() {
        // Strips of every width: whole ones, a partial last one, and a canvas narrower than a strip; and one row.
        for (w, h) in [(2 * STRIP, 37), (STRIP + 5, 50), (7, 90), (150, 1)] {
            let f = Image::from_fn(w, h, 2, ColorSpace::LINEAR, |x, y, c| (((x * 31 + y * 17 + c * 7) % 23) as f32).sin()).unwrap();
            let guide = plane(w, h, |x, y| ((x * 13 + y * 29) % 19) as f32 / 19.0);
            let domain = Domain::new(&guide, 8.0, 0.05).unwrap();
            let (_, down) = domain.distances.as_ref().unwrap();
            let across_transposed = Domain::new(&transpose(&guide), 8.0, 0.05).unwrap().distances.unwrap().0;
            assert_eq!(transpose(down), across_transposed, "{w}x{h}: the distances down the columns");
            let mut by_columns = f.clone();
            recursive_columns(&mut by_columns, down, 0.7);
            let mut by_rows = transpose(&f);
            recursive_rows(&mut by_rows, &across_transposed, 0.7);
            assert_eq!(by_columns, transpose(&by_rows), "{w}x{h}: the same arithmetic, bit for bit");
        }
    }

    #[test]
    fn weights_smoothed_alone_are_the_weights_smoothed_with_values() {
        let guide = plane(90, 70, |x, y| ((x * 7 + y * 3) % 31) as f32 / 31.0);
        let values = plane(90, 70, |x, y| (x + 2 * y) as f32);
        let weights = plane(90, 70, |x, y| if (x * y) % 5 == 0 { 0.0 } else { 1.0 });
        for sigma in [0.0, 4.0, 16.0] {
            let domain = Domain::new(&guide, sigma, 0.05).unwrap();
            let (_, together) = smooth(&values, &weights, &domain).unwrap();
            assert_eq!(smooth_weights(&weights, &domain).unwrap(), together, "sigma {sigma}");
        }
        assert!(smooth_weights(&plane(9, 70, |_, _| 1.0), &Domain::new(&guide, 4.0, 0.05).unwrap()).is_err(), "a plane of another size");
    }

    #[test]
    fn smoothing_stops_at_the_guides_edges() {
        // A guide with an edge down the middle; values 0 on the left and 1 on the right, weighted everywhere but a
        // gap on each side of the edge.
        let guide = plane(80, 40, |x, _| if x < 40 { 0.2 } else { 0.8 });
        let values = plane(80, 40, |x, _| if x < 40 { 0.0 } else { 1.0 });
        let weights = plane(80, 40, |x, _| if (30..50).contains(&x) { 0.0 } else { 1.0 });
        let (smoothed, weight) = smooth(&values, &weights, &Domain::new(&guide, 16.0, 0.1).unwrap()).unwrap();
        // The gap is filled from its own side of the edge.
        assert!(smoothed.get(38, 20, 0).unwrap() < 0.01, "{}", smoothed.get(38, 20, 0).unwrap());
        assert!(smoothed.get(41, 20, 0).unwrap() > 0.99, "{}", smoothed.get(41, 20, 0).unwrap());
        assert!(weight.get(35, 20, 0).unwrap() > 0.0 && weight.get(10, 20, 0).unwrap() > 0.9);
        // Without the edge, the values blend across.
        let flat = plane(80, 40, |_, _| 0.5);
        let (blended, _) = smooth(&values, &weights, &Domain::new(&flat, 16.0, 0.1).unwrap()).unwrap();
        let mid = blended.get(40, 20, 0).unwrap();
        assert!((0.2..0.8).contains(&mid), "{mid}");
        // No smoothing changes no weighted value.
        let (same, _) = smooth(&values, &weights, &Domain::new(&guide, 0.0, 0.1).unwrap()).unwrap();
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
