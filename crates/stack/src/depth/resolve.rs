//! The depth map from pass 1's statistics, for a threshold and a smoothing: [`FocusStats::resolve`], which reads no
//! frame and is fast enough to run on every change of the threshold. See the module documentation of [`super`].
//!
//! 1. **Threshold**: a pixel is confident when some frame covers it, its confidence is at least the threshold's level
//!    (and above 0) and the mask doesn't mark it. A percentile is read off the covered pixels' confidence; `auto` is
//!    below.
//! 2. **Support**: a confident pixel whose neighborhood is mostly not confident ([`MIN_SUPPORT`]) is left out too. In
//!    a region without detail, such as a smooth out-of-focus background, the noise lifts some pixels' confidence over
//!    the threshold, in blobs as wide as the focus measure's window, each with the depth of whichever frame the noise
//!    favored; followed, they show as seams between frames whose brightness differs a little, and as dark specks where
//!    a frame focused elsewhere spreads a dark subject's edge over the background. The neighborhood is the edge-aware
//!    filter's below, at the smoothing radius, so that a thin structure's pixels are judged among themselves.
//! 3. **Smoothing**: the confident pixels' depth smoothed by the edge-aware filter (`filter.rs`), guided by the
//!    sharpest frames' luminance on a perceptual scale (its cube root, as CIE L* has it), so that the depth of one
//!    object doesn't spill over the edge of another.
//! 4. **Filling**: each pixel keeps the smoothed depth as far as its neighborhood is confident ([`REACHED`]) and takes
//!    the rest from coarser scales (push-pull), which fill regions without detail smoothly from their edges.
//!
//! Measured on the local camera stacks: a wasp on a smooth background (three quarters of the canvas without
//! detail) came out with seams and dark specks around its legs without the support test, and clean with it; a cricket
//! on textured ground came out the same either way.
//!
//! With no confident pixel at all, every covered pixel's own depth is smoothed instead.
//!
//! # The automatic threshold
//!
//! Where a region has no detail, its pixels' depth is the frame the noise favored, at random from pixel to pixel; where
//! it has some, however faint, the depth of neighbors agrees. The automatic threshold starts from the level that best
//! splits the covered pixels' confidence into two classes on a logarithmic scale (N. Otsu, "A threshold selection
//! method from gray-level histograms", IEEE Transactions on Systems, Man, and Cybernetics 9(1), 1979), and takes it if
//! the class below it follows noise: if its pixels' depth differs from their neighbors' by at least
//! [`NOISE_INCOHERENCE`] frames, and [`NOISE_RATIO`] times as much as the class above does. Each pixel's difference is
//! the least of those to the four pixels a few windows away along the axes (beyond the focus measure's window, which
//! makes close neighbors agree), so that a pixel on an edge between depths, which agrees with its neighbors along the
//! edge, counts as coherent. Otherwise, as when the two classes are a scene's faint texture and its strong edges, the
//! threshold is the [`AUTO_PERCENTILE`]th percentile. Measured on the local camera stacks: on a wasp on a smooth
//! background, the 82% of the pixels below Otsu's level were 1.14 frames from their neighbors' depth, those above 0.03,
//! and they were filled; on a cricket on textured ground, 0.06 and 0.02, and the percentile was taken, as on the
//! synthetic stacks with texture everywhere. A noise-only backdrop of a 12-frame synthetic stack measured 0.98.

use std::sync::Arc;

use florastack_core::{ColorSpace, Image};
use rayon::prelude::*;

use super::composite::CompositeKernel;
use super::filter::{push_pull, smooth};
use super::focus::FocusStats;
use super::{MAX_SMOOTHING, Threshold};
use crate::error::StackError;
use crate::tiled::{Tiled, Tiling};

/// How strongly the guide's edges stop the smoothing: a step of this much in the cube root of luminance (a twentieth
/// of the way from black to white) counts as far as the smoothing radius.
pub const EDGE: f64 = 0.05;
/// The share of its neighborhood that must be confident for a pixel to keep the smoothed depth whole; below it, the
/// depth is blended with the coarser scales' in proportion.
pub const REACHED: f32 = 0.5;
/// The share of a confident pixel's neighborhood that must be confident too for it to count (see the module
/// documentation).
pub const MIN_SUPPORT: f32 = 0.5;
/// At most about this many pixels are sorted to find a percentile, or counted for the automatic threshold: larger
/// canvases are sampled on a lattice.
const PERCENTILE_SAMPLES: usize = 1 << 22;
/// The bins of the histogram Otsu's level is found on.
const OTSU_BINS: usize = 256;
/// The least median depth difference, in frames, between the pixels below Otsu's level and their neighbors for the
/// automatic threshold to take that level.
pub const NOISE_INCOHERENCE: f32 = 0.5;
/// How many times the class above's median difference the class below's must be.
pub const NOISE_RATIO: f32 = 4.0;
/// The percentile the automatic threshold takes when the pixels below Otsu's level don't follow noise.
pub const AUTO_PERCENTILE: f64 = 10.0;

/// A level of confidence as the statistics hold it.
#[allow(clippy::cast_possible_truncation)] // A level of contrast, small.
fn to_level(v: f64) -> f32 {
    v as f32
}

/// The median of `values`, 0 for none.
fn median(mut values: Vec<f32>) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let middle = values.len() / 2;
    *values.select_nth_unstable_by(middle, f32::total_cmp).1
}

/// A depth map, made from the focus statistics for one threshold and smoothing, which pass 2 composites from.
#[derive(Clone, Debug, PartialEq)]
pub struct DepthMap {
    /// The frames the depth indexes.
    pub frames: usize,
    /// Each pixel's depth, in frames from 0 (the first frame) to `frames - 1`, between frames where it lies between
    /// them: interpolated, smoothed and filled; 0 where no frame covers. Shared with the compositors made from it.
    pub depth: Arc<Image<f32>>,
    /// Each pixel's confidence on a scale from 0 to 1, `c / (c + level)`: 0.5 at the threshold, below it where the
    /// depth was filled from the confident pixels around; 0 where no frame covers.
    pub confidence: Image<f32>,
    /// The threshold's level of confidence.
    pub level: f32,
    /// How many covered pixels were below the threshold, masked or without support, whose depth was filled.
    pub filled: usize,
}

/// A sample in `[0, 1]` as a 16-bit one.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped to [0, 65535] first.
fn to_u16(v: f32) -> u16 {
    if v.is_finite() { (v.clamp(0.0, 1.0) * 65535.0).round() as u16 } else { 0 }
}

impl DepthMap {
    pub fn width(&self) -> usize {
        self.depth.width()
    }

    pub fn height(&self) -> usize {
        self.depth.height()
    }

    /// The depth map as a 16-bit image: the frame index scaled to the full range, 0 for the first frame and 65535 for
    /// the last (0 everywhere for a single frame).
    pub fn depth_u16(&self) -> Result<Image<u16>, StackError> {
        #[allow(clippy::cast_precision_loss)] // At most MAX_FRAMES.
        let last = self.frames.saturating_sub(1) as f32;
        let scale = if last > 0.0 { 1.0 / last } else { 0.0 };
        let mut out = Image::new(self.width(), self.height(), 1, ColorSpace::LINEAR)?;
        out.par_rows_mut().zip(self.depth.par_rows()).for_each(|(o, d)| o.iter_mut().zip(d).for_each(|(o, &d)| *o = to_u16(d * scale)));
        Ok(out)
    }

    /// The confidence map as a 16-bit image.
    pub fn confidence_u16(&self) -> Result<Image<u16>, StackError> {
        let mut out = Image::new(self.width(), self.height(), 1, ColorSpace::LINEAR)?;
        out.par_rows_mut().zip(self.confidence.par_rows()).for_each(|(o, c)| o.iter_mut().zip(c).for_each(|(o, &c)| *o = to_u16(c)));
        Ok(out)
    }

    /// A pass-2 stacker that composites the same frames, in the same order, from this depth map.
    pub fn compositor(&self, tiling: &Tiling) -> Result<Tiled<CompositeKernel>, StackError> {
        Ok(Tiled::new(CompositeKernel::new(Arc::clone(&self.depth), self.frames)?, tiling.clone()))
    }
}

impl FocusStats {
    /// Whether pixel `(x, y)`'s row `covered` (or the whole canvas) has a covering frame.
    fn covers(covered: Option<&[f32]>, x: usize) -> bool {
        covered.is_none_or(|row| row.get(x).is_some_and(|&k| k > 0.0))
    }

    /// The threshold's level: the automatic level, the level itself, or the percentile of the covered pixels'
    /// confidence.
    pub fn level(&self, threshold: Threshold) -> f32 {
        match threshold {
            Threshold::Auto => self.auto(),
            #[allow(clippy::cast_possible_truncation)] // A level of contrast, small.
            Threshold::Level(level) => level as f32,
            Threshold::Percentile(p) => self.percentile(p),
        }
    }

    /// The step of a lattice that holds at most about [`PERCENTILE_SAMPLES`] pixels.
    fn step(&self) -> usize {
        let (w, h) = (self.width(), self.height());
        (1..=w.max(h)).find(|s| (w.div_ceil(*s)).saturating_mul(h.div_ceil(*s)) <= PERCENTILE_SAMPLES).unwrap_or(1)
    }

    /// The automatic threshold's level: see the module documentation.
    fn auto(&self) -> f32 {
        let otsu = self.otsu();
        let (below, above) = self.incoherence(otsu);
        tracing::debug!("depth map: Otsu's level {otsu}, depth incoherence {below} frames below it, {above} above");
        if below >= NOISE_INCOHERENCE && below >= NOISE_RATIO * above { otsu } else { self.percentile(AUTO_PERCENTILE) }
    }

    /// The median depth incoherence, in frames, of the sampled pixels with some confidence below `level`, and of
    /// those at or above it: see the module documentation.
    fn incoherence(&self, level: f32) -> (f32, f32) {
        let (w, h) = (self.width(), self.height());
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // At most 4 * 64 + 1.
        let delta = ((4.0 * self.radius).ceil() as usize + 1).max(2);
        let covered = self.covered.as_ref();
        let covers = |x: usize, y: usize| FocusStats::covers(covered.and_then(|k| k.row(y)), x);
        let depth = |x: usize, y: usize| if x < w && y < h && covers(x, y) { self.depth.get(x, y, 0) } else { None };
        let step = self.step();
        let (mut low, mut high) = (Vec::new(), Vec::new());
        for y in (0..h).step_by(step) {
            for x in (0..w).step_by(step) {
                let (Some(c), Some(d)) = (self.confidence.get(x, y, 0), depth(x, y)) else { continue };
                if c.is_nan() || c <= 0.0 {
                    continue;
                }
                let around =
                    [(x.checked_sub(delta), Some(y)), (Some(x + delta), Some(y)), (Some(x), y.checked_sub(delta)), (Some(x), Some(y + delta))];
                let least = around.into_iter().filter_map(|(nx, ny)| depth(nx?, ny?)).map(|n| (n - d).abs()).fold(f32::INFINITY, f32::min);
                if least.is_finite() {
                    if c < level { low.push(least) } else { high.push(least) }
                }
            }
        }
        (median(low), median(high))
    }

    /// Otsu's level on the logarithm of the covered pixels' confidence (those above 0, the others being below any
    /// level), over [`OTSU_BINS`] bins from the least to the largest: the level that maximizes the variance between
    /// the pixels below it and those above.
    fn otsu(&self) -> f32 {
        let logs: Vec<f64> = self.samples().into_iter().filter(|&c| c > 0.0 && c.is_finite()).map(|c| f64::from(c).ln()).collect();
        let (lo, hi) = logs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &l| (lo.min(l), hi.max(l)));
        if logs.is_empty() || hi <= lo {
            // No confidence, or all alike: a level at it leaves nothing below.
            return if logs.is_empty() { 0.0 } else { to_level(lo.exp()) };
        }
        let mut histogram = [0_u64; OTSU_BINS];
        let scale = OTSU_BINS as f64 / (hi - lo);
        for &l in &logs {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Non-negative, clamped to the bins.
            let bin = (((l - lo) * scale) as usize).min(OTSU_BINS - 1);
            if let Some(count) = histogram.get_mut(bin) {
                *count += 1;
            }
        }
        let total = logs.len() as f64;
        let moment: f64 = histogram.iter().enumerate().map(|(i, &n)| i as f64 * n as f64).sum();
        let (mut below, mut below_moment, mut best, mut split) = (0.0, 0.0, -1.0, 0);
        for (i, &n) in histogram.iter().enumerate().take(OTSU_BINS - 1) {
            below += n as f64;
            below_moment += i as f64 * n as f64;
            let above = total - below;
            if below <= 0.0 || above <= 0.0 {
                continue;
            }
            let between = below * above * (below_moment / below - (moment - below_moment) / above).powi(2);
            if between > best {
                (best, split) = (between, i);
            }
        }
        // The level is the upper edge of the last bin below.
        to_level((lo + (split + 1) as f64 / scale).exp())
    }

    /// The covered pixels' confidence, on a lattice that holds at most about [`PERCENTILE_SAMPLES`] of them.
    fn samples(&self) -> Vec<f32> {
        let (w, h) = (self.width(), self.height());
        let step = self.step();
        let mut samples: Vec<f32> = Vec::new();
        for y in (0..h).step_by(step) {
            let (Some(c), covered) = (self.confidence.row(y), self.covered.as_ref().and_then(|k| k.row(y))) else { continue };
            samples.extend((0..w).step_by(step).filter(|&x| FocusStats::covers(covered, x)).filter_map(|x| c.get(x).copied()));
        }
        samples
    }

    fn percentile(&self, p: f64) -> f32 {
        let mut samples = self.samples();
        if samples.is_empty() {
            return 0.0;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A rank within the samples.
        let rank = ((p.clamp(0.0, 100.0) / 100.0 * (samples.len() - 1) as f64).round() as usize).min(samples.len() - 1);
        let (_, value, _) = samples.select_nth_unstable_by(rank, f32::total_cmp);
        *value
    }

    /// The depth map for `threshold` and `smoothing` (in pixels), with the pixels `mask` marks treated as
    /// low-confidence (one channel, the canvas's size; a value of at least 0.5 marks). See the module documentation.
    pub fn resolve(&self, threshold: Threshold, smoothing: f64, mask: Option<&Image<f32>>) -> Result<DepthMap, StackError> {
        threshold.validate()?;
        if !(smoothing.is_finite() && (0.0..=MAX_SMOOTHING).contains(&smoothing)) {
            return Err(StackError::Setting { name: "smoothing", value: smoothing, expected: "from 0 to 256 pixels" });
        }
        let (w, h) = (self.width(), self.height());
        if let Some(m) = mask
            && (m.width(), m.height(), m.channels()) != (w, h, 1)
        {
            return Err(StackError::Plane {
                what: "low-confidence mask",
                width: m.width(),
                height: m.height(),
                channels: m.channels(),
                expected_width: w,
                expected_height: h,
            });
        }
        let level = self.level(threshold);
        let covered = self.covered.as_ref();
        let mut weights = Image::new(w, h, 1, ColorSpace::LINEAR)?;
        let confident: usize = weights
            .par_rows_mut()
            .zip(self.confidence.par_rows())
            .enumerate()
            .map(|(y, (k, c))| {
                let (cov, m) = (covered.and_then(|i| i.row(y)), mask.and_then(|i| i.row(y)));
                let mut n = 0;
                for (x, (k, &c)) in k.iter_mut().zip(c).enumerate() {
                    let masked = m.and_then(|r| r.get(x)).is_some_and(|&v| v >= 0.5);
                    let sure = FocusStats::covers(cov, x) && c > 0.0 && c >= level && !masked;
                    *k = if sure { 1.0 } else { 0.0 };
                    n += usize::from(sure);
                }
                n
            })
            .sum();
        let covered_pixels = w * h - self.uncovered;
        if confident == 0 {
            weights.par_rows_mut().enumerate().for_each(|(y, k)| {
                let cov = covered.and_then(|i| i.row(y));
                k.iter_mut().enumerate().for_each(|(x, k)| *k = if FocusStats::covers(cov, x) { 1.0 } else { 0.0 });
            });
        }
        let mut guide = self.guide.clone();
        guide.par_rows_mut().for_each(|row| row.iter_mut().for_each(|g| *g = g.max(0.0).cbrt()));
        let mut confident = confident;
        if smoothing > 0.0 && confident > 0 {
            // The support: the confident share of each pixel's neighborhood, as the smoothing sees it.
            let (_, support) = smooth(&weights, &weights, &guide, smoothing, EDGE)?;
            confident = weights
                .par_rows_mut()
                .zip(support.par_rows())
                .map(|(k, s)| {
                    let mut n = 0;
                    for (k, &s) in k.iter_mut().zip(s) {
                        if *k > 0.0 && s < MIN_SUPPORT {
                            *k = 0.0;
                        }
                        n += usize::from(*k > 0.0);
                    }
                    n
                })
                .sum();
        }
        let (smoothed, mut reach) = smooth(&self.depth, &weights, &guide, smoothing, EDGE)?;
        drop((weights, guide));
        reach.par_rows_mut().for_each(|row| row.iter_mut().for_each(|k| *k = (*k / REACHED).min(1.0)));
        let mut depth = push_pull(&smoothed, &reach)?;
        drop((smoothed, reach));
        #[allow(clippy::cast_precision_loss)] // At most MAX_FRAMES.
        let last = self.frames.saturating_sub(1) as f32;
        depth.par_rows_mut().enumerate().for_each(|(y, d)| {
            let cov = covered.and_then(|i| i.row(y));
            for (x, d) in d.iter_mut().enumerate() {
                *d = if FocusStats::covers(cov, x) && d.is_finite() { d.clamp(0.0, last) } else { 0.0 };
            }
        });
        let mut confidence = self.confidence.clone();
        confidence.par_rows_mut().for_each(|row| {
            for c in row.iter_mut() {
                *c = if *c <= 0.0 {
                    0.0
                } else if level > 0.0 {
                    *c / (*c + level)
                } else {
                    1.0
                };
            }
        });
        Ok(DepthMap { frames: self.frames, depth: Arc::new(depth), confidence, level, filled: covered_pixels.saturating_sub(confident) })
    }
}
