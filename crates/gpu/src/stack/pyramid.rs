//! The pyramid method on the GPU: florastack-stack's `Pyramid` (`pyramid/kernel.rs`), whose module documentation
//! describes the method, its tiles' state and its reach. The finest [`TILED_LEVELS`] levels are computed in tiles,
//! each tile reading the canvas from a multiple of `2^TILED_LEVELS` pixels so that its levels lie on the canvas's
//! lattice; the coarser levels and the residual are computed on the whole canvas at `1 / 2^TILED_LEVELS` of its size,
//! from each tile's share of the frame's Gaussian level there, pasted into one plane on the GPU.
//!
//! Per frame and tile: the samples made safe (`clean`), the Gaussian levels by normalized convolution, the band-pass
//! levels and their local energy, each level folded into the tile's running choice (`select`: picks, or the finest
//! level's blend), and the range limit's bounds (`bounds`). After the frame's tiles, the whole-canvas levels are
//! folded the same way and the residual summed (`residual`). The result collapses the levels from the coarsest down
//! (`coefficients`, EXPAND) and holds each pixel within the frames' range (`limit`). The coarse limit (off by default)
//! isn't ported: the engine stacks on the CPU when it is set.

use florastack_core::Rect;

use super::{Patch, Shape, TileMethod, View};
use crate::batch::{Batch, Bind, Groups, Params};
use crate::device::Gpu;
use crate::error::GpuError;
use crate::levels::{Span, combined, expand, local_energy, reduce_covered};
use crate::ops::{copy_rect, fill, fill_words};
use crate::plane::Plane;
use crate::shaders::Kernel;

/// The finest levels, computed in tiles (florastack-stack's `TILED_LEVELS`).
pub const TILED_LEVELS: usize = 3;
/// The most levels a pyramid may have (`MAX_LEVELS`).
pub const MAX_LEVELS: usize = 10;
/// A range-limit word before any frame: the least bound +inf (low 16 bits), the largest -inf (high 16 bits).
const EMPTY_BOUNDS: u32 = 0xff80_7f80;

/// The levels a `width` x `height` canvas gets for a `levels` setting (0 chooses): florastack-stack's
/// `PyramidSettings::levels_for`.
pub fn levels_for(levels: u32, width: usize, height: usize) -> usize {
    let short = width.min(height).max(1);
    let most = (short.ilog2() as usize).max(1);
    let chosen = if levels == 0 { (short.ilog2() as usize).saturating_sub(4) } else { levels as usize };
    chosen.clamp(1, MAX_LEVELS).min(most)
}

/// What a stack in progress knows about its canvas.
#[derive(Clone, Copy, Debug)]
struct Canvas {
    width: usize,
    height: usize,
    channels: usize,
    levels: usize,
    tiled: usize,
}

impl Canvas {
    /// The whole span of level `k`.
    fn level(&self, k: usize) -> Span {
        let a = 1usize << k;
        Span::whole(self.width.div_ceil(a).max(1), self.height.div_ceil(a).max(1))
    }
}

/// How far level `k`'s state reaches beyond the inner region (florastack-stack's `margin`).
fn margin(k: usize) -> usize {
    if k == 0 { 0 } else { 3 * (1 << k) - 2 }
}

fn signed(v: usize) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// The samples of level `k` around the canvas rectangle `rect` grown by `grow` pixels (`level_span`).
fn level_span(level: Span, rect: Rect, k: usize, grow: usize) -> Span {
    let a = signed(1 << k);
    let x0 = (signed(rect.x) - signed(grow)).div_euclid(a);
    let y0 = (signed(rect.y) - signed(grow)).div_euclid(a);
    let x1 = (signed(rect.x + rect.width) + signed(grow) + a - 1).div_euclid(a);
    let y1 = (signed(rect.y + rect.height) + signed(grow) + a - 1).div_euclid(a);
    level.of_level(x0, y0, x1, y1)
}

/// The samples of `level` whose position at level 0 lies in the core `core` (`core_span`): the cores partition the
/// canvas, so these partition the level.
fn core_span(level: Span, core: Rect, k: usize) -> Option<Span> {
    let a = 1usize << k;
    let (x0, x1) = (core.x.div_ceil(a), (core.x + core.width).div_ceil(a).min(level.full_width));
    let (y0, y1) = (core.y.div_ceil(a), (core.y + core.height).div_ceil(a).min(level.full_height));
    (x1 > x0 && y1 > y0).then(|| Span { x: x0, y: y0, width: x1 - x0, height: y1 - y0, ..level })
}

/// The whole-canvas part: the frame's level `tiled` as the tiles share it, the coarse levels' choices (coefficients
/// and their energy, by level from `tiled` up) and the residual's sums.
struct Coarse {
    level: Plane,
    coverage: Plane,
    levels: Vec<(Plane, Plane)>,
    residual: Plane,
    weight: Plane,
}

pub(crate) struct Pyramid {
    setting: u32,
    /// The finest level's blend power, `1 / grit`; `None` picks.
    blend: Option<f32>,
    channel_decisions: bool,
    limit: f32,
    canvas: Option<Canvas>,
    coarse: Option<Coarse>,
    /// The fused Gaussian level `tiled`, once prepared.
    fused: Option<Plane>,
}

impl Pyramid {
    pub(crate) fn new(levels: u32, blend: Option<f32>, channel_decisions: bool, range_limit: f32) -> Result<Pyramid, GpuError> {
        if levels as usize > MAX_LEVELS {
            return Err(GpuError::shape("the pyramid's levels", levels, "from 0 (automatic) to 10"));
        }
        if blend.is_some_and(|p| !(p.is_finite() && (1.0..=4.0).contains(&p))) {
            return Err(GpuError::shape("the pyramid's blend power", blend.unwrap_or(0.0), "from 1 to 4 (a grit from 0.25 to 1)"));
        }
        if !(range_limit.is_finite() && (0.0..=1.0).contains(&range_limit)) {
            return Err(GpuError::shape("the pyramid's range limit", range_limit, "from 0 to 1"));
        }
        Ok(Pyramid { setting: levels, blend, channel_decisions, limit: range_limit, canvas: None, coarse: None, fused: None })
    }

    fn canvas(&self) -> Result<Canvas, GpuError> {
        self.canvas.ok_or(GpuError::Unsupported { what: "the pyramid before `begin`" })
    }

    fn decisions(&self, channels: usize) -> usize {
        if self.channel_decisions { channels } else { 1 }
    }

    /// Records the fold of a band (of `span`, with its energy and coverage) into a level's choice planes (of `at`).
    #[allow(clippy::too_many_arguments)] // A level's planes and where each lies.
    fn select(
        &self,
        batch: &mut Batch<'_>,
        band: &Plane,
        energy: &Plane,
        coverage: &Plane,
        span: Span,
        at: Span,
        planes: (&Plane, &Plane),
        blend: bool,
    ) -> Result<(), GpuError> {
        let (coefficients, weights) = planes;
        let params = Params::new()
            .u(at.width)
            .u(at.height)
            .u(band.channels())
            .u(energy.channels())
            .u(span.width)
            .u(at.x - span.x)
            .u(at.y - span.y)
            .flag(blend)
            .f(self.blend.unwrap_or(1.0));
        let binds = [band.into(), energy.into(), coverage.into(), coefficients.into(), weights.into()];
        batch.run(Kernel::Select, &params, &binds, Groups::Plane { width: at.width, height: at.height })
    }

    /// Records a frame's Gaussian levels from level 0 (`image` on `span`, with its coverage) up `top` levels.
    fn gaussian(batch: &mut Batch<'_>, image: Plane, coverage: Plane, span: Span, top: usize) -> Result<Vec<(Plane, Plane, Span)>, GpuError> {
        let mut levels = vec![(image, coverage, span)];
        for _ in 0..top {
            let Some((g, k, s)) = levels.last() else { break };
            let next = reduce_covered(batch, g, k, *s)?;
            levels.push(next);
        }
        Ok(levels)
    }

    /// Records the band and its energy of each of `levels` but the last.
    fn bands(&self, batch: &mut Batch<'_>, levels: &[(Plane, Plane, Span)]) -> Result<Vec<(Plane, Plane)>, GpuError> {
        let mut out = Vec::with_capacity(levels.len().saturating_sub(1));
        for pair in levels.windows(2) {
            let [(g, _, span), (next, _, next_span)] = pair else { continue };
            let up = expand(batch, next, *next_span, *span)?;
            let band = combined(batch, g, &up, true)?;
            let energy = local_energy(batch, &band, *span, !self.channel_decisions)?;
            out.push((band, energy));
        }
        Ok(out)
    }

    /// Records the coefficients of a level's state into a new plane: the blend's quotient, or the picks.
    fn coefficients(batch: &mut Batch<'_>, coefficients: &Plane, weights: &Plane, blend: bool) -> Result<Plane, GpuError> {
        let out = batch.gpu().plane("a level's coefficients", coefficients.width(), coefficients.height(), coefficients.channels())?;
        let n = coefficients.width() * coefficients.height();
        let params = Params::new().u(n).u(coefficients.channels()).u(weights.channels()).flag(blend);
        batch.run(Kernel::Coefficients, &params, &[coefficients.into(), weights.into(), (&out).into()], Groups::Flat(n))?;
        Ok(out)
    }
}

impl TileMethod for Pyramid {
    fn context(&self) -> usize {
        // florastack-stack's `Pyramid::context`: the tiled levels' reach, the share's, and the lattice's rounding.
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

    fn start(&mut self, gpu: &Gpu, shape: Shape) -> Result<(), GpuError> {
        let levels = levels_for(self.setting, shape.width, shape.height);
        let canvas = Canvas { width: shape.width, height: shape.height, channels: shape.channels, levels, tiled: levels.min(TILED_LEVELS) };
        let (c, d) = (shape.channels, self.decisions(shape.channels));
        let base = canvas.level(canvas.tiled);
        let mut batch = Batch::new(gpu);
        let mut coarse_levels = Vec::new();
        for k in canvas.tiled..canvas.levels {
            let s = canvas.level(k);
            let energy = gpu.plane("a coarse level's energy", s.width, s.height, d)?;
            fill(&mut batch, &energy, -1.0)?;
            coarse_levels.push((gpu.plane("a coarse level's coefficients", s.width, s.height, c)?, energy));
        }
        let top = canvas.level(canvas.levels);
        self.coarse = Some(Coarse {
            level: gpu.plane("the frame's coarse level", base.width, base.height, c)?,
            coverage: gpu.plane("the coarse level's coverage", base.width, base.height, 1)?,
            levels: coarse_levels,
            residual: gpu.plane("the residual's sum", top.width, top.height, c)?,
            weight: gpu.plane("the residual's weights", top.width, top.height, 1)?,
        });
        batch.finish()?;
        self.canvas = Some(canvas);
        self.fused = None;
        Ok(())
    }

    fn state(&self, batch: &mut Batch<'_>, view: &View, shape: Shape) -> Result<Vec<Plane>, GpuError> {
        let canvas = self.canvas()?;
        let gpu = batch.gpu();
        let d = self.decisions(shape.channels);
        let mut planes = Vec::new();
        for k in 0..canvas.tiled {
            let s = level_span(canvas.level(k), view.inner, k, margin(k));
            let weights = gpu.plane("a level's weights", s.width, s.height, d)?;
            fill(batch, &weights, if k == 0 && self.blend.is_some() { 0.0 } else { -1.0 })?;
            planes.push(gpu.plane("a level's coefficients", s.width, s.height, shape.channels)?);
            planes.push(weights);
        }
        if self.limit > 0.0 {
            let bounds = gpu.plane("the range bounds", view.inner.width, view.inner.height, shape.channels)?;
            fill_words(batch, &bounds, EMPTY_BOUNDS)?;
            planes.push(bounds);
        }
        Ok(planes)
    }

    fn fold(&self, batch: &mut Batch<'_>, state: &[Plane], patch: &Patch, view: &View, shape: Shape) -> Result<(), GpuError> {
        let canvas = self.canvas()?;
        let coarse = self.coarse.as_ref().ok_or(GpuError::Unsupported { what: "the pyramid before `begin`" })?;
        let gpu = batch.gpu();
        let (w, h, c) = (view.region.width, view.region.height, shape.channels);
        let n = w * h;
        let span = Span { x: view.region.x, y: view.region.y, width: w, height: h, full_width: canvas.width, full_height: canvas.height };
        let image = gpu.plane("a cleaned patch", w, h, c)?;
        let coverage = gpu.plane("a cleaned coverage", w, h, 1)?;
        let given = patch.coverage.as_ref().map_or(Bind::Buffer(gpu.dummy()), Bind::Plane);
        let params = Params::new().u(n).u(c).flag(patch.coverage.is_some());
        batch.run(Kernel::Clean, &params, &[(&patch.image).into(), given, (&image).into(), (&coverage).into()], Groups::Flat(n))?;
        let levels = Pyramid::gaussian(batch, image, coverage, span, canvas.tiled)?;
        let bands = self.bands(batch, &levels)?;
        for (k, ((band, energy), (_, cov, s))) in bands.iter().zip(&levels).enumerate() {
            let at = level_span(canvas.level(k), view.inner, k, margin(k));
            let (Some(coefficients), Some(weights)) = (state.get(2 * k), state.get(2 * k + 1)) else {
                return Err(GpuError::shape("the pyramid's state", state.len(), "two planes a tiled level"));
            };
            self.select(batch, band, energy, cov, *s, at, (coefficients, weights), k == 0 && self.blend.is_some())?;
        }
        if self.limit > 0.0 {
            let (Some(bounds), Some((image, cov, _))) = (state.get(2 * canvas.tiled), levels.first()) else {
                return Err(GpuError::shape("the pyramid's state", state.len(), "the range bounds"));
            };
            let (ox, oy) = view.offset();
            let (iw, ih) = (view.inner.width, view.inner.height);
            let params = Params::new().u(iw).u(ih).u(c).u(w).u(ox).u(oy);
            batch.run(Kernel::Bounds, &params, &[image.into(), cov.into(), bounds.into()], Groups::Plane { width: iw, height: ih })?;
        }
        // The tile's share of the frame's level `tiled`: its core's samples, pasted where they lie on the level.
        let k = canvas.tiled;
        if let (Some(core), Some((g, cov, s))) = (core_span(canvas.level(k), view.core, k), levels.get(k)) {
            let from = (core.x - s.x, core.y - s.y);
            copy_rect(batch, g, from, &coarse.level, (core.x, core.y), (core.width, core.height))?;
            copy_rect(batch, cov, from, &coarse.coverage, (core.x, core.y), (core.width, core.height))?;
        }
        Ok(())
    }

    fn end_frame(&mut self, batch: &mut Batch<'_>) -> Result<(), GpuError> {
        let canvas = self.canvas()?;
        let coarse = self.coarse.as_ref().ok_or(GpuError::Unsupported { what: "the pyramid before `begin`" })?;
        let gpu = batch.gpu();
        let base = canvas.level(canvas.tiled);
        let top = canvas.levels - canvas.tiled;
        // The coarse pyramid owns its levels: it starts on a copy of the level the tiles shared.
        let level = gpu.plane("the frame's coarse level", base.width, base.height, canvas.channels)?;
        let coverage = gpu.plane("its coverage", base.width, base.height, 1)?;
        batch.copy(&coarse.level, &level)?;
        batch.copy(&coarse.coverage, &coverage)?;
        let levels = Pyramid::gaussian(batch, level, coverage, base, top)?;
        let bands = self.bands(batch, &levels)?;
        for ((band, energy), ((_, cov, s), (coefficients, weights))) in bands.iter().zip(levels.iter().zip(&coarse.levels)) {
            self.select(batch, band, energy, cov, *s, *s, (coefficients, weights), false)?;
        }
        let Some((g, cov, s)) = levels.last() else { return Ok(()) };
        let n = s.width * s.height;
        let params = Params::new().u(n).u(canvas.channels);
        batch.run(Kernel::Residual, &params, &[g.into(), cov.into(), (&coarse.residual).into(), (&coarse.weight).into()], Groups::Flat(n))
    }

    fn prepare(&mut self, batch: &mut Batch<'_>) -> Result<(), GpuError> {
        let canvas = self.canvas()?;
        let coarse = self.coarse.as_ref().ok_or(GpuError::Unsupported { what: "the pyramid before `begin`" })?;
        let gpu = batch.gpu();
        let top = canvas.level(canvas.levels);
        let n = top.width * top.height;
        let mut g = gpu.plane("the fused residual", top.width, top.height, canvas.channels)?;
        let unused = gpu.plane("unused coverage", top.width, top.height, 1)?;
        let params = Params::new().u(n).u(canvas.channels);
        batch.run(
            Kernel::Normalize,
            &params,
            &[(&coarse.residual).into(), (&coarse.weight).into(), (&g).into(), (&unused).into()],
            Groups::Flat(n),
        )?;
        for (i, (coefficients, weights)) in coarse.levels.iter().enumerate().rev() {
            let k = canvas.tiled + i;
            let band = Pyramid::coefficients(batch, coefficients, weights, false)?;
            let up = expand(batch, &g, canvas.level(k + 1), canvas.level(k))?;
            g = combined(batch, &band, &up, false)?;
        }
        self.fused = Some(g);
        Ok(())
    }

    fn result(&self, batch: &mut Batch<'_>, state: &[Plane], view: &View, shape: Shape) -> Result<(Plane, Plane), GpuError> {
        let canvas = self.canvas()?;
        let fused = self.fused.as_ref().ok_or(GpuError::Unsupported { what: "the pyramid's result before it is prepared" })?;
        let malformed = || GpuError::shape("the pyramid's state", state.len(), "two planes a tiled level");
        // Collapse from the fused level `tiled` down through the tile's own levels.
        let mut g: Option<Plane> = None;
        let mut g_span = canvas.level(canvas.tiled);
        for k in (0..canvas.tiled).rev() {
            let at = level_span(canvas.level(k), view.inner, k, margin(k));
            let (Some(coefficients), Some(weights)) = (state.get(2 * k), state.get(2 * k + 1)) else { return Err(malformed()) };
            let up = expand(batch, g.as_ref().unwrap_or(fused), g_span, at)?;
            let band = Pyramid::coefficients(batch, coefficients, weights, k == 0 && self.blend.is_some())?;
            g = Some(combined(batch, &band, &up, false)?);
            g_span = at;
        }
        let g = g.ok_or_else(malformed)?;
        let gpu = batch.gpu();
        let (w, h) = (view.inner.width, view.inner.height);
        let covered = gpu.plane("the tile's coverage", w, h, 1)?;
        let weights = state.get(1).ok_or_else(malformed)?;
        let bounds = if self.limit > 0.0 { state.get(2 * canvas.tiled).map(Bind::Plane).ok_or_else(malformed)? } else { Bind::Buffer(gpu.dummy()) };
        let params = Params::new().u(w * h).u(shape.channels).u(weights.channels()).flag(self.blend.is_some()).flag(self.limit > 0.0).f(self.limit);
        batch.run(Kernel::Limit, &params, &[(&g).into(), weights.into(), bounds, (&covered).into()], Groups::Flat(w * h))?;
        Ok((g, covered))
    }
}
