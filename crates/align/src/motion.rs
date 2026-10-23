//! Subject movement: what moved in a frame against its aligned neighbor, once the frame's own transform is taken out,
//! such as a leaf in the wind or a live insect's leg. Stacking such frames mixes two positions of the same detail; the
//! user can leave the frame out, or retouch it.
//!
//! Each frame that aligned gets a [`MotionMap`] in its own pixels, from what [`StackAligner`](crate::StackAligner)
//! prepared anyway: the band-pass pyramids of the frame and of its reference, the neighbor it was aligned to. For each
//! cell of a grid on the frame (cells of at least 8 pixels, at most 64 on the longer side, at a level of at most 1024
//! pixels), the local displacement that best maps the reference, warped by the frame's transform, onto the frame
//! around the cell: the residual motion that the global transform leaves.
//!
//! The displacement is found by Lucas–Kanade (B. D. Lucas and T. Kanade, "An iterative image registration technique
//! with an application to stereo vision", IJCAI 1981), coarse to fine over three levels of the pyramid as J.-Y.
//! Bouguet describes ("Pyramidal implementation of the Lucas Kanade feature tracker", Intel, 2000), in a window of
//! 17 pixels around it (or the cell, if larger), with two more terms that the global alignment has too: a gain and a
//! defocus term (the frame's Laplacian), so that the change of focus between neighbors doesn't pass for motion. A small
//! damping on the displacement keeps a cell on a straight edge, which can't tell motion along the edge, from inventing
//! one.
//!
//! Windows leave out four pixels along both frames' edges, where the band-pass sees the clamped border. A cell is
//! measured when its window has detail in two directions (the smaller eigenvalue of its gradients' structure
//! tensor at least a tenth of the level's mean gradient energy); flat cells and cells on a single edge say nothing.
//! A measured cell **moved** when its displacement reaches [`AlignSettings::min_motion`] frame pixels, and at least
//! three quarters of a pixel of its level (on frames larger than 1024 pixels, a few frame pixels: below that, the
//! apparent motion of defocus and noise passes for movement), or when, once displaced, it still correlates with the
//! reference below 0.5: what moved too far, or changed shape. Dust left on the frames moves against the scene from
//! frame to frame, as far as the frames shift: inpaint it first (`defects.rs`). The fill of an inpainted spot stays on
//! the sensor too, so the cells whose window comes within four level pixels of a spot of the frames' defect mask
//! ([`StackAligner::set_defects`](crate::StackAligner::set_defects)) aren't measured.
//!
//! Movement matters where the frame gives the stack detail: once the stack is aligned, a moved cell counts only where
//! its frame is among the sharpest at that point of the scene (its focus energy at least half the sharpest frame's,
//! from the frames' focus maps, `focus.rs`). Elsewhere the frame is out of focus, the stack takes that part from other
//! frames, and the edges of a foreground far out of focus, which widen as the focus moves away, would pass for
//! movement.
//!
//! [`Motion`] sums a frame's map up: its **score**, the share of the measured cells that moved where the frame is in
//! focus; its largest displacement; and its **regions**, the 8-connected groups of those cells, as rectangles of frame
//! pixels. A frame with a region of at least two cells is flagged [`Flag::Moved`](crate::Flag::Moved), never rejected:
//! the user decides.
//! The movement is the frame's against its reference, so a subject that moves once, between two frames, flags the
//! frame of the two that is farther from the master in the stack's order; a subject that moves and comes back flags
//! two frames.
//!
//! [`AlignSettings::min_motion`]: crate::AlignSettings::min_motion

use florastack_core::{Affine, Image};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::defects::DefectMask;
use crate::ecc::{cholesky, solve_cholesky};
use crate::prepare::{Level, Mapping, Prepared};

/// Motion is measured on the finest pyramid level of at most this many pixels on its longer side.
const LEVEL_SIDE: usize = 1024;
/// The grid: cells of at least this many level pixels, at most this many on the longer side.
const MIN_CELL: usize = 8;
const MAX_CELLS: usize = 64;
/// The levels above the motion level that the search starts from.
const COARSER: usize = 2;
/// The window's radius around a cell's center, in level pixels: at least this, and half the cell, so that the windows
/// of cells of 16 pixels or more touch, and those of smaller cells overlap.
const WINDOW: usize = 8;
/// Lucas–Kanade iterations per level, and the step below which they stop, in level pixels.
const ITERATIONS: usize = 4;
const CONVERGED: f64 = 0.01;
/// The damping of the displacement, as a share of the window's mean gradient energy.
const DAMPING: f64 = 0.01;
/// A cell is measured when its structure tensor's smaller eigenvalue, per pixel, is at least this share of the level's
/// mean gradient energy, and at least half its window lies inside both frames.
const TEXTURE: f64 = 0.1;
/// A cell is measured only when, once displaced, it correlates with the reference at least this well: where noise or
/// changed content dominates the window, the displacement found means nothing. Neighbors in focus correlate above 0.93
/// on 999 cells in 1000 of the synthetic stacks, and band-passed noise next to nothing.
const MIN_CORRELATION: f64 = 0.8;
/// A cell isn't measured when a spot of the defect mask lies within this many level pixels of its window: the fill of
/// a defect stays on the sensor while the scene moves.
const DEFECT_MARGIN: f64 = 4.0;
/// A frame is flagged as moved when one of its regions has at least this many cells.
const MIN_REGION_CELLS: usize = 2;
/// The smallest displacement that counts, in pixels of the motion level, whatever the threshold in frame pixels: on
/// the synthetic stacks, defocus and noise displace one cell in a thousand by up to 0.9 pixels.
const FLOOR: f64 = 0.75;
/// The field's channels: the band-pass, its x and y derivatives and its Laplacian.
const FIELD: usize = 4;
/// Windows leave out the pixels this close to either frame's edge, where the band-pass sees the clamped border.
const BORDER: usize = 4;

/// A frame's residual motion against its reference, cell by cell. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionMap {
    /// From the map's cells to the frame's pixels (a cell's coordinates give its center).
    mapping: Mapping,
    width: usize,
    height: usize,
    /// The cells, row by row; `None` where a cell couldn't be measured.
    cells: Vec<Option<MotionCell>>,
    /// The displacement from which a cell counts as moved, in frame pixels.
    threshold: f64,
}

/// A measured cell of a [`MotionMap`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionCell {
    /// The displacement of the frame's content against the reference, along x and y, in frame pixels.
    pub dx: f32,
    pub dy: f32,
    /// Their correlation once displaced, from 0.8 (less isn't measured) to 1.
    pub correlation: f32,
}

impl MotionMap {
    /// The map's size, in cells.
    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// From cell coordinates to the frame's pixels: cell `(x, y)` is centered on `to_frame().apply(x, y)`.
    pub fn to_frame(&self) -> Affine {
        self.mapping.to_frame()
    }

    /// The size of a cell in frame pixels, along x and y.
    pub fn cell_size(&self) -> (f64, f64) {
        self.mapping.scale
    }

    /// The cells, row by row; `None` where a cell wasn't measured.
    pub fn cells(&self) -> &[Option<MotionCell>] {
        &self.cells
    }

    /// Cell `(x, y)`, if it was measured.
    pub fn cell(&self, x: usize, y: usize) -> Option<MotionCell> {
        if x >= self.width {
            return None;
        }
        self.cells.get(y * self.width + x).copied().flatten()
    }

    /// The displacement of cell `(x, y)`, in frame pixels, if it was measured.
    pub fn displacement(&self, x: usize, y: usize) -> Option<(f64, f64)> {
        self.cell(x, y).map(|c| (f64::from(c.dx), f64::from(c.dy)))
    }

    /// The length of cell `(x, y)`'s displacement, if it was measured.
    pub fn magnitude(&self, x: usize, y: usize) -> Option<f64> {
        self.displacement(x, y).map(|(dx, dy)| dx.hypot(dy))
    }

    /// The displacement from which a cell counts as moved, in frame pixels: [`AlignSettings::min_motion`], or more on
    /// frames larger than 1024 pixels.
    ///
    /// [`AlignSettings::min_motion`]: crate::AlignSettings::min_motion
    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Whether cell `(x, y)` was measured and moved by at least [`MotionMap::threshold`].
    pub fn moved(&self, x: usize, y: usize) -> bool {
        self.magnitude(x, y).is_some_and(|m| m >= self.threshold)
    }
}

/// A region where the subject moved: a rectangle of frame pixels around an 8-connected group of moved cells.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MotionRegion {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    /// How many cells moved in it.
    pub cells: usize,
    /// How far it moved: the median displacement of its cells, along x and y, in frame pixels. Cells on its border,
    /// which see two positions of the subject at once, can be off by more.
    pub dx: f64,
    pub dy: f64,
    /// Its largest displacement, in frame pixels.
    pub peak: f64,
}

/// What moved in a frame against its reference. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct Motion {
    pub map: MotionMap,
    /// The share of the measured cells that moved, from 0 to 1.
    pub score: f64,
    /// The largest displacement of a measured cell, in frame pixels.
    pub peak: f64,
    /// The regions that moved, largest first.
    pub regions: Vec<MotionRegion>,
}

impl Motion {
    /// The summary of `map`, counting the moved cells for which `keep` holds of their center in frame pixels.
    pub(crate) fn new(map: MotionMap, keep: impl Fn((f64, f64)) -> bool) -> Motion {
        summarize(map, keep)
    }

    /// Whether the frame is flagged as moved: one of its regions has at least two cells.
    pub fn moved(&self) -> bool {
        self.regions.iter().any(|r| r.cells >= MIN_REGION_CELLS)
    }
}

/// The motion map of `moving` against `reference`, the frame it was aligned to, whose pixels `link` maps to its own,
/// with cells counted as moved from `threshold` frame pixels (and [`FLOOR`] level pixels). `None` when the link can't
/// be inverted. Cells near a spot of `defects` aren't measured.
pub(crate) fn measure(reference: &Prepared, moving: &Prepared, link: &Affine, threshold: f64, defects: Option<&DefectMask>) -> Option<MotionMap> {
    let back = link.inverse()?;
    let base = moving.levels.iter().position(|l| l.field.width().max(l.field.height()) <= LEVEL_SIDE).unwrap_or(moving.levels.len() - 1);
    let top = (base + COARSER).min(moving.levels.len() - 1).min(reference.levels.len() - 1);
    if base > top {
        return None;
    }
    let pairs: Vec<Pair> = (base..=top)
        .filter_map(|j| {
            let (m, r) = (moving.levels.get(j)?, reference.levels.get(j)?);
            Some(Pair::new(m, r, &back))
        })
        .collect();
    let finest = moving.levels.get(base)?;
    let (w, h) = (finest.field.width(), finest.field.height());
    let k = MIN_CELL.max(w.max(h).div_ceil(MAX_CELLS));
    let (cols, rows) = ((w / k).max(1), (h / k).max(1));
    let energy = mean_energy(&finest.field);
    let scale = finest.mapping.scale;
    let to_frame = finest.mapping.to_frame();
    let spots: Vec<crate::Spot> = defects.map(|d| d.spots().collect()).unwrap_or_default();
    let cells: Vec<Option<MotionCell>> = (0..cols * rows)
        .into_par_iter()
        .map(|i| {
            let center = ((i % cols * k) as f64 + (k as f64 - 1.0) / 2.0, (i / cols * k) as f64 + (k as f64 - 1.0) / 2.0);
            // The window, grown by the margin, in frame pixels.
            let reach = k as f64 + DEFECT_MARGIN;
            let (x0, y0) = to_frame.apply(center.0 - reach, center.1 - reach);
            let (x1, y1) = to_frame.apply(center.0 + reach, center.1 + reach);
            let near_defect =
                spots.iter().any(|s| (s.x as f64) <= x1 && ((s.x + s.width) as f64) >= x0 && (s.y as f64) <= y1 && ((s.y + s.height) as f64) >= y0);
            if near_defect {
                return None;
            }
            let (u, correlation) = track(&pairs, center, k, energy)?;
            let cell = MotionCell { dx: to_f32(-u.0 * scale.0), dy: to_f32(-u.1 * scale.1), correlation: to_f32(correlation) };
            (cell.dx.is_finite() && cell.dy.is_finite() && correlation >= MIN_CORRELATION).then_some(cell)
        })
        .collect();
    let threshold = threshold.max(FLOOR * scale.0.max(scale.1));
    Some(MotionMap { mapping: finest.mapping.blocks(k), width: cols, height: rows, cells, threshold })
}

/// One level of the frame and of its reference, and the warp from the frame's level pixels to the reference's.
struct Pair<'a> {
    moving: &'a Image<f32>,
    reference: &'a Image<f32>,
    /// From the frame's level pixels to the reference's, and its linear part's transpose, which takes the reference's
    /// gradient to the frame's pixels.
    back: Affine,
    gradient: [[f64; 2]; 2],
}

impl<'a> Pair<'a> {
    fn new(moving: &'a Level, reference: &'a Level, back: &Affine) -> Pair<'a> {
        let back = moving.mapping.level_transform(back);
        let [[a, b, _], [c, d, _]] = back.m;
        Pair { moving: &moving.field, reference: &reference.field, back, gradient: [[a, c], [b, d]] }
    }
}

/// The level's mean gradient energy (`gx^2 + gy^2`), over its finite samples. Rows are summed in parallel and added up
/// in row order, so that the mean doesn't depend on the thread count.
fn mean_energy(field: &Image<f32>) -> f64 {
    let rows: Vec<(f64, usize)> = field
        .par_rows()
        .map(|row| {
            row.as_chunks::<FIELD>().0.iter().fold((0.0, 0_usize), |(s, n), &[_, gx, gy, _]| {
                if gx.is_finite() && gy.is_finite() { (s + f64::from(gx).powi(2) + f64::from(gy).powi(2), n + 1) } else { (s, n) }
            })
        })
        .collect();
    let (sum, n) = rows.into_iter().fold((0.0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
    if n == 0 { 0.0 } else { sum / n as f64 }
}

/// The band-pass and its derivatives of `field` at `(u, v)`, bilinearly, if the point lies inside it, [`BORDER`]
/// pixels from its edges.
fn sample(field: &Image<f32>, u: f64, v: f64) -> Option<[f64; 3]> {
    let (w, h) = (field.width(), field.height());
    let (lo, hi_x, hi_y) = (BORDER as f64, w.saturating_sub(BORDER + 1) as f64, h.saturating_sub(BORDER + 1) as f64);
    if !(u >= lo && v >= lo && u <= hi_x && v <= hi_y) {
        return None;
    }
    let (fu, fv) = (u.floor(), v.floor());
    let (x, y) = (to_index(fu), to_index(fv));
    // Inside the border, the pixel to the right and the one below exist.
    let (top, bottom) = (field.row(y)?, field.row(y + 1)?);
    let (top, bottom) = (top.get(x * FIELD..(x + 2) * FIELD)?, bottom.get(x * FIELD..(x + 2) * FIELD)?);
    let (tu, tv) = (to_f32(u - fu), to_f32(v - fv));
    let mut out = [0.0; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let (a, b, cc, d) = (top[c], top[c + FIELD], bottom[c], bottom[c + FIELD]);
        let upper = a + tu * (b - a);
        let lower = cc + tu * (d - cc);
        *o = f64::from(upper + tv * (lower - upper));
    }
    Some(out)
}

/// A non-negative, finite coordinate's whole part.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Checked to lie inside the level before.
fn to_index(v: f64) -> usize {
    v as usize
}

/// A displacement or correlation as the map's samples are.
#[allow(clippy::cast_possible_truncation)] // A few pixels, or a correlation: far inside f32's range.
fn to_f32(v: f64) -> f32 {
    v as f32
}

/// The displacement `u` of the cell centered on `center` (pixels of the finest pair's level), with `k` pixels a side,
/// that maps the reference onto the frame, and the correlation once displaced; `None` when the cell can't be measured.
/// See the module documentation.
fn track(pairs: &[Pair], center: (f64, f64), k: usize, energy: f64) -> Option<((f64, f64), f64)> {
    let radius = signed(WINDOW.max(k / 2));
    let mut u = (0.0, 0.0);
    for (j, pair) in pairs.iter().enumerate().rev() {
        let factor = f64::from(1_u32 << j.min(16));
        let c = (center.0 / factor, center.1 / factor);
        let c = (round(c.0), round(c.1));
        for _ in 0..ITERATIONS {
            let sums = window(pair, c, radius, u);
            let Some(step) = sums.step() else { break };
            u = (u.0 + step.0, u.1 + step.1);
            if step.0.hypot(step.1) < CONVERGED {
                break;
            }
        }
        if j > 0 {
            u = (2.0 * u.0, 2.0 * u.1);
        }
    }
    let finest = pairs.first()?;
    let sums = window(finest, (round(center.0), round(center.1)), radius, u);
    let side = (2 * radius + 1).unsigned_abs();
    if sums.n * 2 < side * side || !(sums.texture() >= TEXTURE * energy && energy > 0.0) {
        return None;
    }
    Some((u, sums.correlation()))
}

/// A cell's center, rounded to a pixel.
#[allow(clippy::cast_possible_truncation)] // A pixel coordinate of a level: far inside isize's range.
fn round(v: f64) -> isize {
    v.round() as isize
}

/// A small length as a signed offset.
fn signed(v: usize) -> isize {
    isize::try_from(v).unwrap_or(isize::MAX)
}

/// The sums of a window: the normal equations of the step, the frame's own gradient tensor and the correlation's sums.
#[derive(Default)]
struct Sums {
    n: usize,
    /// The normal equations, `h theta = -b`, of the unknowns: the displacement, the gain and the defocus term.
    h: [[f64; 4]; 4],
    b: [f64; 4],
    /// The frame's gradient tensor: `sum gx^2`, `sum gx gy`, `sum gy^2`.
    tensor: [f64; 3],
    /// For the correlation: the sums of the frame `f`, the reference `r`, `f^2`, `r^2` and `f r`.
    f: f64,
    r: f64,
    ff: f64,
    rr: f64,
    fr: f64,
}

impl Sums {
    /// The displacement step, if the normal equations can be solved.
    fn step(&self) -> Option<(f64, f64)> {
        let mut h = self.h;
        let damping = DAMPING * (h[0][0] + h[1][1]) / 2.0;
        h[0][0] += damping;
        h[1][1] += damping;
        let l = cholesky(&h)?;
        let theta = solve_cholesky(&l, &self.b.map(|v| -v));
        let step = (theta[0], theta[1]);
        (step.0.is_finite() && step.1.is_finite()).then_some(step)
    }

    /// The smaller eigenvalue of the frame's gradient tensor, per pixel.
    fn texture(&self) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        let n = self.n as f64;
        let [a, b, c] = self.tensor.map(|v| v / n);
        let half = f64::midpoint(a, c);
        half - (((a - c) / 2.0).powi(2) + b * b).sqrt()
    }

    /// The correlation of the frame and the displaced reference over the window.
    fn correlation(&self) -> f64 {
        let n = self.n as f64;
        if self.n == 0 {
            return f64::NAN;
        }
        let cov = self.fr - self.f * self.r / n;
        let vf = self.ff - self.f * self.f / n;
        let vr = self.rr - self.r * self.r / n;
        if vf > 0.0 && vr > 0.0 { cov / (vf * vr).sqrt() } else { f64::NAN }
    }
}

/// The sums over the window of `radius` pixels around `c` on `pair`, the reference displaced by `u`.
fn window(pair: &Pair, c: (isize, isize), radius: isize, u: (f64, f64)) -> Sums {
    let mut s = Sums::default();
    let [[a, b, tx], [cc, d, ty]] = pair.back.m;
    let g = pair.gradient;
    let (w, h) = (pair.moving.width(), pair.moving.height());
    let inside = |v: usize, len: usize| v >= BORDER && v + BORDER < len;
    for qy in c.1 - radius..=c.1 + radius {
        let Ok(y) = usize::try_from(qy) else { continue };
        if !inside(y, h) {
            continue;
        }
        let Some(row) = pair.moving.row(y) else { continue };
        for qx in c.0 - radius..=c.0 + radius {
            let Ok(x) = usize::try_from(qx) else { continue };
            if !inside(x, w) {
                continue;
            }
            let Some(&[f, fx, fy, lap]) = row.get(x * FIELD..x * FIELD + FIELD).and_then(|p| <&[f32; FIELD]>::try_from(p).ok()) else {
                continue;
            };
            let (px, py) = (x as f64 + u.0, y as f64 + u.1);
            let (rx, ry) = (a * px + b * py + tx, cc * px + d * py + ty);
            let Some([r, gx, gy]) = sample(pair.reference, rx, ry) else { continue };
            let (f, fx, fy, lap) = (f64::from(f), f64::from(fx), f64::from(fy), f64::from(lap));
            if !(f.is_finite() && fx.is_finite() && fy.is_finite() && lap.is_finite() && r.is_finite()) {
                continue;
            }
            // The reference's gradient in the frame's pixels.
            let (gx, gy) = (g[0][0] * gx + g[0][1] * gy, g[1][0] * gx + g[1][1] * gy);
            let jacobian = [gx, gy, -f, -lap];
            for (i, row) in s.h.iter_mut().enumerate() {
                for (j, v) in row.iter_mut().enumerate() {
                    *v += jacobian[i] * jacobian[j];
                }
            }
            for (bi, ji) in s.b.iter_mut().zip(jacobian) {
                *bi += ji * r;
            }
            s.tensor[0] += fx * fx;
            s.tensor[1] += fx * fy;
            s.tensor[2] += fy * fy;
            (s.f, s.r, s.ff, s.rr, s.fr) = (s.f + f, s.r + r, s.ff + f * f, s.rr + r * r, s.fr + f * r);
            s.n += 1;
        }
    }
    s
}

/// The score, the peak and the regions of a map, counting the moved cells for which `keep` holds of their center.
fn summarize(map: MotionMap, keep: impl Fn((f64, f64)) -> bool) -> Motion {
    let (w, h) = (map.width(), map.height());
    let mut measured = 0_usize;
    let mut peak: f64 = 0.0;
    let mut moved = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let Some(m) = map.magnitude(x, y) else { continue };
            measured += 1;
            peak = peak.max(m);
            if let Some(flag) = moved.get_mut(y * w + x) {
                *flag = map.moved(x, y) && keep(map.to_frame().apply(x as f64, y as f64));
            }
        }
    }
    let count = moved.iter().filter(|&&m| m).count();
    let score = if measured == 0 { 0.0 } else { count as f64 / measured as f64 };
    let mut regions = regions(&map, &moved);
    regions.sort_by(|a, b| b.cells.cmp(&a.cells).then(a.y.cmp(&b.y)).then(a.x.cmp(&b.x)));
    Motion { map, score, peak, regions }
}

/// The 8-connected groups of `moved` cells, as rectangles of frame pixels.
fn regions(map: &MotionMap, moved: &[bool]) -> Vec<MotionRegion> {
    let (w, h) = (map.width(), map.height());
    let mut seen = vec![false; moved.len()];
    let mut out = Vec::new();
    let to_frame = map.to_frame();
    let (sx, sy) = map.cell_size();
    for start in 0..moved.len() {
        if !moved.get(start).copied().unwrap_or(false) || seen.get(start).copied().unwrap_or(true) {
            continue;
        }
        let mut stack = vec![start];
        if let Some(s) = seen.get_mut(start) {
            *s = true;
        }
        let (mut x0, mut y0, mut x1, mut y1, mut cells, mut peak) = (usize::MAX, usize::MAX, 0, 0, 0, 0.0_f64);
        let (mut dxs, mut dys) = (Vec::new(), Vec::new());
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            cells += 1;
            peak = peak.max(map.magnitude(x, y).unwrap_or(0.0));
            if let Some((dx, dy)) = map.displacement(x, y) {
                dxs.push(dx);
                dys.push(dy);
            }
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let n = ny * w + nx;
                    if moved.get(n).copied().unwrap_or(false) && !seen.get(n).copied().unwrap_or(true) {
                        if let Some(s) = seen.get_mut(n) {
                            *s = true;
                        }
                        stack.push(n);
                    }
                }
            }
        }
        // The cells' extent in frame pixels: half a cell beyond the centers of the outermost ones.
        let (left, top) = to_frame.apply(x0 as f64, y0 as f64);
        let (right, bottom) = to_frame.apply(x1 as f64, y1 as f64);
        let (left, top) = (pixel(left - sx / 2.0), pixel(top - sy / 2.0));
        let (right, bottom) = (pixel(right + sx / 2.0), pixel(bottom + sy / 2.0));
        let (dx, dy) = (median(&mut dxs), median(&mut dys));
        out.push(MotionRegion {
            x: left,
            y: top,
            width: right.saturating_sub(left).max(1),
            height: bottom.saturating_sub(top).max(1),
            cells,
            dx,
            dy,
            peak,
        });
    }
    out
}

/// The median of `values` (the mean of the middle two of an even count), 0 for none.
fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    match (values.get(n / 2), n.checked_sub(1).and_then(|i| values.get(i / 2))) {
        (Some(&hi), Some(&lo)) => f64::midpoint(hi, lo),
        _ => 0.0,
    }
}

/// A frame coordinate, rounded to a whole pixel, at least 0.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A position on the frame: small and clamped at 0.
fn pixel(v: f64) -> usize {
    if v.is_finite() { v.round().max(0.0) as usize } else { 0 }
}

#[cfg(test)]
mod tests {
    use florastack_core::ColorSpace;
    use florastack_core::ops::{Border, Interpolation, warp};
    use florastack_core::testutil::scene;

    use super::*;

    fn prepared(image: &Image<f32>) -> Prepared {
        Prepared::new(image, 2048, false).unwrap()
    }

    #[test]
    fn a_still_frame_has_no_motion_and_a_moved_patch_is_found() {
        let (sharp, _) = scene::textured(320, 240, 4).unwrap();
        let reference = prepared(&sharp);
        let all = |_: (f64, f64)| true;
        let still = Motion::new(measure(&reference, &prepared(&sharp), &Affine::IDENTITY, 1.0, None).unwrap(), all);
        assert!(still.regions.is_empty() && still.score == 0.0 && still.peak < 0.05, "{:?} {}", still.regions, still.peak);
        // A patch of the frame moved by 4 pixels to the right and 2 down.
        let shifted = warp(&sharp, &Affine::translation(-4.0, -2.0), Interpolation::Bicubic, Border::Clamp).unwrap();
        let moved = Image::from_fn(320, 240, 3, ColorSpace::LINEAR, |x, y, c| {
            let inside = (100..180).contains(&x) && (80..150).contains(&y);
            if inside { shifted.get(x, y, c).unwrap() } else { sharp.get(x, y, c).unwrap() }
        })
        .unwrap();
        let motion = Motion::new(measure(&reference, &prepared(&moved), &Affine::IDENTITY, 1.0, None).unwrap(), all);
        assert!(motion.moved(), "{motion:?}");
        let region = motion.regions[0];
        assert!(region.x <= 110 && region.x + region.width >= 170 && region.y <= 90 && region.y + region.height >= 140, "{region:?}");
        assert!((region.dx - 4.0).abs() < 0.2 && (region.dy - 2.0).abs() < 0.2 && region.peak >= 4.0, "{region:?}");
        // Inside the patch, the displacement is the patch's.
        let (cx, cy) = motion.map.to_frame().inverse().unwrap().apply(140.0, 115.0);
        let d = motion.map.displacement(pixel(cx), pixel(cy)).unwrap();
        assert!((d.0 - 4.0).abs() < 0.2 && (d.1 - 2.0).abs() < 0.2, "{d:?}");
        // Over a spot of the defect mask, nothing is measured.
        let mask = DefectMask::from_fn(320, 240, |x, y| (x, y) == (140, 115)).unwrap();
        let masked = measure(&reference, &prepared(&moved), &Affine::IDENTITY, 1.0, Some(&mask)).unwrap();
        assert_eq!(masked.displacement(pixel(cx), pixel(cy)), None);
        // Cells kept out (where the frame is out of focus) don't count.
        let none = Motion::new(measure(&reference, &prepared(&moved), &Affine::IDENTITY, 1.0, None).unwrap(), |_| false);
        assert!(none.regions.is_empty() && none.score == 0.0 && !none.moved());
    }
}
