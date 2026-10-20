//! Mean squared error and peak signal-to-noise ratio.

use florastack_core::Image;

use crate::{MetricsError, check_pair, color_channels};

/// The mean squared difference between `result` and `truth` over every color sample (alpha is ignored).
pub fn mse(result: &Image<f32>, truth: &Image<f32>) -> Result<f64, MetricsError> {
    check_pair(result, truth)?;
    let (c, colors) = (result.channels(), color_channels(result));
    let mut sum = 0.0;
    for (a, b) in result.rows().zip(truth.rows()) {
        for (pa, pb) in a.chunks_exact(c).zip(b.chunks_exact(c)) {
            for (&x, &y) in pa.iter().zip(pb).take(colors) {
                let d = f64::from(x) - f64::from(y);
                if !d.is_finite() {
                    return Err(MetricsError::NonFinite);
                }
                sum += d * d;
            }
        }
    }
    Ok(sum / (result.pixel_count() * colors) as f64)
}

/// The peak signal-to-noise ratio in decibels, `10 log10(1 / MSE)`: samples are nominally in `[0, 1]`, so the peak
/// is 1. Infinite for identical images.
pub fn psnr(result: &Image<f32>, truth: &Image<f32>) -> Result<f64, MetricsError> {
    let mse = mse(result, truth)?;
    Ok(if mse == 0.0 { f64::INFINITY } else { -10.0 * mse.log10() })
}

#[cfg(test)]
mod tests {
    use florastack_core::{ColorSpace, Encoding};

    use super::*;

    fn gray(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Image<f32> {
        Image::from_fn(w, h, 1, ColorSpace::LINEAR, |x, y, _| f(x, y)).unwrap()
    }

    #[test]
    fn psnr_of_a_known_error() {
        let truth = gray(16, 8, |x, y| (x + y) as f32 / 30.0);
        assert_eq!(psnr(&truth, &truth).unwrap(), f64::INFINITY);
        // An error of 0.1 everywhere: MSE 0.01, 20 dB.
        let off = gray(16, 8, |x, y| (x + y) as f32 / 30.0 + 0.1);
        assert!((psnr(&off, &truth).unwrap() - 20.0).abs() < 1e-5);
        // Half the samples off by 0.2: MSE 0.02.
        let half = gray(16, 8, |x, y| (x + y) as f32 / 30.0 + if x % 2 == 0 { 0.2 } else { 0.0 });
        assert!((mse(&half, &truth).unwrap() - 0.02).abs() < 1e-8);
    }

    #[test]
    fn alpha_is_ignored_and_mismatches_are_errors() {
        let truth = Image::from_fn(4, 4, 4, ColorSpace::LINEAR, |_, _, c| if c == 3 { 1.0 } else { 0.5 }).unwrap();
        let other_alpha = Image::from_fn(4, 4, 4, ColorSpace::LINEAR, |_, _, c| if c == 3 { 0.0 } else { 0.5 }).unwrap();
        assert_eq!(mse(&other_alpha, &truth).unwrap(), 0.0);
        let small = gray(4, 3, |_, _| 0.5);
        assert!(matches!(mse(&small, &truth), Err(MetricsError::Shape { result: [4, 3, 1], truth: [4, 4, 4] })));
        let mut srgb = truth.clone();
        srgb.set_color_space(ColorSpace::SRGB);
        assert_eq!(mse(&srgb, &truth), Err(MetricsError::Encoding { result: Encoding::Srgb, truth: Encoding::Linear }));
        let nan = Image::from_fn(4, 4, 4, ColorSpace::LINEAR, |x, _, _| if x == 2 { f32::NAN } else { 0.5 }).unwrap();
        assert_eq!(psnr(&nan, &truth), Err(MetricsError::NonFinite));
    }
}
