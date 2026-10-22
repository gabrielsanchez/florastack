//! Phase correlation (C. D. Kuglin and D. C. Hines, "The phase correlation image alignment method", Proceedings of
//! the IEEE International Conference on Cybernetics and Society, 1975): the shift between two images from the phase
//! of their cross-power spectrum, which gives alignment its first guess, and the peaks of their correlation, which
//! tell a repeating pattern (a grid, a fabric) from a unique match.
//!
//! Both images are made zero-mean, tapered to zero at their edges by a Hann window so that the borders don't
//! correlate, and zero-padded to powers of two. Two correlation surfaces come from their spectra, both weighted by a
//! Gaussian low-pass: a defocused frame keeps the phase of its low frequencies (a symmetric blur only scales them) but
//! has only noise left at high ones, which the weight keeps out.
//!
//! - **The shift** comes from the whitened cross-power spectrum, in which every frequency counts alike whatever the
//!   scene's contrast. Its peak is sharp, about a pixel wide with the low-pass, and a parabola through the peak and its
//!   neighbors finds it to a fraction of a pixel. Whitening also lets fine detail that doesn't repeat outvote a strong
//!   pattern that does, which picks the right period of a grid when the scene has anything else to go by.
//! - **The ambiguity** comes from the plain cross-correlation, in which frequencies count by their energy, as they do
//!   in the correlation coefficient that alignment then maximizes. A unique match has one peak there; a pattern that
//!   repeats every `p` pixels has peaks every `p` pixels, nearly as high, and alignment could settle on any of them.
//!   The highest peak beyond the chosen one, as a fraction of the chosen one, measures that.
//!
//! The surfaces peak at the shift `d` for which `moving(x + d)` matches `reference(x)`. When several peaks of the
//! whitened surface are about as high, the one nearest the expected shift is taken.

use florastack_core::Image;

use crate::fft::{Complex, Fft2};

/// The low-pass weight's standard deviation, in cycles per pixel: the whitened peak is then a Gaussian of about one
/// pixel.
const LOW_PASS: f64 = 0.15;
/// The whitening floor, relative to the mean magnitude of the cross-power spectrum: keeps frequencies with almost no
/// energy from amplifying rounding noise.
const FLOOR: f64 = 1e-3;
/// Peaks nearer than this to the chosen one, in pixels, are part of it.
const PEAK_RADIUS: f64 = 3.0;
/// Whitened peaks at least this high, relative to the highest, compete with it: the one nearest the expected shift
/// is taken.
const NEAR_EQUAL: f64 = 0.8;

/// What phase correlation found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Correlation {
    /// The shift `d`, to a fraction of a pixel, for which `moving(x + d)` matches `reference(x)`.
    pub shift: (f64, f64),
    /// The highest peak of the plain correlation beyond the chosen shift, as a fraction of the peak at the chosen
    /// shift: near 0 for a unique match, near 1 or above for a pattern that repeats.
    pub ambiguity: f64,
}

/// Correlates channel `channel` of two images of the same size, looking for shifts of at most `max_shift` pixels
/// along each axis. `None` when the images are too small, differ in size or have no detail, or when no peak lies in
/// range.
pub(crate) fn correlate(
    reference: &Image<f32>,
    moving: &Image<f32>,
    channel: usize,
    max_shift: (f64, f64),
    expected: (f64, f64),
) -> Option<Correlation> {
    let (w, h) = (reference.width(), reference.height());
    if moving.width() != w || moving.height() != h || w < 4 || h < 4 {
        return None;
    }
    let fft = Fft2::new(w.next_power_of_two(), h.next_power_of_two())?;
    let (nw, nh) = (fft.width(), fft.height());
    let mut a = windowed(reference, channel, nw, nh)?;
    let mut b = windowed(moving, channel, nw, nh)?;
    fft.forward(&mut a);
    fft.forward(&mut b);

    // The cross-power spectrum conj(A) B, low-passed: plain, and whitened.
    let mut plain: Vec<Complex> = a.iter().zip(&b).map(|(a, b)| a.conj() * *b).collect();
    let mean = plain.iter().map(|c| c.norm()).sum::<f64>() / plain.len() as f64;
    if !(mean.is_finite() && mean > 0.0) {
        return None;
    }
    let floor = FLOOR * mean;
    let mut whitened = plain.clone();
    for (k, (p, white)) in plain.iter_mut().zip(&mut whitened).enumerate() {
        let (fx, fy) = (frequency(k % nw, nw), frequency(k / nw, nh));
        let weight = (-(fx * fx + fy * fy) / (2.0 * LOW_PASS * LOW_PASS)).exp();
        *white = p.scale(weight / (p.norm() + floor));
        *p = p.scale(weight);
    }
    fft.inverse(&mut plain);
    fft.inverse(&mut whitened);
    let surface = |values: Vec<Complex>| Surface { values: values.iter().map(|c| c.re).collect(), width: nw, height: nh };
    let (plain, whitened) = (surface(plain), surface(whitened));

    let reach = |max: f64, n: usize| to_reach(max.min((n / 2) as f64 - 2.0));
    let range = (reach(max_shift.0, nw), reach(max_shift.1, nh));
    let peaks = whitened.peaks(range);
    let &(highest, _, _) = peaks.first()?;
    if highest.is_nan() || highest <= 0.0 {
        return None;
    }
    // Among the whitened peaks about as high as the highest, the one nearest the expected shift.
    let distance = |dx: isize, dy: isize, to: (f64, f64)| (dx as f64 - to.0).hypot(dy as f64 - to.1);
    let &(_, px, py) = peaks
        .iter()
        .take_while(|p| p.0 >= NEAR_EQUAL * highest)
        .min_by(|a, b| distance(a.1, a.2, expected).total_cmp(&distance(b.1, b.2, expected)))?;
    let shift = (px as f64 + whitened.vertex(px, py, (1, 0)), py as f64 + whitened.vertex(px, py, (0, 1)));

    // The plain correlation's peak at the chosen shift, and the highest one beyond it.
    let plain_peaks = plain.peaks(range);
    let chosen = (px as f64, py as f64);
    let at_shift = plain_peaks.iter().find(|p| distance(p.1, p.2, chosen) <= PEAK_RADIUS).map_or_else(|| plain.at(px, py), |p| p.0);
    let rival = plain_peaks.iter().find(|p| distance(p.1, p.2, chosen) > PEAK_RADIUS).map_or(0.0, |p| p.0);
    let ambiguity = if at_shift > 0.0 { (rival / at_shift).max(0.0) } else { 1.0 };
    Some(Correlation { shift, ambiguity })
}

/// A correlation surface, indexed by shift with wrap-around.
struct Surface {
    values: Vec<f64>,
    width: usize,
    height: usize,
}

impl Surface {
    fn at(&self, dx: isize, dy: isize) -> f64 {
        let x = dx.rem_euclid(self.width.cast_signed()).cast_unsigned();
        let y = dy.rem_euclid(self.height.cast_signed()).cast_unsigned();
        self.values.get(y * self.width + x).copied().unwrap_or(f64::NEG_INFINITY)
    }

    /// The local maxima within `range` pixels of no shift along each axis, as `(value, dx, dy)`, highest first.
    fn peaks(&self, range: (isize, isize)) -> Vec<(f64, isize, isize)> {
        const AROUND: [(isize, isize); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];
        let mut peaks = Vec::new();
        for dy in -range.1..=range.1 {
            for dx in -range.0..=range.0 {
                let v = self.at(dx, dy);
                if AROUND.iter().all(|&(i, j)| self.at(dx + i, dy + j) < v) {
                    peaks.push((v, dx, dy));
                }
            }
        }
        peaks.sort_by(|a, b| b.0.total_cmp(&a.0).then((a.2, a.1).cmp(&(b.2, b.1))));
        peaks
    }

    /// The offset, within half a pixel, of the vertex of the parabola through the peak at `(x, y)` and its two
    /// neighbors along `axis`.
    fn vertex(&self, x: isize, y: isize, axis: (isize, isize)) -> f64 {
        let (before, at, after) = (self.at(x - axis.0, y - axis.1), self.at(x, y), self.at(x + axis.0, y + axis.1));
        let curvature = before - 2.0 * at + after;
        if curvature < 0.0 { (0.5 * (before - after) / curvature).clamp(-0.5, 0.5) } else { 0.0 }
    }
}

/// The frequency of bin `k` of `n`, in cycles per sample, from -1/2 to 1/2.
fn frequency(k: usize, n: usize) -> f64 {
    let k = k as f64;
    let n = n as f64;
    if k < n / 2.0 { k / n } else { (k - n) / n }
}

/// How many whole pixels a search reaches: `max` rounded up, plus one so that a shift right at the limit is found.
#[allow(clippy::cast_possible_truncation)] // Clamped to a million pixels at most.
fn to_reach(max: f64) -> isize {
    if max.is_finite() && max > 0.0 { (max.ceil() + 1.0).min(1e6) as isize } else { 0 }
}

/// Channel `channel` of `image`, zero-mean and Hann-windowed, in the top-left corner of an `nw` x `nh` buffer of zeros.
fn windowed(image: &Image<f32>, channel: usize, nw: usize, nh: usize) -> Option<Vec<Complex>> {
    let (w, h, c) = (image.width(), image.height(), image.channels());
    if channel >= c {
        return None;
    }
    let samples = |row: &[f32]| row.iter().skip(channel).step_by(c).map(|&v| f64::from(v)).collect::<Vec<f64>>();
    let mean = image.rows().map(|row| samples(row).iter().sum::<f64>()).sum::<f64>() / (w * h) as f64;
    let hann = |i: usize, n: usize| 0.5 - 0.5 * (std::f64::consts::TAU * (i as f64 + 0.5) / n as f64).cos();
    let wx: Vec<f64> = (0..w).map(|x| hann(x, w)).collect();
    let mut out = vec![Complex::ZERO; nw * nh];
    for (y, row) in image.rows().enumerate() {
        let wy = hann(y, h);
        let line = out.get_mut(y * nw..y * nw + w)?;
        for ((o, v), &wx) in line.iter_mut().zip(samples(row)).zip(&wx) {
            *o = Complex::new((v - mean) * wx * wy, 0.0);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use florastack_core::ops::{Border, Interpolation, warp};
    use florastack_core::testutil::scene;
    use florastack_core::{Affine, ColorSpace};

    use super::*;

    /// The green channel of a textured test scene.
    fn texture(w: usize, h: usize, seed: u64) -> Image<f32> {
        let (sharp, _) = scene::textured(w, h, seed).unwrap();
        Image::from_fn(w, h, 1, ColorSpace::LINEAR, |x, y, _| sharp.get(x, y, 1).unwrap()).unwrap()
    }

    /// `image` sampled at `x + shift`: its content moves by `-shift`.
    fn shifted(image: &Image<f32>, shift: (f64, f64)) -> Image<f32> {
        warp(image, &Affine::translation(shift.0, shift.1), Interpolation::Bicubic, Border::Clamp).unwrap()
    }

    #[test]
    fn phase_correlation_finds_whole_and_sub_pixel_shifts() {
        let reference = texture(128, 96, 1);
        for shift in [(0.0, 0.0), (5.0, -3.0), (-12.0, 7.0), (2.4, -1.7), (-0.3, 0.6)] {
            // moving(x + d) = reference(x) for the moving image sampled at x - d.
            let moving = shifted(&reference, (-shift.0, -shift.1));
            let c = correlate(&reference, &moving, 0, (20.0, 20.0), (0.0, 0.0)).unwrap();
            assert!((c.shift.0 - shift.0).abs() < 0.2 && (c.shift.1 - shift.1).abs() < 0.2, "{shift:?}: {c:?}");
            assert!(c.ambiguity < 0.5, "{shift:?}: a textured scene matches once: {c:?}");
        }
    }

    #[test]
    fn shifts_beyond_the_search_are_not_found() {
        let reference = texture(128, 96, 2);
        let moving = shifted(&reference, (-30.0, 0.0));
        let c = correlate(&reference, &moving, 0, (10.0, 10.0), (0.0, 0.0));
        assert!(c.is_none_or(|c| c.shift.0.abs() <= 11.0), "{c:?}");
    }

    #[test]
    fn repeating_patterns_are_ambiguous_and_resolved_toward_the_expected_shift() {
        // A grid with a period of 16 pixels.
        let grid = Image::from_fn(128, 128, 1, ColorSpace::LINEAR, |x, y, _| {
            let (fx, fy) = (x as f32 * std::f32::consts::TAU / 16.0, y as f32 * std::f32::consts::TAU / 16.0);
            0.5 + 0.3 * fx.sin() * fy.sin()
        })
        .unwrap();
        let moving = shifted(&grid, (-3.0, -2.0));
        let c = correlate(&grid, &moving, 0, (40.0, 40.0), (0.0, 0.0)).unwrap();
        assert!(c.ambiguity > 0.5, "{c:?}");
        assert!((c.shift.0 - 3.0).abs() < 0.3 && (c.shift.1 - 2.0).abs() < 0.3, "the peak nearest no motion: {c:?}");
        // sin(x) sin(y) repeats along (8, 8) and (8, -8) too.
        let c = correlate(&grid, &moving, 0, (40.0, 40.0), (12.0, -5.0)).unwrap();
        assert!((c.shift.0 - 11.0).abs() < 0.3 && (c.shift.1 + 6.0).abs() < 0.3, "the peak a period away, nearest what was expected: {c:?}");
        // With faint detail that doesn't repeat, the whitened shift finds the true period, and the plain correlation
        // still sees the pattern.
        let detail = texture(128, 128, 3);
        let textured =
            Image::from_fn(128, 128, 1, ColorSpace::LINEAR, |x, y, _| grid.get(x, y, 0).unwrap() + 0.03 * detail.get(x, y, 0).unwrap()).unwrap();
        let moving = shifted(&textured, (-3.0, -2.0));
        let c = correlate(&textured, &moving, 0, (40.0, 40.0), (12.0, -5.0)).unwrap();
        assert!((c.shift.0 - 3.0).abs() < 0.3 && (c.shift.1 - 2.0).abs() < 0.3, "{c:?}");
        assert!(c.ambiguity > 0.5, "{c:?}");
    }

    #[test]
    fn flat_or_mismatched_images_have_no_correlation() {
        let flat = Image::from_fn(32, 32, 1, ColorSpace::LINEAR, |_, _, _| 0.5).unwrap();
        assert_eq!(correlate(&flat, &flat, 0, (4.0, 4.0), (0.0, 0.0)), None);
        let other = texture(32, 16, 4);
        assert_eq!(correlate(&flat, &other, 0, (4.0, 4.0), (0.0, 0.0)), None);
        let tiny = texture(3, 3, 5);
        assert_eq!(correlate(&tiny, &tiny, 0, (1.0, 1.0), (0.0, 0.0)), None);
        assert_eq!(correlate(&other, &other, 1, (4.0, 4.0), (0.0, 0.0)), None, "no second channel");
    }
}
