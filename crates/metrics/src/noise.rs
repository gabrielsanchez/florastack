//! Noise in flat regions, estimated with Immerkær's method.
//!
//! J. Immerkær, "Fast noise variance estimation", Computer Vision and Image Understanding 64(2), 300-302, 1996: the
//! mask `[[1, -2, 1], [-2, 4, -2], [1, -2, 1]]` (a difference of two Laplacians) cancels every image that is linear
//! in x or in y, so on smooth ground its response is noise alone, and for Gaussian noise of standard deviation
//! sigma the mean absolute response is `6 sigma sqrt(2 / pi)`. Immerkær sums over the whole image; texture and edges
//! bias that up, so here the sum runs only over flat pixels: those whose 3x3 neighborhood in a reference image
//! varies by less than `flat_threshold` in luminance (largest minus smallest value). For a synthetic stack the
//! reference is the noise-free ground truth; without one, pass the image itself and a threshold well above its noise
//! (the range of nine noisy samples is about three standard deviations).

use florastack_core::Image;

use crate::plane::Plane;
use crate::{MetricsError, check_pair, color_channels};

/// Which pixels count as flat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseParams {
    /// A pixel is flat when the reference's luminance varies by less than this over the pixel and its eight
    /// neighbors.
    pub flat_threshold: f32,
}

impl Default for NoiseParams {
    /// 0.01: right for a noise-free reference such as a synthetic stack's ground truth.
    fn default() -> Self {
        NoiseParams { flat_threshold: 0.01 }
    }
}

/// The noise estimate.
#[derive(Clone, Debug, PartialEq)]
pub struct Noise {
    /// The pixels the estimate is taken over.
    pub flat_pixels: usize,
    /// The estimated noise standard deviation of each color channel.
    pub per_channel: Vec<f64>,
    /// Their root mean square: one number for the image.
    pub sigma: f64,
}

/// The `[[1, -2, 1], [-2, 4, -2], [1, -2, 1]]` mask; its squared weights sum to 36.
const MASK: [[f64; 3]; 3] = [[1.0, -2.0, 1.0], [-2.0, 4.0, -2.0], [1.0, -2.0, 1.0]];

/// Estimates the noise of `image` over the regions that are flat in `reference` (the same shape; the image itself
/// is a valid reference).
pub fn noise(image: &Image<f32>, reference: &Image<f32>, params: &NoiseParams) -> Result<Noise, MetricsError> {
    check_pair(image, reference)?;
    let (w, h) = (image.width(), image.height());
    if w < 3 || h < 3 {
        return Err(MetricsError::TooSmall { metric: "the noise estimate", min: 3, width: w, height: h });
    }
    let luminance = Plane::luminance(reference)?;
    let threshold = f64::from(params.flat_threshold);
    // Interior pixels whose 3x3 neighborhood is flat, so that the mask never straddles an edge or texture.
    let flat: Vec<usize> = (1..h - 1)
        .flat_map(|y| (1..w - 1).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let around = (y - 1..=y + 1).flat_map(|j| (x - 1..=x + 1).map(move |i| (i, j))).map(|(i, j)| luminance.at(i, j));
            let (lo, hi) = around.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
            hi - lo < threshold
        })
        .map(|(x, y)| y * w + x)
        .collect();
    if flat.is_empty() {
        return Err(MetricsError::NoFlatRegion(params.flat_threshold));
    }
    let scale = (std::f64::consts::PI / 2.0).sqrt() / 6.0;
    let mut per_channel = Vec::with_capacity(color_channels(image));
    for c in 0..color_channels(image) {
        let plane = Plane::channel(image, c)?;
        let mut sum = 0.0;
        for &p in &flat {
            let (x, y) = (p % w, p / w);
            let mut response = 0.0;
            for (j, row) in MASK.iter().enumerate() {
                for (i, m) in row.iter().enumerate() {
                    response += m * plane.at(x + i - 1, y + j - 1);
                }
            }
            sum += response.abs();
        }
        per_channel.push(scale * sum / flat.len() as f64);
    }
    let sigma = (per_channel.iter().map(|s| s * s).sum::<f64>() / per_channel.len() as f64).sqrt();
    Ok(Noise { flat_pixels: flat.len(), per_channel, sigma })
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)] // Test data: f64 draws narrowed to f32 samples.
mod tests {
    use florastack_core::ColorSpace;
    use florastack_core::testutil::Rng;

    use super::*;

    fn noisy(clean: &Image<f32>, sigma: f64, seed: u64) -> Image<f32> {
        let mut rng = Rng::new(seed);
        let data = clean.as_raw().iter().map(|&v| v + (sigma * rng.normal()) as f32).collect();
        Image::from_vec(clean.width(), clean.height(), clean.channels(), ColorSpace::LINEAR, data).unwrap()
    }

    #[test]
    fn the_estimate_recovers_gaussian_noise_on_flat_and_smooth_ground() {
        // A smooth ramp: the mask cancels it, so only the noise is measured.
        let clean = Image::from_fn(128, 128, 3, ColorSpace::LINEAR, |x, y, c| 0.2 + 0.001 * (x + 2 * y) as f32 + 0.1 * c as f32).unwrap();
        for sigma in [0.005, 0.02] {
            let n = noise(&noisy(&clean, sigma, 1), &clean, &NoiseParams::default()).unwrap();
            assert_eq!(n.flat_pixels, 126 * 126);
            assert!(n.per_channel.iter().all(|s| (s / sigma - 1.0).abs() < 0.05), "{sigma}: {n:?}");
            assert!((n.sigma / sigma - 1.0).abs() < 0.05);
        }
        assert!(noise(&clean, &clean, &NoiseParams::default()).unwrap().sigma < 1e-6);
    }

    #[test]
    fn edges_in_the_reference_are_left_out() {
        // Half dark, half bright: the edge would dominate a whole-image estimate.
        let clean = Image::from_fn(64, 64, 1, ColorSpace::LINEAR, |x, _, _| if x < 32 { 0.1 } else { 0.9 }).unwrap();
        let image = noisy(&clean, 0.01, 2);
        let n = noise(&image, &clean, &NoiseParams::default()).unwrap();
        assert_eq!(n.flat_pixels, 62 * 62 - 2 * 62, "the two columns at the step are out");
        assert!((n.sigma / 0.01 - 1.0).abs() < 0.08, "{n:?}");
        // The image as its own reference, with a threshold above its noise, finds the same flat ground.
        let own = noise(&image, &image, &NoiseParams { flat_threshold: 0.1 }).unwrap();
        assert!((own.sigma / 0.01 - 1.0).abs() < 0.15, "{own:?}");
    }

    #[test]
    fn no_flat_ground_is_an_error() {
        let checker = Image::from_fn(8, 8, 1, ColorSpace::LINEAR, |x, y, _| if (x + y) % 2 == 0 { 0.0 } else { 1.0 }).unwrap();
        assert_eq!(noise(&checker, &checker, &NoiseParams::default()), Err(MetricsError::NoFlatRegion(0.01)));
        let tiny = Image::from_fn(2, 8, 1, ColorSpace::LINEAR, |_, _, _| 0.5).unwrap();
        assert!(matches!(noise(&tiny, &tiny, &NoiseParams::default()), Err(MetricsError::TooSmall { .. })));
    }
}
