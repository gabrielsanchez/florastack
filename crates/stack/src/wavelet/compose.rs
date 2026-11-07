//! Pass 2 of the wavelet method: each frame read again, transformed, and what [`Choices`] gave it kept
//! ([`ComposeKernel`], a [`TileKernel`] that [`Choices::compositor`] runs tile by tile). See the module documentation of
//! [`super`].
//!
//! Synthesis is linear, so the fused image is the sum over the frames of each frame's coefficients, masked by its
//! choices and synthesized: no coefficient outlives its frame. A tile keeps that sum (`4 C` bytes a pixel), whether a
//! frame covered the pixel (4), and with the range limit the frames' bounds (`4 C`): 28 bytes a pixel for RGB. A tile
//! a frame was chosen nowhere in costs that frame no transform.

use std::sync::Arc;

use florastack_core::{ColorSpace, Image, Monitor, Rect, Tile};
use rayon::prelude::*;

use super::frame::{Layout, Split, context, gather, read_region, tile_extension};
use super::select::{Choices, blend_weight, deciding};
use super::transform::{Bank, Details, Spare, activity, analyze, extend, extension, multiply, pool, synthesize};
use super::{FOCUS_POWER, WAVELET};
use crate::average::MIN_COVERAGE;
use crate::depth::MAX_FRAMES;
use crate::error::StackError;
use crate::method::StackShape;
use crate::pyramid::kernel::{filled, level_span, margin, pick_weight};
use crate::pyramid::levels::{Span, expand, local, luminance, signed};
use crate::pyramid::range::{EMPTY_HIGH, EMPTY_LOW, bound_down, bound_up, bound_value, limit_to};
use crate::source::Patch;
use crate::tiled::{Share, StatePlane, TileKernel, TileView};

/// The whole-canvas part: sums over the frames on the 1/8-size low-pass.
#[derive(Clone, Debug)]
struct Coarse {
    /// The coarse levels' details, masked and synthesized.
    details: Image<f32>,
    /// The low-pass's weighted sum and its weights, on the extended plane, and the same for the low-pass that
    /// [`Lowpass::Select`](super::Lowpass) or [`Lowpass::Focus`](super::Lowpass) make.
    low: Image<f32>,
    weight: Image<f32>,
    selected: Image<f32>,
    selected_weight: Image<f32>,
}

/// Pass 2 as a [`TileKernel`]. See the module documentation.
#[derive(Debug)]
pub struct ComposeKernel {
    choices: Arc<Choices>,
    bank: &'static Bank,
    power: Option<f32>,
    limit: f32,
    coarse: Option<Coarse>,
    /// The fused low-pass, once [`TileKernel::prepare`] has made it.
    fused: Option<Image<f32>>,
    added: usize,
    spare: Spare,
}

/// The samples of `map`, a plane of the whole level, around `span` extended by `by`, through `f`. Beyond the level,
/// `map` mirrors as the plane does.
fn window(map: &Image<f32>, span: Span, by: usize, spare: &Spare, f: impl Fn(f32) -> f32 + Sync) -> Result<Image<f32>, StackError> {
    let (w, h) = (map.width(), map.height());
    let indices = |start: usize, len: usize, full: usize| -> Vec<usize> {
        (0..len + 2 * by).map(|i| local(signed(start + i) - signed(by), full, 0, full)).collect()
    };
    let xs = indices(span.x, span.width, w);
    let ys = indices(span.y, span.height, h);
    let mut out = spare.take(xs.len(), ys.len())?;
    out.par_rows_mut().zip(ys.par_iter()).for_each(|(dst, &y)| {
        if let Some(row) = map.row(y) {
            for (v, &x) in dst.iter_mut().zip(&xs) {
                *v = f(row.get(x).copied().unwrap_or(-1.0));
            }
        }
    });
    Ok(out)
}

/// `1.0` at each sample of `span` extended by `by` where `map`, a plane of the whole level, holds `frame`; `None` if it
/// does nowhere.
fn chosen(map: &Image<f32>, frame: f32, span: Span, by: usize, spare: &Spare) -> Result<Option<Image<f32>>, StackError> {
    #[allow(clippy::float_cmp)] // Frame indices, exact in `f32`.
    let mask = window(map, span, by, spare, |v| if v == frame { 1.0 } else { 0.0 })?;
    if mask.par_rows().any(|row| row.iter().any(|&v| v > 0.0)) {
        return Ok(Some(mask));
    }
    spare.give(mask);
    Ok(None)
}

impl ComposeKernel {
    pub(super) fn new(choices: Arc<Choices>) -> Result<ComposeKernel, StackError> {
        if choices.frames == 0 {
            return Err(StackError::NoFrames);
        }
        if choices.frames > MAX_FRAMES {
            return Err(StackError::TooManyFrames { max: MAX_FRAMES });
        }
        let settings = &choices.settings;
        let (bank, power) = (settings.wavelet.bank(), settings.power());
        #[allow(clippy::cast_possible_truncation)] // From 0 to 1.
        let limit = settings.range_limit as f32;
        Ok(ComposeKernel { choices, bank, power, limit, coarse: None, fused: None, added: 0, spare: Spare::default() })
    }

    fn layout(&self) -> Layout {
        self.choices.layout
    }

    /// Each frame's share of the finest level: its blend weight over the sum of the frames', on the extended plane.
    fn blend_mask(&self, power: f32, split: &Split, span: Span, by: usize) -> Result<Option<Image<f32>>, StackError> {
        let Some(total) = self.choices.fine.first() else { return Ok(None) };
        let spare = &self.spare;
        let plane = extend(&luminance(&split.residual)?, span, by, 0, spare)?;
        let (low, details) = analyze(self.bank, &plane, 1, spare)?;
        spare.give(plane);
        spare.give(low);
        let energy = activity(&details, self.choices.settings.window, spare)?;
        details.recycle(spare);
        let coverage = split.coverage().map(|k| extend(k, span, by, 0, spare)).transpose()?.map(|k| pool(&k, 1, spare)).transpose()?;
        let sums = window(total, span, by, spare, |v| v)?;
        let mut mask = energy;
        mask.par_rows_mut().zip(sums.par_rows()).enumerate().for_each(|(y, (row, sums))| {
            let cov = coverage.as_ref().and_then(|k| k.row(y));
            for (x, (v, &sum)) in row.iter_mut().zip(sums).enumerate() {
                let w = blend_weight(power, *v, cov.map_or(1.0, |k| k.get(x).copied().unwrap_or(0.0)));
                *v = if sum > 0.0 { w / sum } else { 0.0 };
            }
        });
        spare.give(sums);
        if let Some(coverage) = coverage {
            spare.give(coverage);
        }
        Ok(Some(mask))
    }

    /// Adds `split`'s residual, channel by channel, as `masks` keep it, to `sum`, a plane of the tile's inner region.
    fn compose(&self, split: &Split, span: Span, by: usize, masks: &[Option<Image<f32>>], at: Rect, sum: &mut Image<f32>) -> Result<(), StackError> {
        let Some(top) = masks.iter().rposition(Option::is_some) else { return Ok(()) };
        let tiled = masks.len();
        let c = sum.channels();
        let spare = &self.spare;
        for channel in 0..c {
            let mut plane = extend(&split.residual, span, by, channel, spare)?;
            let mut details: Vec<Option<Details>> = Vec::with_capacity(top + 1);
            for (k, mask) in masks.iter().enumerate().take(top + 1) {
                let (low, mut level) = analyze(self.bank, &plane, 1 << k, spare)?;
                spare.give(std::mem::replace(&mut plane, low));
                if let Some(m) = mask {
                    level.scale(m);
                    details.push(Some(level));
                } else {
                    level.recycle(spare);
                    details.push(None);
                }
            }
            // The last low-pass is kept where its level is; a level above `top` chose this frame nowhere.
            if top + 1 == tiled
                && let Some(Some(mask)) = masks.last()
            {
                multiply(&mut plane, mask);
            } else {
                plane.par_rows_mut().for_each(|row| row.fill(0.0));
            }
            for (k, level) in details.into_iter().enumerate().rev() {
                let next = synthesize(self.bank, &plane, level.as_ref(), 1 << k, spare)?;
                spare.give(std::mem::replace(&mut plane, next));
                if let Some(level) = level {
                    level.recycle(spare);
                }
            }
            sum.par_rows_mut().enumerate().for_each(|(j, row)| {
                let Some(values) = plane.row(at.y + j).and_then(|r| r.get(at.x..at.x + at.width)) else { return };
                for (pixel, &v) in row.chunks_exact_mut(c).zip(values) {
                    if let Some(s) = pixel.get_mut(channel) {
                        *s += v;
                    }
                }
            });
            spare.give(plane);
        }
        Ok(())
    }

    /// A frame's weights in [`Lowpass::Focus`](super::Lowpass), on the extended plane: what weighs its low-pass against
    /// the most any frame has there, which keeps the power within `f32`, to [`FOCUS_POWER`], times `coverage`. Where no
    /// frame has any, the weights are 0 and the average stands in.
    fn focus_weights(&self, values: &Image<f32>, peak: &Image<f32>, coverage: &Image<f32>) -> Result<Option<Image<f32>>, StackError> {
        let layout = self.layout();
        let by = extension(self.bank, layout.coarse());
        let spare = &self.spare;
        let plane = extend(&luminance(values)?, layout.small(), by, 0, spare)?;
        let mut energies = deciding(self.bank, self.choices.settings.window, &plane, layout.coarse(), spare)?;
        spare.give(plane);
        let Some(mut weights) = energies.pop() else { return Ok(None) };
        for plane in energies {
            spare.give(plane);
        }
        weights.par_rows_mut().zip(peak.par_rows()).zip(coverage.par_rows()).for_each(|((row, peak), coverage)| {
            for ((w, &p), &k) in row.iter_mut().zip(peak).zip(coverage) {
                *w = if p > 0.0 { (*w / p).powi(FOCUS_POWER) * k } else { 0.0 };
            }
        });
        Ok(Some(weights))
    }

    /// Folds a frame's low-pass into the whole-canvas part.
    fn fold_coarse(&mut self, values: &Image<f32>, coverage: &Image<f32>) -> Result<(), StackError> {
        let layout = self.layout();
        let small = layout.small();
        let by = extension(self.bank, layout.coarse());
        #[allow(clippy::cast_precision_loss)] // Below MAX_FRAMES, exact.
        let frame = self.added as f32;
        let spare = &self.spare;
        let masks: Vec<Option<Image<f32>>> = self.choices.coarse.iter().map(|map| chosen(map, frame, small, by, spare)).collect::<Result<_, _>>()?;
        let mut low_mask = self.choices.low.as_ref().map(|map| chosen(map, frame, small, by, spare)).transpose()?.flatten();
        let mut pooled = extend(coverage, small, by, 0, spare)?;
        for j in 0..layout.coarse() {
            let next = pool(&pooled, 1 << j, spare)?;
            spare.give(std::mem::replace(&mut pooled, next));
        }
        pooled.par_rows_mut().for_each(|row| row.iter_mut().for_each(|k| *k = pick_weight(*k)));
        if let Some(peak) = &self.choices.peak {
            low_mask = self.focus_weights(values, peak, &pooled)?;
        }
        let Some(coarse) = self.coarse.as_mut() else { return Err(StackError::NotStarted) };
        let c = layout.channels;
        for channel in 0..c {
            let mut plane = extend(values, small, by, channel, &self.spare)?;
            let mut details: Vec<Option<Details>> = Vec::with_capacity(masks.len());
            for (j, mask) in masks.iter().enumerate() {
                let (low, mut level) = analyze(self.bank, &plane, 1 << j, &self.spare)?;
                self.spare.give(std::mem::replace(&mut plane, low));
                if let Some(m) = mask {
                    level.scale(m);
                    details.push(Some(level));
                } else {
                    level.recycle(&self.spare);
                    details.push(None);
                }
            }
            add_channel(&mut coarse.low, &plane, Some(&pooled), channel);
            if let Some(mask) = &low_mask {
                add_channel(&mut coarse.selected, &plane, Some(mask), channel);
            }
            if details.iter().any(Option::is_some) {
                let mut out = self.spare.take(plane.width(), plane.height())?;
                for (j, level) in details.into_iter().enumerate().rev() {
                    let next = synthesize(self.bank, &out, level.as_ref(), 1 << j, &self.spare)?;
                    self.spare.give(std::mem::replace(&mut out, next));
                    if let Some(level) = level {
                        level.recycle(&self.spare);
                    }
                }
                let inside = out.crop(Rect::new(by, by, small.width, small.height))?;
                self.spare.give(out);
                add_channel(&mut coarse.details, &inside, None, channel);
            }
            self.spare.give(plane);
        }
        add_channel(&mut coarse.weight, &pooled, None, 0);
        if let Some(mask) = &low_mask {
            add_channel(&mut coarse.selected_weight, mask, None, 0);
        }
        self.spare.give(pooled);
        for mask in masks.into_iter().chain([low_mask]).flatten() {
            self.spare.give(mask);
        }
        Ok(())
    }
}

/// `into[channel] += plane * weights`, with `into` of `plane`'s size.
fn add_channel(into: &mut Image<f32>, plane: &Image<f32>, weights: Option<&Image<f32>>, channel: usize) {
    let c = into.channels();
    into.par_rows_mut().zip(plane.par_rows()).enumerate().for_each(|(y, (row, values))| {
        let weight = weights.and_then(|w| w.row(y));
        for (x, (pixel, &v)) in row.chunks_exact_mut(c).zip(values).enumerate() {
            let k = weight.map_or(1.0, |w| w.get(x).copied().unwrap_or(0.0));
            if let Some(s) = pixel.get_mut(channel) {
                *s += k * v;
            }
        }
    });
}

/// Keeps each channel's least and largest value over the covering frames, after focus-stack's range limiting (see
/// `pyramid/range.rs`, whose bounds these are).
fn fold_bounds(split: &Split, inner: Rect, low: &mut Image<u16>, high: &mut Image<u16>) {
    let (image, coverage) = (split.image(), split.coverage());
    let c = image.channels();
    low.par_rows_mut().zip(high.par_rows_mut()).enumerate().for_each(|(j, (lo, hi))| {
        let Some(row) = image.row(inner.y + j) else { return };
        let cov = coverage.and_then(|k| k.row(inner.y + j));
        for (i, (l, h)) in lo.chunks_exact_mut(c).zip(hi.chunks_exact_mut(c)).enumerate() {
            let x = inner.x + i;
            if cov.is_some_and(|k| k.get(x).is_none_or(|&k| k < MIN_COVERAGE)) {
                continue;
            }
            let Some(values) = row.get(x * c..(x + 1) * c) else { continue };
            for ((l, h), &v) in l.iter_mut().zip(h.iter_mut()).zip(values) {
                if v < bound_value(*l) {
                    *l = bound_down(v);
                }
                if v > bound_value(*h) {
                    *h = bound_up(v);
                }
            }
        }
    });
}

impl TileKernel for ComposeKernel {
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
        let layout = self.layout();
        if (shape.width, shape.height) != (layout.width, layout.height) {
            return Err(StackError::Plane {
                what: "wavelet choices",
                width: layout.width,
                height: layout.height,
                channels: 1,
                expected_width: shape.width,
                expected_height: shape.height,
            });
        }
        if shape.channels != layout.channels {
            return Err(StackError::FrameShape {
                width: shape.width,
                height: shape.height,
                channels: shape.channels,
                expected_width: layout.width,
                expected_height: layout.height,
                expected_channels: layout.channels,
            });
        }
        let small = layout.small();
        let by = extension(self.bank, layout.coarse());
        let (w, h, c) = (small.width + 2 * by, small.height + 2 * by, shape.channels);
        self.coarse = Some(Coarse {
            details: filled(small.width, small.height, c, 0.0)?,
            low: filled(w, h, c, 0.0)?,
            weight: filled(w, h, 1, 0.0)?,
            selected: filled(w, h, c, 0.0)?,
            selected_weight: filled(w, h, 1, 0.0)?,
        });
        self.fused = None;
        self.added = 0;
        Ok(())
    }

    fn state(&self, inner: Rect, channels: usize) -> Result<Vec<StatePlane>, StackError> {
        let (w, h) = (inner.width, inner.height);
        let mut planes = vec![StatePlane::F32(filled(w, h, channels, 0.0)?), StatePlane::F32(filled(w, h, 1, 0.0)?)];
        if self.limit > 0.0 {
            let n = w * h * channels;
            planes.push(StatePlane::U16(Image::from_vec(w, h, channels, ColorSpace::LINEAR, vec![EMPTY_LOW; n])?));
            planes.push(StatePlane::U16(Image::from_vec(w, h, channels, ColorSpace::LINEAR, vec![EMPTY_HIGH; n])?));
        }
        Ok(planes)
    }

    fn fold(&self, state: &mut [StatePlane], patch: &Patch, view: &TileView) -> Result<Option<Share>, StackError> {
        if view.frame >= self.choices.frames {
            return Err(StackError::FrameCount { expected: self.choices.frames, got: view.frame + 1 });
        }
        let layout = self.layout();
        let span = layout.span(view.region);
        let split = Split::new(patch, span, layout.tiled)?;
        let by = tile_extension(self.bank, layout.tiled);
        #[allow(clippy::cast_precision_loss)] // Below MAX_FRAMES, exact.
        let frame = view.frame as f32;
        let mut masks = Vec::with_capacity(layout.tiled);
        for (k, map) in self.choices.fine.iter().enumerate() {
            masks.push(match (self.power, k) {
                (Some(power), 0) => self.blend_mask(power, &split, span, by)?,
                _ => chosen(map, frame, span, by, &self.spare)?,
            });
        }
        let inner = view.inner_in_patch();
        let at = Rect::new(inner.x + by, inner.y + by, inner.width, inner.height);
        let planes = state.len();
        let bad = || StackError::Shape { width: 0, height: 0, channels: planes };
        let (sum, rest) = state.split_first_mut().ok_or_else(bad)?;
        let sum = sum.as_f32_mut().ok_or_else(bad)?;
        self.compose(&split, span, by, &masks, at, sum)?;
        for mask in masks.into_iter().flatten() {
            self.spare.give(mask);
        }
        let (covered, bounds) = rest.split_first_mut().ok_or_else(bad)?;
        let covered = covered.as_f32_mut().ok_or_else(bad)?;
        let coverage = split.coverage();
        covered.par_rows_mut().enumerate().for_each(|(j, row)| {
            let cov = coverage.and_then(|k| k.row(inner.y + j));
            for (i, v) in row.iter_mut().enumerate() {
                if cov.is_none_or(|k| k.get(inner.x + i).is_some_and(|&k| k >= MIN_COVERAGE)) {
                    *v = 1.0;
                }
            }
        });
        if let [low, high] = bounds {
            let (Some(low), Some(high)) = (low.as_u16_mut(), high.as_u16_mut()) else { return Err(bad()) };
            fold_bounds(&split, inner, low, high);
        }
        Ok(Some(split.share(&layout, view)?))
    }

    fn absorb(&mut self, shares: Vec<(Tile, Share)>) -> Result<(), StackError> {
        let (values, coverage) = gather(&self.layout(), shares)?;
        self.fold_coarse(&values, &coverage)?;
        self.added += 1;
        Ok(())
    }

    fn prepare(&mut self, monitor: &dyn Monitor) -> Result<(), StackError> {
        monitor.checkpoint()?;
        if self.added != self.choices.frames {
            return Err(StackError::FrameCount { expected: self.choices.frames, got: self.added });
        }
        let layout = self.layout();
        let small = layout.small();
        let by = extension(self.bank, layout.coarse());
        let Some(coarse) = self.coarse.as_ref() else { return Err(StackError::NotStarted) };
        let c = layout.channels;
        let mut fused = coarse.details.clone();
        for channel in 0..c {
            // The low-pass: as the setting makes it where it could, the frames' average elsewhere.
            let mut plane = Image::new(coarse.low.width(), coarse.low.height(), 1, ColorSpace::LINEAR)?;
            plane.par_rows_mut().enumerate().for_each(|(y, row)| {
                for (x, v) in row.iter_mut().enumerate() {
                    let of = |sum: &Image<f32>, weight: &Image<f32>| {
                        let w = weight.get(x, y, 0).unwrap_or(0.0);
                        (w > 0.0).then(|| sum.get(x, y, channel).unwrap_or(0.0) / w)
                    };
                    *v = of(&coarse.selected, &coarse.selected_weight).or_else(|| of(&coarse.low, &coarse.weight)).unwrap_or(0.0);
                }
            });
            for j in (0..layout.coarse()).rev() {
                plane = synthesize(self.bank, &plane, None, 1 << j, &self.spare)?;
            }
            let inside = plane.crop(Rect::new(by, by, small.width, small.height))?;
            add_channel(&mut fused, &inside, None, channel);
        }
        self.fused = Some(fused);
        Ok(())
    }

    fn covered(&self, state: &[StatePlane]) -> Result<Image<f32>, StackError> {
        state.get(1).and_then(StatePlane::as_f32).cloned().ok_or(StackError::Shape { width: 0, height: 0, channels: state.len() })
    }

    fn result(&self, state: &[StatePlane], inner: Rect, channels: usize) -> Result<Image<f32>, StackError> {
        let layout = self.layout();
        let Some(fused) = self.fused.as_ref() else { return Err(StackError::NotStarted) };
        let bad = || StackError::Shape { width: 0, height: 0, channels: state.len() };
        let (Some(sum), Some(covered)) = (state.first().and_then(StatePlane::as_f32), state.get(1).and_then(StatePlane::as_f32)) else {
            return Err(bad());
        };
        // The fused low-pass, expanded to the tile.
        let mut up = fused.clone();
        let mut up_span = layout.small();
        for k in (0..layout.tiled).rev() {
            let at = level_span(layout.level(k), inner, k, margin(k));
            up = expand(&up, up_span, at)?;
            up_span = at;
        }
        let bounds = if self.limit > 0.0 {
            match (state.get(2).and_then(StatePlane::as_u16), state.get(3).and_then(StatePlane::as_u16)) {
                (Some(low), Some(high)) => Some((low, high)),
                _ => return Err(bad()),
            }
        } else {
            None
        };
        let c = channels.max(1);
        let limit = self.limit;
        let mut out = sum.clone();
        out.par_rows_mut().zip(up.par_rows()).enumerate().for_each(|(y, (row, up))| {
            for (x, (pixel, up)) in row.chunks_exact_mut(c).zip(up.chunks_exact(c)).enumerate() {
                if covered.get(x, y, 0).is_none_or(|k| k <= 0.0) {
                    pixel.fill(0.0);
                    continue;
                }
                pixel.iter_mut().zip(up).for_each(|(v, &u)| *v += u);
                if let Some((low, high)) = bounds
                    && let (Some(lo), Some(hi)) = (low.pixel(x, y), high.pixel(x, y))
                {
                    for ((v, &l), &h) in pixel.iter_mut().zip(lo).zip(hi) {
                        *v = limit_to(*v, bound_value(l), bound_value(h), limit);
                    }
                }
            }
        });
        Ok(out)
    }
}
