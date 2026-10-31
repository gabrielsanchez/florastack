//! A stroke's record ([`Stroke`]): what it was painted with and the pointer's path, everything its replay needs. The
//! brush's dabs are placed along the path here ([`Stroke::dabs`]), so that replay depends on the record alone.

use florastack_core::Rect;
use florastack_project::{FrameId, OutputId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::RetouchError;

/// The version of the records this crate writes and renders.
pub const STROKE_VERSION: u32 = 1;
/// The most pointer samples a stroke holds.
pub const MAX_SAMPLES: usize = 100_000;
/// The most dabs a stroke's path may make.
pub const MAX_DABS: usize = 2_000_000;
/// The brush's smallest and largest diameter, in pixels of the output.
pub const MIN_SIZE: f64 = 1.0;
pub const MAX_SIZE: f64 = 4000.0;
/// The dabs' spacing along the path, as a share of their radius.
const SPACING: f64 = 0.15;
/// The smallest radius a dab has, however light the pressure, in pixels.
const MIN_RADIUS: f64 = 0.5;

/// A brush. See the crate documentation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Brush {
    /// Paints the source's detail, its brightness and color matched to the output's, with a hard edge where there is
    /// fine detail and the brush's own hardness elsewhere.
    #[default]
    Details,
    /// Paints the source as it is.
    Pixels,
    /// Paints the source only where it is darker than the result.
    Darken,
    /// Paints the source only where it is lighter than the result.
    Lighten,
    /// Brings back the output as it was before any retouching.
    Eraser,
}

impl Brush {
    /// Whether it paints from a source.
    pub fn needs_source(self) -> bool {
        self != Brush::Eraser
    }
}

/// Where a brush paints from: a frame of the stack, warped onto the output's canvas, or another output of the same
/// size (a slab, or a stack by another method).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Source {
    Frame(FrameId),
    Output(OutputId),
}

/// One stroke: the brush, its settings, its source and the pointer's path, in pixels of the output. Replaying the same
/// record over the same images gives the same pixels, bit for bit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Stroke {
    /// How the record is rendered: 1, this crate's [`STROKE_VERSION`].
    #[serde(default = "version")]
    pub version: u32,
    pub brush: Brush,
    /// Where it paints from: `{"frame": id}` or `{"output": id}`; none for the eraser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// The brush's diameter, in pixels of the output, from 1 to 4000.
    #[schemars(range(min = 1.0, max = 4000.0))]
    pub size: f64,
    /// How much of the brush, from its center out, paints fully before its edge fades, from 0 (a soft edge all the
    /// way) to 1 (a hard edge).
    #[schemars(range(min = 0.0, max = 1.0))]
    pub hardness: f64,
    /// How strongly it paints, from 0 to 1.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub opacity: f64,
    /// The pointer's path: `[x, y, pressure]` samples, in pixels of the output (from its top left corner) and pressure
    /// from 0 to 1 (1 without a pen), at most 100000. Pressure scales the brush's size.
    #[schemars(length(min = 1, max = 100_000))]
    pub samples: Vec<[f64; 3]>,
}

fn version() -> u32 {
    STROKE_VERSION
}

/// A dab of the brush: its center and radius, in pixels of the output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dab {
    pub x: f64,
    pub y: f64,
    pub radius: f64,
}

/// `v` rounded to a multiple of `1 / per`.
fn round_to(v: f64, per: f64) -> f64 {
    (v * per).round() / per
}

impl Stroke {
    /// Checks the record: its version, settings and samples, and that its brush and source go together.
    pub fn validate(&self) -> Result<(), RetouchError> {
        let bad = |what: &'static str, reason: &str| Err(RetouchError::Stroke { what, reason: reason.to_owned() });
        if self.version != STROKE_VERSION {
            return bad("version", &format!("is {}, but only version {STROKE_VERSION} is known", self.version));
        }
        if !(self.size.is_finite() && (MIN_SIZE..=MAX_SIZE).contains(&self.size)) {
            return bad("size", "must be from 1 to 4000 pixels");
        }
        for (what, v) in [("hardness", self.hardness), ("opacity", self.opacity)] {
            if !(v.is_finite() && (0.0..=1.0).contains(&v)) {
                return bad(what, "must be from 0 to 1");
            }
        }
        if self.samples.is_empty() || self.samples.len() > MAX_SAMPLES {
            return bad("samples", "must hold from 1 to 100000 samples");
        }
        if self.samples.iter().any(|&[x, y, p]| !(x.is_finite() && y.is_finite() && p.is_finite() && (0.0..=1.0).contains(&p))) {
            return bad("samples", "must be finite, with a pressure from 0 to 1");
        }
        match (self.brush.needs_source(), self.source) {
            (true, None) => bad("source", "is needed by every brush but the eraser"),
            (false, Some(_)) => bad("source", "isn't taken by the eraser"),
            _ => Ok(()),
        }
    }

    /// Checks the record for an output of `width` x `height` pixels: [`Stroke::validate`], and a path that stays
    /// within a brush's size of the output and makes at most [`MAX_DABS`] dabs.
    pub fn validate_for(&self, width: usize, height: usize) -> Result<(), RetouchError> {
        self.validate()?;
        let (w, h) = (width as f64, height as f64);
        if self.samples.iter().any(|&[x, y, _]| x < -self.size || y < -self.size || x > w + self.size || y > h + self.size) {
            return Err(RetouchError::Stroke { what: "samples", reason: format!("must lie within the brush's size of the {width}x{height} output") });
        }
        // The path's length over the smallest step bounds the dabs.
        let length: f64 = self.samples.windows(2).map(|p| (p[1][0] - p[0][0]).hypot(p[1][1] - p[0][1])).sum();
        if length / (SPACING * MIN_RADIUS) > MAX_DABS as f64 {
            return Err(RetouchError::Stroke { what: "samples", reason: "make too long a path".to_owned() });
        }
        Ok(())
    }

    /// The record as it is stored and painted: samples rounded to a hundredth of a pixel and pressures to a
    /// thousandth, settings to a thousandth, so that what is painted live and what is replayed from the project
    /// file are the same numbers.
    #[must_use]
    pub fn quantized(mut self) -> Stroke {
        for s in &mut self.samples {
            *s = [round_to(s[0], 100.0), round_to(s[1], 100.0), round_to(s[2], 1000.0)];
        }
        self.size = round_to(self.size, 1000.0);
        self.hardness = round_to(self.hardness, 1000.0);
        self.opacity = round_to(self.opacity, 1000.0);
        self
    }

    /// The radius of a dab at `pressure`.
    fn radius(&self, pressure: f64) -> f64 {
        (self.size / 2.0 * pressure).max(MIN_RADIUS)
    }

    /// The brush's dabs along the path: one at the first sample, then one every [`SPACING`] of the radius of the dab
    /// before, with the position and pressure interpolated between samples. A dab depends only on the samples up to
    /// it, so a path's first dabs stay the same as samples are added.
    pub fn dabs(&self) -> Vec<Dab> {
        let Some(&[x0, y0, p0]) = self.samples.first() else { return Vec::new() };
        let mut dabs = vec![Dab { x: x0, y: y0, radius: self.radius(p0) }];
        // The distance travelled since the last dab.
        let mut travelled = 0.0;
        for pair in self.samples.windows(2) {
            let ([ax, ay, ap], [bx, by, bp]) = (pair[0], pair[1]);
            let length = (bx - ax).hypot(by - ay);
            if length <= 0.0 {
                continue;
            }
            let mut along = 0.0;
            loop {
                let step = SPACING * dabs.last().map_or(MIN_RADIUS, |d| d.radius);
                let need = step - travelled;
                if along + need > length {
                    travelled += length - along;
                    break;
                }
                along += need;
                travelled = 0.0;
                let t = along / length;
                let pressure = ap + (bp - ap) * t;
                dabs.push(Dab { x: ax + (bx - ax) * t, y: ay + (by - ay) * t, radius: self.radius(pressure) });
                if dabs.len() > MAX_DABS {
                    return dabs;
                }
            }
        }
        dabs
    }

    /// The pixels `dabs` reach, plus `margin` around them, within a `width` x `height` output; `None` when none.
    pub fn reach(dabs: &[Dab], margin: f64, width: usize, height: usize) -> Option<Rect> {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for d in dabs {
            let r = d.radius + margin;
            (x0, y0, x1, y1) = (x0.min(d.x - r), y0.min(d.y - r), x1.max(d.x + r), y1.max(d.y + r));
        }
        clip_rect(x0, y0, x1, y1, width, height)
    }
}

/// The pixels whose centers can lie within `[x0, x1] x [y0, y1]`, within a `width` x `height` image.
pub(crate) fn clip_rect(x0: f64, y0: f64, x1: f64, y1: f64, width: usize, height: usize) -> Option<Rect> {
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) || x1 < x0 || y1 < y0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped to the image first.
    let px = |v: f64, max: usize| v.clamp(0.0, max as f64) as usize;
    let (left, top) = (px(x0.floor(), width), px(y0.floor(), height));
    let (right, bottom) = (px(x1.ceil(), width), px(y1.ceil(), height));
    (right > left && bottom > top).then(|| Rect::new(left, top, right - left, bottom - top))
}

/// The rectangle that holds both `a` and `b`.
pub fn union(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let (x, y) = (a.x.min(b.x), a.y.min(b.y));
            let right = (a.x + a.width).max(b.x + b.width);
            let bottom = (a.y + a.height).max(b.y + b.height);
            Some(Rect::new(x, y, right - x, bottom - y))
        }
        (a, None) => a,
        (None, b) => b,
    }
}

/// The overlap of `a` and `b`, when they overlap.
pub fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let (x, y) = (a.x.max(b.x), a.y.max(b.y));
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    (right > x && bottom > y).then(|| Rect::new(x, y, right - x, bottom - y))
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Exact dab positions, and a count of them.
mod tests {
    use super::*;

    fn stroke(samples: Vec<[f64; 3]>) -> Stroke {
        Stroke { version: 1, brush: Brush::Eraser, source: None, size: 20.0, hardness: 0.5, opacity: 1.0, samples }
    }

    #[test]
    fn dabs_are_spaced_along_the_path_and_the_first_ones_stay_as_it_grows() {
        let s = stroke(vec![[0.0, 0.0, 1.0], [30.0, 0.0, 1.0], [30.0, 40.0, 1.0]]);
        let dabs = s.dabs();
        // A radius of 10 and a spacing of 1.5 pixels, over 70 pixels of path.
        assert_eq!(dabs.len(), 1 + (70.0f64 / 1.5).floor() as usize);
        assert!(dabs.windows(2).all(|d| ((d[1].x - d[0].x).hypot(d[1].y - d[0].y) - 1.5).abs() < 1e-9 || d[0].x == 30.0 || d[1].x == 30.0));
        let shorter = stroke(s.samples[..2].to_vec()).dabs();
        assert_eq!(dabs[..shorter.len()], shorter[..], "adding samples doesn't move the dabs before them");
        let light = stroke(vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.5]]).dabs();
        assert_eq!(light[0].radius, MIN_RADIUS, "no pressure still paints a pixel");
    }

    #[test]
    fn records_are_checked() {
        assert!(stroke(vec![[1.0, 2.0, 1.0]]).validate().is_ok());
        let mut bad = stroke(vec![[f64::NAN, 2.0, 1.0]]);
        assert!(bad.validate().is_err());
        bad.samples = vec![[1.0, 1.0, 1.5]];
        assert!(bad.validate().is_err(), "pressure beyond 1");
        bad.samples = Vec::new();
        assert!(bad.validate().is_err(), "no samples");
        let mut s = stroke(vec![[1.0, 1.0, 1.0]]);
        s.size = 0.5;
        assert!(s.validate().is_err());
        s.size = 10.0;
        s.brush = Brush::Pixels;
        assert!(s.validate().is_err(), "a source is needed");
        s.version = 2;
        assert!(s.validate().is_err());
        assert!(stroke(vec![[1e6, 1.0, 1.0]]).validate_for(100, 100).is_err(), "far outside the output");
        let zigzag: Vec<[f64; 3]> = (0..100_000).map(|i| [if i % 2 == 0 { 0.0 } else { 100.0 }, 0.0, 1.0]).collect();
        assert!(stroke(zigzag).validate_for(100, 100).is_err(), "too many dabs");
    }

    #[test]
    fn quantizing_rounds_the_record_once_and_for_all() {
        let s = stroke(vec![[1.23456, 7.891, 0.12345]]).quantized();
        assert_eq!(s.samples[0], [1.23, 7.89, 0.123]);
        assert_eq!(s.clone().quantized(), s, "quantizing again changes nothing");
    }

    #[test]
    fn rectangles_join_and_overlap() {
        let (a, b) = (Rect::new(0, 0, 10, 10), Rect::new(5, 5, 10, 10));
        assert_eq!(union(Some(a), Some(b)), Some(Rect::new(0, 0, 15, 15)));
        assert_eq!(intersect(a, b), Some(Rect::new(5, 5, 5, 5)));
        assert_eq!(intersect(a, Rect::new(10, 0, 5, 5)), None);
        assert_eq!(clip_rect(-5.0, -5.0, 3.2, 2.0, 10, 10), Some(Rect::new(0, 0, 4, 2)));
        assert_eq!(clip_rect(20.0, 20.0, 30.0, 30.0, 10, 10), None);
    }
}
