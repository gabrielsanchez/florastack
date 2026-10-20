//! The structural similarity index.
//!
//! Z. Wang, A. C. Bovik, H. R. Sheikh and E. P. Simoncelli, "Image quality assessment: from error visibility to
//! structural similarity", IEEE Transactions on Image Processing 13(4), 600-612, 2004. As in the paper: local means,
//! variances and covariance under an 11x11 circular-symmetric Gaussian window of standard deviation 1.5 (normalized
//! to unit sum), `C1 = (K1 L)^2` and `C2 = (K2 L)^2` with `K1 = 0.01`, `K2 = 0.03` and a dynamic range `L = 1`, and
//! the mean of the SSIM map over every window that lies wholly inside the image (no padding). The paper's measure is
//! for one channel: color images are scored per channel and the channel scores averaged. Unlike the authors' later
//! reference code, nothing is downsampled first.

use florastack_core::Image;

use crate::plane::Plane;
use crate::{MetricsError, check_pair, color_channels};

/// The window size.
const WINDOW: usize = 11;
const SIGMA: f64 = 1.5;
const C1: f64 = 0.01 * 0.01;
const C2: f64 = 0.03 * 0.03;

/// The 11-tap Gaussian whose outer product is the paper's window.
fn window() -> [f64; WINDOW] {
    let mut w: [f64; WINDOW] = std::array::from_fn(|i| {
        let d = i as f64 - (WINDOW / 2) as f64;
        (-(d * d) / (2.0 * SIGMA * SIGMA)).exp()
    });
    let sum: f64 = w.iter().sum();
    for v in &mut w {
        *v /= sum;
    }
    w
}

/// The mean SSIM of `result` against `truth`: 1 for identical images, lower as structure is lost or added. Both
/// must be at least 11x11 pixels.
pub fn ssim(result: &Image<f32>, truth: &Image<f32>) -> Result<f64, MetricsError> {
    check_pair(result, truth)?;
    let (width, height) = (result.width(), result.height());
    if width < WINDOW || height < WINDOW {
        return Err(MetricsError::TooSmall { metric: "SSIM", min: WINDOW, width, height });
    }
    let window = window();
    let colors = color_channels(result);
    let mut total = 0.0;
    for c in 0..colors {
        let (x, y) = (Plane::channel(result, c)?, Plane::channel(truth, c)?);
        total += channel_ssim(&x, &y, &window);
    }
    Ok(total / colors as f64)
}

/// The mean of the SSIM map of two planes of the same size.
fn channel_ssim(x: &Plane, y: &Plane, window: &[f64]) -> f64 {
    let product = |f: fn(f64, f64) -> f64| Plane { w: x.w, h: x.h, data: x.data.iter().zip(&y.data).map(|(&a, &b)| f(a, b)).collect() };
    let mu_x = x.filter_valid(window);
    let mu_y = y.filter_valid(window);
    let xx = product(|a, _| a * a).filter_valid(window);
    let yy = product(|_, b| b * b).filter_valid(window);
    let xy = product(|a, b| a * b).filter_valid(window);
    let mut sum = 0.0;
    for i in 0..mu_x.data.len() {
        let (mx, my) = (mu_x.data[i], mu_y.data[i]);
        let (vx, vy, cov) = (xx.data[i] - mx * mx, yy.data[i] - my * my, xy.data[i] - mx * my);
        sum += ((2.0 * mx * my + C1) * (2.0 * cov + C2)) / ((mx * mx + my * my + C1) * (vx + vy + C2));
    }
    sum / mu_x.data.len() as f64
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)] // Test data: f64 draws narrowed to f32 samples.
mod tests {
    use florastack_core::ColorSpace;
    use florastack_core::testutil::Rng;

    use super::*;

    fn random_gray(w: usize, h: usize, seed: u64) -> Image<f32> {
        let mut rng = Rng::new(seed);
        Image::from_fn(w, h, 1, ColorSpace::LINEAR, |_, _, _| rng.uniform() as f32).unwrap()
    }

    fn add_noise(image: &Image<f32>, sigma: f64, seed: u64) -> Image<f32> {
        let mut rng = Rng::new(seed);
        let data = image.as_raw().iter().map(|&v| v + (sigma * rng.normal()) as f32).collect();
        Image::from_vec(image.width(), image.height(), image.channels(), ColorSpace::LINEAR, data).unwrap()
    }

    /// The paper's equations evaluated directly at every window position, as a check on the separable version.
    fn direct(x: &Image<f32>, y: &Image<f32>) -> f64 {
        let w = window();
        let (cols, rows) = (x.width() - WINDOW + 1, x.height() - WINDOW + 1);
        let mut sum = 0.0;
        for top in 0..rows {
            for left in 0..cols {
                let (mut mx, mut my, mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
                for j in 0..WINDOW {
                    for i in 0..WINDOW {
                        let wt = w[i] * w[j];
                        let a = f64::from(x.get(left + i, top + j, 0).unwrap());
                        let b = f64::from(y.get(left + i, top + j, 0).unwrap());
                        mx += wt * a;
                        my += wt * b;
                        sxx += wt * a * a;
                        syy += wt * b * b;
                        sxy += wt * a * b;
                    }
                }
                let (vx, vy, cov) = (sxx - mx * mx, syy - my * my, sxy - mx * my);
                sum += ((2.0 * mx * my + C1) * (2.0 * cov + C2)) / ((mx * mx + my * my + C1) * (vx + vy + C2));
            }
        }
        sum / (cols * rows) as f64
    }

    #[test]
    fn identical_images_score_one() {
        let x = random_gray(32, 24, 1);
        assert!((ssim(&x, &x).unwrap() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn constant_images_follow_the_luminance_term_alone() {
        // With no variance the structure term is C2 / C2 = 1, leaving (2ab + C1) / (a^2 + b^2 + C1).
        let (a, b) = (0.25f32, 0.5f32);
        let x = Image::from_fn(16, 16, 3, ColorSpace::LINEAR, |_, _, _| a).unwrap();
        let y = Image::from_fn(16, 16, 3, ColorSpace::LINEAR, |_, _, _| b).unwrap();
        let (a, b) = (f64::from(a), f64::from(b));
        let expected = (2.0 * a * b + C1) / (a * a + b * b + C1);
        assert!((ssim(&x, &y).unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn the_separable_version_matches_the_papers_equations() {
        let x = random_gray(24, 20, 2);
        let y = add_noise(&x, 0.1, 3);
        let fast = ssim(&x, &y).unwrap();
        assert!((fast - direct(&x, &y)).abs() < 1e-10, "{fast} vs {}", direct(&x, &y));
        assert!((fast - ssim(&y, &x).unwrap()).abs() < 1e-12, "symmetric");
    }

    #[test]
    fn more_noise_scores_lower() {
        let x = random_gray(40, 40, 4);
        let scores: Vec<f64> = [0.01, 0.05, 0.2].iter().map(|&s| ssim(&add_noise(&x, s, 5), &x).unwrap()).collect();
        assert!(scores[0] > scores[1] && scores[1] > scores[2] && scores[0] < 1.0, "{scores:?}");
    }

    #[test]
    fn images_smaller_than_the_window_are_errors() {
        let x = random_gray(10, 30, 6);
        assert_eq!(ssim(&x, &x), Err(MetricsError::TooSmall { metric: "SSIM", min: 11, width: 10, height: 30 }));
    }
}
