//! Color shift: the CIEDE2000 color difference between a result and its ground truth, in CIELAB.
//!
//! - sRGB transfer curve and primaries: IEC 61966-2-1:1999. The RGB-to-XYZ matrix is the standard one for linear
//!   sRGB with a D65 white, and the white point is its image of RGB white, so neutral grays have `a = b = 0`.
//! - CIELAB: CIE 15:2004, Colorimetry, 3rd edition.
//! - CIEDE2000: G. Sharma, W. Wu and E. N. Dalal, "The CIEDE2000 color-difference formula: implementation notes,
//!   supplementary test data, and mathematical observations", Color Research and Application 30(1), 21-30, 2005,
//!   with `kL = kC = kH = 1`. The tests check every pair of the paper's Table 1.

use florastack_core::{Encoding, Image};

use crate::{MetricsError, check_pair, color_channels};

/// A CIELAB color (D65 white).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

/// Linear sRGB to CIE XYZ (D65).
const RGB_TO_XYZ: [[f64; 3]; 3] =
    [[0.412_456_4, 0.357_576_1, 0.180_437_5], [0.212_672_9, 0.715_152_2, 0.072_175_0], [0.019_333_9, 0.119_192_0, 0.950_304_1]];

/// CIELAB's `6/29`: below its cube, `f(t)` is a line instead of a cube root.
const DELTA: f64 = 6.0 / 29.0;

/// An sRGB-encoded value decoded to linear light (IEC 61966-2-1).
pub fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Linear sRGB (D65) to CIELAB. Values outside `[0, 1]` are extrapolated rather than clipped.
pub fn linear_rgb_to_lab(rgb: [f64; 3]) -> Lab {
    let xyz: [f64; 3] = std::array::from_fn(|i| RGB_TO_XYZ[i].iter().zip(rgb).map(|(m, v)| m * v).sum());
    let white: [f64; 3] = std::array::from_fn(|i| RGB_TO_XYZ[i].iter().sum());
    let f = |t: f64| if t > DELTA * DELTA * DELTA { t.cbrt() } else { t / (3.0 * DELTA * DELTA) + 4.0 / 29.0 };
    let [fx, fy, fz] = std::array::from_fn(|i| f(xyz[i] / white[i]));
    Lab { l: 116.0 * fy - 16.0, a: 500.0 * (fx - fy), b: 200.0 * (fy - fz) }
}

/// The CIEDE2000 difference between two colors (Sharma, Wu and Dalal 2005, equations 1 to 22).
pub fn delta_e2000(lab1: Lab, lab2: Lab) -> f64 {
    let pow7_25 = 25f64.powi(7);
    let (c1, c2) = (lab1.a.hypot(lab1.b), lab2.a.hypot(lab2.b));
    let c_bar7 = f64::midpoint(c1, c2).powi(7);
    let g = 0.5 * (1.0 - (c_bar7 / (c_bar7 + pow7_25)).sqrt());
    let (a1, a2) = ((1.0 + g) * lab1.a, (1.0 + g) * lab2.a);
    let (c1, c2) = (a1.hypot(lab1.b), a2.hypot(lab2.b));
    let hue = |b: f64, a: f64| if b == 0.0 && a == 0.0 { 0.0 } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
    let (h1, h2) = (hue(lab1.b, a1), hue(lab2.b, a2));

    let dl = lab2.l - lab1.l;
    let dc = c2 - c1;
    let chroma_product = c1 * c2;
    let dh = if chroma_product == 0.0 {
        0.0
    } else if (h2 - h1).abs() <= 180.0 {
        h2 - h1
    } else if h2 - h1 > 180.0 {
        h2 - h1 - 360.0
    } else {
        h2 - h1 + 360.0
    };
    let dh_big = 2.0 * chroma_product.sqrt() * (dh / 2.0).to_radians().sin();

    let l_bar = f64::midpoint(lab1.l, lab2.l);
    let c_bar = f64::midpoint(c1, c2);
    let h_bar = if chroma_product == 0.0 {
        h1 + h2
    } else if (h1 - h2).abs() <= 180.0 {
        f64::midpoint(h1, h2)
    } else if h1 + h2 < 360.0 {
        (h1 + h2 + 360.0) / 2.0
    } else {
        (h1 + h2 - 360.0) / 2.0
    };
    let cos = |degrees: f64| degrees.to_radians().cos();
    let t = 1.0 - 0.17 * cos(h_bar - 30.0) + 0.24 * cos(2.0 * h_bar) + 0.32 * cos(3.0 * h_bar + 6.0) - 0.20 * cos(4.0 * h_bar - 63.0);
    let d_theta = 30.0 * (-((h_bar - 275.0) / 25.0).powi(2)).exp();
    let c_bar7 = c_bar.powi(7);
    let r_c = 2.0 * (c_bar7 / (c_bar7 + pow7_25)).sqrt();
    let l50 = (l_bar - 50.0).powi(2);
    let s_l = 1.0 + 0.015 * l50 / (20.0 + l50).sqrt();
    let s_c = 1.0 + 0.045 * c_bar;
    let s_h = 1.0 + 0.015 * c_bar * t;
    let r_t = -(2.0 * d_theta).to_radians().sin() * r_c;
    let (l, c, h) = (dl / s_l, dc / s_c, dh_big / s_h);
    (l * l + c * c + h * h + r_t * c * h).sqrt()
}

/// The CIEDE2000 differences between a result and its ground truth, over every pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeltaE {
    /// The mean difference: about 1 is just noticeable side by side, under 0.5 invisible.
    pub mean: f64,
    /// The largest difference at any pixel.
    pub max: f64,
}

/// The color shift of `result` against `truth`: CIEDE2000 per pixel after converting both to CIELAB from their
/// encoding (gray images as R = G = B).
pub fn color_shift(result: &Image<f32>, truth: &Image<f32>) -> Result<DeltaE, MetricsError> {
    check_pair(result, truth)?;
    let encoding = result.color_space().encoding;
    let (c, colors) = (result.channels(), color_channels(result));
    let to_lab = |pixel: &[f32]| -> Option<Lab> {
        let mut rgb = [0.0; 3];
        for (i, v) in rgb.iter_mut().enumerate() {
            let sample = f64::from(*pixel.get(if colors == 3 { i } else { 0 })?);
            if !sample.is_finite() {
                return None;
            }
            *v = match encoding {
                Encoding::Linear => sample,
                Encoding::Srgb => srgb_to_linear(sample),
            };
        }
        Some(linear_rgb_to_lab(rgb))
    };
    let (mut sum, mut max) = (0.0, 0.0f64);
    for (a, b) in result.rows().zip(truth.rows()) {
        for (pa, pb) in a.chunks_exact(c).zip(b.chunks_exact(c)) {
            let (Some(la), Some(lb)) = (to_lab(pa), to_lab(pb)) else {
                return Err(MetricsError::NonFinite);
            };
            let d = delta_e2000(la, lb);
            sum += d;
            max = max.max(d);
        }
    }
    Ok(DeltaE { mean: sum / result.pixel_count() as f64, max })
}

#[cfg(test)]
mod tests {
    use florastack_core::ColorSpace;

    use super::*;

    /// Table 1 of Sharma, Wu and Dalal (2005): (L1, a1, b1), (L2, a2, b2) and their CIEDE2000 difference.
    const SHARMA: [([f64; 3], [f64; 3], f64); 34] = [
        ([50.0000, 2.6772, -79.7751], [50.0000, 0.0000, -82.7485], 2.0425),
        ([50.0000, 3.1571, -77.2803], [50.0000, 0.0000, -82.7485], 2.8615),
        ([50.0000, 2.8361, -74.0200], [50.0000, 0.0000, -82.7485], 3.4412),
        ([50.0000, -1.3802, -84.2814], [50.0000, 0.0000, -82.7485], 1.0000),
        ([50.0000, -1.1848, -84.8006], [50.0000, 0.0000, -82.7485], 1.0000),
        ([50.0000, -0.9009, -85.5211], [50.0000, 0.0000, -82.7485], 1.0000),
        ([50.0000, 0.0000, 0.0000], [50.0000, -1.0000, 2.0000], 2.3669),
        ([50.0000, -1.0000, 2.0000], [50.0000, 0.0000, 0.0000], 2.3669),
        ([50.0000, 2.4900, -0.0010], [50.0000, -2.4900, 0.0009], 7.1792),
        ([50.0000, 2.4900, -0.0010], [50.0000, -2.4900, 0.0010], 7.1792),
        ([50.0000, 2.4900, -0.0010], [50.0000, -2.4900, 0.0011], 7.2195),
        ([50.0000, 2.4900, -0.0010], [50.0000, -2.4900, 0.0012], 7.2195),
        ([50.0000, -0.0010, 2.4900], [50.0000, 0.0009, -2.4900], 4.8045),
        ([50.0000, -0.0010, 2.4900], [50.0000, 0.0010, -2.4900], 4.8045),
        ([50.0000, -0.0010, 2.4900], [50.0000, 0.0011, -2.4900], 4.7461),
        ([50.0000, 2.5000, 0.0000], [50.0000, 0.0000, -2.5000], 4.3065),
        ([50.0000, 2.5000, 0.0000], [73.0000, 25.0000, -18.0000], 27.1492),
        ([50.0000, 2.5000, 0.0000], [61.0000, -5.0000, 29.0000], 22.8977),
        ([50.0000, 2.5000, 0.0000], [56.0000, -27.0000, -3.0000], 31.9030),
        ([50.0000, 2.5000, 0.0000], [58.0000, 24.0000, 15.0000], 19.4535),
        ([50.0000, 2.5000, 0.0000], [50.0000, 3.1736, 0.5854], 1.0000),
        ([50.0000, 2.5000, 0.0000], [50.0000, 3.2972, 0.0000], 1.0000),
        ([50.0000, 2.5000, 0.0000], [50.0000, 1.8634, 0.5757], 1.0000),
        ([50.0000, 2.5000, 0.0000], [50.0000, 3.2592, 0.3350], 1.0000),
        ([60.2574, -34.0099, 36.2677], [60.4626, -34.1751, 39.4387], 1.2644),
        ([63.0109, -31.0961, -5.8663], [62.8187, -29.7946, -4.0864], 1.2630),
        ([61.2901, 3.7196, -5.3901], [61.4292, 2.2480, -4.9620], 1.8731),
        ([35.0831, -44.1164, 3.7933], [35.0232, -40.0716, 1.5901], 1.8645),
        ([22.7233, 20.0904, -46.6940], [23.0331, 14.9730, -42.5619], 2.0373),
        ([36.4612, 47.8580, 18.3852], [36.2715, 50.5065, 21.2231], 1.4146),
        ([90.8027, -2.0831, 1.4410], [91.1528, -1.6435, 0.0447], 1.4441),
        ([90.9257, -0.5406, -0.9208], [88.6381, -0.8985, -0.7239], 1.5381),
        ([6.7747, -0.2908, -2.4247], [5.8714, -0.0985, -2.2286], 0.6377),
        ([2.0776, 0.0795, -1.1350], [0.9033, -0.0636, -0.5514], 0.9082),
    ];

    fn lab([l, a, b]: [f64; 3]) -> Lab {
        Lab { l, a, b }
    }

    #[test]
    fn ciede2000_matches_every_pair_of_sharmas_table() {
        for (i, (x, y, expected)) in SHARMA.iter().enumerate() {
            let d = delta_e2000(lab(*x), lab(*y));
            // The table gives four decimals.
            assert!((d - expected).abs() < 1e-4, "pair {}: {d:.6} vs {expected}", i + 1);
            assert!((delta_e2000(lab(*y), lab(*x)) - d).abs() < 1e-12, "pair {}: symmetric", i + 1);
        }
        assert_eq!(delta_e2000(lab([40.0, 10.0, -5.0]), lab([40.0, 10.0, -5.0])), 0.0);
    }

    #[test]
    fn srgb_colors_convert_to_their_known_lab_values() {
        let white = linear_rgb_to_lab([1.0, 1.0, 1.0]);
        assert!((white.l - 100.0).abs() < 1e-9 && white.a.abs() < 1e-9 && white.b.abs() < 1e-9, "{white:?}");
        // 18% gray: L* = 116 * 0.18^(1/3) - 16.
        let gray = linear_rgb_to_lab([0.18; 3]);
        assert!((gray.l - (116.0 * 0.18f64.cbrt() - 16.0)).abs() < 1e-9 && gray.a.abs() < 1e-9);
        // sRGB red, green and blue primaries (D65), as tabulated by the usual converters to two decimals.
        for (rgb, expected) in
            [([1.0, 0.0, 0.0], [53.24, 80.09, 67.20]), ([0.0, 1.0, 0.0], [87.73, -86.18, 83.18]), ([0.0, 0.0, 1.0], [32.30, 79.19, -107.86])]
        {
            let got = linear_rgb_to_lab(rgb);
            assert!(
                (got.l - expected[0]).abs() < 0.01 && (got.a - expected[1]).abs() < 0.01 && (got.b - expected[2]).abs() < 0.01,
                "{rgb:?}: {got:?}"
            );
        }
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-12 && srgb_to_linear(0.0) == 0.0);
        assert!((srgb_to_linear(0.5) - 0.214_041_14).abs() < 1e-8);
    }

    #[test]
    fn color_shift_is_zero_for_the_truth_and_grows_with_a_tint() {
        let truth = Image::from_fn(8, 8, 3, ColorSpace::LINEAR, |x, y, c| 0.1 + 0.02 * (x + y + c) as f32).unwrap();
        assert_eq!(color_shift(&truth, &truth).unwrap(), DeltaE { mean: 0.0, max: 0.0 });
        let tint = |amount: f32| {
            Image::from_fn(8, 8, 3, ColorSpace::LINEAR, |x, y, c| 0.1 + 0.02 * (x + y + c) as f32 + if c == 0 { amount } else { 0.0 }).unwrap()
        };
        let (small, large) = (color_shift(&tint(0.005), &truth).unwrap(), color_shift(&tint(0.05), &truth).unwrap());
        assert!(small.mean > 0.0 && small.mean < large.mean && large.max >= large.mean, "{small:?} {large:?}");
        // The same colors stored sRGB-encoded give the same differences.
        let encode = |image: &Image<f32>| {
            let to_srgb = |v: f32| if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
            let mut srgb = Image::from_vec(8, 8, 3, ColorSpace::SRGB, image.as_raw().iter().map(|&v| to_srgb(v)).collect()).unwrap();
            srgb.set_color_space(ColorSpace::SRGB);
            srgb
        };
        let encoded = color_shift(&encode(&tint(0.05)), &encode(&truth)).unwrap();
        assert!((encoded.mean - large.mean).abs() < 1e-3, "{encoded:?} vs {large:?}");
        // Gray images are compared as neutral colors.
        let gray = Image::from_fn(4, 4, 1, ColorSpace::LINEAR, |_, _, _| 0.18).unwrap();
        let lighter = Image::from_fn(4, 4, 1, ColorSpace::LINEAR, |_, _, _| 0.2).unwrap();
        assert!(color_shift(&lighter, &gray).unwrap().mean > 1.0);
    }
}
