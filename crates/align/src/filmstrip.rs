//! The filmstrip: small previews of every frame, as shot and aligned onto the output canvas, so that the user can
//! scrub through the stack and see what moved. The engine caches them; this module makes them.
//!
//! - [`thumbnail`]: the frame reduced to at most a given side, its pixels averaged by blocks as the frame is read once,
//!   then resized with Lanczos3 to the exact size: light is kept, and detail finer than a thumbnail pixel averages out
//!   instead of aliasing.
//! - [`aligned_thumbnail`]: that thumbnail warped onto the canvas, at the thumbnail's own scale, so that scrubbing
//!   through the aligned thumbnails shows the stack still but for what moved or changed focus. It is made from the
//!   small thumbnail, not the frame, so that a new alignment or framing needs no frame read again; what the frame
//!   doesn't cover is 0, with its coverage ([`Covered`]).

use florastack_core::ops::{Border, Interpolation, resize, warp_into};
use florastack_core::{Affine, ColorSpace, Image};
use rayon::prelude::*;

use crate::coverage::{Coverage, Covered};
use crate::error::AlignError;
use crate::framing::Canvas;

/// The smallest side a thumbnail may be asked for.
pub const MIN_THUMBNAIL_SIDE: usize = 16;

/// `frame` reduced to at most `side` pixels on its longer side (see the module documentation), in its own color space
/// and channels. A frame that fits already is returned as it is.
pub fn thumbnail(frame: &Image<f32>, side: usize) -> Result<Image<f32>, AlignError> {
    let side = side.max(MIN_THUMBNAIL_SIDE);
    let (w, h) = (frame.width(), frame.height());
    let long = w.max(h);
    if long <= side {
        return Ok(frame.clone());
    }
    let scaled = |n: usize| (n.saturating_mul(side) / long).max(1);
    let (tw, th) = (scaled(w), scaled(h));
    // Blocks of k x k pixels, k as large as leaves at least the thumbnail's size.
    let k = (w / tw).min(h / th).max(1);
    let reduced = if k > 1 { blocks(frame, k)? } else { frame.clone() };
    if (reduced.width(), reduced.height()) == (tw, th) {
        return Ok(reduced);
    }
    Ok(resize(&reduced, tw, th)?)
}

/// `frame` averaged over blocks of `k` x `k` pixels, every channel: `width / k` x `height / k` pixels.
fn blocks(frame: &Image<f32>, k: usize) -> Result<Image<f32>, AlignError> {
    let c = frame.channels();
    let mut out = Image::new(frame.width() / k, frame.height() / k, c, frame.color_space().clone())?;
    let scale = 1.0 / (k * k) as f32;
    out.par_rows_mut().enumerate().for_each(|(y, dst)| {
        dst.fill(0.0);
        for row in (y * k..y * k + k).filter_map(|r| frame.row(r)) {
            for (d, block) in dst.chunks_exact_mut(c).zip(row.chunks_exact(c * k)) {
                for pixel in block.chunks_exact(c) {
                    for (d, &v) in d.iter_mut().zip(pixel) {
                        *d += if v.is_finite() { v } else { 0.0 };
                    }
                }
            }
        }
        for d in dst.iter_mut() {
            *d *= scale;
        }
    });
    Ok(out)
}

/// The thumbnail of a `width` x `height` frame aligned onto `canvas`: `thumbnail` is the frame's [`thumbnail`] and
/// `transform` its alignment (from the master's pixels to the frame's). The result has the canvas's size at the
/// thumbnail's scale, interpolated bilinearly, with the frame's coverage; what the frame doesn't cover is 0.
pub fn aligned_thumbnail(thumbnail: &Image<f32>, width: usize, height: usize, transform: &Affine, canvas: &Canvas) -> Result<Covered, AlignError> {
    let (tx, ty) = (thumbnail.width() as f64 / width.max(1) as f64, thumbnail.height() as f64 / height.max(1) as f64);
    let scale = f64::midpoint(tx, ty);
    let size = |n: usize| to_size(n as f64 * scale);
    let (ow, oh) = (size(canvas.width), size(canvas.height));
    // From the output's pixels to the canvas's: the extents line up, as in a resize.
    let (sx, sy) = (canvas.width as f64 / ow as f64, canvas.height as f64 / oh as f64);
    let to_canvas = Affine { m: [[sx, 0.0, 0.5 * sx - 0.5], [0.0, sy, 0.5 * sy - 0.5]] };
    let to_thumbnail = Affine { m: [[tx, 0.0, 0.5 * tx - 0.5], [0.0, ty, 0.5 * ty - 0.5]] };
    let to_source = to_canvas.then(&canvas.to_frame(transform)).then(&to_thumbnail);
    let mut image = Image::new(ow, oh, thumbnail.channels(), thumbnail.color_space().clone())?;
    warp_into(thumbnail, &to_source, Interpolation::Bilinear, Border::Constant(0.0), &mut image)?;
    let coverage = Coverage::new(canvas, transform, width, height, 0.0);
    let mut weights = Image::new(ow, oh, 1, ColorSpace::LINEAR)?;
    let c = image.channels();
    image.par_rows_mut().zip(weights.par_rows_mut()).enumerate().for_each(|(y, (row, weights))| {
        for (x, (pixel, w)) in row.chunks_exact_mut(c).zip(weights.iter_mut()).enumerate() {
            let (cx, cy) = to_canvas.apply(x as f64, y as f64);
            *w = coverage.weight(cx, cy);
            if *w <= 0.0 {
                pixel.fill(0.0);
            }
        }
    });
    Ok(Covered { image, coverage: weights })
}

/// A size in pixels, rounded, at least 1.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A canvas's side times a scale below 1.
fn to_size(v: f64) -> usize {
    if v.is_finite() { v.round().max(1.0) as usize } else { 1 }
}

#[cfg(test)]
mod tests {
    use florastack_core::ops::warp;
    use florastack_core::testutil::scene;

    use super::*;

    fn mean(image: &Image<f32>) -> f64 {
        image.rows().flatten().map(|&v| f64::from(v)).sum::<f64>() / (image.width() * image.height() * image.channels()) as f64
    }

    #[test]
    fn thumbnails_keep_the_light_and_fit_the_side() {
        let (sharp, _) = scene::textured(1000, 600, 3).unwrap();
        let small = thumbnail(&sharp, 128).unwrap();
        assert_eq!((small.width(), small.height(), small.channels()), (128, 76, 3));
        assert!((mean(&small) / mean(&sharp) - 1.0).abs() < 0.01);
        assert_eq!(thumbnail(&small, 512).unwrap(), small, "a frame that fits is kept");
        assert_eq!(thumbnail(&sharp, 0).unwrap().width(), MIN_THUMBNAIL_SIDE);
    }

    #[test]
    fn an_aligned_thumbnail_undoes_the_frames_transform() {
        let (master, _) = scene::textured(640, 480, 5).unwrap();
        // The frame shows the scene moved 12 pixels right and 6 down: the master's pixel p is the frame's p + (12, 6).
        let transform = Affine::translation(12.0, 6.0);
        let frame = warp(&master, &transform.inverse().unwrap(), Interpolation::Bicubic, Border::Clamp).unwrap();
        let canvas = Canvas::master(640, 480);
        let master_thumbnail = thumbnail(&master, 160).unwrap();
        let aligned = aligned_thumbnail(&thumbnail(&frame, 160).unwrap(), 640, 480, &transform, &canvas).unwrap();
        assert_eq!((aligned.image.width(), aligned.image.height()), (160, 120));
        // Away from the edge the frame doesn't cover, the aligned thumbnail is the master's, up to the bilinear
        // interpolation of a half-pixel shift (the 6 pixels down are 1.5 thumbnail pixels).
        let (mut sum, mut n) = (0.0, 0);
        for y in 4..110 {
            for x in 4..150 {
                for c in 0..3 {
                    sum += f64::from((aligned.image.get(x, y, c).unwrap() - master_thumbnail.get(x, y, c).unwrap()).abs());
                    n += 1;
                }
            }
        }
        let error = sum / f64::from(n);
        assert!(error < 0.02, "{error}");
        // The frame doesn't reach the canvas's right and bottom edges.
        assert_eq!(aligned.coverage.get(159, 119, 0), Some(0.0));
        assert_eq!(aligned.image.pixel(159, 119), Some(&[0.0, 0.0, 0.0][..]));
        assert_eq!(aligned.coverage.get(80, 60, 0), Some(1.0));
    }
}
