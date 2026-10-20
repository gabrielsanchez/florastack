//! Conversions between sample types and between encodings. All stacking math runs on linear `Image<f32>`:
//! [`Image::to_linear_f32`] brings any input there, and [`Image::to_encoding`] takes a result back to what a file
//! format stores.
//!
//! - **Sample types** ([`Image::convert`]): samples are fractions of white ([`Sample::to_normalized`]), so `255u8`,
//!   `65535u16` and `1.0f32` are all white. Integer targets round to nearest and clamp; every `u8` and `u16` value
//!   survives a trip through `f32` exactly.
//! - **Encodings**: the sRGB transfer curve of IEC 61966-2-1:1999, decoded from `u8` and `u16` through tables built
//!   once in `f64` (exact to the nearest `f32`) and computed otherwise with [`srgb_to_linear`] and [`linear_to_srgb`].
//!   Their power function is evaluated from the exponent and mantissa bits of an `f64` with selects instead of
//!   branches and no calls into libm, so the loops over a row compile to SIMD code; it agrees with `f64::powf` to
//!   about 1e-13 relative. Negative values mirror the curve (`f(-v) = -f(v)`) and values above 1 follow it, so
//!   extended range survives; NaN stays NaN. Every `u8` and `u16` value survives decoding to linear `f32` and encoding
//!   back exactly.
//! - The alpha channel (the second of two, the fourth of four) is only normalized, never transfer-encoded. The ICC
//!   profile is carried over unchanged: it describes the primaries, and a profile whose curve isn't sRGB's needs the
//!   color management of the codecs crate.
//!
//! Rows are converted in parallel with rayon; each sample's result depends on that sample alone, so the output doesn't
//! depend on scheduling.

use std::sync::OnceLock;

use rayon::prelude::*;

use crate::image::{ColorSpace, Encoding, Image, ImageError, Sample};

/// The encoded value below which the sRGB curve is linear (IEC 61966-2-1).
const DECODE_KNEE: f64 = 0.040_45;
/// The linear value below which the sRGB curve is linear.
const ENCODE_KNEE: f64 = 0.003_130_8;
/// The slope of the linear segment.
const SLOPE: f64 = 12.92;
/// The offset and the gamma of the power segment.
const OFFSET: f64 = 0.055;
const GAMMA: f64 = 2.4;

/// An sRGB-encoded value decoded to linear light (IEC 61966-2-1), mirrored for negative values.
pub fn srgb_to_linear(v: f32) -> f32 {
    to_f32(decode(f64::from(v)))
}

/// A linear value encoded with the sRGB curve (IEC 61966-2-1), mirrored for negative values.
pub fn linear_to_srgb(v: f32) -> f32 {
    to_f32(encode(f64::from(v)))
}

/// [`srgb_to_linear`] on every value, in place.
pub fn srgb_to_linear_slice(values: &mut [f32]) {
    for v in values {
        *v = to_f32(decode(f64::from(*v)));
    }
}

/// [`linear_to_srgb`] on every value, in place.
pub fn linear_to_srgb_slice(values: &mut [f32]) {
    for v in values {
        *v = to_f32(encode(f64::from(*v)));
    }
}

/// The sRGB decoding curve, mirrored for negative values. Both segments are computed and one is selected, so that a
/// loop over this function vectorizes.
#[inline]
fn decode(v: f64) -> f64 {
    let a = v.abs();
    let linear = a / SLOPE;
    let power = pow_positive((a.max(DECODE_KNEE) + OFFSET) / (1.0 + OFFSET), GAMMA);
    let r = if a <= DECODE_KNEE { linear } else { power };
    // `max` above drops NaN, so it is put back here.
    if v.is_nan() { v } else { r.copysign(v) }
}

/// The sRGB encoding curve, mirrored for negative values.
#[inline]
fn encode(v: f64) -> f64 {
    let a = v.abs();
    let linear = a * SLOPE;
    let power = (1.0 + OFFSET) * pow_positive(a.max(ENCODE_KNEE), 1.0 / GAMMA) - OFFSET;
    let r = if a <= ENCODE_KNEE { linear } else { power };
    if v.is_nan() { v } else { r.copysign(v) }
}

/// `x^p` for a positive, finite, normal `x` with `p log2 x` in `[-1022, 1023]`; other inputs give some finite value,
/// except that NaN stays NaN. Computed as `exp2(p log2 x)` with [`log2`] and [`exp2`].
#[inline]
fn pow_positive(x: f64, p: f64) -> f64 {
    let r = exp2(p * log2(x));
    // The bits of NaN read as a finite exponent and mantissa, so NaN is put back explicitly (a select, like the rest).
    if x.is_nan() { x } else { r }
}

/// `atanh(s) / s = 1 + s^2/3 + s^4/5 + ...`, as a polynomial in `s^2`: the coefficients `1 / (2k + 1)`.
const ATANH_SERIES: [f64; 8] = [1.0, 1.0 / 3.0, 1.0 / 5.0, 1.0 / 7.0, 1.0 / 9.0, 1.0 / 11.0, 1.0 / 13.0, 1.0 / 15.0];

/// The Taylor series of `e^z` up to `z^11`: the coefficients `1 / k!`.
const EXP_SERIES: [f64; 12] = [
    1.0,
    1.0,
    1.0 / 2.0,
    1.0 / 6.0,
    1.0 / 24.0,
    1.0 / 120.0,
    1.0 / 720.0,
    1.0 / 5040.0,
    1.0 / 40_320.0,
    1.0 / 362_880.0,
    1.0 / 3_628_800.0,
    1.0 / 39_916_800.0,
];

/// The polynomial with `coefficients` (constant term first) at `x`, by Horner's rule. Over a constant array the loop
/// unrolls completely, so it vectorizes with the loop around it.
#[inline]
fn horner(x: f64, coefficients: &[f64]) -> f64 {
    coefficients.iter().rev().fold(0.0, |acc, &c| acc * x + c)
}

/// `log2 x` for a positive normal `x`: `x = m 2^e` with `m` in `[sqrt(1/2), sqrt(2))`, read from the bits, and
/// `ln m = 2 atanh(s) = 2 (s + s^3/3 + s^5/5 + ...)` with `s = (m - 1) / (m + 1)`, `|s| < 0.172` (Abramowitz and Stegun,
/// Handbook of Mathematical Functions, 4.1.27). The terms up to `s^15` leave an error below 1e-14.
#[inline]
fn log2(x: f64) -> f64 {
    const MANTISSA: u64 = (1 << 52) - 1;
    const ONE: u64 = 0x3ff0_0000_0000_0000;
    // 2^52 as an f64, and its bits: OR-ing a small integer into the low mantissa bits and subtracting 2^52 converts
    // it to an f64 exactly, without the 64-bit integer-to-float conversion that x86 can vectorize only from AVX-512.
    const TWO_52: f64 = 4_503_599_627_370_496.0;
    const TWO_52_BITS: u64 = 0x4330_0000_0000_0000;
    let bits = x.to_bits();
    let biased = f64::from_bits(TWO_52_BITS | ((bits >> 52) & 0x7ff)) - TWO_52;
    let m = f64::from_bits((bits & MANTISSA) | ONE);
    let high = m > std::f64::consts::SQRT_2;
    let m = if high { m * 0.5 } else { m };
    let e = biased - 1023.0 + if high { 1.0 } else { 0.0 };
    let s = (m - 1.0) / (m + 1.0);
    e + 2.0 * s * horner(s * s, &ATANH_SERIES) * std::f64::consts::LOG2_E
}

/// `2^y` for `y` in `[-1022, 1023]` (clamped to it; NaN stays NaN): `2^n` built from its bits for `n = round(y)`, times
/// `e^(f ln 2)` for the rest `f` in `[-0.5, 0.5]` from its Taylor series, whose terms past the 11th are below 1e-14.
#[inline]
fn exp2(y: f64) -> f64 {
    // Adding 1.5 * 2^52 rounds to an integer (to nearest, the IEEE default) and leaves it in the low mantissa bits.
    const ROUND: f64 = 6_755_399_441_055_744.0;
    const ROUND_BITS: u64 = 0x4338_0000_0000_0000;
    let y = y.clamp(-1022.0, 1023.0);
    let shifted = y + ROUND;
    let n = shifted - ROUND;
    let z = (y - n) * std::f64::consts::LN_2;
    let scale = f64::from_bits(shifted.to_bits().wrapping_sub(ROUND_BITS).wrapping_add(1023) << 52);
    scale * horner(z, &EXP_SERIES)
}

/// `v` rounded to the nearest `f32` (overflowing to infinity).
#[allow(clippy::cast_possible_truncation)] // f64 to f32 rounds; that is the intent.
fn to_f32(v: f64) -> f32 {
    v as f32
}

/// The sRGB decoding of every `u8` value, built once.
fn srgb_u8_table() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| to_f32(decode_exact(i as f64 / 255.0))))
}

/// The sRGB decoding of every `u16` value, built once (256 KiB). `None` only if the table has the wrong length, which
/// can't happen.
fn srgb_u16_table() -> Option<&'static [f32; 65536]> {
    static TABLE: OnceLock<Box<[f32]>> = OnceLock::new();
    let table = TABLE.get_or_init(|| (0..=u16::MAX).map(|i| to_f32(decode_exact(f64::from(i) / 65535.0))).collect());
    table.as_ref().try_into().ok()
}

/// The sRGB decoding curve with `f64::powf`, for the tables (and as the tests' reference).
fn decode_exact(v: f64) -> f64 {
    let a = v.abs();
    let r = if a <= DECODE_KNEE { a / SLOPE } else { ((a + OFFSET) / (1.0 + OFFSET)).powf(GAMMA) };
    r.copysign(v)
}

/// What a conversion does to the color channels' samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transfer {
    Keep,
    Decode,
    Encode,
}

impl Transfer {
    fn between(from: Encoding, to: Encoding) -> Transfer {
        match (from, to) {
            (Encoding::Srgb, Encoding::Linear) => Transfer::Decode,
            (Encoding::Linear, Encoding::Srgb) => Transfer::Encode,
            _ => Transfer::Keep,
        }
    }
}

/// The index of the alpha channel among `channels`, if there is one.
fn alpha_channel(channels: usize) -> Option<usize> {
    match channels {
        2 => Some(1),
        4 => Some(3),
        _ => None,
    }
}

/// Decodes sRGB-encoded `u8` samples to normalized linear values, through the table.
pub(crate) fn decode_u8_row(src: &[u8], out: &mut [f32]) {
    let table = srgb_u8_table();
    for (o, &s) in out.iter_mut().zip(src) {
        // A u8 always indexes a 256-entry table.
        *o = table[usize::from(s)];
    }
}

/// Decodes sRGB-encoded `u16` samples to normalized linear values, through the table.
pub(crate) fn decode_u16_row(src: &[u16], out: &mut [f32]) {
    if let Some(table) = srgb_u16_table() {
        for (o, &s) in out.iter_mut().zip(src) {
            *o = table[usize::from(s)];
        }
    } else {
        for (o, &s) in out.iter_mut().zip(src) {
            *o = to_f32(decode(f64::from(s) / 65535.0));
        }
    }
}

/// Decodes sRGB-encoded `f32` samples to linear values.
pub(crate) fn decode_f32_row(src: &[f32], out: &mut [f32]) {
    for (o, &s) in out.iter_mut().zip(src) {
        *o = to_f32(decode(f64::from(s)));
    }
}

impl<T: Sample> Image<T> {
    /// The same image with samples of type `U` and the same color space: every sample normalized (white stays
    /// white), rounded to nearest and clamped for an integer `U`.
    pub fn convert<U: Sample>(&self) -> Result<Image<U>, ImageError> {
        self.to_encoding(self.color_space().encoding)
    }

    /// The image as linear `f32`, what stacking computes on: sRGB-encoded samples decoded, linear ones normalized.
    pub fn to_linear_f32(&self) -> Result<Image<f32>, ImageError> {
        self.to_encoding(Encoding::Linear)
    }

    /// The image with samples of type `U` in `encoding`: decoded from sRGB or encoded to it as needed, then rounded
    /// to `U`. The ICC profile is kept; the alpha channel is only normalized.
    pub fn to_encoding<U: Sample>(&self, encoding: Encoding) -> Result<Image<U>, ImageError> {
        let transfer = Transfer::between(self.color_space().encoding, encoding);
        let color = ColorSpace { encoding, icc: self.color_space().icc.clone() };
        let channels = self.channels();
        let alpha = alpha_channel(channels);
        let mut out = Image::<U>::new(self.width(), self.height(), channels, color)?;
        out.par_rows_mut().zip(self.par_rows()).for_each_init(Vec::new, |scratch, (dst, src)| {
            convert_row(src, dst, channels, alpha, transfer, scratch);
        });
        Ok(out)
    }
}

impl Image<f32> {
    /// Re-encodes the samples in place to `encoding` and retags the image (keeping its ICC profile): no allocation,
    /// for images too large to copy. The alpha channel is left as it is.
    pub fn convert_encoding(&mut self, encoding: Encoding) {
        let transfer = Transfer::between(self.color_space().encoding, encoding);
        let channels = self.channels();
        let curve = match transfer {
            Transfer::Keep => None,
            Transfer::Decode => Some(decode as fn(f64) -> f64),
            Transfer::Encode => Some(encode as fn(f64) -> f64),
        };
        if let Some(curve) = curve {
            let alpha = alpha_channel(channels);
            self.par_rows_mut().for_each(|row| match alpha {
                None if transfer == Transfer::Decode => srgb_to_linear_slice(row),
                None => linear_to_srgb_slice(row),
                Some(a) => {
                    for pixel in row.chunks_exact_mut(channels) {
                        for (i, v) in pixel.iter_mut().enumerate() {
                            if i != a {
                                *v = to_f32(curve(f64::from(*v)));
                            }
                        }
                    }
                }
            });
        }
        let icc = self.color_space().icc.clone();
        self.set_color_space(ColorSpace { encoding, icc });
    }
}

/// Converts one row of `channels`-interleaved samples, with `scratch` as working space for the curves.
fn convert_row<T: Sample, U: Sample>(src: &[T], dst: &mut [U], channels: usize, alpha: Option<usize>, transfer: Transfer, scratch: &mut Vec<f32>) {
    if transfer == Transfer::Keep {
        for (d, &s) in dst.iter_mut().zip(src) {
            *d = U::from_normalized(s.to_normalized());
        }
        return;
    }
    scratch.clear();
    scratch.resize(src.len(), 0.0);
    if transfer == Transfer::Decode {
        T::srgb_decode(src, scratch);
    } else {
        for (t, &s) in scratch.iter_mut().zip(src) {
            *t = s.to_normalized();
        }
        linear_to_srgb_slice(scratch);
    }
    if let Some(a) = alpha {
        for (t, s) in scratch.chunks_exact_mut(channels).zip(src.chunks_exact(channels)) {
            if let (Some(t), Some(s)) = (t.get_mut(a), s.get(a)) {
                *t = s.to_normalized();
            }
        }
    }
    for (d, &t) in dst.iter_mut().zip(scratch.iter()) {
        *d = U::from_normalized(t);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Values from `lo` to `hi`, spaced evenly on a log scale.
    fn log_sweep(lo: f64, hi: f64, n: usize) -> impl Iterator<Item = f64> {
        (0..n).map(move |i| lo * (hi / lo).powf(i as f64 / (n - 1) as f64))
    }

    /// The standard curves with `f64::powf`.
    fn encode_exact(v: f64) -> f64 {
        let a = v.abs();
        let r = if a <= ENCODE_KNEE { a * SLOPE } else { (1.0 + OFFSET) * a.powf(1.0 / GAMMA) - OFFSET };
        r.copysign(v)
    }

    #[test]
    fn the_bitwise_power_function_matches_powf() {
        for p in [GAMMA, 1.0 / GAMMA, 0.5, 3.7, -1.25] {
            for x in log_sweep(1e-3, 1e6, 20_011) {
                let (fast, exact) = (pow_positive(x, p), x.powf(p));
                assert!(((fast - exact) / exact).abs() < 1e-13, "{x}^{p}: {fast} vs {exact}");
            }
        }
        assert_eq!(pow_positive(1.0, GAMMA), 1.0);
        assert_eq!(log2(8.0), 3.0);
        assert_eq!(exp2(-3.0), 0.125);
        assert!(pow_positive(f64::NAN, GAMMA).is_nan());
    }

    #[test]
    fn the_curves_agree_with_the_standard_to_the_nearest_f32() {
        let mut worst = 0u32;
        for i in 0..=200_000 {
            let v = -1.0 + 4.0 * f64::from(i) / 200_000.0;
            let v32 = to_f32(v);
            for (fast, exact) in [(srgb_to_linear(v32), decode_exact(f64::from(v32))), (linear_to_srgb(v32), encode_exact(f64::from(v32)))] {
                let exact = to_f32(exact);
                worst = worst.max(fast.to_bits().abs_diff(exact.to_bits()));
            }
        }
        assert!(worst <= 1, "within one f32 ulp everywhere, not {worst}");
    }

    #[test]
    #[allow(clippy::float_cmp)] // The endpoints are exact by construction.
    fn black_and_white_are_exact_and_the_knee_is_continuous() {
        assert_eq!((srgb_to_linear(0.0), srgb_to_linear(1.0), linear_to_srgb(0.0), linear_to_srgb(1.0)), (0.0, 1.0, 0.0, 1.0));
        let below = encode_exact(ENCODE_KNEE);
        let above = encode(ENCODE_KNEE * (1.0 + 1e-12));
        assert!((below - above).abs() < 1e-6, "{below} {above}");
        assert!((decode(DECODE_KNEE) - decode(DECODE_KNEE * (1.0 + 1e-12))).abs() < 1e-7);
        // Monotonic over the whole nominal range.
        let encoded: Vec<f32> = (0..=4096).map(|i| linear_to_srgb(i as f32 / 4096.0)).collect();
        assert!(encoded.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn extended_range_mirrors_and_nan_stays_nan() {
        for v in [0.002f32, 0.3, 0.9, 2.5, 40.0] {
            assert_eq!(srgb_to_linear(-v), -srgb_to_linear(v));
            assert_eq!(linear_to_srgb(-v), -linear_to_srgb(v));
            let back = srgb_to_linear(linear_to_srgb(v));
            assert!(((back - v) / v).abs() < 1e-6, "{v} -> {back}");
        }
        assert!(linear_to_srgb(4.0) > 1.0);
        assert!(srgb_to_linear(f32::NAN).is_nan() && linear_to_srgb(f32::NAN).is_nan());
        assert_eq!((srgb_to_linear(f32::INFINITY), linear_to_srgb(f32::NEG_INFINITY)), (f32::INFINITY, f32::NEG_INFINITY));
        let mut values = [0.25f32, -0.5, 1.5];
        linear_to_srgb_slice(&mut values);
        assert_eq!(values, [linear_to_srgb(0.25), linear_to_srgb(-0.5), linear_to_srgb(1.5)]);
        srgb_to_linear_slice(&mut values);
        assert!((values[2] - 1.5).abs() < 1e-6);
    }

    #[test]
    fn every_integer_sample_survives_linear_f32_and_back() {
        let all_u16 = Image::from_vec(256, 256, 1, ColorSpace::SRGB, (0..=u16::MAX).collect()).unwrap();
        let linear = all_u16.to_linear_f32().unwrap();
        assert_eq!(linear.color_space(), &ColorSpace::LINEAR);
        assert_eq!(linear.to_encoding::<u16>(Encoding::Srgb).unwrap(), all_u16);
        let all_u8 = Image::from_vec(16, 16, 1, ColorSpace::SRGB, (0..=u8::MAX).collect()).unwrap();
        let linear = all_u8.to_linear_f32().unwrap();
        assert_eq!(linear.to_encoding::<u8>(Encoding::Srgb).unwrap(), all_u8);
        // Through linear u16 too: 8-bit sRGB keeps all its levels in 16-bit linear... except the darkest, which
        // linear 16-bit can't separate (the first steps of sRGB are smaller than 1/65535).
        let as_u16 = all_u8.to_encoding::<u16>(Encoding::Linear).unwrap();
        let back = as_u16.to_encoding::<u8>(Encoding::Srgb).unwrap();
        let wrong = back.as_raw().iter().zip(all_u8.as_raw()).filter(|(a, b)| a != b).count();
        assert!(wrong <= 2, "{wrong} levels lost");
        // Linear samples are only normalized.
        let lin = Image::from_vec(256, 256, 1, ColorSpace::LINEAR, (0..=u16::MAX).collect()).unwrap();
        assert_eq!(lin.convert::<f32>().unwrap().convert::<u16>().unwrap(), lin);
        assert_eq!(lin.convert::<f32>().unwrap().get(255, 255, 0), Some(1.0));
    }

    #[test]
    fn decoding_matches_the_tables_and_the_formula() {
        let image = Image::from_fn(300, 2, 3, ColorSpace::SRGB, |x, y, c| (x * 211 + y * 17 + c * 5) as f32 / 1000.0).unwrap();
        let linear = image.to_linear_f32().unwrap();
        for (a, b) in linear.as_raw().iter().zip(image.as_raw()) {
            assert_eq!(*a, srgb_to_linear(*b));
        }
        let u8_image = Image::from_fn(256, 1, 1, ColorSpace::SRGB, |x, _, _| u8::try_from(x).unwrap()).unwrap();
        let from_u8 = u8_image.to_linear_f32().unwrap();
        for (x, v) in from_u8.as_raw().iter().enumerate() {
            assert_eq!(*v, to_f32(decode_exact(x as f64 / 255.0)), "u8 {x}");
        }
    }

    #[test]
    fn alpha_is_only_normalized_and_the_profile_is_kept() {
        let icc: Arc<[u8]> = Arc::from(&b"profile"[..]);
        let color = ColorSpace { encoding: Encoding::Srgb, icc: Some(icc.clone()) };
        let rgba = Image::from_vec(1, 1, 4, color, vec![128u8, 128, 128, 128]).unwrap();
        let linear = rgba.to_linear_f32().unwrap();
        assert_eq!(linear.color_space(), &ColorSpace { encoding: Encoding::Linear, icc: Some(icc) });
        let pixel = linear.pixel(0, 0).unwrap();
        assert_eq!(pixel[0], to_f32(decode_exact(128.0 / 255.0)));
        assert_eq!(pixel[3], 128.0 / 255.0, "alpha is coverage, not light");
        let gray_alpha = Image::from_vec(1, 1, 2, ColorSpace::LINEAR, vec![0.25f32, 0.25]).unwrap();
        let encoded = gray_alpha.to_encoding::<u16>(Encoding::Srgb).unwrap();
        assert_eq!(encoded.pixel(0, 0), Some(&[u16::from_normalized(linear_to_srgb(0.25)), 16384][..]));
    }

    #[test]
    fn in_place_conversion_matches_the_copying_one() {
        for channels in 1..=4 {
            let image = Image::from_fn(37, 5, channels, ColorSpace::LINEAR, |x, y, c| (x + 3 * y + c) as f32 / 50.0 - 0.1).unwrap();
            let mut in_place = image.clone();
            in_place.convert_encoding(Encoding::Srgb);
            assert_eq!(in_place, image.to_encoding::<f32>(Encoding::Srgb).unwrap(), "{channels} channels");
            in_place.convert_encoding(Encoding::Srgb);
            assert_eq!(in_place.color_space().encoding, Encoding::Srgb, "already sRGB: nothing changes");
            in_place.convert_encoding(Encoding::Linear);
            assert!(in_place.as_raw().iter().zip(image.as_raw()).all(|(a, b)| (a - b).abs() < 1e-6));
        }
    }
}
