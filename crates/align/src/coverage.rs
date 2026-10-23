//! Coverage: which pixels of the output a frame has pixels for, so that stacking leaves the frame out where it has none
//! instead of smearing its edge pixels over them.
//!
//! A frame covers a pixel of the [`Canvas`] when the point the pixel samples in the frame, through
//! [`Canvas::to_frame`], lies inside the frame's extent (from `-0.5` to `width - 0.5`, and the same vertically). A warp
//! that clamps at the frame's border gives such uncovered pixels the frame's edge pixels, repeated outward: the streaks
//! that a stack framed wider than its narrowest frame shows along its edges when every frame is blended everywhere.
//!
//! The coverage mask is not stored as pixels: it follows exactly from a frame's transform, its size and the canvas,
//! which the project stores. [`Coverage`] gives it for any pixel or tile, as a weight from 0 to 1 that ramps up over a
//! few pixels inside the frame's edge ([`DEFAULT_FEATHER`]), so that where the number of frames that contribute
//! changes, the blend changes smoothly rather than in a step. Stacking (roadmap 3) multiplies each frame's weights by
//! it, tile by tile, as it warps the frame ([`warp_to_canvas`]), and a pixel that no frame covers ([`coverage_count`]
//! is 0 there, which only wide framing has) is left empty.

use florastack_core::ops::{Border, Interpolation, warp_into};
use florastack_core::{Affine, ColorSpace, Image, Rect};
use rayon::prelude::*;

use crate::error::AlignError;
use crate::framing::Canvas;

/// The default width, in frame pixels, of the ramp of a coverage weight inside a frame's edge.
pub const DEFAULT_FEATHER: f64 = 2.0;

/// Which pixels of a canvas a frame covers, and how much: see the module documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coverage {
    to_frame: Affine,
    width: f64,
    height: f64,
    feather: f64,
}

impl Coverage {
    /// The coverage of `canvas` by a `width` x `height` frame whose alignment is `transform` (from the master's pixels
    /// to the frame's). The weight ramps from 0 at the frame's edge to 1 at `feather` frame pixels inside it; with a
    /// `feather` of 0 (or not finite), it is 1 wherever the frame covers.
    pub fn new(canvas: &Canvas, transform: &Affine, width: usize, height: usize, feather: f64) -> Coverage {
        let feather = if feather.is_finite() { feather.max(0.0) } else { 0.0 };
        Coverage { to_frame: canvas.to_frame(transform), width: width as f64, height: height as f64, feather }
    }

    /// The distance from the point canvas pixel `(x, y)` samples in the frame to the frame's edge, in frame pixels:
    /// positive inside, negative outside (and outside for a point that isn't finite).
    pub fn inset(&self, x: f64, y: f64) -> f64 {
        let (u, v) = self.to_frame.apply(x, y);
        let d = (u + 0.5).min(self.width - 0.5 - u).min(v + 0.5).min(self.height - 0.5 - v);
        if d.is_nan() { f64::NEG_INFINITY } else { d }
    }

    /// The weight of canvas pixel `(x, y)`: 0 where the frame doesn't cover it.
    pub fn weight(&self, x: f64, y: f64) -> f32 {
        let d = self.inset(x, y);
        if d <= 0.0 {
            0.0
        } else if self.feather <= 0.0 || d >= self.feather {
            1.0
        } else {
            to_f32(d / self.feather)
        }
    }

    /// Whether the frame covers canvas pixel `(x, y)`.
    pub fn covers(&self, x: f64, y: f64) -> bool {
        self.inset(x, y) > 0.0
    }

    /// The weights of the pixels of `rect`, a rectangle of `canvas`, as a one-channel linear image.
    pub fn mask(&self, canvas: &Canvas, rect: Rect) -> Result<Image<f32>, AlignError> {
        check_rect(canvas, rect)?;
        let mut mask = Image::new(rect.width, rect.height, 1, ColorSpace::LINEAR)?;
        mask.par_rows_mut().enumerate().for_each(|(j, row)| {
            let y = (rect.y + j) as f64;
            for (i, w) in row.iter_mut().enumerate() {
                *w = self.weight((rect.x + i) as f64, y);
            }
        });
        Ok(mask)
    }
}

/// A weight from 0 to 1, as the mask's samples are.
#[allow(clippy::cast_possible_truncation)] // From 0 to 1.
fn to_f32(v: f64) -> f32 {
    v as f32
}

/// Checks that `rect` lies inside `canvas` and isn't empty.
fn check_rect(canvas: &Canvas, rect: Rect) -> Result<(), AlignError> {
    if rect.is_empty() || !rect.fits_in(canvas.width, canvas.height) {
        return Err(AlignError::Rect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            canvas_width: canvas.width,
            canvas_height: canvas.height,
        });
    }
    Ok(())
}

/// A frame warped onto a rectangle of the canvas, and its coverage there.
#[derive(Clone, Debug, PartialEq)]
pub struct Covered {
    /// The frame's pixels, interpolated bicubically; 0 where the frame doesn't cover the canvas.
    pub image: Image<f32>,
    /// The frame's coverage weights ([`Coverage::mask`]).
    pub coverage: Image<f32>,
}

/// Warps `frame`, whose alignment is `transform`, onto `rect` of `canvas`, with its coverage (see the module
/// documentation): the pixels it doesn't cover are 0, not its edge pixels repeated, and their weight is 0.
pub fn warp_to_canvas(frame: &Image<f32>, transform: &Affine, canvas: &Canvas, rect: Rect, feather: f64) -> Result<Covered, AlignError> {
    check_rect(canvas, rect)?;
    let coverage = Coverage::new(canvas, transform, frame.width(), frame.height(), feather);
    let mask = coverage.mask(canvas, rect)?;
    let mut image = Image::new(rect.width, rect.height, frame.channels(), frame.color_space().clone())?;
    let to_source = Affine::translation(rect.x as f64, rect.y as f64).then(&canvas.to_frame(transform));
    // Clamping gives the pixels just inside the edge real neighbors to interpolate from; what lies beyond is zeroed.
    warp_into(frame, &to_source, Interpolation::Bicubic, Border::Clamp, &mut image)?;
    let c = image.channels();
    image.par_rows_mut().zip(mask.par_rows()).for_each(|(row, weights)| {
        for (pixel, &w) in row.chunks_exact_mut(c).zip(weights) {
            if w <= 0.0 {
                pixel.fill(0.0);
            }
        }
    });
    Ok(Covered { image, coverage: mask })
}

/// How many of the frames, `width` x `height` pixels with these transforms, cover each pixel of the canvas: a
/// one-channel map of whole numbers. Wide framing leaves pixels that no frame covers (0) and pixels covered by only some
/// frames; in common framing, every pixel is covered by every frame.
pub fn coverage_count(canvas: &Canvas, transforms: &[Affine], width: usize, height: usize) -> Result<Image<u16>, AlignError> {
    let coverages: Vec<Coverage> = transforms.iter().map(|t| Coverage::new(canvas, t, width, height, 0.0)).collect();
    let mut count = Image::new(canvas.width, canvas.height, 1, ColorSpace::LINEAR)?;
    count.par_rows_mut().enumerate().for_each(|(y, row)| {
        for (x, n) in row.iter_mut().enumerate() {
            let covering = coverages.iter().filter(|c| c.covers(x as f64, y as f64)).count();
            *n = u16::try_from(covering).unwrap_or(u16::MAX);
        }
    });
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::Framing;
    use crate::model::Similarity;

    #[test]
    fn weights_ramp_up_inside_the_frames_edge() {
        let canvas = Canvas::master(100, 80);
        let c = Coverage::new(&canvas, &Affine::IDENTITY, 100, 80, 2.0);
        assert_eq!(c.weight(0.0, 40.0), 0.25, "the edge pixel's center is half a pixel inside");
        assert_eq!(c.weight(1.0, 40.0), 0.75);
        assert_eq!(c.weight(50.0, 40.0), 1.0);
        assert_eq!(c.weight(-1.0, 40.0), 0.0);
        assert!(c.covers(99.0, 79.0) && !c.covers(100.0, 79.0));
        let hard = Coverage::new(&canvas, &Affine::IDENTITY, 100, 80, 0.0);
        assert_eq!((hard.weight(0.0, 0.0), hard.weight(-0.6, 0.0)), (1.0, 0.0));
        let nan = Coverage::new(&canvas, &Affine { m: [[f64::NAN; 3]; 2] }, 100, 80, 2.0);
        assert_eq!(nan.weight(5.0, 5.0), 0.0, "a point that isn't finite is covered by nothing");
    }

    #[test]
    fn a_warp_onto_a_wider_canvas_leaves_uncovered_pixels_empty() {
        // The master, and a frame that sees 10% more of the scene: the wide canvas is the second frame's view.
        let master = Image::from_fn(60, 40, 2, ColorSpace::LINEAR, |x, y, c| 0.2 + (x + 2 * y + c) as f32 / 200.0).unwrap();
        let wider = Similarity { scale: 1.0 / 1.1, ..Similarity::IDENTITY }.to_affine(60, 40);
        let canvas = Canvas::new(Framing::Wide, &[Affine::IDENTITY, wider], 60, 40, None).unwrap();
        let (w, h) = (canvas.width, canvas.height);
        assert_eq!((w, h), (66, 44));
        let all = Rect::new(0, 0, w, h);
        let covered = warp_to_canvas(&master, &Affine::IDENTITY, &canvas, all, DEFAULT_FEATHER).unwrap();
        assert_eq!(covered.coverage.get(0, 0, 0), Some(0.0), "the master doesn't reach the wide canvas's corner");
        assert_eq!(covered.image.pixel(0, 0), Some(&[0.0, 0.0][..]), "and gets no smeared edge pixels there");
        assert_eq!(covered.coverage.get(w / 2, h / 2, 0), Some(1.0));
        assert_eq!(covered.image.pixel(33, 22), master.pixel(30, 20), "a whole-pixel shift copies pixels");
        // A tile is the same pixels as the whole.
        let tile = warp_to_canvas(&master, &Affine::IDENTITY, &canvas, Rect::new(3, 5, 20, 10), DEFAULT_FEATHER).unwrap();
        assert_eq!(tile.image, covered.image.crop(Rect::new(3, 5, 20, 10)).unwrap());
        assert_eq!(tile.coverage, covered.coverage.crop(Rect::new(3, 5, 20, 10)).unwrap());
        assert!(matches!(warp_to_canvas(&master, &wider, &canvas, Rect::new(0, 0, w + 1, h), 2.0), Err(AlignError::Rect { .. })));
        assert!(warp_to_canvas(&master, &wider, &canvas, Rect::new(0, 0, 0, h), 2.0).is_err());

        let count = coverage_count(&canvas, &[Affine::IDENTITY, wider], 60, 40).unwrap();
        assert_eq!((count.get(0, 0, 0), count.get(w / 2, h / 2, 0)), (Some(1), Some(2)));
    }
}
