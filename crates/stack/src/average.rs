//! The weighted-average method: every frame blended into every pixel, each weighted by how sharp it is there.
//!
//! # The weights
//!
//! For each frame, on each tile:
//!
//! 1. **Luminance** `Y`, linear, with the Rec. 709 coefficients (ITU-R BT.709): contrast is measured once, on luminance,
//!    and the frame's channels are blended with the same weight, so that colors stay together.
//! 2. **Laplacian energy**, `L^2` where `L` is the five-point Laplacian of `Y`, averaged over a Gaussian window of
//!    standard deviation `radius` pixels: the focus measure of shape from focus (S. K. Nayar and Y. Nakagawa, "Shape
//!    from focus", IEEE Transactions on Pattern Analysis and Machine Intelligence 16(8), 1994, whose sum-modified
//!    Laplacian sums the Laplacian's response over a window), in its "energy of Laplacian" form, one of the most
//!    reliable in S. Pertuz, D. Puig and M. A. Garcia's comparison ("Analysis of focus measure operators for
//!    shape-from-focus", Pattern Recognition 46(5), 2013). Defocus is a low-pass filter, so the frame focused at a
//!    pixel's depth has the most Laplacian energy there.
//! 3. **Power**: the energy, scaled to typical image contrast, plus a tiny floor, raised to `power`. The weights are
//!    normalized per pixel at the end, so the power sets how strongly the sharpest frame wins: 1 blends in proportion
//!    to the energy, higher values tend to picking the sharpest frame alone. Weighting each image by its contrast
//!    raised to an exponent and normalizing the weights per pixel is how T. Mertens, J. Kautz and F. Van Reeth blend
//!    differently exposed images ("Exposure fusion", Pacific Graphics 2007); here contrast is the only measure, and the
//!    blend is direct rather than through a pyramid (the pyramid method is roadmap 3.2's). The floor makes flat
//!    regions, where no frame has any detail, the plain average of the frames, which also averages their noise away.
//! 4. **Smoothing**: the weights blurred by a Gaussian of standard deviation `smoothing` pixels, so that the blend
//!    changes gradually from one frame to the next across the image instead of following the noise of the measure from
//!    pixel to pixel. A blur is linear, so smoothing every frame's weights keeps them consistent: the smoothed weights
//!    of the frames still sum to the smoothed sum.
//! 5. **Coverage**: the weights multiplied by the frame's coverage ([`Patch::coverage`]), so that a frame adds
//!    nothing where it doesn't cover the canvas. Its Laplacian is left out where it would read a pixel the frame
//!    doesn't cover (its edge against the zeros beyond it would pass for sharp detail), and samples that aren't finite
//!    count as uncovered.
//!
//! The result is `sum(w_i x_i) / sum(w_i)` per pixel over the frames `x_i`: two accumulator planes per tile, the
//! weighted sum (the frames' channels) and the sum of the weights (one channel), whatever the number of frames. A pixel
//! that no frame covers (only wide framing has them) has no weight at all: it is 0, and the stack's alpha marks it.
//!
//! # Range
//!
//! The energy is scaled by [`ENERGY_SCALE`] (a Laplacian of 0.01, fine texture in linear light), floored at
//! [`ENERGY_FLOOR`] and capped at [`ENERGY_CAP`] of that scale before the power, which is at most 4: the weights stay
//! between `1e-32` and `1e24`, which `f32` holds with room for the sums of thousands of frames of samples up to
//! [`MAX_SAMPLE`] (larger ones are clamped to it). Coverage below [`MIN_COVERAGE`] counts as none.

use florastack_core::ops::{AUTO_EXACT_MAX_SIGMA, BlurMethod, gaussian_blur_in_place, laplacian};
use florastack_core::{ColorSpace, Image, Rect};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::StackError;
use crate::method::{Method, Stacker};
use crate::source::Patch;
use crate::tiled::{Share, StatePlane, TileKernel, TileView, Tiled, Tiling};

/// The method's name, as outputs and commands record it.
pub const AVERAGE: &str = "average";
/// The Laplacian energy that counts as typical contrast: weights are computed on the energy divided by it.
pub const ENERGY_SCALE: f32 = 1e-4;
/// The least scaled energy, so that flat regions average the frames evenly.
pub const ENERGY_FLOOR: f32 = 1e-8;
/// The most scaled energy (a Laplacian of 10, far beyond any edge in linear light below 1).
pub const ENERGY_CAP: f32 = 1e6;
/// Samples are clamped to plus or minus this, so that the weighted sums can't overflow.
pub const MAX_SAMPLE: f32 = 1e6;
/// A frame's coverage below this counts as none.
pub const MIN_COVERAGE: f32 = 1e-3;
/// The largest `radius` and `smoothing`, in pixels.
pub const MAX_RADIUS: f64 = 64.0;
/// The range of `power`.
pub const POWER_RANGE: (f64, f64) = (0.5, 4.0);

/// The weighted average's settings. See the module documentation. Every field has a default, so that a preset can
/// name only some: radius 3, smoothing 8 and power 3, chosen on stacks of 20 MP camera frames. A radius of 1 measured
/// sharpness on the frames' noise and left a veil over fine detail; less smoothing left dark dots and ghost outlines in
/// a smooth background next to the subject, where frames focused elsewhere show the subject's blurred edge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AverageSettings {
    /// The radius over which sharpness is measured: the standard deviation, in pixels, of the window the Laplacian's
    /// energy is averaged over, from 0 to 64. Larger is steadier on noise and smooth surfaces, smaller follows fine
    /// detail more closely.
    #[schemars(range(min = 0.0, max = 64.0))]
    pub radius: f64,
    /// How smoothly the blend moves from one frame to the next across the image: the standard deviation, in pixels, of
    /// the blur of each frame's weights, from 0 to 64. Larger hides the seams between frames, smaller keeps edges between
    /// near and far parts crisper.
    #[schemars(range(min = 0.0, max = 64.0))]
    pub smoothing: f64,
    /// How strongly the sharpest frame wins, from 0.5 to 4: the power the sharpness is raised to before the weights
    /// are normalized. 1 blends in proportion to sharpness; higher values come closer to picking one frame per pixel.
    #[schemars(range(min = 0.5, max = 4.0))]
    pub power: f64,
}

impl Default for AverageSettings {
    fn default() -> Self {
        AverageSettings { radius: 3.0, smoothing: 8.0, power: 3.0 }
    }
}

impl AverageSettings {
    /// Checks every setting's range.
    pub fn validate(&self) -> Result<(), StackError> {
        let lengths = [("radius", self.radius), ("smoothing", self.smoothing)];
        for (name, value) in lengths {
            if !(value.is_finite() && (0.0..=MAX_RADIUS).contains(&value)) {
                return Err(StackError::Setting { name, value, expected: "from 0 to 64 pixels" });
            }
        }
        if !(self.power.is_finite() && (POWER_RANGE.0..=POWER_RANGE.1).contains(&self.power)) {
            return Err(StackError::Setting { name: "power", value: self.power, expected: "from 0.5 to 4" });
        }
        Ok(())
    }
}

impl Method for AverageSettings {
    fn name(&self) -> &'static str {
        AVERAGE
    }

    fn validate(&self) -> Result<(), StackError> {
        AverageSettings::validate(self)
    }

    fn stacker(&self, tiling: &Tiling) -> Result<Box<dyn Stacker>, StackError> {
        Ok(Box::new(Tiled::new(WeightedAverage::new(self)?, tiling.clone())))
    }

    fn reduced(&self, factor: f64) -> Self {
        let factor = if factor.is_finite() && factor > 0.0 { factor } else { 1.0 };
        AverageSettings { radius: self.radius / factor, smoothing: self.smoothing / factor, power: self.power }
    }
}

/// How far, in pixels, a Gaussian blur of standard deviation `sigma` reaches with [`BlurMethod::Auto`]: `ceil(3 sigma)`
/// for the exact kernel, and for the four box passes beyond [`AUTO_EXACT_MAX_SIGMA`], four boxes of radius about
/// `0.87 sigma` plus their extended end taps.
fn reach(sigma: f64) -> usize {
    let r = if sigma <= 0.0 {
        0.0
    } else if sigma <= AUTO_EXACT_MAX_SIGMA {
        (3.0 * sigma).ceil()
    } else {
        (3.5 * sigma).ceil() + 4.0
    };
    to_index(r)
}

/// A small non-negative float as an index.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // At most 3.5 * 64 + 4.
fn to_index(v: f64) -> usize {
    v as usize
}

/// The weighted average as a [`TileKernel`]. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct WeightedAverage {
    settings: AverageSettings,
    /// The power, as `f32`.
    power: f32,
}

/// The Rec. 709 luminance of a pixel of `c` channels: the first channel of gray images, the first three of color
/// ones. `None` when a sample isn't finite.
fn luminance(pixel: &[f32]) -> Option<f32> {
    if !pixel.iter().all(|v| v.is_finite()) {
        return None;
    }
    let y = match pixel {
        [r, g, b, ..] => 0.2126 * r + 0.7152 * g + 0.0722 * b,
        [v, ..] => *v,
        [] => 0.0,
    };
    Some(y.clamp(-MAX_SAMPLE, MAX_SAMPLE))
}

impl WeightedAverage {
    pub fn new(settings: &AverageSettings) -> Result<WeightedAverage, StackError> {
        settings.validate()?;
        #[allow(clippy::cast_possible_truncation)] // From 0.5 to 4.
        let power = settings.power as f32;
        Ok(WeightedAverage { settings: settings.clone(), power })
    }

    pub fn settings(&self) -> &AverageSettings {
        &self.settings
    }

    /// The scaled energy raised to the power.
    fn sharpen(&self, energy: f32) -> f32 {
        let e = (energy.max(0.0) / ENERGY_SCALE).min(ENERGY_CAP) + ENERGY_FLOOR;
        // Whole powers take the fast paths (powf costs a third of a 24 MP frame's time); powf gives the same for them up
        // to rounding.
        #[allow(clippy::float_cmp)]
        if self.power == 2.0 {
            e * e
        } else if self.power == 3.0 {
            e * e * e
        } else if self.power == 4.0 {
            let s = e * e;
            s * s
        } else if self.power == 1.0 {
            e
        } else {
            e.powf(self.power)
        }
    }

    /// Each pixel's weight for the frame `patch` holds: the CPU reference of the weights (see the module
    /// documentation), a one-channel image of the patch's size. Pixels within the kernel's context of the patch's edge
    /// see a clamped border.
    pub fn weights(&self, patch: &Patch) -> Result<Image<f32>, StackError> {
        let image = &patch.image;
        let (w, h, c) = (image.width(), image.height(), image.channels());
        // Luminance, and the coverage that counts: none below the minimum, nor where a sample isn't finite.
        let mut y = Image::new(w, h, 1, ColorSpace::LINEAR)?;
        let mut coverage = Image::new(w, h, 1, ColorSpace::LINEAR)?;
        let given = patch.coverage.as_ref();
        y.par_rows_mut().zip(coverage.par_rows_mut()).zip(image.par_rows()).enumerate().for_each(|(j, ((y_row, cov_row), pixels))| {
            let given_row = given.and_then(|g| g.row(j));
            for (i, ((yv, cv), pixel)) in y_row.iter_mut().zip(cov_row.iter_mut()).zip(pixels.chunks_exact(c)).enumerate() {
                let weight = given_row.and_then(|r| r.get(i)).copied().unwrap_or(1.0);
                let weight = if weight.is_finite() && weight >= MIN_COVERAGE { weight.min(1.0) } else { 0.0 };
                (*yv, *cv) = luminance(pixel).map_or((0.0, 0.0), |l| (l, weight));
            }
        });
        // The Laplacian's energy, where its five taps all fall on covered pixels.
        let mut energy = laplacian(&y)?;
        drop(y);
        let rows: Vec<&[f32]> = coverage.rows().collect();
        energy.par_rows_mut().enumerate().for_each(|(j, row)| {
            let at = |r: usize| rows.get(r).copied().unwrap_or(&[]);
            let (up, mid, down) = (at(j.saturating_sub(1)), at(j), at((j + 1).min(h - 1)));
            for (i, e) in row.iter_mut().enumerate() {
                let (left, right) = (i.saturating_sub(1), (i + 1).min(w - 1));
                let taps = [mid.get(i), mid.get(left), mid.get(right), up.get(i), down.get(i)];
                let covered = taps.iter().all(|t| t.is_some_and(|&v| v > 0.0));
                *e = if covered { *e * *e } else { 0.0 };
            }
        });
        drop(rows);
        gaussian_blur_in_place(&mut energy, self.settings.radius, BlurMethod::Auto)?;
        energy.par_rows_mut().for_each(|row| row.iter_mut().for_each(|e| *e = self.sharpen(*e)));
        gaussian_blur_in_place(&mut energy, self.settings.smoothing, BlurMethod::Auto)?;
        energy.par_rows_mut().zip(coverage.par_rows()).for_each(|(row, cov)| {
            for (e, &k) in row.iter_mut().zip(cov) {
                *e = if k > 0.0 { *e * k } else { 0.0 };
            }
        });
        Ok(energy)
    }
}

/// The weighted average's two planes: the weighted sum of the frames, and the sum of the weights.
fn planes(state: &[StatePlane]) -> Result<(&Image<f32>, &Image<f32>), StackError> {
    match state {
        [StatePlane::F32(sum), StatePlane::F32(weight)] => Ok((sum, weight)),
        _ => Err(StackError::Shape { width: 0, height: 0, channels: state.len() }),
    }
}

impl TileKernel for WeightedAverage {
    fn name(&self) -> &'static str {
        AVERAGE
    }

    fn context(&self) -> usize {
        // The Laplacian and the covered-taps test reach one pixel, then the two blurs.
        1 + reach(self.settings.radius) + reach(self.settings.smoothing)
    }

    fn state(&self, inner: Rect, channels: usize) -> Result<Vec<StatePlane>, StackError> {
        Ok(vec![
            StatePlane::F32(Image::new(inner.width, inner.height, channels, ColorSpace::LINEAR)?),
            StatePlane::F32(Image::new(inner.width, inner.height, 1, ColorSpace::LINEAR)?),
        ])
    }

    fn fold(&self, state: &mut [StatePlane], patch: &Patch, view: &TileView) -> Result<Option<Share>, StackError> {
        let weights = self.weights(patch)?;
        let [StatePlane::F32(sum), StatePlane::F32(total)] = state else {
            return Err(StackError::Shape { width: 0, height: 0, channels: state.len() });
        };
        let inner = view.inner_in_patch();
        let c = patch.image.channels();
        // The patch's rows and columns that the state keeps.
        let pixels = patch.image.rows().skip(inner.y).take(inner.height).map(|row| row.get(inner.x * c..(inner.x + inner.width) * c).unwrap_or(&[]));
        let weights = weights.rows().skip(inner.y).take(inner.height).map(|row| row.get(inner.x..inner.x + inner.width).unwrap_or(&[]));
        let rows: Vec<(&[f32], &[f32])> = pixels.zip(weights).collect();
        sum.par_rows_mut().zip(total.par_rows_mut()).zip(rows.into_par_iter()).for_each(|((sum, total), (pixels, weights))| {
            for (((s, t), pixel), &w) in sum.chunks_exact_mut(c).zip(total.iter_mut()).zip(pixels.chunks_exact(c)).zip(weights) {
                // A weight is positive only where the pixel is covered and every sample is finite.
                if w > 0.0 {
                    for (s, &v) in s.iter_mut().zip(pixel) {
                        *s += w * v.clamp(-MAX_SAMPLE, MAX_SAMPLE);
                    }
                    *t += w;
                }
            }
        });
        Ok(None)
    }

    fn covered(&self, state: &[StatePlane]) -> Result<Image<f32>, StackError> {
        let (_, total) = planes(state)?;
        let mut covered = total.clone();
        covered.par_rows_mut().for_each(|row| row.iter_mut().for_each(|t| *t = if *t > 0.0 { 1.0 } else { 0.0 }));
        Ok(covered)
    }

    fn result(&self, state: &[StatePlane], _inner: Rect, channels: usize) -> Result<Image<f32>, StackError> {
        let (sum, total) = planes(state)?;
        let mut out = sum.clone();
        let c = channels.max(1);
        out.par_rows_mut().zip(total.par_rows()).for_each(|(row, totals)| {
            for (pixel, &t) in row.chunks_exact_mut(c).zip(totals) {
                if t > 0.0 {
                    let inverse = 1.0 / t;
                    for v in pixel.iter_mut() {
                        *v *= inverse;
                    }
                } else {
                    pixel.fill(0.0);
                }
            }
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_checked_and_scale_for_previews() {
        assert!(AverageSettings::default().validate().is_ok());
        for bad in [
            AverageSettings { radius: -1.0, ..AverageSettings::default() },
            AverageSettings { smoothing: 65.0, ..AverageSettings::default() },
            AverageSettings { power: 0.25, ..AverageSettings::default() },
            AverageSettings { power: f64::NAN, ..AverageSettings::default() },
            AverageSettings { radius: f64::INFINITY, ..AverageSettings::default() },
        ] {
            assert!(matches!(bad.validate(), Err(StackError::Setting { .. })), "{bad:?}");
            assert!(WeightedAverage::new(&bad).is_err());
        }
        let quarter = AverageSettings::default().reduced(4.0);
        assert_eq!(quarter, AverageSettings { radius: 0.75, smoothing: 2.0, power: 3.0 });
        assert_eq!(AverageSettings::default().reduced(0.0), AverageSettings::default(), "a factor that isn't one changes nothing");
    }

    #[test]
    fn the_context_covers_the_filters_reach() {
        assert_eq!(reach(0.0), 0);
        assert_eq!(reach(2.0), 6);
        assert_eq!(reach(4.0), 12);
        assert_eq!(reach(8.0), 32);
        let kernel = WeightedAverage::new(&AverageSettings { radius: 2.0, smoothing: 8.0, power: 2.0 }).unwrap();
        assert_eq!(kernel.context(), 1 + 6 + 32);
    }

    #[test]
    fn weights_follow_sharpness_and_coverage() {
        // A sharp checkerboard on the left half, flat gray on the right.
        let image = Image::from_fn(64, 32, 3, ColorSpace::LINEAR, |x, y, _| if x < 32 { ((x / 2 + y / 2) % 2) as f32 * 0.5 } else { 0.25 }).unwrap();
        let kernel = WeightedAverage::new(&AverageSettings { radius: 1.0, smoothing: 1.0, power: 1.0 }).unwrap();
        let weights = kernel.weights(&Patch { image: image.clone(), coverage: None }).unwrap();
        let (sharp, flat) = (weights.get(10, 16, 0).unwrap(), weights.get(54, 16, 0).unwrap());
        assert!(sharp > 1e3 * flat && flat > 0.0, "{sharp} {flat}");
        // Uncovered pixels weigh nothing, and the edge against them isn't sharp detail.
        let coverage = Image::from_fn(64, 32, 1, ColorSpace::LINEAR, |x, _, _| if x < 48 { 1.0 } else { 0.0 }).unwrap();
        let mut zeroed = image.clone();
        for y in 0..32 {
            for x in 48..64 {
                zeroed.pixel_mut(x, y).unwrap().fill(0.0);
            }
        }
        let covered = kernel.weights(&Patch { image: zeroed, coverage: Some(coverage) }).unwrap();
        assert_eq!(covered.get(50, 16, 0), Some(0.0));
        assert!(covered.get(46, 16, 0).unwrap() < 10.0 * flat, "no false detail at the coverage edge: {}", covered.get(46, 16, 0).unwrap());
        // A sample that isn't finite makes its pixel uncovered.
        let mut broken = image;
        broken.pixel_mut(20, 10).unwrap()[1] = f32::NAN;
        let weights = kernel.weights(&Patch { image: broken, coverage: None }).unwrap();
        assert_eq!(weights.get(20, 10, 0), Some(0.0));
        assert!(weights.as_raw().iter().all(|w| w.is_finite()));
    }
}
