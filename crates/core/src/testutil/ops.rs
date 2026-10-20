//! Numeric helpers for the generator and the scenes, and the tests' oracle for [`crate::ops`]: an exact separable
//! Gaussian blur and a bicubic affine warp, both with clamp-to-edge borders, written as plain loops.
//!
//! The generator used these until roadmap step 1.1 and now uses the public operations (the exact kernel and the
//! bicubic warp with clamped borders), which are parallel and much faster. The tests below hold the public operations
//! to these references bit for bit, so the synthetic stacks didn't change when the generator switched, and they keep
//! the references honest with their own checks.

#[cfg(test)]
use crate::geometry::Affine;
#[cfg(test)]
use crate::image::{Image, ImageError};

/// `v` as an index: NaN and negative values give 0, and the caller clamps the upper end.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Saturating float-to-int casts, on clamped values.
pub(super) fn to_index(v: f64) -> usize {
    v as usize
}

/// `v` rounded to the nearest `f32`.
#[allow(clippy::cast_possible_truncation)] // f64 to f32 rounds; that is the intent.
pub(super) fn to_f32(v: f64) -> f32 {
    v as f32
}

/// The normalized Gaussian kernel of standard deviation `sigma` pixels, with radius `ceil(3 sigma)`: `[1.0]` when
/// `sigma` is zero, negative or NaN.
#[cfg(test)]
pub(super) fn gaussian_kernel(sigma: f64) -> Vec<f32> {
    if sigma.is_nan() || sigma <= 0.0 {
        return vec![1.0];
    }
    let radius = to_index((3.0 * sigma).ceil());
    let weights: Vec<f64> = (0..=2 * radius).map(|i| (-((i as f64 - radius as f64).powi(2)) / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f64 = weights.iter().sum();
    weights.iter().map(|w| to_f32(w / sum)).collect()
}

/// `image` blurred with a Gaussian of standard deviation `sigma` pixels (a copy when `sigma` is 0).
#[cfg(test)]
pub(super) fn gaussian_blur(image: &Image<f32>, sigma: f64) -> Result<Image<f32>, ImageError> {
    let kernel = gaussian_kernel(sigma);
    if kernel.len() == 1 {
        return Ok(image.clone());
    }
    let radius = kernel.len() / 2;
    let (w, h, c) = (image.width(), image.height(), image.channels());
    let mut across = Image::new(w, h, c, image.color_space().clone())?;
    for (src, dst) in image.rows().zip(across.rows_mut()) {
        for x in 0..w {
            for ch in 0..c {
                let mut acc = 0.0;
                for (k, weight) in kernel.iter().enumerate() {
                    let sx = (x + k).saturating_sub(radius).min(w - 1);
                    acc += weight * src[sx * c + ch];
                }
                dst[x * c + ch] = acc;
            }
        }
    }
    let rows: Vec<&[f32]> = across.rows().collect();
    let mut out = Image::new(w, h, c, image.color_space().clone())?;
    for (y, dst) in out.rows_mut().enumerate() {
        for (i, value) in dst.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (k, weight) in kernel.iter().enumerate() {
                let sy = (y + k).saturating_sub(radius).min(h - 1);
                acc += weight * rows[sy][i];
            }
            *value = acc;
        }
    }
    Ok(out)
}

/// The weights of Keys' cubic convolution (a = -0.5) for the four samples around a point a fraction `t` past the
/// second one. At `t = 0` they are exactly `[0, 1, 0, 0]`.
#[cfg(test)]
fn keys_weights(t: f64) -> [f64; 4] {
    const A: f64 = -0.5;
    let near = |d: f64| ((A + 2.0) * d - (A + 3.0)) * d * d + 1.0;
    let far = |d: f64| ((A * d - 5.0 * A) * d + 8.0 * A) * d - 4.0 * A;
    [far(1.0 + t), near(t), near(1.0 - t), far(2.0 - t)]
}

/// The four sample indices around coordinate `v` on an axis of `len` samples, clamped to the edge, and their weights.
#[cfg(test)]
fn taps(v: f64, len: usize) -> ([usize; 4], [f64; 4]) {
    let floor = v.floor();
    let last = (len - 1) as f64;
    let index = |offset: f64| to_index((floor + offset).clamp(0.0, last));
    ([index(-1.0), index(0.0), index(1.0), index(2.0)], keys_weights(v - floor))
}

/// The image of `src` under the transform whose inverse is `inverse`: output pixel `p` is `src` sampled at
/// `inverse(p)`, with the same size as `src`. Sampling is Keys' bicubic convolution (R. G. Keys, "Cubic convolution
/// interpolation for digital image processing", IEEE Transactions on Acoustics, Speech, and Signal Processing 29(6),
/// 1981), which reproduces a sample exactly when the point lands on it; outside `src` the edge pixels repeat.
#[cfg(test)]
pub(super) fn warp(src: &Image<f32>, inverse: &Affine) -> Result<Image<f32>, ImageError> {
    let (w, h, c) = (src.width(), src.height(), src.channels());
    let rows: Vec<&[f32]> = src.rows().collect();
    let mut out = Image::new(w, h, c, src.color_space().clone())?;
    for (y, dst) in out.rows_mut().enumerate() {
        for x in 0..w {
            let (sx, sy) = inverse.apply(x as f64, y as f64);
            let (xs, wx) = taps(sx, w);
            let (ys, wy) = taps(sy, h);
            for ch in 0..c {
                let mut acc = 0.0;
                for (&row, wy) in ys.iter().zip(wy) {
                    let line = rows[row];
                    let across: f64 = xs.iter().zip(wx).map(|(&col, wx)| wx * f64::from(line[col * c + ch])).sum();
                    acc += wy * across;
                }
                dst[x * c + ch] = to_f32(acc);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::rng::Rng;
    use super::*;
    use crate::image::ColorSpace;
    use crate::ops::{self, BlurMethod, Border, Interpolation};

    /// A `w` x `h` image of random samples in `[-0.5, 1.5)`, with `pad` NaN samples of padding after each row.
    fn random_image(w: usize, h: usize, c: usize, pad: usize, seed: u64) -> Image<f32> {
        let mut rng = Rng::new(seed);
        let mut data = Vec::new();
        for _ in 0..h {
            data.extend((0..w * c).map(|_| to_f32(rng.range(-0.5, 1.5))));
            data.extend(std::iter::repeat_n(f32::NAN, pad));
        }
        Image::from_vec_with_stride(w, h, c, w * c + pad, ColorSpace::LINEAR, data).unwrap()
    }

    fn same_bits(a: &Image<f32>, b: &Image<f32>) -> bool {
        a.same_shape(b) && a.rows().flatten().zip(b.rows().flatten()).all(|(x, y)| x.to_bits() == y.to_bits())
    }

    #[test]
    fn the_public_exact_blur_matches_this_one_bit_for_bit() {
        // Taller than the public blur's 64-row bands too, with radii whose ring of rows wraps around (2 x 36 + 64 < 150,
        // 2 x 90 + 64 < 300).
        let cases = [
            (37, 23, 3, 0, 0.8),
            (64, 40, 1, 3, 2.5),
            (19, 51, 4, 1, 4.0),
            (300, 7, 2, 0, 7.5),
            (5, 3, 3, 0, 12.0),
            (1, 9, 1, 0, 1.0),
            (13, 200, 3, 2, 2.0),
            (50, 150, 1, 0, 12.0),
            (8, 300, 2, 1, 30.0),
        ];
        for (i, (w, h, c, pad, sigma)) in cases.into_iter().enumerate() {
            let image = random_image(w, h, c, pad, i as u64);
            let expected = gaussian_blur(&image, sigma).unwrap();
            let mut exact = image.clone();
            ops::gaussian_blur_in_place(&mut exact, sigma, BlurMethod::Exact).unwrap();
            assert!(same_bits(&exact, &expected), "{w}x{h}x{c} sigma {sigma}");
            assert_eq!(ops::gaussian_kernel(sigma), gaussian_kernel(sigma));
            if sigma <= ops::AUTO_EXACT_MAX_SIGMA {
                assert!(same_bits(&ops::gaussian_blur(&image, sigma).unwrap(), &expected), "auto is exact up to its threshold");
            }
        }
    }

    #[test]
    fn the_public_bicubic_warp_matches_this_one_bit_for_bit() {
        let mut rng = Rng::new(77);
        for (w, h, c, pad) in [(40, 30, 3, 0), (17, 33, 1, 2), (64, 8, 4, 0), (3, 2, 2, 1)] {
            let src = random_image(w, h, c, pad, rng.next_u64());
            for _ in 0..4 {
                let center = ((w - 1) as f64 / 2.0, (h - 1) as f64 / 2.0);
                let shift = (rng.range(-3.0, 3.0), rng.range(-3.0, 3.0));
                let transform = Affine::similarity(center, rng.range(0.9, 1.1), rng.range(-0.1, 0.1), shift);
                let inverse = transform.inverse().unwrap();
                let expected = warp(&src, &inverse).unwrap();
                let got = ops::warp(&src, &inverse, Interpolation::Bicubic, Border::Clamp).unwrap();
                assert!(same_bits(&got, &expected), "{w}x{h}x{c}: {transform:?}");
            }
        }
    }

    fn ramp(w: usize, h: usize) -> Image<f32> {
        Image::from_fn(w, h, 2, ColorSpace::LINEAR, |x, y, c| (x as f32) * 0.1 + (y as f32) * 0.01 + c as f32).unwrap()
    }

    #[test]
    fn kernels_are_normalized_and_symmetric() {
        assert_eq!(gaussian_kernel(0.0), [1.0]);
        assert_eq!(gaussian_kernel(f64::NAN), [1.0]);
        let k = gaussian_kernel(1.5);
        assert_eq!(k.len(), 11);
        assert!((k.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(k.iter().zip(k.iter().rev()).all(|(a, b)| a.to_bits() == b.to_bits()), "exactly symmetric");
    }

    #[test]
    fn blur_keeps_flat_images_and_the_mean() {
        let flat = Image::from_fn(9, 7, 3, ColorSpace::LINEAR, |_, _, c| 0.25 * (c as f32 + 1.0)).unwrap();
        let blurred = gaussian_blur(&flat, 2.0).unwrap();
        assert!(blurred.as_raw().iter().zip(flat.as_raw()).all(|(a, b)| (a - b).abs() < 1e-6));
        // A centered impulse spreads but keeps its energy (its tails stay inside the image).
        let mut impulse = Image::<f32>::new(21, 21, 1, ColorSpace::LINEAR).unwrap();
        impulse.pixel_mut(10, 10).unwrap()[0] = 1.0;
        let spread = gaussian_blur(&impulse, 1.0).unwrap();
        assert!((spread.as_raw().iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(spread.get(10, 10, 0).unwrap() < 0.2);
        assert_eq!(gaussian_blur(&impulse, 0.0).unwrap(), impulse);
    }

    #[test]
    fn keys_weights_interpolate_exactly_at_samples_and_sum_to_one() {
        assert_eq!(keys_weights(0.0), [0.0, 1.0, 0.0, 0.0]);
        for t in [0.1, 0.25, 0.5, 0.9] {
            assert!((keys_weights(t).iter().sum::<f64>() - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn warps_copy_on_whole_pixel_moves_and_interpolate_ramps_between_them() {
        let src = ramp(12, 8);
        assert_eq!(warp(&src, &Affine::IDENTITY).unwrap(), src);
        // Shift right by 2 px: output (x, y) samples (x - 2, y).
        let shifted = warp(&src, &Affine::translation(-2.0, 0.0)).unwrap();
        assert_eq!(shifted.pixel(5, 3), src.pixel(3, 3));
        assert_eq!(shifted.pixel(0, 3), src.pixel(0, 3), "the edge repeats");
        // Keys' cubic reproduces linear ramps exactly away from the borders.
        let half = warp(&src, &Affine::translation(0.5, 0.25)).unwrap();
        let expected = 0.1 * 4.5 + 0.01 * 3.25;
        assert!((half.get(4, 3, 0).unwrap() - expected).abs() < 1e-5, "{:?}", half.get(4, 3, 0));
    }
}
