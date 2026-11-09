//! The subject of a stacked image: where it has fine detail.
//!
//! A focus stack brings everything the frames had in focus into one sharp image, and leaves what no frame had in
//! focus (an out-of-focus background) as soft as the frames saw it. So the subject is where the stacked image has
//! fine detail, and the background where it has none, whatever the frames were. Fine detail is measured as the
//! energy of the Laplacian of the image's luminance (gamma-encoded, so that a dark subject counts as much as a bright
//! one, and sensor noise in the shadows doesn't stand out; blurred over a pixel first, so that it measures texture
//! rather than noise), averaged over a few pixels, on a log scale; the subject is
//! where it lies above the midpoint of the two classes that best split the image's pixels (N. Otsu, "A threshold
//! selection method from gray-level histograms", IEEE Transactions on Systems, Man, and Cybernetics 9(1), 1979), when
//! the two classes are clearly apart, and the whole image otherwise (a stack whose subject fills the frame has no background
//! to tell). A subject's smooth parts (a shiny shell, a wing's membrane) have little detail of their own: the gaps
//! between its detailed parts are filled over a short reach.
//!
//! The work is done on a copy at most [`WORKING_SIDE`] pixels on its long side, and the result kept at that size:
//! the pipeline samples it at a render's size, and a mask's `refine` makes its edges follow the image's.

use lightcraft_raster::{Plane, Rgb32f};

/// The longest side the subject is found at.
pub const WORKING_SIDE: usize = 1024;
/// The blur before the Laplacian, as a standard deviation in pixels of the working copy.
const PRE_BLUR: f32 = 1.0;
/// The window detail is averaged over, as a standard deviation in pixels of the working copy.
const DETAIL_SIGMA: f32 = 2.0;
/// How far the subject reaches across the gaps between detailed parts, as a fraction of the long side.
const REACH: f32 = 0.012;
/// Half the width of the subject's edge in log detail energy: a pixel goes from background to subject as its energy
/// grows from `e^-1` to `e^1` times the split's.
const EDGE: f32 = 1.0;
/// The least separation in log detail energy (a factor of 16) between the two classes' means for them to be subject
/// and background.
const SEPARATION: f32 = 2.77;

/// How much each pixel of `image` (linear Rec. 2020) belongs to the subject, from 0 to 1, at most [`WORKING_SIDE`]
/// pixels on its long side. See the module documentation.
pub fn subject(image: &Rgb32f) -> Plane {
    let (w, h) = (image.width, image.height);
    let factor = w.max(h).div_ceil(WORKING_SIDE).max(1);
    let (sw, sh) = (w.div_ceil(factor).max(1), h.div_ceil(factor).max(1));
    // Gamma-encoded luminance, averaged over each factor x factor block.
    let mut luma = Plane::new(sw, sh);
    for (by, row) in luma.data.chunks_exact_mut(sw).enumerate() {
        for (bx, v) in row.iter_mut().enumerate() {
            let (mut sum, mut n) = (0.0f32, 0.0f32);
            for y in by * factor..((by + 1) * factor).min(h) {
                for x in bx * factor..((bx + 1) * factor).min(w) {
                    if let Some(p) = image.data.get(y * w + x) {
                        sum += lightcraft_color::luminance_2020(*p).max(0.0).sqrt();
                        n += 1.0;
                    }
                }
            }
            *v = if n > 0.0 { sum / n } else { 0.0 };
        }
    }
    // The Laplacian's energy of the luminance blurred a little, so that it measures texture rather than noise (which
    // has most of its energy at the pixel's scale), averaged, on a log scale.
    let luma = lightcraft_raster::blur::gaussian(&luma, PRE_BLUR);
    let at = |x: usize, y: usize| luma.data.get(y.min(sh - 1) * sw + x.min(sw - 1)).copied().unwrap_or(0.0);
    let mut energy = Plane::new(sw, sh);
    for (y, row) in energy.data.chunks_exact_mut(sw).enumerate() {
        for (x, v) in row.iter_mut().enumerate() {
            let lap = 4.0 * at(x, y) - at(x.saturating_sub(1), y) - at(x + 1, y) - at(x, y.saturating_sub(1)) - at(x, y + 1);
            *v = lap * lap;
        }
    }
    let energy = lightcraft_raster::blur::gaussian(&energy, DETAIL_SIGMA);
    let logs: Vec<f32> = energy.data.iter().map(|e| (e.max(0.0) + 1e-12).ln()).collect();
    let mut out = Plane::new(sw, sh);
    let Some((_, below, above)) = otsu(&logs).filter(|(_, below, above)| above - below >= SEPARATION) else {
        out.data.fill(1.0);
        return out;
    };
    // Otsu's level lies at the edge of the gap between the two classes; their means' midpoint lies in it.
    let split = f32::midpoint(below, above);
    let detailed: Vec<f32> = logs.iter().map(|l| smoothstep(split - EDGE, split + EDGE, *l)).collect();
    out.data.clone_from(&detailed);
    #[allow(clippy::cast_precision_loss)] // A side of at most WORKING_SIDE pixels.
    let reach = (REACH * sw.max(sh) as f32).max(1.0);
    let spread = lightcraft_raster::blur::gaussian(&out, reach);
    // A pixel whose neighborhood is a quarter detailed or more is subject: that fills the gaps and keeps the outline.
    for ((o, s), d) in out.data.iter_mut().zip(&spread.data).zip(&detailed) {
        *o = smoothstep(0.1, 0.35, *s).max(*d);
    }
    out
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Otsu's threshold of the finite `values`, over a histogram of 256 bins between their least and largest: the level,
/// and the means of the values below it and of those above. `None` without two different values.
fn otsu(values: &[f32]) -> Option<(f32, f32, f32)> {
    const BINS: usize = 256;
    let (lo, hi) = values.iter().filter(|v| v.is_finite()).fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    #[allow(clippy::cast_precision_loss)] // A small constant.
    let width = (hi - lo) / BINS as f32;
    let mut counts = [0u64; BINS];
    let mut sums = [0f64; BINS];
    for &v in values.iter().filter(|v| v.is_finite()) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Within [0, BINS) once clamped.
        let bin = (((v - lo) / width) as usize).min(BINS - 1);
        if let (Some(c), Some(s)) = (counts.get_mut(bin), sums.get_mut(bin)) {
            *c += 1;
            *s += f64::from(v);
        }
    }
    #[allow(clippy::cast_precision_loss)] // Pixel counts, far below 2^52.
    let (total, total_sum) = (counts.iter().sum::<u64>() as f64, sums.iter().sum::<f64>());
    let (mut n0, mut s0, mut best) = (0.0f64, 0.0f64, None::<(f64, usize, f64, f64)>);
    for (i, (&c, &s)) in counts.iter().zip(&sums).enumerate().take(BINS - 1) {
        #[allow(clippy::cast_precision_loss)] // As above.
        let c = c as f64;
        n0 += c;
        s0 += s;
        let n1 = total - n0;
        if n0 <= 0.0 || n1 <= 0.0 {
            continue;
        }
        let (m0, m1) = (s0 / n0, (total_sum - s0) / n1);
        let between = n0 * n1 * (m1 - m0) * (m1 - m0);
        if best.is_none_or(|(b, ..)| between > b) {
            best = Some((between, i, m0, m1));
        }
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // Log energies and bin indices, small.
    best.map(|(_, i, m0, m1)| (lo + width * (i + 1) as f32, m0 as f32, m1 as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otsu_splits_two_classes() {
        let values: Vec<f32> = (0..1000).map(|i| if i % 3 == 0 { 8.0 + (i % 7) as f32 * 0.1 } else { 1.0 + (i % 5) as f32 * 0.1 }).collect();
        let (level, below, above) = otsu(&values).unwrap();
        assert!(level > 1.4 && level < 8.0, "{level}");
        assert!((below - 1.2).abs() < 0.1 && (above - 8.3).abs() < 0.1, "{below} {above}");
        assert_eq!(otsu(&[1.0; 10]), None);
        assert_eq!(otsu(&[f32::NAN, 2.0]), None);
    }

    #[test]
    fn a_detailed_half_is_the_subject_and_a_flat_image_all_of_it() {
        let (w, h) = (128, 96);
        let image = Rgb32f::from_fn(w, h, |x, y| {
            if x < w / 2 {
                let v = if (x / 2 + y / 2) % 2 == 0 { 0.6 } else { 0.1 };
                [v, v, v]
            } else {
                [0.3, 0.3, 0.3]
            }
        });
        let s = subject(&image);
        assert_eq!((s.width, s.height), (w, h));
        let at = |x: usize, y: usize| s.data[y * w + x];
        assert!(at(20, 48) > 0.95 && at(110, 48) < 0.05, "{} {}", at(20, 48), at(110, 48));
        let flat = Rgb32f::from_fn(w, h, |_, _| [0.3, 0.3, 0.3]);
        assert!(subject(&flat).data.iter().all(|v| (*v - 1.0).abs() < f32::EPSILON));
    }
}
