//! White balance: a gain per color channel that matches a frame's color to the master's, for
//! [`crate::AlignSettings::white_balance`]: JPEGs shot with automatic white balance, and light that changes color.
//!
//! It is measured as the brightness gain is (`brightness.rs`), on broad tones only: each color channel, linear, low-passed
//! to about 64 pixels on its longer side ([`Colors`]), where a frame focused elsewhere has the master's color up to its
//! gains, since a blur keeps local averages. A blurred frame has less local contrast than a sharp one, and that
//! difference is what stacking picks frames by, so a correction of anything but a gain would see it: on synthetic stacks
//! whose frames differ only in focus, a gain and offset fitted this way, or a tone curve, found contrast changes of a
//! few percent near depth edges, where a blurred near edge spreads over a background of another tone; a gain per
//! channel found none beyond the generator's own error.
//!
//! Each channel's gain is the median ratio of the master's to the frame's samples at the same points of the scene,
//! leaving out the planes' edges, dark samples (whose ratio noise and black-level errors dominate) and cells with
//! clipped samples under them (whose true color is unknown). The three gains are then divided by their
//! luminance-weighted mean, so that they change the color and not the brightness, which the brightness gain matches
//! on its own. They are measured against the master directly, never along the chain, so that their errors don't add
//! up.

use florastack_core::convert::srgb_to_linear;
use florastack_core::ops::{BlurMethod, gaussian_blur_in_place, resize};
use florastack_core::{Affine, ColorSpace, Encoding, Image};
use rayon::prelude::*;

use crate::error::AlignError;
use crate::prepare::{LUMA, Mapping};

/// The longer side of the color planes, and their extra blur in their own pixels: the brightness plane's.
const SIDE: usize = 64;
const SIGMA: f64 = 2.0;
/// The largest block the planes average before they are resized.
const MAX_BLOCK: usize = 64;
/// A linear sample at least this bright counts as clipped.
const CLIP: f32 = 0.98;
/// A cell with more than this share of clipped samples under it is left out.
const CLIPPED_SHARE: f64 = 0.01;
/// Samples this close to the planes' edges are left out: their blur sees the clamped border there.
const MARGIN: usize = 2;
/// Samples darker than this fraction of the master's mean are left out.
const DARK: f64 = 0.05;
/// The fewest samples a gain is measured from.
const MIN_SAMPLES: usize = 64;
/// The smallest and largest gain of a channel against the others: beyond, the frames don't show the same scene.
const GAINS: (f64, f64) = (0.5, 2.0);

/// A frame's planes for white balance: its red, green and blue, linear, low-passed to [`SIDE`] pixels; the share of
/// clipped samples under each cell; and how their pixels map to the frame's.
pub(crate) struct Colors {
    pub mapping: Mapping,
    pub planes: [Image<f32>; 3],
    pub clipped: Image<f32>,
}

impl Colors {
    /// The planes of `frame`, `None` for a frame without three color channels.
    pub fn new(frame: &Image<f32>) -> Result<Option<Colors>, AlignError> {
        let c = frame.channels();
        if c < 3 {
            return Ok(None);
        }
        let (w, h) = (frame.width(), frame.height());
        let long = w.max(h).max(1);
        let mut k = 1;
        while k < MAX_BLOCK && long / (2 * k) >= SIDE {
            k *= 2;
        }
        let factor = SIDE as f64 / long as f64;
        let scaled = |n: usize| ((n / k) as f64 * (factor * k as f64).min(1.0)).round().max(1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Positive and at most the frame's size.
        let size = (scaled(w) as usize, scaled(h) as usize);
        let srgb = frame.color_space().encoding == Encoding::Srgb;
        let linear = move |v: f32| {
            let v = if v.is_finite() { v } else { 0.0 };
            if srgb { srgb_to_linear(v) } else { v }
        };
        let (red, mapping) = low_pass(frame, k, size, &|p| p.first().copied().map_or(0.0, linear))?;
        let (green, _) = low_pass(frame, k, size, &|p| p.get(1).copied().map_or(0.0, linear))?;
        let (blue, _) = low_pass(frame, k, size, &|p| p.get(2).copied().map_or(0.0, linear))?;
        let (clipped, _) = low_pass(frame, k, size, &|p| if p.iter().take(3).any(|&v| linear(v) >= CLIP) { 1.0 } else { 0.0 })?;
        Ok(Some(Colors { mapping, planes: [red, green, blue], clipped }))
    }
}

/// What a plane takes from a pixel's samples.
type Extract<'a> = dyn Fn(&[f32]) -> f32 + Sync + 'a;

/// `f` of `frame`'s pixels, averaged over blocks of `k` x `k`, resized to `size` and blurred, with how its pixels map
/// to the frame's.
fn low_pass(frame: &Image<f32>, k: usize, size: (usize, usize), f: &Extract<'_>) -> Result<(Image<f32>, Mapping), AlignError> {
    let c = frame.channels().max(1);
    let mut blocks = Image::new((frame.width() / k).max(1), (frame.height() / k).max(1), 1, ColorSpace::LINEAR)?;
    let scale = 1.0 / (k * k) as f32;
    blocks.par_rows_mut().enumerate().for_each(|(y, dst)| {
        dst.fill(0.0);
        for row in (y * k..y * k + k).filter_map(|r| frame.row(r)) {
            for (d, block) in dst.iter_mut().zip(row.chunks_exact(c * k)) {
                *d += block.chunks_exact(c).map(f).sum::<f32>();
            }
        }
        for d in dst.iter_mut() {
            *d = if d.is_finite() { *d * scale } else { 0.0 };
        }
    });
    let mapping = Mapping::IDENTITY.blocks(k);
    let from = (blocks.width(), blocks.height());
    let (mut plane, mapping) = if from == size { (blocks, mapping) } else { (resize(&blocks, size.0, size.1)?, mapping.resized(from, size)) };
    gaussian_blur_in_place(&mut plane, SIGMA, BlurMethod::Exact)?;
    Ok((plane, mapping))
}

/// The gains, red, green and blue, that match `frame`'s color to `master`'s, where `transform` maps the master's
/// pixels to the frame's; their luminance-weighted mean is 1. `None` when they share too little to measure them, or
/// they would be beyond what one scene allows.
pub(crate) fn measure(master: &Colors, frame: &Colors, transform: &Affine) -> Option<[f64; 3]> {
    let to_plane = frame.mapping.to_frame().inverse()?;
    let map = master.mapping.to_frame().then(transform).then(&to_plane);
    let mut gains = [0.0; 3];
    for (gain, (m, f)) in gains.iter_mut().zip(master.planes.iter().zip(&frame.planes)) {
        *gain = median_ratio(master, m, frame, f, &map)?;
    }
    let mean: f64 = gains.iter().zip(LUMA).map(|(g, w)| g * f64::from(w)).sum();
    if !(mean.is_finite() && mean > 0.0) {
        return None;
    }
    let balanced = gains.map(|g| g / mean);
    balanced.iter().all(|g| g.is_finite() && (GAINS.0..=GAINS.1).contains(g)).then_some(balanced)
}

/// The median ratio of `m`, a plane of `master`, to `f`, the same plane of `frame`, where `map` takes the master
/// plane's pixels to the frame plane's.
fn median_ratio(master: &Colors, m: &Image<f32>, frame: &Colors, f: &Image<f32>, map: &Affine) -> Option<f64> {
    let (w, h) = (m.width(), m.height());
    if w <= 2 * MARGIN || h <= 2 * MARGIN {
        return None;
    }
    let mean = m.rows().flatten().map(|&v| f64::from(v)).sum::<f64>() / (w * h) as f64;
    if !(mean.is_finite() && mean > 0.0) {
        return None;
    }
    let floor = DARK * mean;
    let [[m0, m1, m2], [m3, m4, m5]] = map.m;
    let (hi_x, hi_y) = ((f.width() - 1) as f64, (f.height() - 1) as f64);
    let mut ratios = Vec::new();
    for y in MARGIN..h - MARGIN {
        for x in MARGIN..w - MARGIN {
            let (xf, yf) = (x as f64, y as f64);
            let (u, v) = (m0 * xf + m1 * yf + m2, m3 * xf + m4 * yf + m5);
            if !(u >= 0.0 && u <= hi_x && v >= 0.0 && v <= hi_y) {
                continue;
            }
            let clipped = master.clipped.get(x, y, 0).is_none_or(|c| f64::from(c) > CLIPPED_SHARE)
                || bilinear(&frame.clipped, u, v).is_none_or(|c| c > CLIPPED_SHARE);
            let (Some(mv), Some(fv)) = (m.get(x, y, 0).map(f64::from), bilinear(f, u, v)) else { continue };
            if !clipped && mv > floor && fv > floor {
                ratios.push(mv / fv);
            }
        }
    }
    if ratios.len() < MIN_SAMPLES {
        return None;
    }
    let middle = ratios.len() / 2;
    let (_, &mut median, _) = ratios.select_nth_unstable_by(middle, f64::total_cmp);
    median.is_finite().then_some(median)
}

/// `plane` at `(u, v)`, which lies inside it, interpolated bilinearly.
fn bilinear(plane: &Image<f32>, u: f64, v: f64) -> Option<f64> {
    let (fu, fv) = (u.floor(), v.floor());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Inside the plane: non-negative and small.
    let (x, y) = (fu as usize, fv as usize);
    let (w, h) = (plane.width(), plane.height());
    let at = |x: usize, y: usize| plane.get(x.min(w - 1), y.min(h - 1), 0).map(f64::from);
    let (tu, tv) = (u - fu, v - fv);
    let upper = at(x, y)? + tu * (at(x + 1, y)? - at(x, y)?);
    let lower = at(x, y + 1)? + tu * (at(x + 1, y + 1)? - at(x, y + 1)?);
    Some(upper + tv * (lower - upper))
}

/// `gains` divided by `base`'s, channel by channel, balanced again: a frame's white balance against a new master whose
/// own was `base`. Either may be none, which changes no color.
pub(crate) fn rebased(gains: Option<[f64; 3]>, base: Option<[f64; 3]>) -> Option<[f64; 3]> {
    let (gains, base) = match (gains, base) {
        (None, None) => return None,
        (g, b) => (g.unwrap_or([1.0; 3]), b.unwrap_or([1.0; 3])),
    };
    let ratio = [0, 1, 2].map(|c| gains[c] / base[c]);
    let mean: f64 = ratio.iter().zip(LUMA).map(|(g, w)| g * f64::from(w)).sum();
    let balanced = ratio.map(|g| g / mean);
    balanced.iter().all(|g| g.is_finite() && *g > 0.0).then_some(balanced)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gains_are_balanced_and_rebased() {
        let rebased = rebased(Some([1.1, 1.0, 0.9]), Some([1.1, 1.0, 0.9])).unwrap();
        assert!(rebased.iter().all(|g| (g - 1.0).abs() < 1e-12), "{rebased:?}");
        let back = super::rebased(None, Some([1.2, 1.0, 0.8])).unwrap();
        let mean: f64 = back.iter().zip(LUMA).map(|(g, w)| g * f64::from(w)).sum();
        assert!((mean - 1.0).abs() < 1e-12 && back[0] < 1.0 && back[2] > 1.0, "{back:?}");
        assert_eq!(super::rebased(None, None), None);
    }

    #[test]
    fn a_tinted_copy_is_measured_and_a_gray_frame_has_no_planes() {
        let base = florastack_core::testutil::scene::textured(240, 180, 2).unwrap().0;
        let tint = [1.1_f32, 1.0, 0.85];
        let tinted = Image::from_fn(240, 180, 3, ColorSpace::LINEAR, |x, y, c| base.get(x, y, c).unwrap_or(0.0) * tint[c]).unwrap();
        let (master, frame) = (Colors::new(&base).unwrap().unwrap(), Colors::new(&tinted).unwrap().unwrap());
        let gains = measure(&master, &frame, &Affine::IDENTITY).unwrap();
        // The gains undo the tint, balanced so that a neutral color keeps its brightness.
        let mean: f32 = tint.iter().zip(LUMA).map(|(t, w)| w / t).sum();
        for c in 0..3 {
            let want = f64::from(1.0 / tint[c] / mean);
            assert!((gains[c] - want).abs() < 1e-4, "{c}: {} against {want}", gains[c]);
        }
        let gray = Image::<f32>::new(240, 180, 1, ColorSpace::LINEAR).unwrap();
        assert!(Colors::new(&gray).unwrap().is_none());
    }
}
