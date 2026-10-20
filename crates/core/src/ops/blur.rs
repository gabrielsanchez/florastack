//! Gaussian blur, separable: rows first, then columns, each with clamp-to-edge borders.
//!
//! - **Exact**: the sampled Gaussian of standard deviation `sigma`, radius `ceil(3 sigma)`, normalized to sum 1. Each
//!   output sample sums its taps in order, starting from zero, so the result is the same bit for bit as the plain
//!   loop of the synthetic generator's reference blur.
//! - **Boxes**: for large sigmas, `n` passes of an extended box filter computed with running sums, so that the cost per
//!   pixel doesn't grow with the radius. A cascade of box filters tends to a Gaussian (W. M. Wells, "Efficient synthesis
//!   of Gaussian filters by cascaded uniform filters", IEEE Transactions on Pattern Analysis and Machine Intelligence
//!   8(2), 1986); the extended box adds a fractional weight at both ends so that the passes' variances add up to
//!   `sigma^2` exactly rather than to the nearest integer box widths (P. Gwosdek, S. Grewenig, A. Bruhn and J. Weickert,
//!   "Theoretical foundations of Gaussian convolution by extended box filtering", Scale Space and Variational Methods in
//!   Computer Vision, LNCS 6667, 2011). The passes run in `f64`, rounded to `f32` once at the end.
//!
//! Both work in place. Rows are filtered in parallel, each through a scratch row. The exact column pass goes down the
//! image in bands of rows, keeping the original rows it still needs in one ring of `64 + 2 ceil(3 sigma)` rows and
//! computing each band's rows in parallel. The box column passes stream vertical strips through all the passes at
//! once, each pass keeping its last `2 r + 3` input rows, so that a strip is read and written only once; a strip's
//! scratch stays under 1 MiB. (Copying whole strips into buffers allocated afresh in every parallel task cost more in
//! page faults than the filtering itself, which is why the scratch is kept small.)

use rayon::prelude::*;

use super::{OpError, to_f32, to_index};
use crate::image::Image;

/// The largest sigma that [`BlurMethod::Auto`] blurs with the exact kernel; beyond it, with box filters, which cost
/// the same at any sigma (on a 24 MP image they break even with the exact kernel around this sigma).
pub const AUTO_EXACT_MAX_SIGMA: f64 = 4.0;
/// The number of box passes [`BlurMethod::Auto`] uses.
const AUTO_BOX_PASSES: usize = 4;
/// The largest sigma [`BlurMethod::Exact`] accepts: its kernel has `2 ceil(3 sigma) + 1` taps.
pub const MAX_EXACT_SIGMA: f64 = 256.0;
/// The largest sigma any method accepts, far beyond any image.
pub const MAX_SIGMA: f64 = 10_000.0;
/// The most passes [`BlurMethod::Boxes`] may ask for.
pub const MAX_BOX_PASSES: usize = 8;
/// The rows per band of the exact column pass.
const BAND: usize = 64;
/// The widest vertical strip, in samples, of the box column passes.
const STRIP: usize = 512;
/// The most scratch memory one box strip may use, in bytes; wider radii make for narrower strips.
const STRIP_BYTES: usize = 1 << 20;

/// How [`gaussian_blur_in_place`] blurs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlurMethod {
    /// [`BlurMethod::Exact`] up to [`AUTO_EXACT_MAX_SIGMA`], four box passes beyond.
    #[default]
    Auto,
    /// The sampled Gaussian kernel, up to [`MAX_EXACT_SIGMA`].
    Exact,
    /// `passes` extended box filters (1 to [`MAX_BOX_PASSES`]) whose variances add up to `sigma^2`. More passes are
    /// closer to a Gaussian: along one axis the largest difference from the sampled Gaussian is about 6% of its peak
    /// with three passes, 3.6% with four and 2.5% with six (about twice that for a 2D impulse); smooth images differ
    /// far less.
    Boxes { passes: usize },
}

/// The normalized Gaussian kernel of standard deviation `sigma` pixels, with radius `ceil(3 sigma)`: `[1.0]` when
/// `sigma` is zero, negative or NaN. Weights are computed in `f64` and rounded to `f32`.
pub fn gaussian_kernel(sigma: f64) -> Vec<f32> {
    if sigma.is_nan() || sigma <= 0.0 {
        return vec![1.0];
    }
    let radius = to_index((3.0 * sigma).ceil());
    let weights: Vec<f64> = (0..=2 * radius).map(|i| (-((i as f64 - radius as f64).powi(2)) / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f64 = weights.iter().sum();
    weights.iter().map(|w| to_f32(w / sum)).collect()
}

/// `image` blurred with a Gaussian of standard deviation `sigma` pixels, by [`BlurMethod::Auto`].
pub fn gaussian_blur(image: &Image<f32>, sigma: f64) -> Result<Image<f32>, OpError> {
    let mut out = image.clone();
    gaussian_blur_in_place(&mut out, sigma, BlurMethod::Auto)?;
    Ok(out)
}

/// Blurs `image` in place with a Gaussian of standard deviation `sigma` pixels (nothing happens at 0).
pub fn gaussian_blur_in_place(image: &mut Image<f32>, sigma: f64, method: BlurMethod) -> Result<(), OpError> {
    if !(sigma.is_finite() && (0.0..=MAX_SIGMA).contains(&sigma)) {
        return Err(OpError::Param { name: "sigma", value: sigma, expected: "finite, from 0 to 10000" });
    }
    let passes = match method {
        BlurMethod::Auto if sigma <= AUTO_EXACT_MAX_SIGMA => None,
        BlurMethod::Auto => Some(AUTO_BOX_PASSES),
        BlurMethod::Exact if sigma > MAX_EXACT_SIGMA => {
            return Err(OpError::Param { name: "sigma", value: sigma, expected: "at most 256 for the exact kernel" });
        }
        BlurMethod::Exact => None,
        BlurMethod::Boxes { passes } if passes == 0 || passes > MAX_BOX_PASSES => {
            return Err(OpError::Param { name: "passes", value: passes as f64, expected: "from 1 to 8" });
        }
        BlurMethod::Boxes { passes } => Some(passes),
    };
    if sigma == 0.0 {
        return Ok(());
    }
    match passes {
        None => blur_exact(image, &gaussian_kernel(sigma)),
        Some(passes) => {
            blur_boxes(image, &ExtendedBox::for_sigma(sigma / (passes as f64).sqrt()), passes);
            Ok(())
        }
    }
}

/// The exact separable blur with `kernel` (odd length).
fn blur_exact(image: &mut Image<f32>, kernel: &[f32]) -> Result<(), OpError> {
    let (w, c) = (image.width(), image.channels());
    image.par_rows_mut().for_each_init(Vec::new, |scratch, row| {
        scratch.clear();
        scratch.extend_from_slice(row);
        convolve_row(scratch, row, kernel, w, c);
    });
    columns_exact(image, kernel)
}

/// `dst[x c + ch] = sum over k of kernel[k] src[clamp(x + k - r) c + ch]`, summed in order of `k` from zero, for a row
/// of `w` pixels of `c` channels. The taps that stay inside the row are added a whole slice at a time, which
/// vectorizes; only the clamped ones near the ends go pixel by pixel.
fn convolve_row(src: &[f32], dst: &mut [f32], kernel: &[f32], w: usize, c: usize) {
    let r = kernel.len() / 2;
    let (Some(first), Some(last)) = (src.get(..c), src.get((w - 1) * c..w * c)) else {
        return;
    };
    dst.fill(0.0);
    for (k, &weight) in kernel.iter().enumerate() {
        // Output pixel x reads pixel x + k - r: clamped to 0 for x < r - k, to w - 1 for x >= w + r - k.
        let left = r.saturating_sub(k).min(w);
        let right = (w + r).saturating_sub(k).clamp(left, w);
        for out in dst[..left * c].chunks_exact_mut(c) {
            for (o, s) in out.iter_mut().zip(first) {
                *o += weight * s;
            }
        }
        if left < right {
            let from = (left + k - r) * c;
            for (o, s) in dst[left * c..right * c].iter_mut().zip(&src[from..]) {
                *o += weight * s;
            }
        }
        for out in dst[right * c..].chunks_exact_mut(c) {
            for (o, s) in out.iter_mut().zip(last) {
                *o += weight * s;
            }
        }
    }
}

/// The exact column pass, in place: output row `y` is `sum over k of kernel[k] row(clamp(y + k - r))` of the original
/// rows, summed in order of `k` from zero. Bands of [`BAND`] rows are computed in parallel from a ring that holds the
/// original rows they need: row `i` lives in slot `i % slots` from just before the first band that needs it until no
/// band does.
fn columns_exact(image: &mut Image<f32>, kernel: &[f32]) -> Result<(), OpError> {
    let (h, row_len) = (image.height(), image.width() * image.channels());
    let r = kernel.len() / 2;
    let slots = (BAND + 2 * r).min(h);
    let mut ring: Vec<f32> = Vec::new();
    ring.try_reserve_exact(slots * row_len).map_err(|_| OpError::Alloc("rows of the blur"))?;
    ring.resize(slots * row_len, 0.0);
    let mut loaded = 0;
    for y0 in (0..h).step_by(BAND) {
        let y1 = (y0 + BAND).min(h);
        // Rows up to y1 - 1 + r are needed; they replace rows older than y0 - r, which no band needs any more.
        let need = (y1 - 1 + r).min(h - 1) + 1;
        let source: &Image<f32> = image;
        ring.par_chunks_mut(row_len).enumerate().for_each(|(slot, dst)| {
            let row = loaded + (slot + slots - loaded % slots) % slots;
            if row < need
                && let Some(src) = source.row(row)
            {
                dst.copy_from_slice(src);
            }
        });
        loaded = need;
        let ring = &ring;
        image.par_rows_mut().enumerate().skip(y0).take(y1 - y0).for_each(|(y, out)| {
            out.fill(0.0);
            for (k, &weight) in kernel.iter().enumerate() {
                let slot = (y + k).saturating_sub(r).min(h - 1) % slots;
                if let Some(line) = ring.get(slot * row_len..(slot + 1) * row_len) {
                    for (o, s) in out.iter_mut().zip(line) {
                        *o += weight * s;
                    }
                }
            }
        });
    }
    Ok(())
}

/// An extended box filter: weight 1 on `-radius ..= radius`, `alpha` on `-radius - 1` and `radius + 1`, normalized.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ExtendedBox {
    radius: usize,
    alpha: f64,
    /// `1 / (2 radius + 1 + 2 alpha)`.
    norm: f64,
}

impl ExtendedBox {
    /// The extended box of variance `sigma^2` (Gwosdek et al. 2011): the widest plain box whose variance
    /// `r (r + 1) / 3` doesn't exceed it, plus the end weight that makes up the rest.
    fn for_sigma(sigma: f64) -> ExtendedBox {
        let var = sigma * sigma;
        let radius = to_index(((1.0 + 12.0 * var).sqrt() - 1.0) / 2.0);
        let r = radius as f64;
        let alpha = ((2.0 * r + 1.0) * (var - r * (r + 1.0) / 3.0) / (2.0 * ((r + 1.0).powi(2) - var))).clamp(0.0, 1.0);
        ExtendedBox { radius, alpha, norm: 1.0 / (2.0 * r + 1.0 + 2.0 * alpha) }
    }

    /// The filter's variance.
    #[cfg(test)]
    fn variance(&self) -> f64 {
        let r = self.radius as f64;
        (r * (r + 1.0) * (2.0 * r + 1.0) / 3.0 + 2.0 * self.alpha * (r + 1.0).powi(2)) * self.norm
    }
}

/// `passes` box filters along the rows, then along the columns.
fn blur_boxes(image: &mut Image<f32>, filter: &ExtendedBox, passes: usize) {
    let c = image.channels();
    let pad = filter.radius + 1;
    image.par_rows_mut().for_each_init(
        || (Vec::new(), Vec::new()),
        |(padded, out): &mut (Vec<f64>, Vec<f64>), row| {
            out.clear();
            out.extend(row.iter().map(|v| f64::from(*v)));
            for _ in 0..passes {
                pad_row(out, padded, c, pad);
                box_row(padded, out, c, filter);
            }
            for (d, v) in row.iter_mut().zip(out.iter()) {
                *d = to_f32(*v);
            }
        },
    );
    let h = image.height();
    // Each pass keeps 2 r + 3 rows of the strip; narrower strips for radii that would need more than STRIP_BYTES.
    let ring = (2 * filter.radius + 3).min(h);
    let width = (STRIP_BYTES / (8 * (passes * (ring + 2) + 1))).clamp(16, STRIP);
    let strips = for_each_strip(image, width);
    strips.into_par_iter().for_each_init(Vec::new, |stages: &mut Vec<Stage>, mut strip| {
        box_strip(&mut strip, filter, passes, ring, stages);
    });
}

/// `row` (`c` channels) copied into `padded` with `pad` copies of its first and last pixels on either side.
fn pad_row(row: &[f64], padded: &mut Vec<f64>, c: usize, pad: usize) {
    padded.clear();
    if let (Some(first), Some(last)) = (row.get(..c), row.get(row.len().saturating_sub(c)..)) {
        for _ in 0..pad {
            padded.extend_from_slice(first);
        }
        padded.extend_from_slice(row);
        for _ in 0..pad {
            padded.extend_from_slice(last);
        }
    }
}

/// One extended box pass along a row, from `padded` (the row with `radius + 1` pixels of padding either side) into
/// `out`. Output pixel `x` sums padded pixels `x + 1 ..= x + 2 r + 1` and takes `alpha` of pixels `x` and
/// `x + 2 r + 2`; the window then slides by one.
fn box_row(padded: &[f64], out: &mut [f64], c: usize, filter: &ExtendedBox) {
    let r = filter.radius;
    let mut sums = [0.0f64; crate::image::MAX_CHANNELS];
    for pixel in padded.chunks_exact(c).skip(1).take(2 * r + 1) {
        for (s, v) in sums.iter_mut().zip(pixel) {
            *s += v;
        }
    }
    let (low, leave, high) = (padded.chunks_exact(c), padded.chunks_exact(c).skip(1), padded.chunks_exact(c).skip(2 * r + 2));
    for (((o, lo), le), hi) in out.chunks_exact_mut(c).zip(low).zip(leave).zip(high) {
        for ch in 0..c.min(crate::image::MAX_CHANNELS) {
            let (l, e, h) = (lo[ch], le[ch], hi[ch]);
            o[ch] = (sums[ch] + filter.alpha * (l + h)) * filter.norm;
            sums[ch] += h - e;
        }
    }
}

/// One box pass of a strip of columns, streamed: it takes the rows of its input one by one and gives out each output
/// row as soon as the inputs it needs have arrived, keeping its last `ring` input rows.
struct Stage {
    rows: Vec<f64>,
    sums: Vec<f64>,
    out: Vec<f64>,
    received: usize,
    emitted: usize,
}

/// What every [`Stage`] of a strip shares.
struct Strip<'a> {
    filter: &'a ExtendedBox,
    /// The image height.
    h: usize,
    /// The rows a stage keeps.
    ring: usize,
    /// The strip width, in samples.
    width: usize,
}

impl Stage {
    fn reset(&mut self, s: &Strip<'_>) {
        self.rows.clear();
        self.rows.resize(s.ring * s.width, 0.0);
        self.sums.clear();
        self.sums.resize(s.width, 0.0);
        self.out.clear();
        self.out.resize(s.width, 0.0);
        self.received = 0;
        self.emitted = 0;
    }

    fn push(&mut self, row: &[f64], s: &Strip<'_>) {
        let slot = self.received % s.ring;
        if let Some(dst) = self.rows.get_mut(slot * s.width..(slot + 1) * s.width) {
            dst.copy_from_slice(row);
        }
        self.received += 1;
    }

    /// Computes the next output row into `out` if its inputs, up to row `y + r + 1`, have arrived.
    fn emit(&mut self, s: &Strip<'_>) -> bool {
        let (y, r) = (self.emitted, s.filter.radius);
        if y >= s.h || self.received < (y + r + 2).min(s.h) {
            return false;
        }
        let rows = &self.rows;
        if y == 0 {
            // The window of row 0: rows -r ..= r, the ones above the image being row 0.
            self.sums.fill(0.0);
            for i in 0..=r {
                let weight = if i == 0 { (r + 1) as f64 } else { 1.0 };
                for (acc, v) in self.sums.iter_mut().zip(input(rows, i, s)) {
                    *acc += weight * v;
                }
            }
        }
        let (above, below) = (input(rows, y.saturating_sub(r + 1), s), input(rows, y + r + 1, s));
        let (alpha, norm) = (s.filter.alpha, s.filter.norm);
        for (((o, sum), a), b) in self.out.iter_mut().zip(&self.sums).zip(above).zip(below) {
            *o = (sum + alpha * (a + b)) * norm;
        }
        // Slide the window: row y + r + 1 enters, row y - r leaves.
        let (enter, leave) = (input(rows, y + r + 1, s), input(rows, y.saturating_sub(r), s));
        for ((acc, e), l) in self.sums.iter_mut().zip(enter).zip(leave) {
            *acc += e - l;
        }
        self.emitted += 1;
        true
    }
}

/// Input row `i` of a stage (clamped to the image) from its ring `rows`; it must still be there.
fn input<'a>(rows: &'a [f64], i: usize, s: &Strip<'_>) -> &'a [f64] {
    let slot = i.min(s.h - 1) % s.ring;
    rows.get(slot * s.width..(slot + 1) * s.width).unwrap_or(&[])
}

/// Runs the box passes down a strip of columns, in place: each input row goes through the stages, and the last stage's
/// rows are written back, always to rows already read.
fn box_strip(strip: &mut [&mut [f32]], filter: &ExtendedBox, passes: usize, ring: usize, stages: &mut Vec<Stage>) {
    let width = strip.first().map_or(0, |row| row.len());
    let s = Strip { filter, h: strip.len(), ring, width };
    stages.resize_with(passes, || Stage { rows: Vec::new(), sums: Vec::new(), out: Vec::new(), received: 0, emitted: 0 });
    for stage in stages.iter_mut() {
        stage.reset(&s);
    }
    let mut input = Vec::with_capacity(width);
    for j in 0..s.h {
        input.clear();
        if let Some(row) = strip.get(j) {
            input.extend(row.iter().map(|v| f64::from(*v)));
        }
        if let Some(first) = stages.first_mut() {
            first.push(&input, &s);
        }
        drain(stages, strip, &s);
    }
}

/// Lets every stage give out the rows it can, feeding each into the next and writing the last stage's to the strip.
fn drain(stages: &mut [Stage], strip: &mut [&mut [f32]], s: &Strip<'_>) {
    let Some((stage, rest)) = stages.split_first_mut() else {
        return;
    };
    while stage.emit(s) {
        match rest.first_mut() {
            Some(next) => {
                next.push(&stage.out, s);
                drain(rest, strip, s);
            }
            None => {
                if let Some(dst) = strip.get_mut(stage.emitted - 1) {
                    for (d, v) in dst.iter_mut().zip(&stage.out) {
                        *d = to_f32(*v);
                    }
                }
            }
        }
    }
}

/// `image` cut into vertical strips of up to `width` samples: for each strip, its piece of every row, top to bottom.
fn for_each_strip(image: &mut Image<f32>, width: usize) -> Vec<Vec<&mut [f32]>> {
    let strips = (image.width() * image.channels()).div_ceil(width);
    let height = image.height();
    let mut columns: Vec<Vec<&mut [f32]>> = (0..strips).map(|_| Vec::with_capacity(height)).collect();
    for row in image.rows_mut() {
        for (column, piece) in columns.iter_mut().zip(row.chunks_mut(width)) {
            column.push(piece);
        }
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ColorSpace;

    fn impulse(size: usize) -> Image<f32> {
        let mut image = Image::<f32>::new(size, size, 1, ColorSpace::LINEAR).unwrap();
        image.pixel_mut(size / 2, size / 2).unwrap()[0] = 1.0;
        image
    }

    /// The variance of a centered impulse response along x.
    fn variance_x(image: &Image<f32>) -> f64 {
        let center = (image.width() / 2) as f64;
        let (mut sum, mut moment) = (0.0, 0.0);
        for (y, row) in image.rows().enumerate() {
            let _ = y;
            for (x, v) in row.iter().enumerate() {
                sum += f64::from(*v);
                moment += f64::from(*v) * (x as f64 - center).powi(2);
            }
        }
        moment / sum
    }

    #[test]
    fn kernels_are_normalized_and_symmetric() {
        assert_eq!(gaussian_kernel(0.0), [1.0]);
        let k = gaussian_kernel(1.5);
        assert_eq!(k.len(), 11);
        assert!((k.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(k.iter().zip(k.iter().rev()).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn extended_boxes_have_exactly_the_variance_asked_for() {
        for sigma in [0.3, 1.0, 2.5, 7.0, 16.0, 100.0] {
            let filter = ExtendedBox::for_sigma(sigma);
            assert!((filter.variance() - sigma * sigma).abs() < 1e-9 * sigma * sigma, "{sigma}: {filter:?}");
            assert!((0.0..1.0).contains(&filter.alpha));
        }
        assert_eq!(ExtendedBox::for_sigma(1.0).radius, 1, "a 3-box has variance 2/3, a 5-box 2");
    }

    #[test]
    fn every_method_keeps_flat_images_and_the_energy_of_an_impulse() {
        let flat = Image::from_fn(40, 30, 3, ColorSpace::LINEAR, |_, _, c| 0.25 * (c as f32 + 1.0)).unwrap();
        for method in [BlurMethod::Auto, BlurMethod::Exact, BlurMethod::Boxes { passes: 1 }, BlurMethod::Boxes { passes: 4 }] {
            for sigma in [0.7, 3.0, 9.0] {
                let mut image = flat.clone();
                gaussian_blur_in_place(&mut image, sigma, method).unwrap();
                assert!(image.as_raw().iter().zip(flat.as_raw()).all(|(a, b)| (a - b).abs() < 1e-6), "{method:?} {sigma}");
                let mut spread = impulse(81);
                gaussian_blur_in_place(&mut spread, sigma, method).unwrap();
                let total: f64 = spread.as_raw().iter().map(|v| f64::from(*v)).sum();
                assert!((total - 1.0).abs() < 1e-5, "{method:?} {sigma}: {total}");
            }
        }
    }

    #[test]
    fn box_cascades_match_the_variance_and_the_shape_of_the_gaussian() {
        for sigma in [5.0, 12.0] {
            let mut exact = impulse(161);
            gaussian_blur_in_place(&mut exact, sigma, BlurMethod::Exact).unwrap();
            let peak = exact.as_raw().iter().copied().fold(0.0, f32::max);
            // The largest difference of a 2D impulse response, as a fraction of the exact kernel's peak (measured: 0.114
            // to 0.120 with three passes, 0.071 to 0.075 with four, 0.050 to 0.054 with six).
            for (passes, tolerance) in [(3, 0.125), (4, 0.08), (6, 0.06)] {
                let mut boxes = impulse(161);
                gaussian_blur_in_place(&mut boxes, sigma, BlurMethod::Boxes { passes }).unwrap();
                assert!((variance_x(&boxes) - sigma * sigma).abs() < 0.01 * sigma * sigma, "{sigma} {passes}: {}", variance_x(&boxes));
                let worst = boxes.as_raw().iter().zip(exact.as_raw()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
                assert!(worst < tolerance * peak, "{sigma} {passes}: {} of the peak", worst / peak);
            }
        }
    }

    #[test]
    fn rows_and_columns_are_blurred_alike() {
        let image = Image::from_fn(23, 31, 2, ColorSpace::LINEAR, |x, y, c| ((x * 7 + y * 3 + c) % 11) as f32).unwrap();
        let transposed = Image::from_fn(31, 23, 2, ColorSpace::LINEAR, |x, y, c| image.get(y, x, c).unwrap()).unwrap();
        for method in [BlurMethod::Exact, BlurMethod::Boxes { passes: 3 }] {
            let (mut a, mut b) = (image.clone(), transposed.clone());
            gaussian_blur_in_place(&mut a, 2.5, method).unwrap();
            gaussian_blur_in_place(&mut b, 2.5, method).unwrap();
            for y in 0..31 {
                for x in 0..23 {
                    for c in 0..2 {
                        let (p, q) = (a.get(x, y, c).unwrap(), b.get(y, x, c).unwrap());
                        assert!((p - q).abs() < 1e-4, "{method:?} ({x}, {y}, {c}): {p} vs {q}");
                    }
                }
            }
        }
    }

    #[test]
    fn tiny_images_and_huge_radii_are_fine() {
        for (w, h) in [(1, 1), (1, 7), (5, 1), (2, 3)] {
            let image = Image::from_fn(w, h, 1, ColorSpace::LINEAR, |x, y, _| (x + 10 * y) as f32).unwrap();
            for method in [BlurMethod::Exact, BlurMethod::Boxes { passes: 4 }] {
                let mut out = image.clone();
                gaussian_blur_in_place(&mut out, 50.0, method).unwrap();
                let (lo, hi) = image.as_raw().iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
                assert!(out.as_raw().iter().all(|v| (lo - 1e-3..=hi + 1e-3).contains(v)), "{w}x{h} {method:?}");
            }
        }
    }

    #[test]
    fn bad_parameters_are_errors() {
        let mut image = impulse(9);
        for sigma in [f64::NAN, -1.0, f64::INFINITY, 1e5] {
            assert!(matches!(gaussian_blur_in_place(&mut image, sigma, BlurMethod::Auto), Err(OpError::Param { name: "sigma", .. })));
        }
        assert!(gaussian_blur_in_place(&mut image, 300.0, BlurMethod::Exact).is_err());
        assert!(gaussian_blur_in_place(&mut image, 300.0, BlurMethod::Auto).is_ok());
        for passes in [0, 9] {
            assert!(matches!(gaussian_blur_in_place(&mut image, 2.0, BlurMethod::Boxes { passes }), Err(OpError::Param { name: "passes", .. })));
        }
        let before = impulse(9);
        let mut same = before.clone();
        gaussian_blur_in_place(&mut same, 0.0, BlurMethod::Auto).unwrap();
        assert_eq!(same, before);
    }
}
