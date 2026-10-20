//! Image derivatives on 3x3 neighborhoods, with clamp-to-edge borders.
//!
//! - **Sobel** (I. Sobel and G. Feldman, "A 3x3 isotropic gradient operator for image processing", Stanford Artificial
//!   Intelligence Project, 1968): a central difference smoothed by `[1, 2, 1]` across it, divided by 8 so that each
//!   component estimates the derivative in units per pixel (a ramp rising by 1 per pixel gives exactly 1).
//! - **Laplacian**: the five-point stencil, `up + down + left + right - 4 center`, which is `f_xx + f_yy` of the
//!   underlying function (2 + 2 for `x^2 + y^2`).
//!
//! Each channel is differentiated on its own, and every output row is computed from three input rows in parallel.

use rayon::prelude::*;

use super::OpError;
use crate::image::Image;

/// The Sobel derivatives of an image along x (rightward) and y (downward), with its channels.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradients {
    pub x: Image<f32>,
    pub y: Image<f32>,
}

/// The rows above and below row `y` of an image of `h` rows, clamped to the edges.
fn neighbors<'a>(rows: &[&'a [f32]], y: usize) -> (&'a [f32], &'a [f32], &'a [f32]) {
    let h = rows.len();
    let at = |i: usize| rows.get(i.min(h - 1)).copied().unwrap_or(&[]);
    (at(y.saturating_sub(1)), at(y), at(y + 1))
}

/// `out[i] = f(a[i - c], a[i], a[i + c])` along a row of interleaved samples with `c` channels, with the neighbors of
/// the first and last pixels clamped to them.
fn across(a: &[f32], c: usize, out: &mut [f32], f: impl Fn(f32, f32, f32) -> f32) {
    let n = a.len();
    for (i, o) in out.iter_mut().enumerate() {
        let left = if i >= c { a[i - c] } else { a[i] };
        let right = if i + c < n { a[i + c] } else { a[i] };
        *o = f(left, a[i], right);
    }
}

/// The Sobel derivatives of `src`.
pub fn sobel(src: &Image<f32>) -> Result<Gradients, OpError> {
    let (w, h, c) = (src.width(), src.height(), src.channels());
    let mut gx = Image::new(w, h, c, src.color_space().clone())?;
    let mut gy = Image::new(w, h, c, src.color_space().clone())?;
    let rows: Vec<&[f32]> = src.rows().collect();
    gx.par_rows_mut().zip(gy.par_rows_mut()).enumerate().for_each_init(
        || (Vec::new(), Vec::new()),
        |(smooth, diff), (y, (out_x, out_y))| {
            let (up, mid, down) = neighbors(&rows, y);
            // Smoothed down the column for the x derivative, differenced down it for the y derivative.
            smooth.clear();
            smooth.extend(up.iter().zip(mid).zip(down).map(|((u, m), d)| u + 2.0 * m + d));
            diff.clear();
            diff.extend(up.iter().zip(down).map(|(u, d)| d - u));
            across(smooth, c, out_x, |l, _, r| (r - l) * 0.125);
            across(diff, c, out_y, |l, m, r| (l + 2.0 * m + r) * 0.125);
        },
    );
    Ok(Gradients { x: gx, y: gy })
}

/// The Sobel gradient magnitude of `src`, `sqrt(gx^2 + gy^2)` per sample.
pub fn sobel_magnitude(src: &Image<f32>) -> Result<Image<f32>, OpError> {
    let Gradients { x: mut magnitude, y } = sobel(src)?;
    magnitude.par_rows_mut().zip(y.par_rows()).for_each(|(gx, gy)| {
        for (a, b) in gx.iter_mut().zip(gy) {
            *a = a.hypot(*b);
        }
    });
    Ok(magnitude)
}

/// The five-point Laplacian of `src`.
pub fn laplacian(src: &Image<f32>) -> Result<Image<f32>, OpError> {
    let (w, h, c) = (src.width(), src.height(), src.channels());
    let mut out = Image::new(w, h, c, src.color_space().clone())?;
    let rows: Vec<&[f32]> = src.rows().collect();
    out.par_rows_mut().enumerate().for_each(|(y, dst)| {
        let (up, mid, down) = neighbors(&rows, y);
        across(mid, c, dst, |l, m, r| l + r - 4.0 * m);
        for ((o, u), d) in dst.iter_mut().zip(up).zip(down) {
            *o += u + d;
        }
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ColorSpace;

    #[test]
    #[allow(clippy::float_cmp)] // Small integers: the stencils are exact on them.
    fn ramps_give_their_slopes_and_quadratics_their_curvature() {
        let ramp = Image::from_fn(9, 7, 2, ColorSpace::LINEAR, |x, y, c| 3.0 * x as f32 - 2.0 * y as f32 + c as f32).unwrap();
        let g = sobel(&ramp).unwrap();
        for y in 1..6 {
            for x in 1..8 {
                assert_eq!((g.x.get(x, y, 1), g.y.get(x, y, 1)), (Some(3.0), Some(-2.0)));
            }
        }
        // Clamped borders see half the slope.
        assert_eq!(g.x.get(0, 3, 0), Some(1.5));
        let magnitude = sobel_magnitude(&ramp).unwrap();
        assert!((magnitude.get(4, 3, 0).unwrap() - 13f32.sqrt()).abs() < 1e-6);
        let bowl = Image::from_fn(9, 9, 1, ColorSpace::LINEAR, |x, y, _| (x * x + 2 * y * y) as f32).unwrap();
        let lap = laplacian(&bowl).unwrap();
        assert_eq!(lap.get(4, 4, 0), Some(6.0));
        assert_eq!(laplacian(&ramp).unwrap().get(4, 3, 0), Some(0.0));
    }

    #[test]
    #[allow(clippy::float_cmp)] // Exactly zero on flat input.
    fn flat_images_and_single_pixels_have_no_gradient() {
        let flat = Image::from_fn(5, 4, 3, ColorSpace::LINEAR, |_, _, c| c as f32).unwrap();
        let g = sobel(&flat).unwrap();
        assert!(g.x.as_raw().iter().chain(g.y.as_raw()).all(|v| *v == 0.0));
        assert!(laplacian(&flat).unwrap().as_raw().iter().all(|v| *v == 0.0));
        let one = Image::from_vec(1, 1, 1, ColorSpace::LINEAR, vec![5.0]).unwrap();
        assert_eq!(sobel_magnitude(&one).unwrap().get(0, 0, 0), Some(0.0));
        assert_eq!(laplacian(&one).unwrap().get(0, 0, 0), Some(0.0));
    }

    #[test]
    fn an_impulse_gives_the_stencils() {
        let mut image = Image::<f32>::new(5, 5, 1, ColorSpace::LINEAR).unwrap();
        image.pixel_mut(2, 2).unwrap()[0] = 8.0;
        let g = sobel(&image).unwrap();
        // The x derivative at the impulse's left neighbor sees it on its right, with weight 2/8.
        assert_eq!(g.x.get(1, 2, 0), Some(2.0));
        assert_eq!(g.x.get(1, 1, 0), Some(1.0));
        assert_eq!(g.x.get(3, 2, 0), Some(-2.0));
        assert_eq!(g.y.get(2, 1, 0), Some(2.0));
        let lap = laplacian(&image).unwrap();
        assert_eq!((lap.get(2, 2, 0), lap.get(2, 1, 0), lap.get(1, 1, 0)), (Some(-32.0), Some(8.0), Some(0.0)));
    }
}
