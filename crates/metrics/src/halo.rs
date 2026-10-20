//! The halo metric: how far the result departs from the ground truth in a ring around the truth's high-contrast
//! edges.
//!
//! Stacking methods go wrong next to strong edges between depth layers: pyramid methods overshoot into bright or
//! dark fringes, and depth-map methods smear the out-of-focus side of an edge over its background. Both put error
//! energy in a band a few pixels wide beside the edge that the ground truth doesn't have. The metric finds the
//! truth's edges (Sobel gradient of the luminance at least `min_contrast`), takes the ring of pixels whose
//! chessboard distance to an edge pixel is more than `inner_radius` and at most `outer_radius` (a two-pass distance
//! transform: A. Rosenfeld and J. L. Pfaltz, "Sequential operations in digital picture processing", Journal of the
//! ACM 13(4), 1966), and measures the mean squared luminance error there.

use florastack_core::Image;

use crate::plane::Plane;
use crate::{MetricsError, check_pair};

/// Where the ring around the edges lies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HaloParams {
    /// The smallest step contrast that counts as an edge: the Sobel gradient of the truth's luminance, scaled so that
    /// a step from 0.2 to 0.7 measures 0.5.
    pub min_contrast: f32,
    /// Pixels at most this far from an edge are left out of the ring (0 keeps the pixels right next to it).
    pub inner_radius: u32,
    /// The ring's outer edge, in pixels (chessboard distance).
    pub outer_radius: u32,
}

impl Default for HaloParams {
    /// Edges of at least 0.25 contrast and a ring out to 6 pixels, starting next to the edge.
    fn default() -> Self {
        HaloParams { min_contrast: 0.25, inner_radius: 0, outer_radius: 6 }
    }
}

/// The halo measurement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Halo {
    /// Pixels of the truth's high-contrast edges.
    pub edge_pixels: usize,
    /// Pixels in the ring around them.
    pub ring_pixels: usize,
    /// The mean squared luminance difference from the truth over the ring: 0 without halos.
    pub energy: f64,
    /// The root of `energy` as a fraction of the edges' mean contrast: 0.1 is a fringe whose RMS is a tenth of the
    /// edge's step, comparable across scenes.
    pub relative: f64,
}

/// Measures the halos of `result` around the high-contrast edges of `truth`.
pub fn halo(result: &Image<f32>, truth: &Image<f32>, params: &HaloParams) -> Result<Halo, MetricsError> {
    check_pair(result, truth)?;
    let (r, t) = (Plane::luminance(result)?, Plane::luminance(truth)?);
    let gradient = t.sobel();
    let threshold = f64::from(params.min_contrast);
    let edges: Vec<bool> = gradient.data.iter().map(|&g| g >= threshold).collect();
    let edge_pixels = edges.iter().filter(|&&e| e).count();
    if edge_pixels == 0 {
        return Err(MetricsError::NoEdges(params.min_contrast));
    }
    let contrast = gradient.data.iter().zip(&edges).filter(|(_, e)| **e).map(|(g, _)| g).sum::<f64>() / edge_pixels as f64;
    let distance = chessboard_distance(&edges, t.w, t.h);
    let (mut sum, mut ring_pixels) = (0.0, 0usize);
    for ((&d, a), b) in distance.iter().zip(&r.data).zip(&t.data) {
        if d > params.inner_radius && d <= params.outer_radius {
            sum += (a - b).powi(2);
            ring_pixels += 1;
        }
    }
    let energy = if ring_pixels == 0 { 0.0 } else { sum / ring_pixels as f64 };
    Ok(Halo { edge_pixels, ring_pixels, energy, relative: energy.sqrt() / contrast })
}

/// Each pixel's chessboard distance to the nearest `true` pixel of `mask` (`w * h`, row by row), saturating at
/// `u32::MAX` where there is none.
fn chessboard_distance(mask: &[bool], w: usize, h: usize) -> Vec<u32> {
    let mut d: Vec<u32> = mask.iter().map(|&m| if m { 0 } else { u32::MAX }).collect();
    let at = |d: &[u32], x: usize, y: usize| d.get(y * w + x).copied().unwrap_or(u32::MAX);
    // Forward: the neighbors above and to the left. Backward: below and to the right.
    for y in 0..h {
        for x in 0..w {
            let mut best = at(&d, x, y);
            if x > 0 {
                best = best.min(at(&d, x - 1, y).saturating_add(1));
            }
            if y > 0 {
                best = best.min(at(&d, x, y - 1).saturating_add(1));
                if x > 0 {
                    best = best.min(at(&d, x - 1, y - 1).saturating_add(1));
                }
                if x + 1 < w {
                    best = best.min(at(&d, x + 1, y - 1).saturating_add(1));
                }
            }
            d[y * w + x] = best;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let mut best = at(&d, x, y);
            if x + 1 < w {
                best = best.min(at(&d, x + 1, y).saturating_add(1));
            }
            if y + 1 < h {
                best = best.min(at(&d, x, y + 1).saturating_add(1));
                if x + 1 < w {
                    best = best.min(at(&d, x + 1, y + 1).saturating_add(1));
                }
                if x > 0 {
                    best = best.min(at(&d, x - 1, y + 1).saturating_add(1));
                }
            }
            d[y * w + x] = best;
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use florastack_core::ColorSpace;

    use super::*;

    /// A vertical step from 0.2 to 0.8 between columns 15 and 16 of a 32x16 gray image, plus `fringe(x)`.
    fn step(fringe: impl Fn(usize) -> f32) -> Image<f32> {
        Image::from_fn(32, 16, 1, ColorSpace::LINEAR, |x, _, _| if x < 16 { 0.2 } else { 0.8 } + fringe(x)).unwrap()
    }

    #[test]
    fn the_truth_has_no_halo_and_a_fringe_does() {
        let truth = step(|_| 0.0);
        let clean = halo(&truth, &truth, &HaloParams::default()).unwrap();
        assert_eq!((clean.edge_pixels, clean.energy, clean.relative), (32, 0.0, 0.0), "{clean:?}");
        // The step's two columns are edges; the ring is the 6 columns on each side.
        assert_eq!(clean.ring_pixels, 12 * 16);
        // Overshoot: brighter just right of the step, darker just left, by 0.06 (a tenth of the step).
        let fringed = step(|x| match x {
            12..=14 => -0.06,
            17..=19 => 0.06,
            _ => 0.0,
        });
        let h = halo(&fringed, &truth, &HaloParams::default()).unwrap();
        assert!((h.energy - 0.06f64.powi(2) * 6.0 / 12.0).abs() < 1e-8, "{h:?}");
        assert!((h.relative - (h.energy.sqrt() / 0.6)).abs() < 1e-7, "{h:?}");
    }

    #[test]
    fn changes_far_from_the_edges_are_not_halos() {
        let truth = step(|_| 0.0);
        let far = step(|x| if (3..=28).contains(&x) { 0.0 } else { 0.1 });
        assert_eq!(halo(&far, &truth, &HaloParams::default()).unwrap().energy, 0.0);
        let near = HaloParams { inner_radius: 2, ..HaloParams::default() };
        let touching = step(|x| if x == 14 || x == 17 { 0.1 } else { 0.0 });
        assert_eq!(halo(&touching, &truth, &near).unwrap().energy, 0.0, "within the inner radius");
    }

    #[test]
    fn a_scene_without_strong_edges_is_an_error() {
        let flat = Image::from_fn(16, 16, 3, ColorSpace::LINEAR, |x, _, _| x as f32 * 0.01).unwrap();
        assert_eq!(halo(&flat, &flat, &HaloParams::default()), Err(MetricsError::NoEdges(0.25)));
    }

    #[test]
    fn chessboard_distances_count_diagonal_steps_as_one() {
        let mut mask = vec![false; 25];
        mask[12] = true;
        let d = chessboard_distance(&mask, 5, 5);
        assert_eq!(d, [2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 1, 0, 1, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2]);
        assert!(chessboard_distance(&[false; 4], 2, 2).iter().all(|&v| v == u32::MAX));
    }
}
