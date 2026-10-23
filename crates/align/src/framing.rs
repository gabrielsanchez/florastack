//! Framing the output: which part of the scene the stacked image shows, and its pixel grid, a [`Canvas`].
//!
//! A frame's field of view, in the master's pixels, is the quadrilateral that the inverse of its transform maps its
//! own extent to. [`Framing`] chooses the output's from them:
//!
//! - **Narrow**, the default: the field of view of the frame at the stack's narrow end. Focus breathing makes the frames
//!   toward the other end see more of the scene, so every frame covers it, up to the small shifts between frames. When
//!   the stack is processed from its narrow end, as it is by default ([`crate::ProcessingOrder`]), that frame is the
//!   master and the canvas is the master's own pixel grid.
//! - **Wide**: everything some frame sees, at the master's pixel scale: the bounding box of the union of the fields of
//!   view. Its edges are seen only by the frames toward the wide end, and each frame's [`Coverage`](crate::Coverage)
//!   marks the pixels it has nothing for, so that stacking leaves the frame out there instead of smearing its edge
//!   pixels across them, the streaks of a warp that clamps at the frame's border.
//! - **Common**: only what every frame sees, at the master's pixel scale: the largest rectangle of whole pixels inside
//!   the intersection of the fields of view, a convex polygon (clipped as I. E. Sutherland and G. W. Hodgman,
//!   "Reentrant polygon clipping", Communications of the ACM 17(1), 1974, clip polygons), so that every frame covers
//!   every output pixel with samples between its own pixel centers.
//!
//! Wide and common framings take the frames whose alignment was accepted; a rejected frame's transform is only its
//! reference's. A canvas more than [`MAX_FACTOR`] times the frames' size on a side is an error, since only wrong
//! alignments make one.

use florastack_core::Affine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::AlignError;

/// A canvas may be at most this many times the frames' size on each side.
pub const MAX_FACTOR: usize = 4;

/// The output's field of view. See the module documentation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Framing {
    /// Crop to the field of view of the stack's narrow end, which every frame covers.
    #[default]
    Narrow,
    /// Keep the widest field of view; what a frame doesn't cover is marked in its coverage mask.
    Wide,
    /// Crop to the region that every frame covers once aligned.
    Common,
}

impl Framing {
    pub const ALL: [Framing; 3] = [Framing::Narrow, Framing::Wide, Framing::Common];

    /// The framing's name, as settings and commands spell it.
    pub fn name(self) -> &'static str {
        match self {
            Framing::Narrow => "narrow",
            Framing::Wide => "wide",
            Framing::Common => "common",
        }
    }
}

/// The output's pixel grid: `width` x `height` pixels, whose pixel `(x, y)` is the master frame's point
/// `to_master.apply(x, y)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    /// From the canvas's pixels to the master frame's.
    pub to_master: Affine,
}

impl Canvas {
    /// The master frame's own grid.
    pub fn master(width: usize, height: usize) -> Canvas {
        Canvas { width, height, to_master: Affine::IDENTITY }
    }

    /// From the canvas's pixels to those of a frame whose alignment is `transform` (from the master's pixels to the
    /// frame's): what a warp onto the canvas samples the frame at.
    pub fn to_frame(&self, transform: &Affine) -> Affine {
        self.to_master.then(transform)
    }

    /// The canvas that `framing` gives frames of `width` x `height` pixels whose transforms from the master's pixels
    /// to theirs are `transforms`. `narrow` is the index in `transforms` of the frame at the stack's narrow end, whose
    /// field of view [`Framing::Narrow`] keeps; without it, the master's.
    pub fn new(framing: Framing, transforms: &[Affine], width: usize, height: usize, narrow: Option<usize>) -> Result<Canvas, AlignError> {
        if width == 0 || height == 0 {
            return Err(AlignError::CanvasSize { width, height, frame_width: width, frame_height: height });
        }
        let inverses = transforms
            .iter()
            .enumerate()
            .map(|(index, t)| {
                if t.m.iter().flatten().all(|v| v.is_finite()) {
                    t.inverse().ok_or(AlignError::Transform { index })
                } else {
                    Err(AlignError::Transform { index })
                }
            })
            .collect::<Result<Vec<Affine>, AlignError>>()?;
        if inverses.is_empty() {
            return Ok(Canvas::master(width, height));
        }
        let canvas = match framing {
            Framing::Narrow => match narrow {
                None => Canvas::master(width, height),
                Some(index) => {
                    let to_master = *inverses.get(index).ok_or(AlignError::Transform { index })?;
                    Canvas { width, height, to_master }
                }
            },
            Framing::Wide => wide(&inverses, width, height)?,
            Framing::Common => common(&inverses, width, height)?,
        };
        if canvas.width > MAX_FACTOR.saturating_mul(width) || canvas.height > MAX_FACTOR.saturating_mul(height) {
            return Err(AlignError::CanvasSize { width: canvas.width, height: canvas.height, frame_width: width, frame_height: height });
        }
        Ok(canvas)
    }
}

/// The corners of a `width` x `height` frame's extent, from `-0.5` to `side - 0.5`, or, `centers`, of its pixel
/// centers, from 0 to `side - 1`; in order around it.
fn corners(width: usize, height: usize, centers: bool) -> [(f64, f64); 4] {
    let (lo, w, h) = if centers { (0.0, (width - 1) as f64, (height - 1) as f64) } else { (-0.5, width as f64 - 0.5, height as f64 - 0.5) };
    [(lo, lo), (w, lo), (w, h), (lo, h)]
}

/// `v` as a whole number of pixels, if it is one a canvas can have.
#[allow(clippy::cast_possible_truncation)] // Checked to be a small whole number first.
fn whole(v: f64) -> Option<i64> {
    (v.is_finite() && v.abs() < 1e12).then_some(v as i64)
}

/// The canvas of the pixels from `(x0, y0)` to `(x1, y1)`, inclusive, of the master's grid.
fn rectangle(x0: i64, y0: i64, x1: i64, y1: i64, width: usize, height: usize) -> Result<Canvas, AlignError> {
    let side = |lo: i64, hi: i64| usize::try_from(hi.saturating_sub(lo).saturating_add(1)).ok().filter(|&s| s > 0);
    let (Some(w), Some(h)) = (side(x0, x1), side(y0, y1)) else {
        return Err(AlignError::NoCommonArea);
    };
    if w > MAX_FACTOR.saturating_mul(width) || h > MAX_FACTOR.saturating_mul(height) {
        return Err(AlignError::CanvasSize { width: w, height: h, frame_width: width, frame_height: height });
    }
    Ok(Canvas { width: w, height: h, to_master: Affine::translation(x0 as f64, y0 as f64) })
}

/// The bounding box of the frames' fields of view: every pixel of the master's grid whose center lies inside it.
fn wide(inverses: &[Affine], width: usize, height: usize) -> Result<Canvas, AlignError> {
    let (mut lo, mut hi) = ((f64::INFINITY, f64::INFINITY), (f64::NEG_INFINITY, f64::NEG_INFINITY));
    for inverse in inverses {
        for (x, y) in corners(width, height, false) {
            let (u, v) = inverse.apply(x, y);
            lo = (lo.0.min(u), lo.1.min(v));
            hi = (hi.0.max(u), hi.1.max(v));
        }
    }
    let bad = || AlignError::CanvasSize { width: usize::MAX, height: usize::MAX, frame_width: width, frame_height: height };
    // Pixels whose centers are strictly inside: a center on the boundary has nothing of any frame on one side.
    let (x0, y0) = (whole(lo.0.floor() + 1.0).ok_or_else(bad)?, whole(lo.1.floor() + 1.0).ok_or_else(bad)?);
    let (x1, y1) = (whole(hi.0.ceil() - 1.0).ok_or_else(bad)?, whole(hi.1.ceil() - 1.0).ok_or_else(bad)?);
    rectangle(x0, y0, x1, y1, width, height)
}

/// The largest rectangle of whole pixels inside every frame's field of view (of its pixel centers).
fn common(inverses: &[Affine], width: usize, height: usize) -> Result<Canvas, AlignError> {
    let quad = |inverse: &Affine| corners(width, height, true).map(|(x, y)| inverse.apply(x, y)).to_vec();
    let mut polygon = inverses.first().map(quad).unwrap_or_default();
    for inverse in inverses.iter().skip(1) {
        polygon = clip(&polygon, &quad(inverse));
        if polygon.len() < 3 {
            return Err(AlignError::NoCommonArea);
        }
    }
    largest_rectangle(&polygon, width, height)
}

/// Twice the signed area of a polygon.
fn area2(polygon: &[(f64, f64)]) -> f64 {
    polygon.iter().zip(polygon.iter().cycle().skip(1)).map(|(a, b)| a.0 * b.1 - b.0 * a.1).sum()
}

/// `subject` clipped by the convex polygon `clip` (Sutherland and Hodgman): what of it lies inside every edge.
fn clip(subject: &[(f64, f64)], clip: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let orientation = area2(clip).signum();
    if orientation == 0.0 || !orientation.is_finite() {
        return Vec::new();
    }
    let mut out = subject.to_vec();
    for (&a, &b) in clip.iter().zip(clip.iter().cycle().skip(1)) {
        let side = |p: (f64, f64)| orientation * ((b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0));
        let input = std::mem::take(&mut out);
        for (&p, &q) in input.iter().zip(input.iter().cycle().skip(1)) {
            let (sp, sq) = (side(p), side(q));
            if sp >= 0.0 {
                out.push(p);
            }
            if (sp >= 0.0) != (sq >= 0.0) {
                let t = sp / (sp - sq);
                out.push((p.0 + t * (q.0 - p.0), p.1 + t * (q.1 - p.1)));
            }
        }
        if out.is_empty() {
            break;
        }
    }
    out
}

/// The horizontal chord of a convex polygon at height `y`: the whole pixels `x` from the first to the last whose
/// centers `(x, y)` lie inside it, if any.
fn chord(polygon: &[(f64, f64)], y: f64) -> Option<(i64, i64)> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (&p, &q) in polygon.iter().zip(polygon.iter().cycle().skip(1)) {
        if (p.1 - y) * (q.1 - y) > 0.0 {
            continue;
        }
        let dy = q.1 - p.1;
        // A horizontal edge at this height adds both its ends.
        let xs = if dy.abs() < 1e-12 { [p.0, q.0] } else { [p.0 + (y - p.1) / dy * (q.0 - p.0); 2] };
        for x in xs {
            lo = lo.min(x);
            hi = hi.max(x);
        }
    }
    // A hair of tolerance, so that a center exactly on an edge, as the master's own corners are, counts as inside.
    let (lo, hi) = (whole((lo - 1e-9).ceil())?, whole((hi + 1e-9).floor())?);
    (lo <= hi).then_some((lo, hi))
}

/// The largest rectangle of whole pixels whose centers all lie inside a convex polygon. For a convex polygon, the
/// rows from `y0` to `y1` hold the columns from `x0` to `x1` when the first and the last row do: every row in between
/// has a chord that contains the intersection of theirs. So every pair of rows is tried, from their chords.
fn largest_rectangle(polygon: &[(f64, f64)], width: usize, height: usize) -> Result<Canvas, AlignError> {
    let (lo, hi) = polygon.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
    let (Some(first), Some(last)) = (whole((lo - 1e-9).ceil()), whole((hi + 1e-9).floor())) else {
        return Err(AlignError::NoCommonArea);
    };
    let rows = usize::try_from(last.saturating_sub(first).saturating_add(1)).unwrap_or(0);
    if rows == 0 {
        return Err(AlignError::NoCommonArea);
    }
    if rows > MAX_FACTOR.saturating_mul(height) {
        return Err(AlignError::CanvasSize { width, height: rows, frame_width: width, frame_height: height });
    }
    // Each row and its chord, top to bottom.
    let chords: Vec<(i64, (i64, i64))> = (first..=last).filter_map(|y| chord(polygon, y as f64).map(|c| (y, c))).collect();
    let mut best: Option<(i64, (i64, i64, i64, i64))> = None;
    for (i, &(y0, top)) in chords.iter().enumerate() {
        for &(y1, bottom) in chords.iter().skip(i) {
            let (x0, x1) = (top.0.max(bottom.0), top.1.min(bottom.1));
            if x1 < x0 {
                continue;
            }
            let area = (x1 - x0 + 1).saturating_mul(y1 - y0 + 1);
            if best.is_none_or(|(a, _)| area > a) {
                best = Some((area, (x0, y0, x1, y1)));
            }
        }
    }
    let Some((_, (x0, y0, x1, y1))) = best else {
        return Err(AlignError::NoCommonArea);
    };
    rectangle(x0, y0, x1, y1, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Similarity;

    const W: usize = 200;
    const H: usize = 100;

    fn similarity(shift: (f64, f64), scale: f64, rotation_deg: f64) -> Affine {
        Similarity { shift_x: shift.0, shift_y: shift.1, scale, rotation_deg }.to_affine(W, H)
    }

    #[test]
    fn the_master_alone_frames_itself() {
        for framing in Framing::ALL {
            let canvas = Canvas::new(framing, &[Affine::IDENTITY], W, H, Some(0)).unwrap();
            assert_eq!(canvas, Canvas::master(W, H), "{framing:?}");
        }
    }

    #[test]
    fn wide_framing_holds_every_field_of_view() {
        // A frame that sees the scene 10% smaller sees 5% more on each side: 10 and 5 pixels.
        let wider = similarity((0.0, 0.0), 1.0 / 1.1, 0.0);
        let canvas = Canvas::new(Framing::Wide, &[Affine::IDENTITY, wider], W, H, None).unwrap();
        assert_eq!((canvas.width, canvas.height), (220, 110));
        assert_eq!(canvas.to_master, Affine::translation(-10.0, -5.0));
        // Its corner pixels are the wider frame's corner pixels.
        let corner = canvas.to_frame(&wider).apply(0.0, 0.0);
        assert!(corner.0.abs() < 0.5 && corner.1.abs() < 0.5, "{corner:?}");
    }

    #[test]
    fn common_framing_is_inside_every_frame() {
        let frames = [Affine::IDENTITY, similarity((3.0, -2.0), 1.02, 0.5), similarity((-4.5, 1.5), 0.97, -0.8)];
        let canvas = Canvas::new(Framing::Common, &frames, W, H, None).unwrap();
        assert!(canvas.width > 180 && canvas.height > 85, "{canvas:?}");
        for t in &frames {
            let to_frame = canvas.to_frame(t);
            for (x, y) in corners(canvas.width, canvas.height, true) {
                let (u, v) = to_frame.apply(x, y);
                let inside = (-1e-6..=(W - 1) as f64 + 1e-6).contains(&u) && (-1e-6..=(H - 1) as f64 + 1e-6).contains(&v);
                assert!(inside, "{canvas:?}: corner ({x}, {y}) is at ({u}, {v}) in a frame");
            }
        }
        // Frames that share nothing have no common area.
        let away = Affine::translation(500.0, 0.0);
        assert_eq!(Canvas::new(Framing::Common, &[Affine::IDENTITY, away], W, H, None), Err(AlignError::NoCommonArea));
    }

    #[test]
    fn narrow_framing_takes_the_narrow_frames_grid() {
        let narrow = similarity((1.0, 0.0), 1.05, 0.0);
        let canvas = Canvas::new(Framing::Narrow, &[Affine::IDENTITY, narrow], W, H, Some(1)).unwrap();
        assert_eq!((canvas.width, canvas.height), (W, H));
        let back = canvas.to_frame(&narrow).apply(17.0, 33.0);
        assert!((back.0 - 17.0).abs() < 1e-9 && (back.1 - 33.0).abs() < 1e-9, "the narrow frame's own pixels: {back:?}");
    }

    #[test]
    fn hostile_transforms_are_errors() {
        let nan = Affine { m: [[f64::NAN, 0.0, 0.0], [0.0, 1.0, 0.0]] };
        assert_eq!(Canvas::new(Framing::Wide, &[nan], W, H, None), Err(AlignError::Transform { index: 0 }));
        assert_eq!(Canvas::new(Framing::Wide, &[Affine::scaling(0.0)], W, H, None), Err(AlignError::Transform { index: 0 }));
        let tiny = Affine::scaling(0.01);
        assert!(matches!(Canvas::new(Framing::Wide, &[tiny], W, H, None), Err(AlignError::CanvasSize { .. })));
        let huge = Affine::translation(1e300, 0.0);
        assert!(Canvas::new(Framing::Wide, &[huge], W, H, None).is_err());
        assert!(Canvas::new(Framing::Common, &[huge], W, H, None).is_err());
        assert_eq!(Canvas::new(Framing::Narrow, &[Affine::IDENTITY], W, H, Some(3)), Err(AlignError::Transform { index: 3 }));
        assert!(Canvas::new(Framing::Narrow, &[], 0, H, None).is_err());
        // One-pixel frames have no area in common but their single row.
        assert!(Canvas::new(Framing::Common, &[Affine::IDENTITY], 1, 1, None).is_ok());
    }
}
