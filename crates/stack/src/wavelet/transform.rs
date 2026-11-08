//! The stationary wavelet transform by the "a trous" algorithm: separable filters dilated by `2^level`, nothing
//! decimated, so a shifted image gives shifted coefficients (M. Holschneider, R. Kronland-Martinet, J. Morlet and
//! P. Tchamitchian, "A real-time algorithm for signal analysis with the help of the wavelet transform", 1989;
//! G. P. Nason and B. W. Silverman, "The stationary wavelet transform and some statistical applications", 1995).
//!
//! # Filter banks
//!
//! A [`Bank`] holds four filters, analysis low and high and synthesis low and high, such that synthesis undoes analysis:
//! `G0 H0 + G1 H1 = 1`. Nothing is decimated, so no alias has to cancel, and that one identity is the whole
//! reconstruction condition, at every dilation. Low-pass filters sum to 1, so a level keeps the image's brightness.
//!
//! - [`Wavelet::Bior22`]: the biorthogonal spline of A. Cohen, I. Daubechies and J.-C. Feauveau ("Biorthogonal bases of
//!   compactly supported wavelets", 1992), the 5/3 pair. Its filters are symmetric, so a decision doesn't drift sideways
//!   from the detail it was made on, its taps are exact in `f32`, and its short synthesis low-pass spreads a change of
//!   frame over few pixels.
//! - [`Wavelet::Db2`]: I. Daubechies's orthogonal wavelet with two vanishing moments, whose synthesis filters are the
//!   analysis filters reversed.
//!
//! A new bank is a new [`Wavelet`] variant and its [`Bank`]: everything else reads [`Bank::reach`] and the taps. The same
//! holds for a transform with other subbands, such as a dual-tree complex one: [`Details`] is what the fusion rules read.
//!
//! # Boundaries
//!
//! A plane is extended once, mirrored about the canvas's edge samples ([`extend`]), by as far as the filters reach, and
//! the transform runs on the extended plane. Filters then read beyond it only within that margin, whose samples are
//! never kept: they repeat the plane's edge sample. A symmetric bank would keep a mirrored plane mirrored at every level
//! anyway; extending first gives an asymmetric bank the same exact reconstruction, with no limit on the plane's size.
//!
//! # Arithmetic
//!
//! Every output adds its taps in order from the first, in `f32`, rows before columns in analysis and columns before rows
//! in synthesis, so a tile and the whole canvas compute every sample they share bit for bit alike.

use florastack_core::sync::Mutex;
use florastack_core::{ColorSpace, Image, Rect};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::StackError;
use crate::pyramid::MAX_LEVELS;
use crate::pyramid::levels::{KERNEL, Span, local, signed};

/// Which filter bank the transform uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Wavelet {
    /// The biorthogonal spline 2.2 (the 5/3 pair): symmetric, so decisions stay on the detail they were made on.
    #[default]
    Bior22,
    /// Daubechies's db2, orthogonal and asymmetric.
    Db2,
}

/// A filter: `out[n] = sum of taps[i] * in[n + (first + i) * dilation]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Filter {
    first: isize,
    taps: &'static [f32],
}

impl Filter {
    /// How far the filter reads from a sample, undilated.
    fn reach(self) -> usize {
        let last = self.first + signed(self.taps.len()) - 1;
        self.first.unsigned_abs().max(last.unsigned_abs())
    }
}

/// An analysis and synthesis pair. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bank {
    low: Filter,
    high: Filter,
    synthesis_low: Filter,
    synthesis_high: Filter,
}

impl Bank {
    /// How far any of the bank's filters reads from a sample, undilated.
    pub fn reach(&self) -> usize {
        [self.low, self.high, self.synthesis_low, self.synthesis_high].iter().map(|f| f.reach()).max().unwrap_or(0)
    }
}

/// The 5/3 pair, zero-phase: `(-1, 2, 6, 2, -1) / 8` and `(1, 2, 1) / 4`, the high-pass filters their modulations.
static BIOR22: Bank = Bank {
    low: Filter { first: -2, taps: &[-0.125, 0.25, 0.75, 0.25, -0.125] },
    high: Filter { first: -1, taps: &[-0.25, 0.5, -0.25] },
    synthesis_low: Filter { first: -1, taps: &[0.25, 0.5, 0.25] },
    synthesis_high: Filter { first: -2, taps: &[-0.125, -0.25, 0.75, -0.25, -0.125] },
};

/// db2, its scaling filter `(1 + s, 3 + s, 3 - s, 1 - s) / 8` with `s` the square root of 3, summing to 1; its wavelet
/// filter the quadrature mirror, and the synthesis filters both reversed.
static DB2: Bank = Bank {
    low: Filter { first: 0, taps: &[0.341_506_35, 0.591_506_35, 0.158_493_65, -0.091_506_35] },
    high: Filter { first: 0, taps: &[-0.091_506_35, -0.158_493_65, 0.591_506_35, -0.341_506_35] },
    synthesis_low: Filter { first: -3, taps: &[-0.091_506_35, 0.158_493_65, 0.591_506_35, 0.341_506_35] },
    synthesis_high: Filter { first: -3, taps: &[-0.341_506_35, 0.591_506_35, -0.158_493_65, -0.091_506_35] },
};

/// The window of a 3x3 activity: `(1, 2, 1) / 4` on each axis.
static WINDOW_3: Filter = Filter { first: -1, taps: &[0.25, 0.5, 0.25] };
/// The window of a 5x5 activity, the pyramid's binomial kernel.
static WINDOW_5: Filter = Filter { first: -2, taps: &KERNEL };

impl Wavelet {
    pub fn bank(self) -> &'static Bank {
        match self {
            Wavelet::Bior22 => &BIOR22,
            Wavelet::Db2 => &DB2,
        }
    }
}

/// One level's detail subbands, each the plane's size.
#[derive(Clone, Debug, PartialEq)]
pub struct Details {
    /// Low-pass along rows, high-pass along columns.
    pub horizontal: Image<f32>,
    /// High-pass along rows, low-pass along columns.
    pub vertical: Image<f32>,
    /// High-pass along both.
    pub diagonal: Image<f32>,
}

impl Details {
    /// Multiplies every subband by `mask`, one plane of their size.
    pub(super) fn scale(&mut self, mask: &Image<f32>) {
        for band in [&mut self.horizontal, &mut self.vertical, &mut self.diagonal] {
            multiply(band, mask);
        }
    }

    /// Hands the subbands' planes back to `spare`.
    pub fn recycle(self, spare: &Spare) {
        spare.give(self.horizontal);
        spare.give(self.vertical);
        spare.give(self.diagonal);
    }
}

/// Planes to use again. Every filter writes a new plane, and on tiles of 1024 pixels a stack takes a fifth less time
/// when those planes aren't new: touching a fresh plane costs about as much as filtering it. The transform takes its
/// planes from here, and what it hands back goes to the next filter on any thread. A kernel owns one, so that the planes
/// go with the stack. Not handing a plane back costs only a new one.
#[derive(Debug, Default)]
pub struct Spare {
    planes: Mutex<Vec<Vec<f32>>>,
}

impl Spare {
    /// The most bytes kept for later.
    const BUDGET: usize = 256 << 20;

    /// A plane of zeros.
    pub fn take(&self, width: usize, height: usize) -> Result<Image<f32>, StackError> {
        self.plane(width, height, true)
    }

    /// A plane whose samples are anything: for a filter, which writes them all.
    fn take_written(&self, width: usize, height: usize) -> Result<Image<f32>, StackError> {
        self.plane(width, height, false)
    }

    /// A copy of `plane`. A plane that didn't come from here adds to what is kept when it is handed back, frame after
    /// frame, so this is what stands in for `clone`.
    pub fn copy(&self, plane: &Image<f32>) -> Result<Image<f32>, StackError> {
        let mut out = self.take_written(plane.width(), plane.height())?;
        out.par_rows_mut().zip(plane.par_rows()).for_each(|(dst, src)| dst.copy_from_slice(src));
        Ok(out)
    }

    fn plane(&self, width: usize, height: usize, clear: bool) -> Result<Image<f32>, StackError> {
        let n = width.saturating_mul(height);
        let reused = self.planes.lock().ok().and_then(|mut planes| planes.pop());
        match reused {
            Some(data) if data.len() == n && !clear => Ok(Image::from_vec(width, height, 1, ColorSpace::LINEAR, data)?),
            Some(mut data) if data.capacity() >= n => {
                data.clear();
                data.resize(n, 0.0);
                Ok(Image::from_vec(width, height, 1, ColorSpace::LINEAR, data)?)
            }
            _ => Ok(Image::new(width, height, 1, ColorSpace::LINEAR)?),
        }
    }

    /// Keeps `plane` for later, unless there is enough already.
    pub fn give(&self, plane: Image<f32>) {
        let data = plane.into_raw();
        if let Ok(mut planes) = self.planes.lock() {
            let kept: usize = planes.iter().map(|p| p.capacity() * size_of::<f32>()).sum();
            if kept + data.capacity() * size_of::<f32>() <= Spare::BUDGET {
                planes.push(data);
            }
        }
    }
}

/// `a *= b`, sample by sample (planes of the same size).
pub(super) fn multiply(a: &mut Image<f32>, b: &Image<f32>) {
    a.par_rows_mut().zip(b.par_rows()).for_each(|(a, b)| a.iter_mut().zip(b).for_each(|(v, &s)| *v *= s));
}

/// `a += b`, sample by sample (planes of the same size).
fn add(a: &mut Image<f32>, b: &Image<f32>) {
    a.par_rows_mut().zip(b.par_rows()).for_each(|(a, b)| a.iter_mut().zip(b).for_each(|(v, &s)| *v += s));
}

/// `dst[x] += tap * row[x + shift]`, reading `row`'s end samples beyond it; with `FIRST`, `dst` is written instead, so
/// that its samples needn't be cleared first.
fn add_shifted<const FIRST: bool>(dst: &mut [f32], row: &[f32], tap: f32, shift: isize) {
    let put = |v: &mut f32, s: f32| {
        if FIRST {
            *v = tap * s;
        } else {
            *v += tap * s;
        }
    };
    let n = signed(dst.len());
    let from = usize::try_from((-shift).clamp(0, n)).unwrap_or(0);
    let to = usize::try_from((n - shift).clamp(0, n)).unwrap_or(0).max(from);
    if let Some(&first) = row.first() {
        dst.iter_mut().take(from).for_each(|v| put(v, first));
    }
    if let (Some(out), Some(src)) =
        (dst.get_mut(from..to), row.get(usize::try_from(signed(from) + shift).unwrap_or(0)..usize::try_from(signed(to) + shift).unwrap_or(0)))
    {
        out.iter_mut().zip(src).for_each(|(v, &s)| put(v, s));
    }
    if let Some(&last) = row.last() {
        dst.iter_mut().skip(to).for_each(|v| put(v, last));
    }
}

fn filter_rows(src: &Image<f32>, filter: Filter, dilation: usize, spare: &Spare) -> Result<Image<f32>, StackError> {
    let mut out = spare.take_written(src.width(), src.height())?;
    out.par_rows_mut().zip(src.par_rows()).for_each(|(dst, row)| {
        for (i, &tap) in filter.taps.iter().enumerate() {
            let shift = (filter.first + signed(i)) * signed(dilation);
            if i == 0 {
                add_shifted::<true>(dst, row, tap, shift);
            } else {
                add_shifted::<false>(dst, row, tap, shift);
            }
        }
    });
    Ok(out)
}

fn filter_columns(src: &Image<f32>, filter: Filter, dilation: usize, spare: &Spare) -> Result<Image<f32>, StackError> {
    let mut out = spare.take_written(src.width(), src.height())?;
    let last = signed(src.height()) - 1;
    out.par_rows_mut().enumerate().for_each(|(y, dst)| {
        for (i, &tap) in filter.taps.iter().enumerate() {
            let at = (signed(y) + (filter.first + signed(i)) * signed(dilation)).clamp(0, last);
            if let Some(row) = usize::try_from(at).ok().and_then(|at| src.row(at)) {
                if i == 0 {
                    dst.iter_mut().zip(row).for_each(|(v, &s)| *v = tap * s);
                } else {
                    dst.iter_mut().zip(row).for_each(|(v, &s)| *v += tap * s);
                }
            }
        }
    });
    Ok(out)
}

/// `plane`'s next level at `dilation`: its low-pass and its details.
pub fn analyze(bank: &Bank, plane: &Image<f32>, dilation: usize, spare: &Spare) -> Result<(Image<f32>, Details), StackError> {
    let low = filter_rows(plane, bank.low, dilation, spare)?;
    let high = filter_rows(plane, bank.high, dilation, spare)?;
    let details = Details {
        horizontal: filter_columns(&low, bank.high, dilation, spare)?,
        vertical: filter_columns(&high, bank.low, dilation, spare)?,
        diagonal: filter_columns(&high, bank.high, dilation, spare)?,
    };
    let next = filter_columns(&low, bank.low, dilation, spare)?;
    spare.give(low);
    spare.give(high);
    Ok((next, details))
}

/// The plane [`analyze`] took `low` and `details` from, or, with no details, what `low` alone gives: synthesis is
/// linear, so a plane's parts can be synthesized apart and added.
pub fn synthesize(bank: &Bank, low: &Image<f32>, details: Option<&Details>, dilation: usize, spare: &Spare) -> Result<Image<f32>, StackError> {
    let mut rows = filter_columns(low, bank.synthesis_low, dilation, spare)?;
    let Some(details) = details else {
        let out = filter_rows(&rows, bank.synthesis_low, dilation, spare)?;
        spare.give(rows);
        return Ok(out);
    };
    let added = |to: &mut Image<f32>, plane: Image<f32>| {
        add(to, &plane);
        spare.give(plane);
    };
    added(&mut rows, filter_columns(&details.horizontal, bank.synthesis_high, dilation, spare)?);
    let mut high = filter_columns(&details.vertical, bank.synthesis_low, dilation, spare)?;
    added(&mut high, filter_columns(&details.diagonal, bank.synthesis_high, dilation, spare)?);
    let mut out = filter_rows(&rows, bank.synthesis_low, dilation, spare)?;
    added(&mut out, filter_rows(&high, bank.synthesis_high, dilation, spare)?);
    spare.give(rows);
    spare.give(high);
    Ok(out)
}

/// A level's activity: the energy of its three subbands together, over a `window` x `window` binomial window, 3 or 5.
/// The window stays that small at every level: on the layered stacks, dilating it like the level cost 0.003 to 0.009 of
/// SSIM, since a wider window carries a frame's choice past the edge of what it has in focus.
pub fn activity(details: &Details, window: u32, spare: &Spare) -> Result<Image<f32>, StackError> {
    let mut energy = spare.take(details.horizontal.width(), details.horizontal.height())?;
    let rows = energy.par_rows_mut().zip(details.horizontal.par_rows()).zip(details.vertical.par_rows()).zip(details.diagonal.par_rows());
    rows.for_each(|(((e, h), v), d)| {
        for (((e, &h), &v), &d) in e.iter_mut().zip(h).zip(v).zip(d) {
            *e = h * h + v * v + d * d;
        }
    });
    let filter = if window == 3 { WINDOW_3 } else { WINDOW_5 };
    let rows = filter_rows(&energy, filter, 1, spare)?;
    spare.give(energy);
    let out = filter_columns(&rows, filter, 1, spare)?;
    spare.give(rows);
    Ok(out)
}

/// `coverage` pooled to the next level at `dilation`: the binomial kernel, whose taps are all positive.
pub(super) fn pool(coverage: &Image<f32>, dilation: usize, spare: &Spare) -> Result<Image<f32>, StackError> {
    let rows = filter_rows(coverage, WINDOW_5, dilation, spare)?;
    let out = filter_columns(&rows, WINDOW_5, dilation, spare)?;
    spare.give(rows);
    Ok(out)
}

/// How far `levels` levels of `bank` read from a sample, there and back: what a plane must be extended by.
pub fn extension(bank: &Bank, levels: usize) -> usize {
    2 * bank.reach() * ((1usize << levels.min(MAX_LEVELS as usize)) - 1)
}

/// Channel `channel` of `image`, an image of `span`, extended by `by` samples on every side: mirrored about the edges of
/// `span`'s level, and repeating `span`'s own edge samples where the level goes on beyond it.
pub fn extend(image: &Image<f32>, span: Span, by: usize, channel: usize, spare: &Spare) -> Result<Image<f32>, StackError> {
    let c = image.channels();
    let indices = |start: usize, len: usize, full: usize| -> Vec<usize> {
        (0..len + 2 * by).map(|i| local(signed(start + i) - signed(by), full, start, len)).collect()
    };
    let xs = indices(span.x, span.width, span.full_width);
    let ys = indices(span.y, span.height, span.full_height);
    let mut out = spare.take(xs.len(), ys.len())?;
    out.par_rows_mut().zip(ys.par_iter()).for_each(|(dst, &y)| {
        if let Some(row) = image.row(y) {
            for (v, &x) in dst.iter_mut().zip(&xs) {
                *v = row.get(x * c + channel).copied().unwrap_or(0.0);
            }
        }
    });
    Ok(out)
}

/// A plane's stationary wavelet transform: what [`decompose`] gives and [`Decomposition::reconstruct`] inverts.
#[derive(Clone, Debug, PartialEq)]
pub struct Decomposition {
    /// The levels' details, finest first, on the extended plane.
    pub details: Vec<Details>,
    /// The coarsest low-pass, on the extended plane.
    pub low: Image<f32>,
    /// The samples the plane was extended by on every side.
    pub extended: usize,
    bank: &'static Bank,
    width: usize,
    height: usize,
}

/// The stationary wavelet transform of `plane` (one channel, any size). The levels' dilations are 1, 2, 4...
pub fn decompose(plane: &Image<f32>, wavelet: Wavelet, levels: usize) -> Result<Decomposition, StackError> {
    let (width, height) = (plane.width(), plane.height());
    if plane.channels() != 1 {
        return Err(StackError::Plane { what: "plane", width, height, channels: plane.channels(), expected_width: width, expected_height: height });
    }
    if levels > MAX_LEVELS as usize {
        #[allow(clippy::cast_precision_loss)] // A setting's size, for a message.
        return Err(StackError::Setting { name: "levels", value: levels as f64, expected: "at most 10" });
    }
    let bank = wavelet.bank();
    let extended = extension(bank, levels);
    let spare = Spare::default();
    let mut low = extend(plane, Span::whole(width, height), extended, 0, &spare)?;
    let mut details = Vec::with_capacity(levels);
    for k in 0..levels {
        let (next, level) = analyze(bank, &low, 1 << k, &spare)?;
        low = next;
        details.push(level);
    }
    Ok(Decomposition { details, low, extended, bank, width, height })
}

impl Decomposition {
    /// The plane this was made from, up to the rounding of `f32` sums.
    pub fn reconstruct(&self) -> Result<Image<f32>, StackError> {
        let spare = Spare::default();
        let mut plane = self.low.clone();
        for (k, details) in self.details.iter().enumerate().rev() {
            plane = synthesize(self.bank, &plane, Some(details), 1 << k, &spare)?;
        }
        Ok(plane.crop(Rect::new(self.extended, self.extended, self.width, self.height))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The filter a 1-D impulse comes back through: `a` then `b`.
    fn through(a: Filter, b: Filter) -> Vec<(isize, f64)> {
        let mut out = std::collections::BTreeMap::new();
        for (i, &x) in a.taps.iter().enumerate() {
            for (j, &y) in b.taps.iter().enumerate() {
                *out.entry(a.first + signed(i) + b.first + signed(j)).or_insert(0.0) += f64::from(x) * f64::from(y);
            }
        }
        out.into_iter().collect()
    }

    #[test]
    fn every_bank_is_perfect_reconstruction_and_keeps_brightness() {
        for wavelet in [Wavelet::Bior22, Wavelet::Db2] {
            let bank = wavelet.bank();
            let mut sum = std::collections::BTreeMap::new();
            for (at, v) in through(bank.low, bank.synthesis_low).into_iter().chain(through(bank.high, bank.synthesis_high)) {
                *sum.entry(at).or_insert(0.0) += v;
            }
            for (at, v) in sum {
                let want = if at == 0 { 1.0 } else { 0.0 };
                assert!((v - want).abs() < 1e-6, "{wavelet:?}: G0 H0 + G1 H1 is {v} at {at}");
            }
            for filter in [bank.low, bank.synthesis_low] {
                assert!((filter.taps.iter().sum::<f32>() - 1.0).abs() < 1e-6, "{wavelet:?}: low-pass filters sum to 1");
            }
            assert!(bank.high.taps.iter().sum::<f32>().abs() < 1e-6, "{wavelet:?}: high-pass filters pass no brightness");
        }
    }

    #[test]
    fn a_reused_plane_keeps_nothing_of_its_past() {
        // Filters write into planes they don't clear, so a sample a filter missed would show as NaN here.
        let (width, height) = (37, 23);
        let plane = Image::from_fn(width, height, 1, ColorSpace::LINEAR, |x, y, _| ((x * 7 + y * 13) % 11) as f32 / 11.0).unwrap();
        let bank = Wavelet::Bior22.bank();
        let fresh = Spare::default();
        let (low, details) = analyze(bank, &plane, 2, &fresh).unwrap();
        let want = synthesize(bank, &low, Some(&details), 2, &fresh).unwrap();
        let spare = Spare::default();
        for _ in 0..16 {
            let poison = Image::from_vec(width, height, 1, ColorSpace::LINEAR, vec![f32::NAN; width * height]).unwrap();
            spare.give(poison);
        }
        let (low, details) = analyze(bank, &plane, 2, &spare).unwrap();
        let got = synthesize(bank, &low, Some(&details), 2, &spare).unwrap();
        assert_eq!(got.as_raw(), want.as_raw());
        // What must start from zeros does, and a plane of another size is no reason to fail.
        spare.give(Image::from_vec(4, 1, 1, ColorSpace::LINEAR, vec![f32::NAN; 4]).unwrap());
        assert!(spare.take(2, 2).unwrap().as_raw().iter().all(|&v| v == 0.0));
        spare.give(Image::from_vec(2, 1, 1, ColorSpace::LINEAR, vec![f32::NAN; 2]).unwrap());
        assert!(spare.take_written(64, 64).unwrap().as_raw().iter().all(|&v| v == 0.0));
    }

    #[test]
    fn shifted_samples_read_the_row_and_repeat_its_ends() {
        let row = [1.0, 2.0, 3.0];
        for (shift, want) in [(0, [1.0, 2.0, 3.0]), (1, [2.0, 3.0, 3.0]), (-1, [1.0, 1.0, 2.0]), (9, [3.0, 3.0, 3.0]), (-9, [1.0, 1.0, 1.0])] {
            let mut out = [0.0; 3];
            add_shifted::<false>(&mut out, &row, 1.0, shift);
            assert_eq!(out, want, "shift {shift}");
        }
    }
}
