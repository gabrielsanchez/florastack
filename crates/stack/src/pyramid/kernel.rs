//! The pyramid method as a [`TileKernel`]: the finest [`TILED_LEVELS`] levels in each tile's state, the coarser ones and
//! the residual in a whole-canvas part fed by the tiles' shares. See the module documentation of [`super`].
//!
//! # A tile's state
//!
//! With `C` channels and `D` decision channels (1 on luminance, `C` on each channel), the planes are, in order:
//!
//! - level 0, the tile's inner region: the blend's weighted sum of coefficients (`C`) and sum of weights (`D`), or,
//!   when `grit` is 0, the picked coefficients and their energy (-1 where no frame covers);
//! - each level `k` from 1 to `TILED_LEVELS - 1`: the picked coefficients (`C`) and their energy (`D`, -1 where none),
//!   on the level's samples around the inner region (grown by `3 2^k - 2` pixels, so that collapsing the levels yields
//!   every pixel of the inner region);
//! - with the range limit on, the least and the largest value of each channel over the covering frames, as bfloat16
//!   bits (`u16`, `C` each).
//!
//! # A tile's reach
//!
//! The band-pass level `k` and its 5x5 energy need `2^(k+3) - 2` pixels of input around a sample, and level `k`'s state
//! reaches `3 2^k - 2` pixels beyond the inner region: the context is the largest sum over the tiled levels, plus
//! `2^TILED_LEVELS` for the lattice's rounding (48 pixels).

use florastack_core::{ColorSpace, Image, Monitor, Rect, Tile};
use rayon::prelude::*;

use super::levels::{Span, expand, local_energy, luminance, reduce, reduce_covered, signed, subtract};
use super::range::{EMPTY_HIGH, EMPTY_LOW, bound_down, bound_up, bound_value, limit_to};
use super::{Decisions, PYRAMID, PyramidSettings, TILED_LEVELS};
use crate::average::{ENERGY_CAP, ENERGY_FLOOR, ENERGY_SCALE, MAX_SAMPLE, MIN_COVERAGE};
use crate::error::StackError;
use crate::method::StackShape;
use crate::source::Patch;
use crate::tiled::{Share, StatePlane, TileKernel, TileView};

/// What a stack in progress knows about its canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Canvas {
    width: usize,
    height: usize,
    channels: usize,
    /// The pyramid's band-pass levels.
    levels: usize,
    /// How many of them are computed in tiles: `min(TILED_LEVELS, levels)`.
    tiled: usize,
}

impl Canvas {
    /// The whole span of level `k`.
    fn level(&self, k: usize) -> Span {
        let a = 1usize << k;
        Span::whole(self.width.div_ceil(a).max(1), self.height.div_ceil(a).max(1))
    }
}

/// One coarse level of the whole-canvas part.
#[derive(Clone, Debug)]
struct CoarseLevel {
    /// The picked coefficients and their energy (-1 where none).
    coefficients: Image<f32>,
    energy: Image<f32>,
    /// With the coarse limit: the picked coefficients' frame (per decision channel), the depth-consistent frame's
    /// coefficients, its pooled focus (-1 where none) and its index.
    limit: Option<Limit>,
}

#[derive(Clone, Debug)]
struct Limit {
    picked: Image<f32>,
    coefficients: Image<f32>,
    focus: Image<f32>,
    frame: Image<f32>,
}

/// The whole-canvas part: the coarse levels and the residual's sums.
#[derive(Clone, Debug)]
struct Coarse {
    levels: Vec<CoarseLevel>,
    residual: Image<f32>,
    weight: Image<f32>,
    frames: usize,
}

/// The pyramid method's kernel. See the module documentation of [`super`].
#[derive(Debug)]
pub struct Pyramid {
    settings: PyramidSettings,
    /// The finest level's blend power, `1 / grit`; `None` picks.
    power: Option<f32>,
    limit: f32,
    canvas: Option<Canvas>,
    coarse: Option<Coarse>,
    /// The fused Gaussian level `tiled`, once [`TileKernel::prepare`] has made it.
    fused: Option<Image<f32>>,
}

/// The weight of a frame's coverage `c` in a choice at a coarse level: its fourth power, 0 below [`MIN_COVERAGE`].
fn pick_weight(c: f32) -> f32 {
    if c >= MIN_COVERAGE {
        let c = c.min(1.0);
        let s = c * c;
        s * s
    } else {
        0.0
    }
}

/// How far, in pixels, level `k`'s state reaches beyond the inner region (none at level 0).
fn margin(k: usize) -> usize {
    if k == 0 { 0 } else { 3 * (1 << k) - 2 }
}

/// The samples of level `k` (whole span `level`) around the canvas rectangle `rect` grown by `grow` pixels.
fn level_span(level: Span, rect: Rect, k: usize, grow: usize) -> Span {
    let a = signed(1 << k);
    let x0 = (signed(rect.x) - signed(grow)).div_euclid(a);
    let y0 = (signed(rect.y) - signed(grow)).div_euclid(a);
    let x1 = (signed(rect.x + rect.width) + signed(grow) + a - 1).div_euclid(a);
    let y1 = (signed(rect.y + rect.height) + signed(grow) + a - 1).div_euclid(a);
    level.of_level(x0, y0, x1, y1)
}

/// The samples of `level` whose position at level 0 (`2^k` times theirs) lies in the canvas rectangle `core`: the
/// cores partition the canvas, so these partition the level.
fn core_span(level: Span, core: Rect, k: usize) -> Option<Span> {
    let a = 1usize << k;
    let (x0, x1) = (core.x.div_ceil(a), (core.x + core.width).div_ceil(a).min(level.full_width));
    let (y0, y1) = (core.y.div_ceil(a), (core.y + core.height).div_ceil(a).min(level.full_height));
    (x1 > x0 && y1 > y0).then(|| Span { x: x0, y: y0, width: x1 - x0, height: y1 - y0, ..level })
}

/// The pixels of `span` (a span of the same level as `within`'s) out of `image`, an image of `within`.
fn crop(image: &Image<f32>, within: Span, span: Span) -> Result<Image<f32>, StackError> {
    Ok(image.crop(Rect::new(span.x - within.x, span.y - within.y, span.width, span.height))?)
}

/// A frame's samples made safe: non-finite ones are uncovered (0, coverage 0), the rest clamped to [`MAX_SAMPLE`];
/// coverage below [`MIN_COVERAGE`] is none. The coverage is `None` when the frame covers every pixel.
fn clean(patch: &Patch) -> Result<(Image<f32>, Option<Image<f32>>), StackError> {
    let mut image = patch.image.clone();
    let c = image.channels();
    let broken = image.par_rows().any(|row| row.iter().any(|v| !v.is_finite()));
    if patch.coverage.is_none() && !broken {
        image.par_rows_mut().for_each(|row| row.iter_mut().for_each(|v| *v = v.clamp(-MAX_SAMPLE, MAX_SAMPLE)));
        return Ok((image, None));
    }
    let mut coverage = match &patch.coverage {
        Some(k) => k.clone(),
        None => Image::from_fn(image.width(), image.height(), 1, ColorSpace::LINEAR, |_, _, _| 1.0)?,
    };
    image.par_rows_mut().zip(coverage.par_rows_mut()).for_each(|(row, cov)| {
        for (pixel, k) in row.chunks_exact_mut(c).zip(cov.iter_mut()) {
            let finite = pixel.iter().all(|v| v.is_finite());
            *k = if finite && k.is_finite() && *k >= MIN_COVERAGE { k.min(1.0) } else { 0.0 };
            if *k > 0.0 {
                for v in pixel.iter_mut() {
                    *v = v.clamp(-MAX_SAMPLE, MAX_SAMPLE);
                }
            } else {
                pixel.fill(0.0);
            }
        }
    });
    Ok((image, Some(coverage)))
}

/// A frame's Gaussian pyramid on a tile's region, levels 0 to `top`, with its coverage pyramid.
struct Gaussian {
    levels: Vec<Image<f32>>,
    coverage: Vec<Option<Image<f32>>>,
    spans: Vec<Span>,
}

impl Gaussian {
    fn new(image: Image<f32>, coverage: Option<Image<f32>>, span: Span, top: usize) -> Result<Gaussian, StackError> {
        let mut g = Gaussian { levels: vec![image], coverage: vec![coverage], spans: vec![span] };
        for k in 0..top {
            let (src, cov, s) = (&g.levels[k], &g.coverage[k], g.spans[k]);
            let (next, next_cov, next_span) = if let Some(cov) = cov {
                let (v, c, s) = reduce_covered(src, cov, s)?;
                (v, Some(c), s)
            } else {
                let (v, s) = reduce(src, s)?;
                (v, None, s)
            };
            g.levels.push(next);
            g.coverage.push(next_cov);
            g.spans.push(next_span);
        }
        Ok(g)
    }

    /// Band-pass level `k`.
    fn band(&self, k: usize) -> Result<Image<f32>, StackError> {
        let up = expand(&self.levels[k + 1], self.spans[k + 1], self.spans[k])?;
        subtract(&self.levels[k], &up)
    }
}

impl Pyramid {
    pub fn new(settings: &PyramidSettings) -> Result<Pyramid, StackError> {
        settings.validate()?;
        #[allow(clippy::cast_possible_truncation)] // From 1 to 4.
        let power = (settings.grit > 0.0).then(|| (1.0 / settings.grit) as f32);
        #[allow(clippy::cast_possible_truncation)] // From 0 to 1.
        let limit = settings.range_limit as f32;
        Ok(Pyramid { settings: settings.clone(), power, limit, canvas: None, coarse: None, fused: None })
    }

    pub fn settings(&self) -> &PyramidSettings {
        &self.settings
    }

    /// The number of levels of the stack in progress.
    pub fn levels(&self) -> Option<usize> {
        self.canvas.map(|c| c.levels)
    }

    fn canvas(&self) -> Result<Canvas, StackError> {
        self.canvas.ok_or(StackError::NotStarted)
    }

    /// The decision channels for frames of `channels` channels.
    fn decisions(&self, channels: usize) -> usize {
        match self.settings.decisions {
            Decisions::Luminance => 1,
            Decisions::Channels => channels,
        }
    }

    /// The energy the choices at a level are made on, for a band of `span`.
    fn energy(&self, band: &Image<f32>, span: Span) -> Result<Image<f32>, StackError> {
        match self.settings.decisions {
            Decisions::Luminance => local_energy(&luminance(band)?, span),
            Decisions::Channels => local_energy(band, span),
        }
    }

    /// The finest level's blend weight for an energy `e` and coverage weight `k`.
    fn blend_weight(power: f32, e: f32, k: f32) -> f32 {
        let e = (e.max(0.0) / ENERGY_SCALE).min(ENERGY_CAP) + ENERGY_FLOOR;
        #[allow(clippy::float_cmp)] // The common powers take fast paths that powf gives too, up to rounding.
        let w = if power == 2.0 {
            e * e
        } else if power == 1.0 {
            e
        } else if power == 4.0 {
            let s = e * e;
            s * s
        } else {
            e.powf(power)
        };
        w * k
    }

    /// Folds a band of level `k` (on `span`, with energy `energy` and coverage `coverage`) into the state's planes
    /// `coefficients` and `weights` (on `at`).
    #[allow(clippy::too_many_arguments)] // The planes of one level, and where each lies.
    fn fold_level(
        &self,
        k: usize,
        band: &Image<f32>,
        energy: &Image<f32>,
        coverage: Option<&Image<f32>>,
        span: Span,
        coefficients: &mut Image<f32>,
        weights: &mut Image<f32>,
        at: Span,
    ) {
        let c = band.channels();
        let d = energy.channels();
        let (dx, dy) = (at.x - span.x, at.y - span.y);
        let blend = if k == 0 { self.power } else { None };
        coefficients.par_rows_mut().zip(weights.par_rows_mut()).enumerate().for_each(|(j, (coef, wts))| {
            let (Some(b), Some(e)) = (band.row(dy + j), energy.row(dy + j)) else { return };
            let cov = coverage.and_then(|k| k.row(dy + j));
            for (i, (out, w)) in coef.chunks_exact_mut(c).zip(wts.chunks_exact_mut(d)).enumerate() {
                let x = dx + i;
                let k = cov.map_or(1.0, |r| r.get(x).copied().unwrap_or(0.0));
                let (Some(values), Some(energies)) = (b.get(x * c..(x + 1) * c), e.get(x * d..(x + 1) * d)) else { continue };
                if let Some(power) = blend {
                    if k < MIN_COVERAGE {
                        continue;
                    }
                    for (ch, (o, &v)) in out.iter_mut().zip(values).enumerate() {
                        let dch = if d == 1 { 0 } else { ch };
                        let weight = Pyramid::blend_weight(power, energies.get(dch).copied().unwrap_or(0.0), k);
                        *o += weight * v;
                        if (d > 1 || ch == 0)
                            && let Some(sum) = w.get_mut(dch)
                        {
                            *sum += weight;
                        }
                    }
                } else {
                    let pk = pick_weight(k);
                    if pk <= 0.0 {
                        continue;
                    }
                    for dch in 0..d {
                        let candidate = energies.get(dch).copied().unwrap_or(0.0) * pk;
                        if let Some(best) = w.get_mut(dch)
                            && candidate > *best
                        {
                            *best = candidate;
                            if d == 1 {
                                out.copy_from_slice(values);
                            } else if let (Some(o), Some(&v)) = (out.get_mut(dch), values.get(dch)) {
                                *o = v;
                            }
                        }
                    }
                }
            }
        });
    }

    /// The level-`k` coefficients a tile's or the whole-canvas part's planes hold: the picks (0 where none), or the
    /// finest level's blend.
    fn coefficients(&self, k: usize, coefficients: &Image<f32>, weights: &Image<f32>) -> Image<f32> {
        let mut out = coefficients.clone();
        let c = out.channels();
        let d = weights.channels();
        let blend = k == 0 && self.power.is_some();
        out.par_rows_mut().zip(weights.par_rows()).for_each(|(row, w)| {
            for (pixel, w) in row.chunks_exact_mut(c).zip(w.chunks_exact(d)) {
                for (ch, v) in pixel.iter_mut().enumerate() {
                    let wt = w.get(if d == 1 { 0 } else { ch }).copied().unwrap_or(0.0);
                    if blend {
                        *v = if wt > 0.0 { *v / wt } else { 0.0 };
                    } else if wt < 0.0 {
                        *v = 0.0;
                    }
                }
            }
        });
        out
    }

    /// Folds a frame's Gaussian level `tiled`, assembled from the tiles' shares, into the whole-canvas part.
    fn fold_coarse(&mut self, canvas: Canvas, level: Image<f32>, coverage: Image<f32>, focus: Option<Image<f32>>) -> Result<(), StackError> {
        let k0 = canvas.tiled;
        let top = canvas.levels - k0;
        let g = Gaussian::new(level, Some(coverage), canvas.level(k0), top)?;
        let mut focus_levels = Vec::new();
        if let Some(f) = focus {
            let mut f = f;
            let mut span = canvas.level(k0);
            focus_levels.push(f.clone());
            for _ in 0..top {
                let (next, s) = reduce(&f, span)?;
                focus_levels.push(next.clone());
                f = next;
                span = s;
            }
        }
        let mut bands = Vec::with_capacity(top);
        for i in 0..top {
            let band = g.band(i)?;
            let energy = self.energy(&band, g.spans[i])?;
            bands.push((band, energy));
        }
        let Some(coarse) = self.coarse.as_mut() else { return Err(StackError::NotStarted) };
        let frame = coarse.frames as f32;
        for (i, ((band, energy), level)) in bands.iter().zip(coarse.levels.iter_mut()).enumerate() {
            let cov = g.coverage[i].as_ref();
            let c = band.channels();
            let d = energy.channels();
            let focus = focus_levels.get(i);
            let CoarseLevel { coefficients, energy: best, limit } = level;
            for (y, (coef, wts)) in coefficients.rows_mut().zip(best.rows_mut()).enumerate() {
                let (Some(b), Some(e)) = (band.row(y), energy.row(y)) else { continue };
                let cv = cov.and_then(|k| k.row(y));
                for (x, (out, w)) in coef.chunks_exact_mut(c).zip(wts.chunks_exact_mut(d)).enumerate() {
                    let pk = pick_weight(cv.and_then(|r| r.get(x)).copied().unwrap_or(0.0));
                    if pk <= 0.0 {
                        continue;
                    }
                    let (Some(values), Some(energies)) = (b.get(x * c..(x + 1) * c), e.get(x * d..(x + 1) * d)) else { continue };
                    for dch in 0..d {
                        let candidate = energies.get(dch).copied().unwrap_or(0.0) * pk;
                        if let Some(best) = w.get_mut(dch)
                            && candidate > *best
                        {
                            *best = candidate;
                            if d == 1 {
                                out.copy_from_slice(values);
                            } else if let (Some(o), Some(&v)) = (out.get_mut(dch), values.get(dch)) {
                                *o = v;
                            }
                            if let Some(l) = limit.as_mut()
                                && let Some(p) = l.picked.pixel_mut(x, y).and_then(|p| p.get_mut(dch))
                            {
                                *p = frame;
                            }
                        }
                    }
                    if let (Some(l), Some(f)) = (limit.as_mut(), focus.and_then(|f| f.get(x, y, 0))) {
                        let f = f * pk;
                        if l.focus.get(x, y, 0).is_some_and(|best| f > best) {
                            if let Some(p) = l.focus.pixel_mut(x, y) {
                                p[0] = f;
                            }
                            if let Some(p) = l.frame.pixel_mut(x, y) {
                                p[0] = frame;
                            }
                            if let Some(p) = l.coefficients.pixel_mut(x, y) {
                                p.copy_from_slice(values);
                            }
                        }
                    }
                }
            }
        }
        // The residual: the sum of the frames' coarsest level, weighted as choices are by their pooled coverage, so that
        // a frame's low-pass extrapolated beyond its edge hardly reaches where other frames cover.
        let (top_level, top_cov) = (&g.levels[top], g.coverage[top].as_ref());
        let c = top_level.channels();
        for ((sum, weight), (values, cov)) in
            coarse.residual.rows_mut().zip(coarse.weight.rows_mut()).zip(top_level.rows().zip(top_cov.map(Image::rows).into_iter().flatten()))
        {
            for ((s, w), (v, &k)) in sum.chunks_exact_mut(c).zip(weight.iter_mut()).zip(values.chunks_exact(c).zip(cov)) {
                let k = pick_weight(k);
                if k > 0.0 {
                    for (s, &v) in s.iter_mut().zip(v) {
                        *s += k * v;
                    }
                    *w += k;
                }
            }
        }
        coarse.frames += 1;
        Ok(())
    }
}

/// A plane of `width` x `height` x `channels` filled with `value`.
fn filled(width: usize, height: usize, channels: usize, value: f32) -> Result<Image<f32>, StackError> {
    Ok(Image::from_fn(width, height, channels, ColorSpace::LINEAR, |_, _, _| value)?)
}

impl TileKernel for Pyramid {
    fn name(&self) -> &'static str {
        PYRAMID
    }

    fn context(&self) -> usize {
        // The levels a tile computes reach `margin(k) + 2^(k+3) - 2`; the share of level TILED_LEVELS (and its pooled
        // focus) at most `2 (2^K - 1) + 6`; plus the lattice's rounding.
        let k = TILED_LEVELS;
        let bands = (0..k).map(|k| margin(k) + (1 << (k + 3)) - 2).max().unwrap_or(0);
        let share = 2 * ((1 << k) - 1) + 6;
        bands.max(share) + (1 << k)
    }

    fn read_region(&self, outer: Rect, width: usize, height: usize) -> Rect {
        let a = 1 << TILED_LEVELS;
        let (x, y) = (outer.x / a * a, outer.y / a * a);
        let (x1, y1) = ((outer.x + outer.width).min(width), (outer.y + outer.height).min(height));
        Rect::new(x, y, x1 - x, y1 - y)
    }

    fn start(&mut self, shape: StackShape) -> Result<(), StackError> {
        let levels = self.settings.levels_for(shape.width, shape.height);
        let canvas = Canvas { width: shape.width, height: shape.height, channels: shape.channels, levels, tiled: levels.min(TILED_LEVELS) };
        let (c, d) = (shape.channels, self.decisions(shape.channels));
        let limited = self.settings.coarse_limit.is_some();
        let levels = (canvas.tiled..canvas.levels)
            .map(|k| {
                let s = canvas.level(k);
                let (w, h) = (s.width, s.height);
                Ok(CoarseLevel {
                    coefficients: filled(w, h, c, 0.0)?,
                    energy: filled(w, h, d, -1.0)?,
                    limit: if limited {
                        Some(Limit {
                            picked: filled(w, h, d, -1.0)?,
                            coefficients: filled(w, h, c, 0.0)?,
                            focus: filled(w, h, 1, -1.0)?,
                            frame: filled(w, h, 1, -1.0)?,
                        })
                    } else {
                        None
                    },
                })
            })
            .collect::<Result<Vec<_>, StackError>>()?;
        let top = canvas.level(canvas.levels);
        self.coarse =
            Some(Coarse { levels, residual: filled(top.width, top.height, c, 0.0)?, weight: filled(top.width, top.height, 1, 0.0)?, frames: 0 });
        self.canvas = Some(canvas);
        self.fused = None;
        Ok(())
    }

    fn state(&self, inner: Rect, channels: usize) -> Result<Vec<StatePlane>, StackError> {
        let canvas = self.canvas()?;
        let d = self.decisions(channels);
        let mut planes = Vec::new();
        for k in 0..canvas.tiled {
            let s = level_span(canvas.level(k), inner, k, margin(k));
            let empty = if k == 0 && self.power.is_some() { 0.0 } else { -1.0 };
            planes.push(StatePlane::F32(filled(s.width, s.height, channels, 0.0)?));
            planes.push(StatePlane::F32(filled(s.width, s.height, d, empty)?));
        }
        if self.limit > 0.0 {
            let n = inner.width * inner.height * channels;
            planes.push(StatePlane::U16(Image::from_vec(inner.width, inner.height, channels, ColorSpace::LINEAR, vec![EMPTY_LOW; n])?));
            planes.push(StatePlane::U16(Image::from_vec(inner.width, inner.height, channels, ColorSpace::LINEAR, vec![EMPTY_HIGH; n])?));
        }
        Ok(planes)
    }

    fn fold(&self, state: &mut [StatePlane], patch: &Patch, view: &TileView) -> Result<Option<Share>, StackError> {
        let canvas = self.canvas()?;
        let span = Span {
            x: view.region.x,
            y: view.region.y,
            width: view.region.width,
            height: view.region.height,
            full_width: canvas.width,
            full_height: canvas.height,
        };
        let (image, coverage) = clean(patch)?;
        let g = Gaussian::new(image, coverage, span, canvas.tiled)?;
        let mut focus = None;
        for k in 0..canvas.tiled {
            let band = g.band(k)?;
            let energy = self.energy(&band, g.spans[k])?;
            if k == 0 && self.settings.coarse_limit.is_some() {
                focus = Some(match self.settings.decisions {
                    Decisions::Luminance => energy.clone(),
                    Decisions::Channels => local_energy(&luminance(&band)?, g.spans[0])?,
                });
            }
            let at = level_span(canvas.level(k), view.inner, k, margin(k));
            let planes = state.len();
            let (Some(coefficients), Some(weights)) = (match state.get_mut(2 * k..2 * k + 2) {
                Some([a, b]) => (a.as_f32_mut(), b.as_f32_mut()),
                _ => (None, None),
            }) else {
                return Err(StackError::Shape { width: 0, height: 0, channels: planes });
            };
            self.fold_level(k, &band, &energy, g.coverage[k].as_ref(), g.spans[k], coefficients, weights, at);
        }
        // The range limit's bounds: each channel's least and largest value over the covering frames, after focus-stack's
        // range limiting (src/task_reassign.cc at 0cd289e, MIT, Copyright (c) 2019 Petteri Aimonen; see range.rs).
        if self.limit > 0.0 {
            let base = 2 * canvas.tiled;
            let (low, high) = state.split_at_mut(base + 1);
            let (Some(low), Some(high)) = (low.get_mut(base).and_then(StatePlane::as_u16_mut), high.first_mut().and_then(StatePlane::as_u16_mut))
            else {
                return Err(StackError::Shape { width: 0, height: 0, channels: state.len() });
            };
            let inner = view.inner_in_patch();
            let (x0, image, cov) = (inner.x, &g.levels[0], g.coverage[0].as_ref());
            let c = image.channels();
            low.par_rows_mut().zip(high.par_rows_mut()).enumerate().for_each(|(j, (lo, hi))| {
                let (Some(row), cov) = (image.row(inner.y + j), cov.and_then(|k| k.row(inner.y + j))) else { return };
                for (i, (l, h)) in lo.chunks_exact_mut(c).zip(hi.chunks_exact_mut(c)).enumerate() {
                    let x = x0 + i;
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
        // The tile's share of the frame's Gaussian level `tiled`: its core's samples.
        let k = canvas.tiled;
        let Some(core) = core_span(canvas.level(k), view.core, k) else { return Ok(Some(Vec::new())) };
        let mut share = vec![crop(&g.levels[k], g.spans[k], core)?];
        share.push(match &g.coverage[k] {
            Some(cov) => crop(cov, g.spans[k], core)?,
            None => filled(core.width, core.height, 1, 1.0)?,
        });
        if let Some(mut f) = focus {
            let mut s = g.spans[0];
            for _ in 0..k {
                let (next, ns) = reduce(&f, s)?;
                f = next;
                s = ns;
            }
            share.push(crop(&f, s, core)?);
        }
        Ok(Some(share))
    }

    fn absorb(&mut self, shares: Vec<(Tile, Share)>) -> Result<(), StackError> {
        let canvas = self.canvas()?;
        let k = canvas.tiled;
        let level = canvas.level(k);
        let (w, h, c) = (level.width, level.height, canvas.channels);
        let limited = self.settings.coarse_limit.is_some();
        let mut values = filled(w, h, c, 0.0)?;
        let mut coverage = filled(w, h, 1, 0.0)?;
        let mut focus = if limited { Some(filled(w, h, 1, 0.0)?) } else { None };
        for (tile, share) in shares {
            let Some(core) = core_span(level, tile.core, k) else { continue };
            let mut planes = share.into_iter();
            let (Some(v), Some(k)) = (planes.next(), planes.next()) else { continue };
            paste(&mut values, &v, core);
            paste(&mut coverage, &k, core);
            if let (Some(f), Some(share)) = (focus.as_mut(), planes.next()) {
                paste(f, &share, core);
            }
        }
        self.fold_coarse(canvas, values, coverage, focus)
    }

    fn prepare(&mut self, monitor: &dyn Monitor) -> Result<(), StackError> {
        monitor.checkpoint()?;
        let canvas = self.canvas()?;
        let Some(coarse) = self.coarse.as_ref() else { return Err(StackError::NotStarted) };
        let c = canvas.channels;
        let mut g = coarse.residual.clone();
        g.par_rows_mut().zip(coarse.weight.par_rows()).for_each(|(row, w)| {
            for (pixel, &w) in row.chunks_exact_mut(c).zip(w) {
                if w > 0.0 {
                    let inverse = 1.0 / w;
                    for v in pixel.iter_mut() {
                        *v *= inverse;
                    }
                } else {
                    pixel.fill(0.0);
                }
            }
        });
        let limit = self.settings.coarse_limit.map(|d| d as f32);
        for (i, level) in coarse.levels.iter().enumerate().rev() {
            let k = canvas.tiled + i;
            let mut band = self.coefficients(k, &level.coefficients, &level.energy);
            if let (Some(d), Some(l)) = (limit, &level.limit) {
                let dch = l.picked.channels();
                band.par_rows_mut().enumerate().for_each(|(y, row)| {
                    for (x, pixel) in row.chunks_exact_mut(c).enumerate() {
                        let (Some(dc), Some(picked), Some(fallback)) = (l.frame.get(x, y, 0), l.picked.pixel(x, y), l.coefficients.pixel(x, y))
                        else {
                            continue;
                        };
                        if dc < 0.0 {
                            continue;
                        }
                        for (ch, v) in pixel.iter_mut().enumerate() {
                            let p = picked.get(if dch == 1 { 0 } else { ch }).copied().unwrap_or(-1.0);
                            if p >= 0.0 && (p - dc).abs() > d {
                                *v = fallback.get(ch).copied().unwrap_or(0.0);
                            }
                        }
                    }
                });
            }
            let up = expand(&g, canvas.level(k + 1), canvas.level(k))?;
            g = band;
            g.par_rows_mut().zip(up.par_rows()).for_each(|(o, r)| o.iter_mut().zip(r).for_each(|(v, &s)| *v += s));
        }
        self.fused = Some(g);
        Ok(())
    }

    fn covered(&self, state: &[StatePlane]) -> Result<Image<f32>, StackError> {
        let Some(weights) = state.get(1).and_then(StatePlane::as_f32) else {
            return Err(StackError::Shape { width: 0, height: 0, channels: state.len() });
        };
        let d = weights.channels();
        let blend = self.power.is_some();
        Ok(Image::from_fn(weights.width(), weights.height(), 1, ColorSpace::LINEAR, |x, y, _| {
            let covered = weights.pixel(x, y).is_some_and(|w| w.iter().take(d).any(|&w| if blend { w > 0.0 } else { w >= 0.0 }));
            if covered { 1.0 } else { 0.0 }
        })?)
    }

    fn result(&self, state: &[StatePlane], inner: Rect, channels: usize) -> Result<Image<f32>, StackError> {
        let canvas = self.canvas()?;
        let Some(fused) = self.fused.as_ref() else { return Err(StackError::NotStarted) };
        let bad = || StackError::Shape { width: 0, height: 0, channels: state.len() };
        // Collapse from the fused level `tiled` down through the tile's own levels.
        let mut g = fused.clone();
        let mut g_span = canvas.level(canvas.tiled);
        for k in (0..canvas.tiled).rev() {
            let at = level_span(canvas.level(k), inner, k, margin(k));
            let (Some(coefficients), Some(weights)) =
                (state.get(2 * k).and_then(StatePlane::as_f32), state.get(2 * k + 1).and_then(StatePlane::as_f32))
            else {
                return Err(bad());
            };
            let up = expand(&g, g_span, at)?;
            let mut next = self.coefficients(k, coefficients, weights);
            next.par_rows_mut().zip(up.par_rows()).for_each(|(o, r)| o.iter_mut().zip(r).for_each(|(v, &s)| *v += s));
            g = next;
            g_span = at;
        }
        // The range limit, and nothing where no frame covers.
        let c = channels.max(1);
        let covered = self.covered(state)?;
        let bounds = if self.limit > 0.0 {
            let base = 2 * canvas.tiled;
            match (state.get(base).and_then(StatePlane::as_u16), state.get(base + 1).and_then(StatePlane::as_u16)) {
                (Some(low), Some(high)) => Some((low, high)),
                _ => return Err(bad()),
            }
        } else {
            None
        };
        let limit = self.limit;
        g.par_rows_mut().enumerate().for_each(|(y, row)| {
            for (x, pixel) in row.chunks_exact_mut(c).enumerate() {
                if covered.get(x, y, 0).is_none_or(|k| k <= 0.0) {
                    pixel.fill(0.0);
                    continue;
                }
                if let Some((low, high)) = bounds
                    && let (Some(lo), Some(hi)) = (low.pixel(x, y), high.pixel(x, y))
                {
                    for ((v, &l), &h) in pixel.iter_mut().zip(lo).zip(hi) {
                        *v = limit_to(*v, bound_value(l), bound_value(h), limit);
                    }
                }
            }
        });
        Ok(g)
    }
}

/// Copies `block` into `into` at the samples of `at`.
fn paste(into: &mut Image<f32>, block: &Image<f32>, at: Span) {
    let c = into.channels();
    for (j, row) in block.rows().enumerate() {
        if let Some(dst) = into.row_mut(at.y + j).and_then(|r| r.get_mut(at.x * c..(at.x + at.width) * c))
            && dst.len() == row.len()
        {
            dst.copy_from_slice(row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_follow_the_lattice() {
        let level2 = Span::whole(25, 13); // 100 x 50 at level 2.
        let s = level_span(level2, Rect::new(10, 6, 30, 20), 2, margin(2));
        // Pixels 0 to 50 (10 - 10 clipped) and up to 50 + 10: samples 0 .. ceil(50 / 4) = 13 (clipped).
        assert_eq!((s.x, s.y, s.width, s.height), (0, 0, 13, 9));
        let cores = [Rect::new(0, 0, 37, 50), Rect::new(37, 0, 63, 50)];
        let spans: Vec<Span> = cores.iter().filter_map(|&c| core_span(Span::whole(13, 7), c, 3)).collect();
        assert_eq!(spans.iter().map(|s| (s.x, s.width)).collect::<Vec<_>>(), [(0, 5), (5, 8)], "cores partition the level");
        let k = Pyramid::new(&PyramidSettings::default()).unwrap();
        assert_eq!(k.context(), 48);
        assert_eq!(k.read_region(Rect::new(13, 21, 50, 40), 100, 61), Rect::new(8, 16, 55, 45));
        assert_eq!(pick_weight(0.5), 0.0625);
        assert_eq!(pick_weight(0.0), 0.0);
    }
}
