//! Pass 1 of the depth-map method: every frame's focus measure folded into a running record per pixel ([`FocusKernel`],
//! a [`TileKernel`] run tile by tile by [`Estimator`]), which ends as the focus statistics ([`FocusStats`]). See the
//! module documentation of [`super`].
//!
//! # The running record
//!
//! A tile's state is a record of [`RECORD`] values per pixel, over its inner region, in two planes (an image holds at
//! most four channels):
//!
//! - the sharpest frame so far, its focus, and the focus of the frames just before and after it (-1 where there is
//!   none, or the frame doesn't cover the pixel), for the interpolation between frames;
//! - the focus of the last frame folded, which becomes the "before" of the next frame if that one is sharper;
//! - the runner-up: the most focus of the frames at least two from the sharpest, which lie outside the peak around
//!   it, kept exactly as the sharpest frame changes (when it moves on by one frame, the frame before the old
//!   sharpest joins them; when it moves further, the old sharpest itself);
//! - the least focus of any covering frame: the floor of the pixel's focus curve;
//! - the luminance of the sharpest frame, for the guide image.
//!
//! Frames arrive in order, so the frame after the sharpest is the next one folded. Ties keep the earlier frame. A
//! pixel that some frame covers has a focus of at least 0, so a focus of -1 marks one that none does.
//!
//! # Confidence
//!
//! Where a pixel has detail, its focus curve peaks at the frame focused at its depth and falls far below the peak in
//! the frames focused well away from it. Where it has none (a smooth out-of-focus background), the curve is the
//! frames' noise, which never falls far below its own highest value: the floor is close to the peak. Below
//! [`MIN_PROMINENCE`] of the peak's prominence over the floor, `1 - sqrt(floor / peak)`, no frame stands out and the
//! confidence is 0, whatever the frames' contrast and exposure; above it, the confidence is the peak's local contrast
//! less the runner-up's, `sqrt(peak) - sqrt(runner)`, low where frames at different depths are about as sharp.

use florastack_core::ops::{BlurMethod, gaussian_blur_in_place};
use florastack_core::{ColorSpace, Image, Monitor, Rect};
use rayon::prelude::*;

use super::{DEPTH_MAP, DepthSettings, MAX_FRAMES};
use crate::average::{MIN_COVERAGE, luminance, reach};
use crate::error::StackError;
use crate::method::{StackShape, Stacked, Stacker};
use crate::source::{FrameSource, Patch};
use crate::tiled::{Share, StatePlane, TileKernel, TileView, Tiled, Tiling};

/// The values of a pixel's running record: the sharpest frame, its focus, its neighbors' focus, the last frame's, the
/// runner-up's, the floor and the guide (see the module documentation).
pub const RECORD: usize = 8;

/// The least prominence of a pixel's sharpest frame over the floor of its focus curve, `1 - sqrt(floor / peak)`, for
/// any confidence: the least sharp frame's focus at most a quarter of the sharpest's, its local contrast at most half.
/// See the module documentation.
pub const MIN_PROMINENCE: f32 = 0.5;

/// The channels of a tile's result: the interpolated depth, the confidence and the guide luminance.
const STATS: usize = 3;

/// A record with no frame yet.
const EMPTY: [f32; RECORD] = [-1.0, -1.0, -1.0, -1.0, -1.0, -1.0, f32::INFINITY, 0.0];

/// A blurred coverage at least this is a fully covered neighborhood, short of the blur's rounding.
const FULL_WEIGHT: f32 = 1.0 - 1e-4;

/// A pixel's confidence from its peak focus, its runner-up's and its floor: see the module documentation.
fn confidence(peak: f32, runner: f32, floor: f32) -> f32 {
    if peak.is_nan() || peak <= 0.0 || 1.0 - (floor.max(0.0) / peak).sqrt() < MIN_PROMINENCE {
        return 0.0;
    }
    (peak.sqrt() - runner.max(0.0).sqrt()).max(0.0)
}

/// Pass 1 as a [`TileKernel`]. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct FocusKernel {
    radius: f64,
}

impl FocusKernel {
    pub fn new(settings: &DepthSettings) -> Result<FocusKernel, StackError> {
        settings.validate()?;
        Ok(FocusKernel { radius: settings.radius })
    }

    /// The focus measure of the frame `patch` holds, on every pixel of the patch, and its luminance: the modified
    /// Laplacian of the luminance blurred over the radius (by normalized convolution where the frame covers only part
    /// of the patch, so that its edge against the pixels it doesn't cover is no detail), squared and averaged over the
    /// radius, its taps left out where they would read a pixel the frame doesn't cover; -1 where the frame doesn't
    /// cover the pixel (coverage below [`MIN_COVERAGE`], or a sample that isn't finite). Pixels within the kernel's
    /// context of the patch's edge see a clamped border. The CPU reference of the focus measure.
    pub fn measure(&self, patch: &Patch) -> Result<(Image<f32>, Image<f32>), StackError> {
        let image = &patch.image;
        let (w, h, c) = (image.width(), image.height(), image.channels());
        let mut y = Image::new(w, h, 1, ColorSpace::LINEAR)?;
        let mut covered = Image::new(w, h, 1, ColorSpace::LINEAR)?;
        let given = patch.coverage.as_ref();
        y.par_rows_mut().zip(covered.par_rows_mut()).zip(image.par_rows()).enumerate().for_each(|(j, ((y_row, cov_row), pixels))| {
            let given_row = given.and_then(|g| g.row(j));
            for (i, ((yv, cv), pixel)) in y_row.iter_mut().zip(cov_row.iter_mut()).zip(pixels.chunks_exact(c)).enumerate() {
                let k = given_row.and_then(|r| r.get(i)).copied().unwrap_or(1.0);
                let covers = k.is_finite() && k >= MIN_COVERAGE;
                (*yv, *cv) = match luminance(pixel) {
                    Some(l) if covers => (l, 1.0),
                    _ => (0.0, 0.0),
                };
            }
        });
        let whole = covered.as_raw().iter().all(|&k| k > 0.0);
        let mut smooth = y.clone();
        gaussian_blur_in_place(&mut smooth, self.radius, BlurMethod::Auto)?;
        if !whole {
            // Normalized convolution: the blur of the covered pixels, divided by the blur of the coverage where the
            // blur reaches pixels the frame doesn't cover. Elsewhere the coverage's blur is 1 but for its rounding, and
            // dividing by it would make the patch's pixels depend on whether the patch holds uncovered pixels at all,
            // and so on the tiling.
            let mut weight = covered.clone();
            gaussian_blur_in_place(&mut weight, self.radius, BlurMethod::Auto)?;
            smooth.par_rows_mut().zip(weight.par_rows()).for_each(|(s, k)| {
                for (s, &k) in s.iter_mut().zip(k) {
                    if k < FULL_WEIGHT {
                        *s = if k > 1e-6 { *s / k } else { 0.0 };
                    }
                }
            });
        }
        let mut focus = Image::new(w, h, 1, ColorSpace::LINEAR)?;
        let (ys, cs): (Vec<&[f32]>, Vec<&[f32]>) = (smooth.rows().collect(), covered.rows().collect());
        focus.par_rows_mut().enumerate().for_each(|(j, row)| {
            let (up, down) = (j.saturating_sub(1), (j + 1).min(h - 1));
            let (ym, yu, yd) = (at(&ys, j), at(&ys, up), at(&ys, down));
            let (cm, cu, cd) = (at(&cs, j), at(&cs, up), at(&cs, down));
            for (i, f) in row.iter_mut().enumerate() {
                let (l, r) = (i.saturating_sub(1), (i + 1).min(w - 1));
                let taps = [cm.get(i), cm.get(l), cm.get(r), cu.get(i), cd.get(i)];
                let values = (ym.get(i), ym.get(l), ym.get(r), yu.get(i), yd.get(i));
                *f = match values {
                    (Some(&v), Some(&vl), Some(&vr), Some(&vu), Some(&vd)) if taps.iter().all(|t| t.is_some_and(|&k| k > 0.0)) => {
                        let ml = (2.0 * v - vl - vr).abs() + (2.0 * v - vu - vd).abs();
                        ml * ml
                    }
                    _ => 0.0,
                };
            }
        });
        drop((ys, cs));
        drop(smooth);
        gaussian_blur_in_place(&mut focus, self.radius, BlurMethod::Auto)?;
        focus.par_rows_mut().zip(covered.par_rows()).for_each(|(row, cov)| {
            for (f, &k) in row.iter_mut().zip(cov) {
                if k <= 0.0 {
                    *f = -1.0;
                }
            }
        });
        Ok((focus, y))
    }
}

/// The samples of `row` (of one channel) in `rect`'s columns.
fn columns(row: &[f32], rect: Rect) -> &[f32] {
    row.get(rect.x..rect.x + rect.width).unwrap_or(&[])
}

/// Row `r` of `rows`, or no samples.
fn at<'a>(rows: &[&'a [f32]], r: usize) -> &'a [f32] {
    rows.get(r).copied().unwrap_or(&[])
}

/// Folds frame `i`'s focus `v` (-1 where it doesn't cover) and luminance `y` into a pixel's record.
#[allow(clippy::float_cmp)] // Frame indices are whole numbers, exact in `f32` up to MAX_FRAMES.
fn update(record: &mut [f32; RECORD], v: f32, y: f32, i: f32) {
    let [best, peak, before, after, last, runner, floor, guide] = record;
    if v < 0.0 {
        *last = -1.0;
        return;
    }
    if v > *peak {
        if *peak >= 0.0 {
            // The runner-up's frames are those at least two from the sharpest: from the new one at `i`, every frame up
            // to `i - 2`. An old sharpest that far back is the largest of them; one at `i - 1` brings its own
            // "before", at `i - 2`, to those it already had.
            *runner = if *best <= i - 2.0 { *peak } else { runner.max(*before) };
        }
        *before = *last;
        *after = -1.0;
        *best = i;
        *peak = v;
        *guide = y;
    } else if *best + 1.0 == i {
        *after = v;
    } else {
        *runner = runner.max(v);
    }
    *floor = floor.min(v);
    *last = v;
}

/// The sharpest frame's index refined between frames: the peak of the Gaussian through the focus of the frames before,
/// at and after it, within half a frame of it; the index itself where a neighbor is missing.
fn interpolate(best: f32, before: f32, peak: f32, after: f32) -> f32 {
    /// Keeps the logarithms of a focus of 0 finite.
    const TINY: f32 = 1e-30;
    if before < 0.0 || after < 0.0 || peak <= 0.0 {
        return best;
    }
    let (lb, lp, la) = ((before + TINY).ln(), (peak + TINY).ln(), (after + TINY).ln());
    let curvature = lb - 2.0 * lp + la;
    // The peak is the largest of the three, so the curvature is negative unless they are all alike (or not numbers).
    if curvature.is_nan() || curvature >= 0.0 {
        return best;
    }
    best + (0.5 * (lb - la) / curvature).clamp(-0.5, 0.5)
}

/// A tile's state: its records in two planes of four values each.
fn planes(state: &[StatePlane]) -> Result<[&Image<f32>; 2], StackError> {
    match state {
        [StatePlane::F32(a), StatePlane::F32(b)] => Ok([a, b]),
        _ => Err(StackError::Shape { width: 0, height: 0, channels: state.len() }),
    }
}

/// A pixel's record, from its values in the two planes.
fn join(a: [f32; 4], b: [f32; 4]) -> [f32; RECORD] {
    let ([a0, a1, a2, a3], [b0, b1, b2, b3]) = (a, b);
    [a0, a1, a2, a3, b0, b1, b2, b3]
}

/// A pixel's record split into its values in the two planes.
fn split(r: [f32; RECORD]) -> ([f32; 4], [f32; 4]) {
    let [a0, a1, a2, a3, b0, b1, b2, b3] = r;
    ([a0, a1, a2, a3], [b0, b1, b2, b3])
}

impl TileKernel for FocusKernel {
    fn name(&self) -> &'static str {
        DEPTH_MAP
    }

    fn context(&self) -> usize {
        // The blur of the luminance, the modified Laplacian and its covered-taps test (one pixel), then the window.
        2 * reach(self.radius) + 1
    }

    fn state(&self, inner: Rect, _channels: usize) -> Result<Vec<StatePlane>, StackError> {
        let (w, h) = (inner.width, inner.height);
        let mut a = Image::new(w, h, 4, ColorSpace::LINEAR)?;
        let mut b = Image::new(w, h, 4, ColorSpace::LINEAR)?;
        let (ea, eb) = split(EMPTY);
        a.par_rows_mut().for_each(|row| row.as_chunks_mut::<4>().0.fill(ea));
        b.par_rows_mut().for_each(|row| row.as_chunks_mut::<4>().0.fill(eb));
        Ok(vec![StatePlane::F32(a), StatePlane::F32(b)])
    }

    fn fold(&self, state: &mut [StatePlane], patch: &Patch, view: &TileView) -> Result<Option<Share>, StackError> {
        if view.frame >= MAX_FRAMES {
            return Err(StackError::TooManyFrames { max: MAX_FRAMES });
        }
        let (focus, y) = self.measure(patch)?;
        let [StatePlane::F32(a), StatePlane::F32(b)] = state else {
            return Err(StackError::Shape { width: 0, height: 0, channels: state.len() });
        };
        let inner = view.inner_in_patch();
        #[allow(clippy::cast_precision_loss)] // Below MAX_FRAMES, exact.
        let i = view.frame as f32;
        let rows: Vec<(&[f32], &[f32])> =
            focus.rows().zip(y.rows()).skip(inner.y).take(inner.height).map(|(f, y)| (columns(f, inner), columns(y, inner))).collect();
        a.par_rows_mut().zip(b.par_rows_mut()).zip(rows.into_par_iter()).for_each(|((a, b), (focus, y))| {
            let (a, b) = (a.as_chunks_mut::<4>().0, b.as_chunks_mut::<4>().0);
            for (((a, b), &v), &l) in a.iter_mut().zip(b.iter_mut()).zip(focus).zip(y) {
                let mut r = join(*a, *b);
                update(&mut r, v, l, i);
                (*a, *b) = split(r);
            }
        });
        Ok(None)
    }

    fn covered(&self, state: &[StatePlane]) -> Result<Image<f32>, StackError> {
        let [a, _] = planes(state)?;
        let mut covered = Image::new(a.width(), a.height(), 1, ColorSpace::LINEAR)?;
        covered.par_rows_mut().zip(a.par_rows()).for_each(|(out, a)| {
            for (o, &[_, peak, ..]) in out.iter_mut().zip(a.as_chunks::<4>().0) {
                *o = if peak >= 0.0 { 1.0 } else { 0.0 };
            }
        });
        Ok(covered)
    }

    fn result(&self, state: &[StatePlane], _inner: Rect, _channels: usize) -> Result<Image<f32>, StackError> {
        let [a, b] = planes(state)?;
        let mut out = Image::new(a.width(), a.height(), STATS, ColorSpace::LINEAR)?;
        out.par_rows_mut().zip(a.par_rows().zip(b.par_rows())).for_each(|(out, (a, b))| {
            let (a, b) = (a.as_chunks::<4>().0, b.as_chunks::<4>().0);
            for (o, (&a, &b)) in out.as_chunks_mut::<STATS>().0.iter_mut().zip(a.iter().zip(b)) {
                let [best, peak, before, after, _, runner, floor, guide] = join(a, b);
                *o = if peak >= 0.0 { [interpolate(best, before, peak, after), confidence(peak, runner, floor), guide] } else { [0.0; STATS] };
            }
        });
        Ok(out)
    }
}

/// Pass 1 of the depth-map method: frames folded one at a time into the focus statistics. See the module
/// documentation of [`super`].
pub struct Estimator {
    tiled: Tiled<FocusKernel>,
}

impl Estimator {
    pub fn new(settings: &DepthSettings, tiling: &Tiling) -> Result<Estimator, StackError> {
        Ok(Estimator { tiled: Tiled::new(FocusKernel::new(settings)?, tiling.clone()) })
    }

    pub fn kernel(&self) -> &FocusKernel {
        self.tiled.kernel()
    }

    /// Starts on a canvas of `shape`, dropping any pass in progress.
    pub fn begin(&mut self, shape: StackShape) -> Result<(), StackError> {
        self.tiled.begin(shape)
    }

    /// Folds the next frame in, as [`Stacker::add_frame`] does.
    pub fn add_frame(&mut self, frame: &dyn FrameSource, monitor: &dyn Monitor) -> Result<(), StackError> {
        self.tiled.add_frame(frame, monitor)
    }

    /// The statistics of the frames added since `begin`, which ends the pass.
    pub fn finish(&mut self, monitor: &dyn Monitor) -> Result<FocusStats, StackError> {
        let Stacked { image, alpha, frames, uncovered } = self.tiled.finish(monitor)?;
        FocusStats::from_stats(&image, alpha, frames, uncovered, self.tiled.kernel().radius)
    }
}

/// What pass 1 found, kept for the depth map and every change of its threshold: whole-canvas planes of one channel.
#[derive(Clone, Debug, PartialEq)]
pub struct FocusStats {
    /// The frames measured.
    pub frames: usize,
    /// Each pixel's sharpest frame, interpolated between frames: from 0 to `frames - 1`, 0 where no frame covers.
    pub depth: Image<f32>,
    /// How clearly that frame stands out: see the module documentation of [`super`]; 0 where no frame covers.
    pub confidence: Image<f32>,
    /// The sharpest frame's linear luminance at each pixel, which guides the smoothing of the depth.
    pub guide: Image<f32>,
    /// 1 where some frame covers the pixel, 0 where none does; `None` when frames cover every pixel.
    pub covered: Option<Image<f32>>,
    /// How many pixels no frame covers.
    pub uncovered: usize,
    /// The estimation radius the focus was measured over.
    pub radius: f64,
}

impl FocusStats {
    fn from_stats(stats: &Image<f32>, covered: Option<Image<f32>>, frames: usize, uncovered: usize, radius: f64) -> Result<FocusStats, StackError> {
        let (w, h) = (stats.width(), stats.height());
        let mut depth = Image::new(w, h, 1, ColorSpace::LINEAR)?;
        let mut confidence = Image::new(w, h, 1, ColorSpace::LINEAR)?;
        let mut guide = Image::new(w, h, 1, ColorSpace::LINEAR)?;
        depth.par_rows_mut().zip(confidence.par_rows_mut()).zip(guide.par_rows_mut()).zip(stats.par_rows()).for_each(|(((d, c), g), s)| {
            for (((d, c), g), &[depth, confidence, guide]) in d.iter_mut().zip(c.iter_mut()).zip(g.iter_mut()).zip(s.as_chunks::<STATS>().0) {
                (*d, *c, *g) = (depth, confidence, guide);
            }
        });
        Ok(FocusStats { frames, depth, confidence, guide, covered, uncovered, radius })
    }

    pub fn width(&self) -> usize {
        self.depth.width()
    }

    pub fn height(&self) -> usize {
        self.depth.height()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEST: usize = 0;
    const BEFORE: usize = 2;
    const AFTER: usize = 3;
    const RUNNER: usize = 5;
    const FLOOR: usize = 6;
    const GUIDE: usize = 7;

    /// Runs `focus` (one value per frame) through a fresh record.
    fn run(focus: &[f32]) -> [f32; RECORD] {
        let mut r = EMPTY;
        for (i, &v) in focus.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            update(&mut r, v, 0.1 * i as f32, i as f32);
        }
        r
    }

    /// The record's runner-up computed directly: the most focus at least two frames from the sharpest.
    fn runner(focus: &[f32]) -> f32 {
        let best = focus.iter().enumerate().fold((0, -1.0_f32), |(b, p), (i, &v)| if v > p { (i, v) } else { (b, p) }).0;
        focus.iter().enumerate().filter(|&(i, &v)| i.abs_diff(best) >= 2 && v >= 0.0).map(|(_, &v)| v).fold(-1.0, f32::max)
    }

    #[test]
    fn the_record_keeps_the_sharpest_frame_its_neighbors_the_runner_up_and_the_floor() {
        let r = run(&[0.1, 0.4, 0.9, 0.5, 0.2, 0.3]);
        assert_eq!(&r[..GUIDE], &[2.0, 0.9, 0.4, 0.5, 0.3, 0.3, 0.1]);
        assert!((r[GUIDE] - 0.2).abs() < 1e-6, "the guide is the sharpest frame's luminance");
        // Ties keep the earlier frame; a frame that doesn't cover is no neighbor, and has no say in the floor.
        let r = run(&[0.5, 0.5, -1.0, 0.2]);
        assert_eq!((r[BEST], r[BEFORE], r[AFTER], r[RUNNER], r[FLOOR]), (0.0, -1.0, 0.5, 0.2, 0.2));
        let r = run(&[0.1, -1.0, 0.8, 0.3]);
        assert_eq!((r[BEST], r[BEFORE], r[AFTER]), (2.0, -1.0, 0.3));
        assert_eq!(run(&[-1.0, -1.0]), EMPTY, "no covering frame leaves the record empty");
        // The runner-up, kept as the sharpest frame moves, against the direct computation, on random focus curves with
        // gaps.
        let mut seed = 7_u64;
        for _ in 0..2000 {
            let focus: Vec<f32> = (0..9)
                .map(|_| {
                    seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                    #[allow(clippy::cast_precision_loss)]
                    let u = (seed >> 40) as f32 / (1u64 << 24) as f32;
                    if u < 0.1 { -1.0 } else { u }
                })
                .collect();
            let r = run(&focus);
            assert_eq!(r[RUNNER], runner(&focus), "{focus:?}");
            assert_eq!(r[FLOOR], focus.iter().copied().filter(|&v| v >= 0.0).fold(f32::INFINITY, f32::min), "{focus:?}");
        }
    }

    #[test]
    fn noise_has_no_confident_peak() {
        assert_eq!(confidence(1.0, -1.0, 0.01), 1.0, "a clear peak and no runner-up: all of its contrast");
        assert!((confidence(1.0, 0.04, 0.0) - 0.8).abs() < 1e-6);
        assert_eq!(confidence(1.0, 0.3, 0.3), 0.0, "a curve that never falls far below its peak: noise");
        assert!(confidence(1.0, 0.3, 0.24) > 0.0);
        assert_eq!(confidence(0.0, -1.0, 0.0), 0.0);
        assert_eq!(confidence(f32::NAN, -1.0, 0.0), 0.0);
    }

    #[test]
    fn the_peak_is_interpolated_between_frames() {
        // A Gaussian focus curve peaking at 4.3: the fit recovers the peak from its three samples.
        let curve = |i: f32| (-(i - 4.3) * (i - 4.3) / 2.0).exp();
        let d = interpolate(4.0, curve(3.0), curve(4.0), curve(5.0));
        assert!((d - 4.3).abs() < 1e-4, "{d}");
        assert_eq!(interpolate(0.0, -1.0, 1.0, 0.5), 0.0, "no frame before the first");
        assert_eq!(interpolate(3.0, 0.2, 0.0, 0.0), 3.0, "no detail anywhere");
        let d = interpolate(2.0, 0.999, 1.0, 1e-9);
        assert!((1.5..2.0).contains(&d), "within half a frame: {d}");
    }

    #[test]
    fn the_focus_measure_follows_detail_and_coverage() {
        let kernel = FocusKernel::new(&DepthSettings { radius: 1.0, ..DepthSettings::default() }).unwrap();
        // A checkerboard of 4-pixel squares on the left, flat on the right.
        let image = Image::from_fn(64, 32, 3, ColorSpace::LINEAR, |x, y, _| if x < 32 { ((x / 4 + y / 4) % 2) as f32 * 0.5 } else { 0.25 }).unwrap();
        let (focus, y) = kernel.measure(&Patch { image: image.clone(), coverage: None }).unwrap();
        let (sharp, flat) = (focus.get(12, 16, 0).unwrap(), focus.get(56, 16, 0).unwrap());
        assert!(sharp > 1e-3 && flat < 1e-9, "{sharp} {flat}");
        assert!((y.get(54, 16, 0).unwrap() - 0.25).abs() < 1e-6);
        // The frame's edge against the pixels it doesn't cover is no detail, nor its zeros beyond.
        let coverage = Image::from_fn(64, 32, 1, ColorSpace::LINEAR, |x, _, _| if x < 48 { 1.0 } else { 0.0 }).unwrap();
        let mut zeroed = image;
        for y in 0..32 {
            for x in 48..64 {
                zeroed.pixel_mut(x, y).unwrap().fill(0.0);
            }
        }
        let (focus, _) = kernel.measure(&Patch { image: zeroed, coverage: Some(coverage) }).unwrap();
        assert_eq!(focus.get(50, 16, 0), Some(-1.0));
        assert!(focus.get(47, 16, 0).unwrap() < 1e-9, "no false detail at the coverage edge: {}", focus.get(47, 16, 0).unwrap());
        // A sample that isn't finite leaves its pixel uncovered.
        let mut broken = Image::from_fn(16, 16, 3, ColorSpace::LINEAR, |_, _, _| 0.5).unwrap();
        broken.pixel_mut(5, 5).unwrap()[2] = f32::NAN;
        let (focus, _) = kernel.measure(&Patch { image: broken, coverage: None }).unwrap();
        assert_eq!(focus.get(5, 5, 0), Some(-1.0));
        assert!(focus.as_raw().iter().all(|v| v.is_finite()));
    }
}
