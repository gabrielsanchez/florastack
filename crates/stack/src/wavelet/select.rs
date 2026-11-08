//! Pass 1 of the wavelet method: each frame's activity at every level, and the frame with the most of it at each
//! position ([`SelectKernel`], a [`TileKernel`] that [`Selector`] runs tile by tile), which end as the [`Choices`] pass 2
//! composes from. See the module documentation of [`super`].
//!
//! Decisions are made on luminance alone, which is a linear combination of the channels, so only one plane is
//! transformed here. A tile keeps, per level, the most activity so far and its frame (8 bytes a pixel), or, for a
//! finest level that blends, the sum of the frames' blend weights (4). The levels of the whole-canvas part keep the same
//! on the 1/8-size low-pass.

use std::sync::Arc;

use florastack_core::{ColorSpace, Image, Monitor, Rect, Tile};
use rayon::prelude::*;

use super::frame::{Layout, Split, context, gather, read_region, tile_extension};
use super::transform::{Bank, Spare, activity, analyze, extend, extension, pool};
use super::{Lowpass, WAVELET, WaveletSettings};
use crate::depth::MAX_FRAMES;
use crate::error::StackError;
use crate::method::{StackShape, Stacked, Stacker};
use crate::pyramid::TILED_LEVELS;
use crate::pyramid::kernel::{Pyramid, filled, pick_weight};
use crate::pyramid::levels::{luminance, mirror, signed};
use crate::source::{FrameSource, Patch};
use crate::tiled::{Share, StatePlane, TileKernel, TileView, Tiled, Tiling};

/// No frame covers this position.
pub(super) const NO_FRAME: f32 = -1.0;

/// The level of the whole-canvas part that decides for the levels above it.
const ANCHOR: usize = 1;

/// What decides each of the `levels` levels of the whole-canvas part, from a frame's `luminance` there, and last what
/// weighs its low-pass: a level's activity up to [`ANCHOR`], then that level's, pooled to the scale of each level above
/// it. See the module documentation of [`super`]. Without levels, nothing.
pub(super) fn deciding(bank: &Bank, window: u32, luminance: &Image<f32>, levels: usize, spare: &Spare) -> Result<Vec<Image<f32>>, StackError> {
    let mut out = Vec::with_capacity(levels + 1);
    if levels == 0 {
        return Ok(out);
    }
    let anchor = ANCHOR.min(levels - 1);
    let mut plane = spare.copy(luminance)?;
    for j in 0..=anchor {
        let (low, details) = analyze(bank, &plane, 1 << j, spare)?;
        out.push(activity(&details, window, spare)?);
        details.recycle(spare);
        spare.give(std::mem::replace(&mut plane, low));
    }
    spare.give(plane);
    let Some(last) = out.last() else { return Ok(out) };
    let mut energy = spare.copy(last)?;
    let mut pooled = 0;
    for j in anchor + 1..=levels {
        while pooled < j {
            let next = pool(&energy, 1 << pooled, spare)?;
            spare.give(std::mem::replace(&mut energy, next));
            pooled += 1;
        }
        out.push(spare.copy(&energy)?);
    }
    spare.give(energy);
    Ok(out)
}

/// The most activity so far at a coarse level, and its frame.
#[derive(Clone, Debug)]
struct CoarsePick {
    best: Image<f32>,
    frame: Image<f32>,
}

/// Pass 1 as a [`TileKernel`]. See the module documentation.
#[derive(Debug)]
pub struct SelectKernel {
    settings: WaveletSettings,
    bank: &'static Bank,
    /// The finest level's blend power, `1 / grit`; `None` picks.
    power: Option<f32>,
    layout: Option<Layout>,
    coarse: Vec<CoarsePick>,
    /// With [`Lowpass::Focus`], the most of what weighs a low-pass so far, on the extended plane.
    peak: Option<Image<f32>>,
    frames: usize,
    spare: Spare,
}

/// Folds a level's `energy` into `best` and `frame`, planes of `at`'s size: a frame replaces the one before it where it
/// has more activity, weighed by its pooled coverage; a tie keeps the earlier frame, as the pyramid's does.
fn pick(energy: &Image<f32>, coverage: Option<&Image<f32>>, at: Rect, index: f32, best: &mut Image<f32>, frame: &mut Image<f32>) {
    best.par_rows_mut().zip(frame.par_rows_mut()).enumerate().for_each(|(j, (best, frame))| {
        let Some(e) = energy.row(at.y + j).and_then(|r| r.get(at.x..at.x + at.width)) else { return };
        let cov = coverage.and_then(|k| k.row(at.y + j)).and_then(|r| r.get(at.x..at.x + at.width));
        for (x, ((best, frame), &e)) in best.iter_mut().zip(frame.iter_mut()).zip(e).enumerate() {
            let weight = pick_weight(cov.map_or(1.0, |k| k.get(x).copied().unwrap_or(0.0)));
            if weight > 0.0 && e * weight > *best {
                *best = e * weight;
                *frame = index;
            }
        }
    });
}

impl SelectKernel {
    pub fn new(settings: &WaveletSettings) -> Result<SelectKernel, StackError> {
        settings.validate()?;
        Ok(SelectKernel {
            settings: settings.clone(),
            bank: settings.wavelet.bank(),
            power: settings.power(),
            layout: None,
            coarse: Vec::new(),
            peak: None,
            frames: 0,
            spare: Spare::default(),
        })
    }

    fn layout(&self) -> Result<Layout, StackError> {
        self.layout.ok_or(StackError::NotStarted)
    }

    /// The state's plane of level `k`: its most activity (or sum of blend weights), and after it its frame.
    fn plane(&self, k: usize) -> usize {
        match (self.power.is_some(), k) {
            (true, 0) => 0,
            (true, k) => 2 * k - 1,
            (false, k) => 2 * k,
        }
    }
}

/// Folds a frame's blend weights at the finest level into `sum`, a plane of `at`'s size.
fn blend(power: f32, energy: &Image<f32>, coverage: Option<&Image<f32>>, at: Rect, sum: &mut Image<f32>) {
    sum.par_rows_mut().enumerate().for_each(|(j, sum)| {
        let Some(e) = energy.row(at.y + j).and_then(|r| r.get(at.x..at.x + at.width)) else { return };
        let cov = coverage.and_then(|k| k.row(at.y + j)).and_then(|r| r.get(at.x..at.x + at.width));
        for (x, (sum, &e)) in sum.iter_mut().zip(e).enumerate() {
            *sum += blend_weight(power, e, cov.map_or(1.0, |k| k.get(x).copied().unwrap_or(0.0)));
        }
    });
}

/// A frame's weight in the finest level's blend, for an activity `energy` and a pooled coverage `coverage`.
pub(super) fn blend_weight(power: f32, energy: f32, coverage: f32) -> f32 {
    if coverage >= crate::average::MIN_COVERAGE { Pyramid::blend_weight(power, energy, coverage.min(1.0)) } else { 0.0 }
}

impl TileKernel for SelectKernel {
    fn name(&self) -> &'static str {
        WAVELET
    }

    fn context(&self) -> usize {
        context(self.bank)
    }

    fn read_region(&self, outer: Rect, width: usize, height: usize) -> Rect {
        read_region(outer, width, height)
    }

    fn start(&mut self, shape: StackShape) -> Result<(), StackError> {
        let levels = self.settings.levels_for(shape.width, shape.height);
        let layout = Layout { width: shape.width, height: shape.height, channels: shape.channels, levels, tiled: levels.min(TILED_LEVELS) };
        let small = layout.small();
        self.coarse = (0..layout.coarse())
            .map(|_| Ok(CoarsePick { best: filled(small.width, small.height, 1, NO_FRAME)?, frame: filled(small.width, small.height, 1, NO_FRAME)? }))
            .collect::<Result<_, StackError>>()?;
        let by = extension(self.bank, layout.coarse());
        self.peak = (self.settings.lowpass == Lowpass::Focus && layout.coarse() > 0)
            .then(|| filled(small.width + 2 * by, small.height + 2 * by, 1, 0.0))
            .transpose()?;
        self.layout = Some(layout);
        self.frames = 0;
        Ok(())
    }

    fn state(&self, inner: Rect, _channels: usize) -> Result<Vec<StatePlane>, StackError> {
        let layout = self.layout()?;
        let mut planes = Vec::new();
        for k in 0..layout.tiled {
            if k == 0 && self.power.is_some() {
                planes.push(StatePlane::F32(filled(inner.width, inner.height, 1, 0.0)?));
            } else {
                planes.push(StatePlane::F32(filled(inner.width, inner.height, 1, NO_FRAME)?));
                planes.push(StatePlane::F32(filled(inner.width, inner.height, 1, NO_FRAME)?));
            }
        }
        Ok(planes)
    }

    fn fold(&self, state: &mut [StatePlane], patch: &Patch, view: &TileView) -> Result<Option<Share>, StackError> {
        let layout = self.layout()?;
        let span = layout.span(view.region);
        let split = Split::new(patch, span, layout.tiled)?;
        let by = tile_extension(self.bank, layout.tiled);
        let spare = &self.spare;
        let mut plane = extend(&luminance(&split.residual)?, span, by, 0, spare)?;
        let mut coverage = split.coverage().map(|k| extend(k, span, by, 0, spare)).transpose()?;
        let inner = view.inner_in_patch();
        let at = Rect::new(inner.x + by, inner.y + by, inner.width, inner.height);
        #[allow(clippy::cast_precision_loss)] // Below MAX_FRAMES, exact.
        let index = view.frame as f32;
        let planes = state.len();
        let bad = || StackError::Shape { width: 0, height: 0, channels: planes };
        for k in 0..layout.tiled {
            let dilation = 1 << k;
            let (low, details) = analyze(self.bank, &plane, dilation, spare)?;
            let energy = activity(&details, self.settings.window, spare)?;
            details.recycle(spare);
            if let Some(last) = coverage.take() {
                coverage = Some(pool(&last, dilation, spare)?);
                spare.give(last);
            }
            let first = self.plane(k);
            if let (Some(power), 0) = (self.power, k) {
                let sum = state.get_mut(first).and_then(StatePlane::as_f32_mut).ok_or_else(bad)?;
                blend(power, &energy, coverage.as_ref(), at, sum);
            } else {
                let Some([best, frame]) = state.get_mut(first..first + 2) else { return Err(bad()) };
                let (Some(best), Some(frame)) = (best.as_f32_mut(), frame.as_f32_mut()) else { return Err(bad()) };
                pick(&energy, coverage.as_ref(), at, index, best, frame);
            }
            spare.give(energy);
            spare.give(std::mem::replace(&mut plane, low));
        }
        spare.give(plane);
        if let Some(coverage) = coverage {
            spare.give(coverage);
        }
        Ok(Some(split.share(&layout, view)?))
    }

    fn absorb(&mut self, shares: Vec<(Tile, Share)>) -> Result<(), StackError> {
        let layout = self.layout()?;
        let (values, coverage) = gather(&layout, shares)?;
        let small = layout.small();
        let by = extension(self.bank, layout.coarse());
        let plane = extend(&luminance(&values)?, small, by, 0, &self.spare)?;
        let mut pooled = extend(&coverage, small, by, 0, &self.spare)?;
        let at = Rect::new(by, by, small.width, small.height);
        #[allow(clippy::cast_precision_loss)] // Below MAX_FRAMES, exact.
        let index = self.frames as f32;
        let energies = deciding(self.bank, self.settings.window, &plane, layout.coarse(), &self.spare)?;
        for (j, (level, energy)) in self.coarse.iter_mut().zip(&energies).enumerate() {
            let next = pool(&pooled, 1 << j, &self.spare)?;
            self.spare.give(std::mem::replace(&mut pooled, next));
            pick(energy, Some(&pooled), at, index, &mut level.best, &mut level.frame);
        }
        if let (Some(peak), Some(energy)) = (self.peak.as_mut(), energies.last()) {
            peak.par_rows_mut().zip(energy.par_rows()).for_each(|(peak, energy)| {
                for (p, &e) in peak.iter_mut().zip(energy) {
                    *p = p.max(e);
                }
            });
        }
        self.spare.give(plane);
        self.spare.give(pooled);
        for energy in energies {
            self.spare.give(energy);
        }
        self.frames += 1;
        Ok(())
    }

    fn covered(&self, state: &[StatePlane]) -> Result<Image<f32>, StackError> {
        let Some(first) = state.first().and_then(StatePlane::as_f32) else {
            return Err(StackError::Shape { width: 0, height: 0, channels: state.len() });
        };
        let blended = self.power.is_some();
        let mut covered = first.clone();
        covered.par_rows_mut().for_each(|row| {
            for v in row {
                *v = if (blended && *v > 0.0) || (!blended && *v >= 0.0) { 1.0 } else { 0.0 };
            }
        });
        Ok(covered)
    }

    /// One channel per tiled level: its frames, or for a finest level that blends, the sum of its weights.
    fn result(&self, state: &[StatePlane], inner: Rect, _channels: usize) -> Result<Image<f32>, StackError> {
        let layout = self.layout()?;
        let c = layout.tiled;
        let bad = || StackError::Shape { width: 0, height: 0, channels: state.len() };
        let mut out = Image::new(inner.width, inner.height, c, ColorSpace::LINEAR)?;
        for k in 0..c {
            let at = if k == 0 && self.power.is_some() { self.plane(0) } else { self.plane(k) + 1 };
            let plane = state.get(at).and_then(StatePlane::as_f32).ok_or_else(bad)?;
            out.par_rows_mut().zip(plane.par_rows()).for_each(|(row, values)| {
                for (pixel, &v) in row.chunks_exact_mut(c).zip(values) {
                    if let Some(o) = pixel.get_mut(k) {
                        *o = v;
                    }
                }
            });
        }
        Ok(out)
    }
}

/// Pass 1 of the wavelet method, one frame at a time.
pub struct Selector {
    tiled: Tiled<SelectKernel>,
}

impl Selector {
    pub fn new(settings: &WaveletSettings, tiling: &Tiling) -> Result<Selector, StackError> {
        Ok(Selector { tiled: Tiled::new(SelectKernel::new(settings)?, super::wider(tiling)) })
    }

    /// Starts on a canvas of `shape`, dropping any pass in progress.
    pub fn begin(&mut self, shape: StackShape) -> Result<(), StackError> {
        self.tiled.begin(shape)
    }

    /// Folds the next frame in, as [`Stacker::add_frame`] does.
    pub fn add_frame(&mut self, frame: &dyn FrameSource, monitor: &dyn Monitor) -> Result<(), StackError> {
        self.tiled.add_frame(frame, monitor)
    }

    /// The choices of the frames added since `begin`, which ends the pass.
    pub fn finish(&mut self, monitor: &dyn Monitor) -> Result<Choices, StackError> {
        let Stacked { image, frames, .. } = self.tiled.finish(monitor)?;
        if frames > MAX_FRAMES {
            return Err(StackError::TooManyFrames { max: MAX_FRAMES });
        }
        let kernel = self.tiled.kernel();
        Choices::new(kernel, &image, frames)
    }
}

/// What pass 1 decided, on the whole canvas: what pass 2 composes from.
#[derive(Clone, Debug, PartialEq)]
pub struct Choices {
    pub(super) settings: WaveletSettings,
    pub(super) layout: Layout,
    /// Each tiled level's frames, finest first, or for a finest level that blends, the sum of its weights.
    pub(super) fine: Vec<Image<f32>>,
    /// Each coarse level's frames, on the 1/8-size low-pass.
    pub(super) coarse: Vec<Image<f32>>,
    /// With [`Lowpass::Select`], the low-pass's frames there.
    pub(super) low: Option<Image<f32>>,
    /// With [`Lowpass::Focus`], the most of what weighs a low-pass, on the extended plane.
    pub(super) peak: Option<Image<f32>>,
    pub(super) frames: usize,
}

impl Choices {
    fn new(kernel: &SelectKernel, stitched: &Image<f32>, frames: usize) -> Result<Choices, StackError> {
        let layout = kernel.layout()?;
        let blended = kernel.power.is_some();
        let consistent = kernel.settings.consistency;
        let mut fine = Vec::with_capacity(layout.tiled);
        for k in 0..layout.tiled {
            let c = stitched.channels();
            let mut plane = Image::new(stitched.width(), stitched.height(), 1, ColorSpace::LINEAR)?;
            let weights = k == 0 && blended;
            plane.par_rows_mut().zip(stitched.par_rows()).for_each(|(row, values)| {
                for (v, pixel) in row.iter_mut().zip(values.chunks_exact(c)) {
                    let s = pixel.get(k).copied().unwrap_or(0.0);
                    // The stitch feathers the seams between equal values, which can round a frame's index.
                    *v = if weights { s } else { s.round() };
                }
            });
            fine.push(if consistent && !weights { majority(&plane, 1 << k) } else { plane });
        }
        let mut coarse = Vec::with_capacity(layout.coarse());
        for (j, level) in kernel.coarse.iter().enumerate() {
            coarse.push(if consistent { majority(&level.frame, 1 << j) } else { level.frame.clone() });
        }
        let low = match kernel.settings.lowpass {
            Lowpass::Average | Lowpass::Focus => None,
            Lowpass::Select => {
                let last = coarse.len().saturating_sub(1);
                match coarse.last() {
                    Some(level) => Some(most_common(level, 1 << last)),
                    None => fine_frames(&layout, &fine, blended)?.map(|f| most_common(&f, 1)),
                }
            }
        };
        Ok(Choices { settings: kernel.settings.clone(), layout, fine, coarse, low, peak: kernel.peak.clone(), frames })
    }

    /// The frames folded in pass 1.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The pass-2 stacker that composes these choices, tiled as `tiling` says.
    pub fn compositor(self: Arc<Self>, tiling: &Tiling) -> Result<Tiled<super::compose::ComposeKernel>, StackError> {
        Ok(Tiled::new(super::compose::ComposeKernel::new(self)?, super::wider(tiling)))
    }
}

/// With no coarse level, the coarsest tiled level's frames on the low-pass's samples; none if that level blends.
fn fine_frames(layout: &Layout, fine: &[Image<f32>], blended: bool) -> Result<Option<Image<f32>>, StackError> {
    if layout.tiled == 1 && blended {
        return Ok(None);
    }
    let Some(level) = fine.last() else { return Ok(None) };
    let small = layout.small();
    let a = 1usize << layout.tiled;
    let plane = Image::from_fn(small.width, small.height, 1, ColorSpace::LINEAR, |x, y, _| {
        level.get((x * a).min(level.width() - 1), (y * a).min(level.height() - 1), 0).unwrap_or(NO_FRAME)
    })?;
    Ok(Some(plane))
}

/// The frames around a position, `step` samples apart over a `size` x `size` window, mirrored at the plane's edges, each
/// with how many of the window's samples chose it; positions no frame covers don't count.
fn tally(plane: &Image<f32>, x: usize, y: usize, step: usize, size: isize, counts: &mut Vec<(f32, usize)>) {
    counts.clear();
    let half = size / 2;
    for dy in -half..=half {
        for dx in -half..=half {
            let sx = mirror(signed(x) + dx * signed(step), plane.width());
            let sy = mirror(signed(y) + dy * signed(step), plane.height());
            let Some(v) = plane.get(sx, sy, 0) else { continue };
            if v < 0.0 {
                continue;
            }
            #[allow(clippy::float_cmp)] // Frame indices, exact in `f32`.
            match counts.iter_mut().find(|(frame, _)| *frame == v) {
                Some((_, n)) => *n += 1,
                None => counts.push((v, 1)),
            }
        }
    }
}

/// The most common frame in a tally; a tie takes the lowest index, so the choice doesn't depend on the scan's order.
fn winner(counts: &[(f32, usize)]) -> Option<(f32, usize)> {
    counts.iter().copied().max_by(|a, b| a.1.cmp(&b.1).then(b.0.total_cmp(&a.0)))
}

/// The consistency check of H. Li, B. S. Manjunath and S. K. Mitra ("Multisensor image fusion using the wavelet
/// transform", 1995): a position takes the frame that a strict majority of its 3x3 neighborhood chose, dilated like its
/// level. An isolated pick, which noise makes, gives way to its neighbors, and a thin structure with a real neighborhood
/// of its own stays.
pub(super) fn majority(plane: &Image<f32>, dilation: usize) -> Image<f32> {
    let mut out = plane.clone();
    out.par_rows_mut().enumerate().for_each(|(y, row)| {
        let mut counts = Vec::with_capacity(9);
        for (x, v) in row.iter_mut().enumerate() {
            if *v < 0.0 {
                continue;
            }
            tally(plane, x, y, dilation, 3, &mut counts);
            if let Some((frame, n)) = winner(&counts)
                && n >= 5
            {
                *v = frame;
            }
        }
    });
    out
}

/// The frame chosen most often in the 5x5 neighborhood of each position, dilated like its level.
fn most_common(plane: &Image<f32>, dilation: usize) -> Image<f32> {
    let mut out = plane.clone();
    out.par_rows_mut().enumerate().for_each(|(y, row)| {
        let mut counts = Vec::with_capacity(25);
        for (x, v) in row.iter_mut().enumerate() {
            tally(plane, x, y, dilation, 5, &mut counts);
            if let Some((frame, _)) = winner(&counts) {
                *v = frame;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(width: usize, values: &[f32]) -> Image<f32> {
        Image::from_vec(width, values.len() / width, 1, ColorSpace::LINEAR, values.to_vec()).unwrap()
    }

    #[test]
    fn the_majority_replaces_an_isolated_pick_and_keeps_an_edge() {
        let mut lone = vec![2.0; 25];
        lone[12] = 7.0;
        assert_eq!(majority(&plane(5, &lone), 1), plane(5, &[2.0; 25]), "an isolated pick gives way");
        let edge: Vec<f32> = (0..36).map(|i| if i % 6 < 3 { 1.0 } else { 2.0 }).collect();
        assert_eq!(majority(&plane(6, &edge), 1), plane(6, &edge), "a straight edge between two frames stays");
        let mut holes = vec![3.0; 25];
        holes[12] = NO_FRAME;
        assert_eq!(majority(&plane(5, &holes), 1).get(2, 2, 0), Some(NO_FRAME), "nothing is chosen where no frame covers");
    }

    #[test]
    fn a_tie_takes_the_lowest_frame() {
        assert_eq!(winner(&[(4.0, 2), (1.0, 2), (9.0, 1)]), Some((1.0, 2)));
    }
}
