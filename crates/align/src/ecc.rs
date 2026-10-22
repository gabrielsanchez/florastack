//! The enhanced correlation coefficient (G. D. Evangelidis and E. Z. Psarakis, "Parametric image alignment using
//! enhanced correlation coefficient maximization", IEEE Transactions on Pattern Analysis and Machine Intelligence
//! 30(10), 2008), maximized coarse to fine over a pyramid of band-passed luminance (J. R. Bergen, P. Anandan, K. J.
//! Hanna and R. Hingorani, "Hierarchical model-based motion estimation", ECCV 1992; `prepare` says why band-pass).
//!
//! The template `t` is the reference frame's band-pass at the pixels of a level, and the input `i(p)` the moving
//! frame's, sampled bilinearly where the warp with parameters `p` puts those pixels. Both are made zero-mean over the
//! pixels where both are defined, and the correlation coefficient `rho = t.i / (|t| |i|)` is maximized: a gain or an
//! offset between the frames changes nothing. Each iteration linearizes the warped input, `i(p + dp) = i(p) + G dp`
//! with `G` its zero-mean Jacobian, and takes the paper's closed-form step,
//!
//! ```text
//! dp = H^-1 G^T (lambda t - i),    H = G^T G,
//! lambda = (|i|^2 - i^T P i) / (t^T i - t^T P i),    P = G H^-1 G^T,
//! ```
//!
//! which maximizes the correlation of the linearized input; a denominator that isn't positive means the images don't
//! correlate, and the level fails. This is the forward-additive form: the Jacobian is that of the input at the current
//! warp, so any subset of the terms can be estimated (the inverse-compositional form needs the enabled terms to form
//! a group, and "rotation without vertical shift" doesn't). Parameters beyond their limits are clamped, and a term the
//! step keeps pushing past its limit is reported.
//!
//! Two additions keep frames focused at different depths from seeming to move (tests/accuracy.rs measures what is
//! left):
//!
//! - **A defocus term.** Blurring an image by a little more or less adds a multiple of its Laplacian, to first order
//!   (the heat equation), so the template's Laplacian `a` joins the gain in the photometric model, `i = lambda t +
//!   beta a`. It is projected out of every vector before the step (as G. D. Hager and P. N. Belhumeur project out
//!   illumination in "Efficient region tracking with parametric models of geometry and illumination", IEEE
//!   Transactions on Pattern Analysis and Machine Intelligence 20(10), 1998), so that what a change of focus explains
//!   isn't taken for motion.
//! - **Robust weights.** From the second iteration on, each pixel is weighted by Tukey's biweight of its residual (A.
//!   E. Beaton and J. W. Tukey, "The fitting of power series, meaning polynomials, illustrated on band-spectroscopic
//!   data", Technometrics 16(2), 1974), iteratively reweighted: see [`Robust`] for which residual and why.
//!
//! Each row's sums are accumulated in `f64` and the rows' sums are added in row order, so the result doesn't depend on
//! how rayon schedules the rows. The number of terms is a compile-time constant of the inner loop, so that a stack
//! that needs fewer terms is aligned with less work per pixel. The score is the plain correlation coefficient, with
//! neither weights nor the defocus term.

use rayon::prelude::*;

use crate::model::{Geometry, Params, index};
use crate::phase::correlate;
use crate::prepare::{CORRELATION_SIDE, Level, Prepared};
use crate::settings::Term;

/// Pixels this close to a level's edges are left out, in the template and the input alike: the blurs and the Sobel
/// operator see the clamped border there.
const MARGIN: usize = 4;
/// Pixels whose residual exceeds this many standard deviations of the residual get no weight.
const TUKEY: f64 = 3.0;
/// A level needs at least this many pixels in common to be compared.
const MIN_PIXELS: f64 = 64.0;
/// The iterations allowed on the coarsest level and on the others.
const COARSE_ITERATIONS: usize = 60;
const ITERATIONS: usize = 30;
/// A level has converged when a step moves no point by more than this, in pixels of the level: the finest, and the
/// coarser ones, which only need to bring the next level within reach.
const FINE_TOLERANCE: f64 = 1e-3;
const COARSE_TOLERANCE: f64 = 1e-2;
/// A term counts as pushing past its limit when a step would take it beyond by more than this, in frame pixels.
pub(crate) const LIMIT_TOLERANCE: f64 = 0.05;

/// What aligning a moving frame to a reference found: the parameters that map the reference's pixels to the moving
/// frame's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PairAlignment {
    pub params: Params,
    /// The correlation coefficient at the finest level, from -1 to 1 (0 when it couldn't be computed).
    pub score: f64,
    /// Whether the finest level was matched.
    pub matched: bool,
    /// A term the finest level kept pushing past its limit.
    pub pushed: Option<Term>,
    /// How ambiguous the match is: the second correlation peak relative to the first (see [`crate::phase`]).
    pub ambiguity: f64,
}

/// Aligns `moving` to `reference` (same size) for the enabled `terms`, from `start`. With `expected`, the shift first
/// comes from phase correlation, preferring among equally good matches the one nearest `expected` (frame pixels), and
/// the match is checked for repeating patterns at the finest level too.
pub(crate) fn align(
    reference: &Prepared,
    moving: &Prepared,
    geometry: &Geometry,
    terms: &[Term],
    start: Params,
    expected: Option<(f64, f64)>,
) -> PairAlignment {
    let mut params = start;
    let mut ambiguity: f64 = 0.0;
    let shifts = terms.iter().any(|t| matches!(t, Term::ShiftX | Term::ShiftY));
    if let Some(expected) = expected
        && shifts
        && let (Some(level), Some(other)) = (reference.levels.get(reference.correlation_level), moving.levels.get(reference.correlation_level))
    {
        let (ax, ay) = level.mapping.scale;
        let max = (geometry.limits.get(Term::ShiftX) / ax, geometry.limits.get(Term::ShiftY) / ay);
        if let Some(c) = correlate(&level.field, &other.field, 0, max, (expected.0 / ax, expected.1 / ay)) {
            for (term, shift) in [(Term::ShiftX, c.shift.0 * ax), (Term::ShiftY, c.shift.1 * ay)] {
                if terms.contains(&term) {
                    params.set(term, shift.clamp(-geometry.limits.get(term), geometry.limits.get(term)));
                }
            }
            ambiguity = c.ambiguity;
        }
    }

    let coarsest = reference.levels.len().saturating_sub(1);
    let mut finest = None;
    for (l, (template, input)) in reference.levels.iter().zip(&moving.levels).enumerate().rev() {
        let (iterations, tolerance) = if l == 0 {
            (if coarsest == 0 { COARSE_ITERATIONS } else { ITERATIONS }, FINE_TOLERANCE)
        } else {
            (if l == coarsest { COARSE_ITERATIONS } else { ITERATIONS }, COARSE_TOLERANCE)
        };
        let outcome = level(template, input, geometry, terms, params, iterations, tolerance);
        if let Some(p) = outcome.params {
            params = p;
        }
        if l == 0 {
            finest = Some(outcome);
        }
    }
    let (score, matched, pushed) = finest.map_or((0.0, false, None), |o| (o.score, o.params.is_some(), o.pushed));
    if expected.is_some() && matched && reference.correlation_level != 0 {
        ambiguity = ambiguity.max(fine_ambiguity(reference, moving, geometry, &params));
    }
    PairAlignment { params, score, matched, pushed, ambiguity }
}

/// What one level found: the parameters (`None` when the level failed), the correlation at the last evaluation and
/// a term pushed past its limit by the last step.
struct Outcome {
    params: Option<Params>,
    score: f64,
    pushed: Option<Term>,
}

/// Maximizes the correlation on one level, from `start`.
fn level(template: &Level, input: &Level, geometry: &Geometry, terms: &[Term], start: Params, iterations: usize, tolerance: f64) -> Outcome {
    match terms {
        [] => solve::<0>(template, input, geometry, [], start, iterations, tolerance),
        [a] => solve::<1>(template, input, geometry, [*a], start, iterations, tolerance),
        [a, b] => solve::<2>(template, input, geometry, [*a, *b], start, iterations, tolerance),
        [a, b, c] => solve::<3>(template, input, geometry, [*a, *b, *c], start, iterations, tolerance),
        [a, b, c, d, ..] => solve::<4>(template, input, geometry, [*a, *b, *c, *d], start, iterations, tolerance),
    }
}

/// [`level`] for `K` terms. With none, only the correlation is computed.
fn solve<const K: usize>(
    template: &Level,
    input: &Level,
    geometry: &Geometry,
    terms: [Term; K],
    start: Params,
    iterations: usize,
    tolerance: f64,
) -> Outcome {
    let columns = terms.map(index);
    let step_tolerance = tolerance * template.mapping.scale.0.min(template.mapping.scale.1);
    let mut params = start;
    let mut score = 0.0;
    let mut pushed = None;
    let mut robust = None;
    for _ in 0..iterations.max(1) {
        let sums = accumulate::<K>(template, input, geometry, &params, columns, robust.as_ref());
        let Some(stats) = Stats::new(&sums) else {
            return Outcome { params: None, score: 0.0, pushed: None };
        };
        score = stats.score;
        if K == 0 {
            break;
        }
        let Some(step) = stats.step() else {
            return Outcome { params: None, score, pushed: None };
        };
        robust = Some(stats.robust(step.lambda));
        let mut largest: f64 = 0.0;
        pushed = None;
        for (&term, &delta) in terms.iter().zip(&step.delta) {
            let limit = geometry.limits.get(term);
            let wanted = params.get(term) + delta;
            let clamped = wanted.clamp(-limit, limit);
            if (wanted - clamped).abs() > LIMIT_TOLERANCE {
                pushed = Some(term);
            }
            largest = largest.max((clamped - params.get(term)).abs());
            params.set(term, clamped);
        }
        if !largest.is_finite() {
            return Outcome { params: None, score, pushed: None };
        }
        if largest < step_tolerance {
            break;
        }
    }
    Outcome { params: Some(params), score, pushed }
}

/// Tukey's biweight of each pixel's residual from the last iteration, `i - lambda t` (zero-mean values), against
/// [`TUKEY`] times the standard deviation of the residual that remains once the defocus term is taken out. The
/// residual keeps the defocus term on purpose: where a change of focus alters the image most is also where it shifts
/// the image's detail most, so those pixels get the least weight, and the gain and the motion are measured on the rest.
#[derive(Clone, Copy, Debug)]
struct Robust {
    lambda: f64,
    mean_t: f64,
    mean_i: f64,
    cutoff: f64,
}

impl Robust {
    fn weight(&self, t: f64, i: f64) -> f64 {
        let u = ((i - self.mean_i) - self.lambda * (t - self.mean_t)) / self.cutoff;
        if u.abs() < 1.0 { (1.0 - u * u).powi(2) } else { 0.0 }
    }
}

/// The sums an iteration needs, each pixel weighted by its robust weight: the count, and the sums, squares and cross
/// products of the template `t`, the warped input `i`, the defocus term `a` and the Jacobian's columns `g`. And the
/// unweighted count, sums and products of `t` and `i`, for the score.
#[derive(Clone, Copy)]
struct Sums<const K: usize> {
    n: f64,
    t: f64,
    i: f64,
    a: f64,
    tt: f64,
    ii: f64,
    aa: f64,
    ti: f64,
    ta: f64,
    ia: f64,
    g: [f64; K],
    gt: [f64; K],
    gi: [f64; K],
    ga: [f64; K],
    gg: [[f64; K]; K],
    plain: [f64; 6],
}

impl<const K: usize> Sums<K> {
    const ZERO: Sums<K> = Sums {
        n: 0.0,
        t: 0.0,
        i: 0.0,
        a: 0.0,
        tt: 0.0,
        ii: 0.0,
        aa: 0.0,
        ti: 0.0,
        ta: 0.0,
        ia: 0.0,
        g: [0.0; K],
        gt: [0.0; K],
        gi: [0.0; K],
        ga: [0.0; K],
        gg: [[0.0; K]; K],
        plain: [0.0; 6],
    };

    /// Adds one pixel with weight `w`: template value `t`, warped input `i`, defocus term `a` and Jacobian `g`.
    #[allow(clippy::needless_range_loop)] // The upper triangle of a matrix reads best with indices.
    fn add_pixel(&mut self, w: f64, t: f64, i: f64, a: f64, g: [f64; K]) {
        for (sum, v) in self.plain.iter_mut().zip([1.0, t, i, t * t, i * i, t * i]) {
            *sum += v;
        }
        if w <= 0.0 {
            return;
        }
        let (wt, wi, wa) = (w * t, w * i, w * a);
        self.n += w;
        self.t += wt;
        self.i += wi;
        self.a += wa;
        self.tt += wt * t;
        self.ii += wi * i;
        self.aa += wa * a;
        self.ti += wt * i;
        self.ta += wt * a;
        self.ia += wi * a;
        for j in 0..K {
            let wg = w * g[j];
            self.g[j] += wg;
            self.gt[j] += wg * t;
            self.gi[j] += wg * i;
            self.ga[j] += wg * a;
            for k in j..K {
                self.gg[j][k] += wg * g[k];
            }
        }
    }

    fn add(&mut self, o: &Sums<K>) {
        for (a, b) in [
            (&mut self.n, o.n),
            (&mut self.t, o.t),
            (&mut self.i, o.i),
            (&mut self.a, o.a),
            (&mut self.tt, o.tt),
            (&mut self.ii, o.ii),
            (&mut self.aa, o.aa),
            (&mut self.ti, o.ti),
            (&mut self.ta, o.ta),
            (&mut self.ia, o.ia),
        ] {
            *a += b;
        }
        for (a, b) in self.plain.iter_mut().zip(o.plain) {
            *a += b;
        }
        for j in 0..K {
            self.g[j] += o.g[j];
            self.gt[j] += o.gt[j];
            self.gi[j] += o.gi[j];
            self.ga[j] += o.ga[j];
            for k in 0..K {
                self.gg[j][k] += o.gg[j][k];
            }
        }
    }
}

/// The field's channels: the band-pass luminance, its x and y derivatives, and its Laplacian.
const FIELD: usize = 4;

/// The warp and Jacobian of one iteration, in the level's pixels.
struct Warp {
    /// Template pixel `(x, y)` samples the input at `(m[0] x + m[1] y + m[2], m[3] x + m[4] y + m[5])`.
    m: [f64; 6],
    /// Level pixels to frame pixels: `X = ax x + bx`, `Y = ay y + by`, and frame pixels relative to the center.
    ax: f64,
    bx: f64,
    ay: f64,
    by: f64,
    center: (f64, f64),
    /// The Jacobian's factors: `cos / r` and `sin / r` of the rotation, and `scale` times them.
    cos_r: f64,
    sin_r: f64,
    scaled_cos_r: f64,
    scaled_sin_r: f64,
}

impl Warp {
    fn new(level: &Level, geometry: &Geometry, params: &Params) -> Warp {
        let similarity = geometry.similarity(params);
        let transform = similarity.to_affine(geometry.width, geometry.height);
        let m = level.mapping.level_transform(&transform).m;
        let (sin, cos) = similarity.rotation_deg.to_radians().sin_cos();
        let r = geometry.radius;
        Warp {
            m: [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2]],
            ax: level.mapping.scale.0,
            bx: level.mapping.offset.0,
            ay: level.mapping.scale.1,
            by: level.mapping.offset.1,
            center: geometry.center,
            cos_r: cos / r,
            sin_r: sin / r,
            scaled_cos_r: similarity.scale * cos / r,
            scaled_sin_r: similarity.scale * sin / r,
        }
    }
}

/// The sums of one iteration at `params`, rows in parallel and added in row order.
fn accumulate<const K: usize>(
    template: &Level,
    input: &Level,
    geometry: &Geometry,
    params: &Params,
    columns: [usize; K],
    robust: Option<&Robust>,
) -> Sums<K> {
    let warp = Warp::new(template, geometry, params);
    let (w, h) = (template.field.width(), template.field.height());
    let rows: Vec<Sums<K>> = (MARGIN..h.saturating_sub(MARGIN))
        .into_par_iter()
        .map(|y| template.field.row(y).map_or(Sums::ZERO, |row| accumulate_row(row, y, w, input, &warp, columns, robust)))
        .collect();
    let mut total = Sums::ZERO;
    for row in &rows {
        total.add(row);
    }
    total
}

/// The sums of template row `y` (its interleaved field) against the input.
#[allow(clippy::cast_possible_truncation)] // Sample weights to f32, which the samples are.
fn accumulate_row<const K: usize>(
    row: &[f32],
    y: usize,
    w: usize,
    input: &Level,
    warp: &Warp,
    columns: [usize; K],
    robust: Option<&Robust>,
) -> Sums<K> {
    let mut sums = Sums::ZERO;
    let (iw, ih) = (input.field.width(), input.field.height());
    let stride = input.field.stride();
    let raw = input.field.as_raw();
    let (lo, hi_x, hi_y) = (MARGIN as f64, (iw - 1).saturating_sub(MARGIN) as f64, (ih - 1).saturating_sub(MARGIN) as f64);
    let yf = y as f64;
    let dy = warp.ay * yf + warp.by - warp.center.1;
    let [m0, m1, m2, m3, m4, m5] = warp.m;
    let (row_u, row_v) = (m1 * yf + m2, m4 * yf + m5);
    for x in MARGIN..w.saturating_sub(MARGIN) {
        let xf = x as f64;
        let (u, v) = (m0 * xf + row_u, m3 * xf + row_v);
        if !(u >= lo && u <= hi_x && v >= lo && v <= hi_y) {
            continue;
        }
        let (fu, fv) = (u.floor(), v.floor());
        let base = to_index(fv) * stride + to_index(fu) * FIELD;
        let (Some(top), Some(bottom), Some(here)) =
            (raw.get(base..base + 2 * FIELD), raw.get(base + stride..base + stride + 2 * FIELD), row.get(x * FIELD..x * FIELD + FIELD))
        else {
            continue;
        };
        let (tu, tv) = ((u - fu) as f32, (v - fv) as f32);
        let bilinear = |c: usize| {
            let upper = top[c] + tu * (top[c + FIELD] - top[c]);
            let lower = bottom[c] + tu * (bottom[c + FIELD] - bottom[c]);
            f64::from(upper + tv * (lower - upper))
        };
        let (i, t, a) = (bilinear(0), f64::from(here[0]), f64::from(here[3]));
        let weight = robust.map_or(1.0, |r| r.weight(t, i));
        if K == 0 {
            sums.add_pixel(weight, t, i, a, [0.0; K]);
            continue;
        }
        // The input's gradient per frame pixel, and the Jacobian of the warp in frame pixels.
        let (gx, gy) = (bilinear(1) / warp.ax, bilinear(2) / warp.ay);
        let dx = warp.ax * xf + warp.bx - warp.center.0;
        let jacobian = [
            gx,
            gy,
            gx * (warp.cos_r * dx - warp.sin_r * dy) + gy * (warp.sin_r * dx + warp.cos_r * dy),
            gx * (-warp.scaled_sin_r * dx - warp.scaled_cos_r * dy) + gy * (warp.scaled_cos_r * dx - warp.scaled_sin_r * dy),
        ];
        sums.add_pixel(weight, t, i, a, columns.map(|c| jacobian.get(c).copied().unwrap_or(0.0)));
    }
    sums
}

/// A non-negative coordinate, floored, as an index.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Checked to lie inside the level before the cast.
fn to_index(v: f64) -> usize {
    v as usize
}

/// An iteration's step: the change of the parameters, and the gain `lambda` it was taken with.
struct Step<const K: usize> {
    delta: [f64; K],
    lambda: f64,
}

/// An iteration's statistics: the score, and the weighted, zero-mean products with the defocus term projected out.
struct Stats<const K: usize> {
    /// The plain correlation coefficient of `t` and `i`, unweighted.
    score: f64,
    n: f64,
    mean_t: f64,
    mean_i: f64,
    /// `|t|^2`, `|i|^2`, `t^T i`, `H = G^T G`, `G^T t` and `G^T i`, zero-mean, with the defocus term projected out.
    tt: f64,
    ii: f64,
    ti: f64,
    h: [[f64; K]; K],
    gt: [f64; K],
    gi: [f64; K],
}

impl<const K: usize> Stats<K> {
    #[allow(clippy::needless_range_loop)] // Matrix arithmetic reads best with indices.
    fn new(s: &Sums<K>) -> Option<Stats<K>> {
        let [pn, pt, pi, ptt, pii, pti] = s.plain;
        if pn < MIN_PIXELS || s.n < MIN_PIXELS {
            return None;
        }
        let (ptt, pii) = (ptt - pt * pt / pn, pii - pi * pi / pn);
        if !(ptt > 0.0 && pii > 0.0) {
            return None;
        }
        let score = (pti - pt * pi / pn) / (ptt * pii).sqrt();
        let n = s.n;
        let centered = |sum_uv: f64, sum_u: f64, sum_v: f64| sum_uv - sum_u * sum_v / n;
        let (mut tt, mut ii, mut ti) = (centered(s.tt, s.t, s.t), centered(s.ii, s.i, s.i), centered(s.ti, s.t, s.i));
        let (aa, at, ai) = (centered(s.aa, s.a, s.a), centered(s.ta, s.t, s.a), centered(s.ia, s.i, s.a));
        let mut h = [[0.0; K]; K];
        for j in 0..K {
            for k in j..K {
                let v = centered(s.gg[j][k], s.g[j], s.g[k]);
                h[j][k] = v;
                h[k][j] = v;
            }
        }
        let mut gt: [f64; K] = std::array::from_fn(|j| centered(s.gt[j], s.g[j], s.t));
        let mut gi: [f64; K] = std::array::from_fn(|j| centered(s.gi[j], s.g[j], s.i));
        // Project the defocus term out of every vector: what it explains is neither gain nor motion.
        let ga: [f64; K] = std::array::from_fn(|j| centered(s.ga[j], s.g[j], s.a));
        if aa > 0.0 {
            tt -= at * at / aa;
            ii -= ai * ai / aa;
            ti -= at * ai / aa;
            for j in 0..K {
                gt[j] -= ga[j] * at / aa;
                gi[j] -= ga[j] * ai / aa;
                for k in 0..K {
                    h[j][k] -= ga[j] * ga[k] / aa;
                }
            }
        }
        if !(tt > 0.0 && ii > 0.0 && score.is_finite()) {
            return None;
        }
        Some(Stats { score, n, mean_t: s.t / n, mean_i: s.i / n, tt, ii, ti, h, gt, gi })
    }

    /// The paper's step, or `None` when `H` is singular (a term the images say nothing about) or the images don't
    /// correlate.
    fn step(&self) -> Option<Step<K>> {
        let l = cholesky(&self.h)?;
        let ui = solve_cholesky(&l, &self.gi);
        let ut = solve_cholesky(&l, &self.gt);
        let numerator = self.ii - dot(&self.gi, &ui);
        let denominator = self.ti - dot(&self.gt, &ui);
        if denominator.is_nan() || denominator <= 0.0 {
            return None;
        }
        let lambda = numerator / denominator;
        let delta: [f64; K] = std::array::from_fn(|j| lambda * ut[j] - ui[j]);
        delta.iter().all(|v| v.is_finite()).then_some(Step { delta, lambda })
    }

    /// The robust weights for the next iteration (see [`Robust`]).
    fn robust(&self, lambda: f64) -> Robust {
        let variance = (self.ii - 2.0 * lambda * self.ti + lambda * lambda * self.tt).max(0.0) / self.n;
        let cutoff = TUKEY * variance.sqrt();
        // Frames that match exactly would leave a zero cutoff, and every weight 0/0: every pixel keeps its weight.
        let floor = 1e-9 * (self.ii / self.n).sqrt();
        Robust { lambda, mean_t: self.mean_t, mean_i: self.mean_i, cutoff: if cutoff > floor { cutoff } else { f64::INFINITY } }
    }
}

fn dot<const K: usize>(a: &[f64; K], b: &[f64; K]) -> f64 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

/// The lower-triangular `L` with `L L^T = h`, or `None` unless `h` is clearly positive definite.
#[allow(clippy::needless_range_loop)] // Matrix arithmetic reads best with indices.
fn cholesky<const K: usize>(h: &[[f64; K]; K]) -> Option<[[f64; K]; K]> {
    let mut l = [[0.0; K]; K];
    for i in 0..K {
        for j in 0..=i {
            let mut sum = h[i][j];
            for k in 0..j {
                sum -= l[i][k] * l[j][k];
            }
            if i == j {
                // Relative to the diagonal: a pivot that cancels to rounding noise means a singular matrix.
                if !(sum > 1e-12 * h[i][i] && sum > 0.0) {
                    return None;
                }
                l[i][i] = sum.sqrt();
            } else {
                l[i][j] = sum / l[j][j];
            }
        }
    }
    Some(l)
}

/// `x` with `L L^T x = b`.
#[allow(clippy::needless_range_loop)] // Matrix arithmetic reads best with indices.
fn solve_cholesky<const K: usize>(l: &[[f64; K]; K], b: &[f64; K]) -> [f64; K] {
    let mut y = [0.0; K];
    for i in 0..K {
        let mut sum = b[i];
        for k in 0..i {
            sum -= l[i][k] * y[k];
        }
        y[i] = sum / l[i][i];
    }
    let mut x = [0.0; K];
    for i in (0..K).rev() {
        let mut sum = y[i];
        for k in i + 1..K {
            sum -= l[k][i] * x[k];
        }
        x[i] = sum / l[i][i];
    }
    x
}

/// The ambiguity of the match at the finest level: a central crop of the reference's band-pass correlated with the
/// moving frame's, warped by `params`, whose correlation peaks at no shift; a pattern that repeats has more
/// peaks about as high, a period away.
fn fine_ambiguity(reference: &Prepared, moving: &Prepared, geometry: &Geometry, params: &Params) -> f64 {
    let (Some(template), Some(input)) = (reference.levels.first(), moving.levels.first()) else {
        return 0.0;
    };
    let (w, h) = (template.field.width(), template.field.height());
    let (cw, ch) = (w.saturating_sub(2 * MARGIN).min(CORRELATION_SIDE), h.saturating_sub(2 * MARGIN).min(CORRELATION_SIDE));
    if cw < 16 || ch < 16 {
        return 0.0;
    }
    let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
    let warp = Warp::new(template, geometry, params);
    let [m0, m1, m2, m3, m4, m5] = warp.m;
    let (iw, ih) = (input.field.width(), input.field.height());
    let sample = |u: f64, v: f64| {
        let (u, v) = (u.clamp(0.0, (iw - 1) as f64), v.clamp(0.0, (ih - 1) as f64));
        let (fu, fv) = (u.floor(), v.floor());
        let (x, y) = (to_index(fu), to_index(fv));
        let at = |x: usize, y: usize| input.field.get(x.min(iw - 1), y.min(ih - 1), 0).map_or(0.0, f64::from);
        let (tu, tv) = (u - fu, v - fv);
        let upper = at(x, y) + tu * (at(x + 1, y) - at(x, y));
        let lower = at(x, y + 1) + tu * (at(x + 1, y + 1) - at(x, y + 1));
        upper + tv * (lower - upper)
    };
    let crop = |f: &dyn Fn(usize, usize) -> f64| {
        florastack_core::Image::from_fn(cw, ch, 1, florastack_core::ColorSpace::LINEAR, |x, y, _| {
            #[allow(clippy::cast_possible_truncation)] // A band-pass sample, back to the f32 it came from.
            let v = f(x0 + x, y0 + y) as f32;
            v
        })
    };
    let Ok(fixed) = crop(&|x, y| template.field.get(x, y, 0).map_or(0.0, f64::from)) else {
        return 0.0;
    };
    let Ok(warped) = crop(&|x, y| {
        let (xf, yf) = (x as f64, y as f64);
        sample(m0 * xf + m1 * yf + m2, m3 * xf + m4 * yf + m5)
    }) else {
        return 0.0;
    };
    let reach = |n: usize, limit: f64, scale: f64| (n as f64 / 4.0).min(limit / scale);
    let max = (
        reach(cw, geometry.limits.get(Term::ShiftX), template.mapping.scale.0),
        reach(ch, geometry.limits.get(Term::ShiftY), template.mapping.scale.1),
    );
    correlate(&fixed, &warped, 0, max, (0.0, 0.0)).map_or(0.0, |c| c.ambiguity)
}

#[cfg(test)]
mod tests {
    use florastack_core::testutil::scene;
    use florastack_core::{ColorSpace, Image};

    use super::*;
    use crate::model::Similarity;
    use crate::settings::AlignSettings;

    fn frames(truth: Similarity, size: (usize, usize)) -> (Prepared, Prepared, Geometry) {
        let (w, h) = size;
        let (sharp, _) = scene::textured(w, h, 21).unwrap();
        let transform = truth.to_affine(w, h);
        // The moving frame shows the scene through `truth`: moving(T(x)) = reference(x).
        let inverse = transform.inverse().unwrap();
        let moving =
            florastack_core::ops::warp(&sharp, &inverse, florastack_core::ops::Interpolation::Bicubic, florastack_core::ops::Border::Clamp).unwrap();
        let geometry = Geometry::new(w, h, &AlignSettings::default());
        (Prepared::new(&sharp, 2048, false).unwrap(), Prepared::new(&moving, 2048, false).unwrap(), geometry)
    }

    #[test]
    fn each_term_is_recovered_alone_and_disabled_terms_stay_put() {
        let cases = [
            (Similarity { shift_x: 2.7, ..Similarity::IDENTITY }, vec![Term::ShiftX]),
            (Similarity { shift_y: -1.3, ..Similarity::IDENTITY }, vec![Term::ShiftY]),
            (Similarity { scale: 1.006, ..Similarity::IDENTITY }, vec![Term::Scale]),
            (Similarity { rotation_deg: 0.35, ..Similarity::IDENTITY }, vec![Term::Rotation]),
            (Similarity { shift_x: -3.2, shift_y: 1.9, scale: 0.995, rotation_deg: -0.2 }, Term::ALL.to_vec()),
        ];
        for (truth, terms) in cases {
            let (reference, moving, geometry) = frames(truth, (192, 144));
            let found = align(&reference, &moving, &geometry, &terms, Params::default(), Some((0.0, 0.0)));
            assert!(found.matched && found.score > 0.9, "{truth:?}: {found:?}");
            let got = geometry.similarity(&found.params);
            let error = florastack_core::testutil::transform_error(&got.to_affine(192, 144), &truth.to_affine(192, 144), 192, 144);
            assert!(error < 0.05, "{truth:?}: {got:?}, error {error}");
            for term in Term::ALL.into_iter().filter(|t| !terms.contains(t)) {
                assert_eq!(found.params.get(term), 0.0, "{term:?} is off");
            }
        }
    }

    #[test]
    fn limits_clamp_and_report_the_term() {
        let truth = Similarity { shift_x: 6.0, ..Similarity::IDENTITY };
        let (reference, moving, _) = frames(truth, (160, 120));
        // 2% of 160 pixels: at most 3.2 pixels.
        let geometry = Geometry::new(160, 120, &AlignSettings { max_shift: 2.0, ..AlignSettings::default() });
        let found = align(&reference, &moving, &geometry, &Term::ALL, Params::default(), Some((0.0, 0.0)));
        assert_eq!(found.pushed, Some(Term::ShiftX), "{found:?}");
        assert!((found.params.get(Term::ShiftX) - 3.2).abs() < 1e-9, "{found:?}");
    }

    #[test]
    fn unrelated_or_flat_frames_do_not_match() {
        let (sharp, _) = scene::textured(96, 96, 1).unwrap();
        let flat = Image::from_fn(96, 96, 3, ColorSpace::LINEAR, |_, _, _| 0.5).unwrap();
        let geometry = Geometry::new(96, 96, &AlignSettings::default());
        let (a, b) = (Prepared::new(&sharp, 2048, false).unwrap(), Prepared::new(&flat, 2048, false).unwrap());
        let found = align(&a, &b, &geometry, &Term::ALL, Params::default(), Some((0.0, 0.0)));
        assert!(!found.matched && found.score == 0.0, "{found:?}");
        let (other, _) = scene::textured(96, 96, 99).unwrap();
        let c = Prepared::new(&other, 2048, false).unwrap();
        // Another scene of the same kind: four free terms can line up some of its edges, but not without a limit or a
        // low score.
        let found = align(&a, &c, &geometry, &Term::ALL, Params::default(), Some((0.0, 0.0)));
        assert!(!found.matched || found.pushed.is_some() || found.score < 0.5, "{found:?}");
    }

    #[test]
    fn identical_frames_match_exactly() {
        // Converging onto an exact match leaves almost no residual, and the robust weights must still leave every pixel
        // that matches its weight. Starting off target makes the solver iterate, with weights, to get there.
        let (reference, copy, geometry) = frames(Similarity::IDENTITY, (128, 96));
        let found = align(&reference, &copy, &geometry, &Term::ALL, Params([0.3, -0.2, 0.0, 0.0]), None);
        assert!(found.matched && found.score > 0.999_999, "{found:?}");
        assert!(found.params.0.iter().all(|p| p.abs() < 1e-4), "{found:?}");
    }

    #[test]
    fn the_solver_solves() {
        let h = [[4.0, 2.0, 0.6], [2.0, 5.0, 1.0], [0.6, 1.0, 3.0]];
        let l = cholesky(&h).unwrap();
        let x = solve_cholesky(&l, &[1.0, -2.0, 0.5]);
        for (row, b) in h.iter().zip([1.0, -2.0, 0.5]) {
            assert!((dot(row, &x) - b).abs() < 1e-12);
        }
        assert!(cholesky(&[[1.0, 1.0], [1.0, 1.0]]).is_none(), "singular");
        assert!(cholesky(&[[0.0]]).is_none());
    }
}
