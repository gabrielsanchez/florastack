//! Out-of-focus frames: each frame's sharp-content fraction, the share of the stack's in-focus detail on which the frame
//! is among the sharpest, so that the frames focused where the subject isn't, common at the start and the end of a
//! stack, can be flagged for the user to exclude.
//!
//! 1. **Focus map**, once per frame, from what alignment prepares anyway: the energy of the gradient of the frame's
//!    band-pass at the working resolution (the Tenengrad focus measure, here on the band-pass: E. P. Krotkov,
//!    "Focusing", International Journal of Computer Vision 1(3), 1988; S. Pertuz, D. Puig and M. A. Garcia, "Analysis
//!    of focus measure operators for shape-from-focus", Pattern Recognition 46(5), 2013), averaged over cells of at
//!    least [`MIN_BLOCK`] x [`MIN_BLOCK`] working pixels, at most [`SIDE`] cells on the longer side. Defocus takes that
//!    energy away fast, and the band-pass leaves out the smooth ramps that a defocused edge spreads over its
//!    neighbors, which a measure on the luminance's own gradient would credit to the most defocused frames, inside a
//!    flat area next to a strong edge. The band-pass is also far less noisy than a finer measure (the Laplacian's,
//!    which on noisy synthetic stacks found focus peaks on less than a fifth as many cells and credited the
//!    high-contrast structures over the rest).
//! 2. **Comparison**, once the stack is aligned: every frame's map is sampled at the master's cells through the frame's
//!    transform, and scaled by the square of its gain. A cell has a *focus peak* when its sharpest frame has at least
//!    [`PEAK`] times the energy of the median frame: detail that some frames show sharp and most show blurred. Flat or
//!    noisy cells, which look alike in every frame, have no peak, nor has detail that every frame shows sharp; neither
//!    says anything about focus. Cells far darker than the stack's detail are left out too.
//! 3. **Fraction**: a frame is in focus on a peaked cell when its energy there is at least [`NEAR`] of the sharpest
//!    frame's; its sharp-content fraction is the share of the peaked cells it covers on which it is. A frame focused in
//!    front of the subject or behind it is in focus on none.
//!
//! Fractions need at least three frames, and focus peaks on at least [`MIN_PEAKED_SHARE`] of the cells (and
//! [`MIN_PEAKED`] of them): frames that don't differ in focus, such as a stack moved but not refocused, show peaks
//! only where noise or dust happen to differ, and nothing can be told from those. Otherwise the fractions are `None`
//! and no frame is flagged. A frame is flagged as out of focus when its fraction is less than
//! [`AlignSettings::min_sharp_fraction`], 0.2% by default: next to nothing, so that a frame whose only sharp detail is
//! small, such as the tip of an antenna, isn't flagged. The flag never rejects a frame; the user decides. Cells are
//! visited in order and every sum is sequential, so the fractions are deterministic.
//!
//! [`AlignSettings::min_sharp_fraction`]: crate::AlignSettings::min_sharp_fraction

use florastack_core::{Affine, ColorSpace, Image};
use rayon::prelude::*;

use crate::error::AlignError;
use crate::prepare::Mapping;

/// The focus map's longer side is at most this many cells.
const SIDE: usize = 128;
/// A cell averages at least this many working pixels along each side, so that noise averages out.
const MIN_BLOCK: usize = 4;
/// A frame is in focus on a cell when its energy is at least this fraction of the sharpest frame's.
pub(crate) const NEAR: f64 = 0.5;
/// A cell has a focus peak when its sharpest frame has at least this many times the median frame's energy.
pub(crate) const PEAK: f64 = 4.0;
/// Cells whose sharpest frame has less than this fraction of the energy of the stack's detail (the 99th percentile of
/// the cells' sharpest energies) are left out: dark, featureless parts of the scene.
const FLOOR: f64 = 1e-3;
/// The fewest peaked cells that fractions are told from, and the smallest share of the cells. On synthetic stacks,
/// frames defocused through the scene have peaks on about 70% of the cells, and frames moved but not defocused on at
/// most 0.5%, where noise and dust happen to differ.
pub(crate) const MIN_PEAKED: usize = 16;
pub(crate) const MIN_PEAKED_SHARE: f64 = 0.02;

/// A frame's focus map: the gradient energy of its band-pass, by cell. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FocusMap {
    /// From the map's cells to the frame's pixels.
    mapping: Mapping,
    energy: Image<f32>,
}

impl FocusMap {
    /// The focus map of a level's field (the band-pass, its x and y derivatives and its Laplacian, interleaved, as
    /// `prepare` makes it), whose pixels map to the frame's by `mapping`.
    pub(crate) fn new(field: &Image<f32>, mapping: Mapping) -> Result<FocusMap, AlignError> {
        let (w, h) = (field.width(), field.height());
        let k = w.max(h).div_ceil(SIDE).max(MIN_BLOCK);
        let (mw, mh) = ((w / k).max(1), (h / k).max(1));
        let mut energy = Image::new(mw, mh, 1, ColorSpace::LINEAR)?;
        let channels = field.channels();
        energy.par_rows_mut().enumerate().for_each(|(cy, out)| {
            let mut sums = vec![0.0_f64; out.len()];
            for row in (cy * k..cy * k + k).filter_map(|y| field.row(y)) {
                for (cx, sum) in sums.iter_mut().enumerate() {
                    let Some(block) = row.get(cx * k * channels..(cx * k + k) * channels) else { continue };
                    for pixel in block.chunks_exact(channels) {
                        if let [_, gx, gy, ..] = *pixel {
                            let e = f64::from(gx) * f64::from(gx) + f64::from(gy) * f64::from(gy);
                            if e.is_finite() {
                                *sum += e;
                            }
                        }
                    }
                }
            }
            let area = (k * k) as f64;
            for (o, sum) in out.iter_mut().zip(sums) {
                *o = to_f32(sum / area);
            }
        });
        Ok(FocusMap { mapping: mapping.blocks(k), energy })
    }
}

/// A mean of squared samples, back to the `f32` of the samples' type.
#[allow(clippy::cast_possible_truncation)] // A mean of squared band-pass samples: far inside f32's range.
fn to_f32(v: f64) -> f32 {
    v as f32
}

/// What [`sharp_fractions`] found.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Sharpness {
    /// Each frame's sharp-content fraction, or `None` when it can't be told.
    pub fractions: Vec<Option<f64>>,
    /// How many cells had a focus peak, of how many compared.
    pub peaked: usize,
    pub cells: usize,
}

/// How a frame's map is read at the master's cells: master pixels to the map's cells, the map, and the square of the
/// frame's gain.
struct Sampler<'a> {
    to_cells: Affine,
    energy: &'a Image<f32>,
    gain2: f64,
}

impl Sampler<'_> {
    /// The frame's energy at master pixel `p`, if the frame covers it.
    fn at(&self, p: (f64, f64)) -> Option<f64> {
        let (u, v) = self.to_cells.apply(p.0, p.1);
        bilinear(self.energy, u, v).map(|e| e * self.gain2)
    }
}

/// Each frame's sharp-content fraction (see the module documentation), from the frames' focus maps (`None` for a frame
/// without one), their transforms from the master's pixels and their gains; the master's cells are the ones compared.
pub(crate) fn sharp_fractions(maps: &[Option<FocusMap>], transforms: &[Affine], gains: &[f64], master: usize) -> Sharpness {
    let n = maps.len();
    let nothing = Sharpness { fractions: vec![None; n], peaked: 0, cells: 0 };
    let Some(Some(grid)) = maps.get(master) else { return nothing };
    if n < 3 || transforms.len() != n || gains.len() != n {
        return nothing;
    }
    let samplers: Vec<Option<Sampler>> = maps
        .iter()
        .zip(transforms.iter().zip(gains))
        .map(|(map, (transform, &gain))| {
            let map = map.as_ref()?;
            let from_frame = map.mapping.to_frame().inverse()?;
            let gain = if gain.is_finite() && gain > 0.0 { gain } else { 1.0 };
            Some(Sampler { to_cells: transform.then(&from_frame), energy: &map.energy, gain2: gain * gain })
        })
        .collect();
    let to_master = grid.mapping.to_frame();
    let (mw, mh) = (grid.energy.width(), grid.energy.height());
    // The outermost cells see the clamped borders of the band-pass; leave them out when there are others.
    let inner = |len: usize| if len > 2 { 1..len - 1 } else { 0..len };
    let cells: Vec<(f64, f64)> = inner(mh).flat_map(|y| inner(mw).map(move |x| to_master.apply(x as f64, y as f64))).collect();

    let mut values: Vec<Option<f64>> = vec![None; n];
    let sample = |p: (f64, f64), values: &mut Vec<Option<f64>>| {
        for (value, sampler) in values.iter_mut().zip(&samplers) {
            *value = sampler.as_ref().and_then(|s| s.at(p)).filter(|v| v.is_finite());
        }
    };
    // The sharpest energy of every cell, for the floor.
    let mut sharpest: Vec<f64> = cells
        .iter()
        .map(|&p| {
            sample(p, &mut values);
            values.iter().flatten().copied().fold(0.0, f64::max)
        })
        .collect();
    if sharpest.is_empty() {
        return nothing;
    }
    let at = ((sharpest.len() - 1) * 99) / 100;
    let (_, &mut detail, _) = sharpest.select_nth_unstable_by(at, f64::total_cmp);
    let floor = FLOOR * detail;

    let (mut in_focus, mut covered, mut peaked) = (vec![0_usize; n], vec![0_usize; n], 0);
    let mut sorted = Vec::with_capacity(n);
    for &p in &cells {
        sample(p, &mut values);
        sorted.clear();
        sorted.extend(values.iter().flatten().copied());
        if sorted.len() < 3 {
            continue;
        }
        let max = sorted.iter().copied().fold(0.0, f64::max);
        // The lower median, so that of four frames, two sharp and two blurred, the blurred ones set the reference.
        let middle = (sorted.len() - 1) / 2;
        let (_, &mut median, _) = sorted.select_nth_unstable_by(middle, f64::total_cmp);
        if !(max > floor && max > 0.0 && max >= PEAK * median) {
            continue;
        }
        peaked += 1;
        for (i, v) in values.iter().enumerate() {
            if let Some(v) = *v {
                if let Some(c) = covered.get_mut(i) {
                    *c += 1;
                }
                if v >= NEAR * max
                    && let Some(f) = in_focus.get_mut(i)
                {
                    *f += 1;
                }
            }
        }
    }
    let cell_count = cells.len();
    if peaked < MIN_PEAKED || (peaked as f64) < MIN_PEAKED_SHARE * cell_count as f64 {
        return Sharpness { peaked, cells: cell_count, ..nothing };
    }
    let fractions = in_focus.iter().zip(&covered).map(|(&f, &c)| (c > 0).then(|| f as f64 / c as f64)).collect();
    Sharpness { fractions, peaked, cells: cell_count }
}

/// `plane` at `(u, v)`, bilinearly, if the point lies inside its pixel centers.
fn bilinear(plane: &Image<f32>, u: f64, v: f64) -> Option<f64> {
    let (w, h) = (plane.width(), plane.height());
    if !(u >= 0.0 && v >= 0.0 && u <= (w - 1) as f64 && v <= (h - 1) as f64) {
        return None;
    }
    let (fu, fv) = (u.floor(), v.floor());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Inside the plane: non-negative and small.
    let (x, y) = (fu as usize, fv as usize);
    let at = |x: usize, y: usize| plane.get(x.min(w - 1), y.min(h - 1), 0).map(f64::from);
    let (tu, tv) = (u - fu, v - fv);
    let upper = at(x, y)? + tu * (at(x + 1, y)? - at(x, y)?);
    let lower = at(x, y + 1)? + tu * (at(x + 1, y + 1)? - at(x, y + 1)?);
    Some(upper + tv * (lower - upper))
}

#[cfg(test)]
mod tests {
    use florastack_core::ops::{BlurMethod, gaussian_blur_in_place};
    use florastack_core::testutil::scene;

    use super::*;
    use crate::prepare::Prepared;

    fn map(image: &Image<f32>) -> FocusMap {
        Prepared::new(image, 2048, false).unwrap().focus
    }

    #[test]
    fn maps_have_at_most_128_cells_of_at_least_4_pixels() {
        let (sharp, _) = scene::textured(600, 300, 1).unwrap();
        let m = map(&sharp);
        assert_eq!((m.energy.width(), m.energy.height()), (120, 60), "cells of 5 x 5");
        let small = map(&sharp.crop(florastack_core::Rect::new(0, 0, 40, 20)).unwrap());
        assert_eq!((small.energy.width(), small.energy.height()), (10, 5), "cells of 4 x 4");
        // A cell's center: cell 0 of 5 x 5 pixels is centered on pixel 2.
        assert_eq!(m.mapping.to_frame().apply(0.0, 1.0), (2.0, 7.0));
    }

    #[test]
    fn blurred_frames_have_no_sharp_content() {
        // The same scene sharp, a little blurred, and blurred far: the far ones are never among the sharpest.
        let (sharp, _) = scene::textured(240, 180, 2).unwrap();
        let blurred = |sigma: f64| {
            let mut b = sharp.clone();
            gaussian_blur_in_place(&mut b, sigma, BlurMethod::Exact).unwrap();
            b
        };
        let frames = [sharp.clone(), blurred(0.5), blurred(4.0), blurred(6.0)];
        let maps: Vec<Option<FocusMap>> = frames.iter().map(|f| Some(map(f))).collect();
        let transforms = vec![Affine::IDENTITY; 4];
        let found = sharp_fractions(&maps, &transforms, &[1.0; 4], 0);
        assert!(found.peaked >= MIN_PEAKED && found.peaked * 2 > found.cells, "{found:?}");
        let f: Vec<f64> = found.fractions.iter().map(|f| f.unwrap()).collect();
        assert!(f[0] > 0.95 && f[1] > 0.5, "{f:?}");
        assert!(f[2] < 0.01 && f[3] < 0.01, "{f:?}");
        // Two frames can't be judged, and nor can frames that are all alike.
        assert_eq!(sharp_fractions(&maps[..2], &transforms[..2], &[1.0; 2], 0).fractions, [None, None]);
        let same: Vec<Option<FocusMap>> = (0..4).map(|_| Some(map(&sharp))).collect();
        assert_eq!(sharp_fractions(&same, &transforms, &[1.0; 4], 0).fractions, [None; 4], "no focus peak anywhere");
        // A frame without a map, or with a transform that can't be used, is left out, not a crash.
        let mut partial = maps.clone();
        partial[3] = None;
        let bad = [Affine::IDENTITY, Affine::IDENTITY, Affine { m: [[f64::NAN; 3]; 2] }, Affine::IDENTITY];
        let found = sharp_fractions(&partial, &bad, &[1.0, f64::NAN, 1.0, 1.0], 0);
        assert_eq!((found.fractions[2], found.fractions[3]), (None, None), "{found:?}");
    }
}
