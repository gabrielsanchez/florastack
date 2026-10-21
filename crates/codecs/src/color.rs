//! Color management: ICC profiles, the curves that encode samples, and conversions between profiles.
//!
//! [`Profile`] is an ICC profile with what this crate needs from it. [`ColorEngine`] does the work on images:
//! linearizing samples through a profile's curves, converting linear light between two profiles' primaries, and
//! encoding linear light with a profile's curves. The engine here, [`LightCraftColor`], parses and writes profiles
//! with LightCraft's codecs crate (which uses moxcms), takes its curves from there and its matrices from
//! lightcraft-color; profiles that need a full CMS (LUT-based, CMYK) are converted by LightCraft's decoder as the file
//! is read (`crate::decode::fallback`).
//!
//! The rules that tie this to [`florastack_core::ColorSpace`]:
//!
//! - **Reading**: a file without color information is sRGB when its samples are integers, linear (sRGB primaries)
//!   when they are floats. A profile whose curve is sRGB's or linear keeps the samples as they are, tagged with that
//!   encoding and the profile. A profile with any other curve (Adobe RGB's 2.2 gamma, ProPhoto's 1.8, a table) is
//!   applied as the file is read: the samples come back as linear `f32`, with the profile kept for its primaries and
//!   so that the output can be encoded with its curve again.
//! - **Writing** ([`crate::OutputProfile`]): integer files are encoded with the target profile's curve and carry the
//!   profile, except that an untagged sRGB input stays untagged; float files are linear, with a linear profile of the
//!   target's primaries (none for sRGB primaries, which untagged floats already mean). Gray images have no primaries:
//!   they keep a gray profile of their own, or none.

use std::sync::Arc;

use florastack_core::{AnyImage, ColorSpace, Encoding, Image, Sample};
use lightcraft_codecs::Trc;
use lightcraft_codecs::icc::{self, IccColorModel, IccKind};
use lightcraft_color::{Mat3, Xy};
use rayon::prelude::*;

use crate::error::{BackendError, CodecError, Warning};
use crate::options::{BitDepth, OutputProfile, WideSpace};

/// A standard RGB space that a profile's primaries match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NamedSpace {
    Srgb,
    DisplayP3,
    AdobeRgb,
    ProPhoto,
    Rec2020,
}

impl NamedSpace {
    pub fn name(self) -> &'static str {
        match self {
            NamedSpace::Srgb => "sRGB",
            NamedSpace::DisplayP3 => "Display P3",
            NamedSpace::AdobeRgb => "Adobe RGB (1998)",
            NamedSpace::ProPhoto => "ProPhoto RGB",
            NamedSpace::Rec2020 => "Rec. 2020",
        }
    }

    fn lightcraft(self) -> lightcraft_codecs::NamedSpace {
        use lightcraft_codecs::NamedSpace as Lc;
        match self {
            NamedSpace::Srgb => Lc::Srgb,
            NamedSpace::DisplayP3 => Lc::DisplayP3,
            NamedSpace::AdobeRgb => Lc::AdobeRgb,
            NamedSpace::ProPhoto => Lc::ProPhoto,
            NamedSpace::Rec2020 => Lc::Rec2020,
        }
    }

    fn from_lightcraft(space: lightcraft_codecs::NamedSpace) -> NamedSpace {
        use lightcraft_codecs::NamedSpace as Lc;
        match space {
            Lc::Srgb => NamedSpace::Srgb,
            Lc::DisplayP3 => NamedSpace::DisplayP3,
            Lc::AdobeRgb => NamedSpace::AdobeRgb,
            Lc::ProPhoto => NamedSpace::ProPhoto,
            Lc::Rec2020 => NamedSpace::Rec2020,
        }
    }
}

/// What a profile's curve is, as far as the samples' encoding goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveKind {
    /// The sRGB curve (IEC 61966-2-1), which [`Encoding::Srgb`] means.
    Srgb,
    /// No curve: linear light.
    Linear,
    /// A pure power law with this exponent (2.2 for Adobe RGB, 1.8 for ProPhoto RGB).
    Gamma(f32),
    /// Another curve: parametric, a table, or BT.709's.
    Other,
}

/// CIE xy chromaticities of three primaries and a white point, as PNG `cHRM` chunks and OpenEXR files give them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Primaries {
    pub red: (f64, f64),
    pub green: (f64, f64),
    pub blue: (f64, f64),
    pub white: (f64, f64),
}

impl Primaries {
    /// sRGB's: BT.709 primaries and a D65 white.
    pub const SRGB: Primaries = Primaries { red: (0.64, 0.33), green: (0.30, 0.60), blue: (0.15, 0.06), white: (0.3127, 0.3290) };

    /// Whether these are sRGB's, to the precision the files store them with.
    pub fn is_srgb(&self) -> bool {
        let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
        let s = Primaries::SRGB;
        close(self.red, s.red) && close(self.green, s.green) && close(self.blue, s.blue) && close(self.white, s.white)
    }

    /// The standard space with these chromaticities, if there is one.
    fn named(&self) -> Option<NamedSpace> {
        let close = |a: (f64, f64), b: Xy| (a.0 - b.x).abs() < 1e-3 && (a.1 - b.y).abs() < 1e-3;
        [NamedSpace::Srgb, NamedSpace::DisplayP3, NamedSpace::AdobeRgb, NamedSpace::ProPhoto, NamedSpace::Rec2020].into_iter().find(|n| {
            let s = n.lightcraft().rgb_space();
            close(self.red, s.r) && close(self.green, s.g) && close(self.blue, s.b) && close(self.white, s.white)
        })
    }

    /// Linear RGB in these primaries to CIE XYZ relative to D50, as ICC profiles store their colorants: the primaries'
    /// xyz columns scaled so that RGB white is the white point, then adapted from it to D50 (Bradford). Built from
    /// xyz rather than XYZ columns, so that a primary on the x axis (OpenEXR's XYZ files put blue at (0, 0)) works.
    /// `None` when the chromaticities don't make a space.
    fn colorants_d50(self) -> Option<Mat3> {
        let all = [self.red, self.green, self.blue, self.white];
        if all.iter().any(|&(x, y)| !(x.is_finite() && y.is_finite() && x >= 0.0 && y >= 0.0 && x + y <= 1.0 + 1e-6)) || self.white.1 <= 0.0 {
            return None;
        }
        let column = |(x, y): (f64, f64)| [x, y, 1.0 - x - y];
        let (r, g, b) = (column(self.red), column(self.green), column(self.blue));
        let primaries = Mat3([[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]]);
        let (xw, yw) = self.white;
        let white = [xw / yw, 1.0, (1.0 - xw - yw) / yw];
        let scale = primaries.inverse()?.apply(white);
        let to_xyz = primaries.mul(&Mat3::diag(scale[0], scale[1], scale[2]));
        let adapted = lightcraft_color::bradford(Xy::new(xw, yw), lightcraft_color::D50).mul(&to_xyz);
        (adapted.determinant().abs() > 1e-9 && adapted.0.iter().flatten().all(|v| v.is_finite())).then_some(adapted)
    }
}

/// How a profile can be applied.
#[derive(Clone, Debug)]
enum Kind {
    /// An RGB matrix/curve profile: colorants (linear RGB to XYZ relative to D50) and a curve per channel.
    Matrix { to_xyz_d50: Mat3, curves: [Trc; 3] },
    /// A gray profile with one curve.
    Gray { curve: Trc },
    /// LUT-based, CMYK or Lab: only a full color-management system can apply it.
    NeedsCms,
}

/// An ICC profile: its bytes, kept as they are so that output can carry them over, and what this crate reads from
/// them.
#[derive(Clone, Debug)]
pub struct Profile {
    bytes: Arc<[u8]>,
    kind: Kind,
    named: Option<NamedSpace>,
    description: Option<String>,
}

impl PartialEq for Profile {
    /// Two profiles are the same when their bytes are.
    fn eq(&self, other: &Profile) -> bool {
        self.bytes == other.bytes
    }
}

impl Profile {
    /// The profile's bytes, as they are embedded in files.
    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    /// Whether this is a gray (monochrome) profile.
    pub fn is_gray(&self) -> bool {
        matches!(self.kind, Kind::Gray { .. })
    }

    /// Whether only a full color-management system can apply this profile (LUT-based, CMYK or Lab).
    pub fn needs_cms(&self) -> bool {
        matches!(self.kind, Kind::NeedsCms)
    }

    /// The standard space the profile's primaries match, if one does.
    pub fn named(&self) -> Option<NamedSpace> {
        self.named
    }

    /// The profile's description (its name), if it has one.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The kind of curve the profile encodes samples with (the green channel's, for an RGB profile).
    pub fn curve_kind(&self) -> CurveKind {
        match self.curve() {
            Some(curve) => curve_kind(curve),
            None => CurveKind::Other,
        }
    }

    /// The curve of the first color channel (green for RGB), if the profile is matrix/curve or gray.
    fn curve(&self) -> Option<&Trc> {
        match &self.kind {
            Kind::Matrix { curves, .. } => curves.get(1),
            Kind::Gray { curve } => Some(curve),
            Kind::NeedsCms => None,
        }
    }

    /// The curve of each color channel of an image with `color` color channels (1 or 3).
    fn curves(&self, color: usize) -> Option<Vec<Trc>> {
        match (&self.kind, color) {
            (Kind::Matrix { curves, .. }, 3) => Some(curves.to_vec()),
            // Gray samples with an RGB profile: the neutral axis needs the green curve only.
            (Kind::Matrix { curves, .. }, _) => curves.get(1).map(|c| vec![c.clone()]),
            (Kind::Gray { curve }, 1) => Some(vec![curve.clone()]),
            _ => None,
        }
    }

    fn to_xyz_d50(&self) -> Option<&Mat3> {
        match &self.kind {
            Kind::Matrix { to_xyz_d50, .. } => Some(to_xyz_d50),
            _ => None,
        }
    }

    /// Whether every channel's curve is the sRGB curve, or linear: what [`Encoding`] can say without the profile.
    fn encoding(&self) -> Option<Encoding> {
        let curves = match &self.kind {
            Kind::Matrix { curves, .. } => curves.as_slice(),
            Kind::Gray { curve } => std::slice::from_ref(curve),
            Kind::NeedsCms => return None,
        };
        let kinds: Vec<CurveKind> = curves.iter().map(curve_kind).collect();
        if kinds.iter().all(|k| *k == CurveKind::Srgb) {
            Some(Encoding::Srgb)
        } else if kinds.iter().all(|k| *k == CurveKind::Linear) {
            Some(Encoding::Linear)
        } else {
            None
        }
    }
}

/// How close a profile's curve must be to sRGB's, in linear light, to count as sRGB: profiles store their curves
/// with 16-bit tables or 16.16 fixed-point parameters, which keeps them within 1e-5 of it.
const SRGB_TOLERANCE: f32 = 5e-4;

fn curve_kind(curve: &Trc) -> CurveKind {
    if curve.is_linear() {
        CurveKind::Linear
    } else if *curve == Trc::Srgb || curve.approx_eq(&Trc::Srgb, SRGB_TOLERANCE) {
        CurveKind::Srgb
    } else {
        match curve {
            Trc::Gamma(g) => CurveKind::Gamma(*g),
            Trc::Parametric(p) if p.len() == 1 => p.first().map_or(CurveKind::Other, |g| CurveKind::Gamma(*g)),
            _ => CurveKind::Other,
        }
    }
}

/// A curve for a profile FloraStack writes itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StandardCurve {
    /// The space's own curve (sRGB's for sRGB, 1.8 gamma for ProPhoto, BT.709's for Rec. 2020, ...).
    Native,
    Linear,
}

/// Color management on FloraStack's types: the one place that knows how profiles are applied.
pub trait ColorEngine: Send + Sync {
    /// Reads an ICC profile.
    fn parse(&self, icc: &[u8]) -> Result<Profile, CodecError>;

    /// The profile of a standard space with its own curve or a linear one.
    fn standard(&self, space: NamedSpace, curve: StandardCurve) -> Result<Profile, CodecError>;

    /// `image`'s samples, encoded with `profile`'s curves, as linear light: normalized `f32` with the alpha channel
    /// only normalized. The result is tagged linear, with `profile` as its ICC profile.
    fn linearize(&self, image: &AnyImage, profile: &Profile) -> Result<Image<f32>, CodecError>;

    /// Converts linear light in `from`'s primaries to `to`'s, in place (the alpha channel is left alone). Gray images
    /// have no primaries and are left as they are.
    fn convert(&self, image: &mut Image<f32>, from: &Profile, to: &Profile) -> Result<(), CodecError>;

    /// Linear light encoded with `profile`'s curves, as samples of `depth`; integers clip to their range.
    fn encode(&self, image: &Image<f32>, profile: &Profile, depth: BitDepth) -> Result<AnyImage, CodecError>;
}

/// The color engine built on LightCraft's crates.
#[derive(Clone, Copy, Debug, Default)]
pub struct LightCraftColor;

impl ColorEngine for LightCraftColor {
    fn parse(&self, icc: &[u8]) -> Result<Profile, CodecError> {
        let info = icc::parse(icc).ok_or_else(|| CodecError::Profile { source: BackendError::message("not a profile FloraStack can read") })?;
        let kind = match info.kind {
            IccKind::MatrixTrc { to_xyz_d50, trc } if info.model == IccColorModel::Rgb => Kind::Matrix { to_xyz_d50, curves: trc },
            IccKind::Gray { trc } if info.model == IccColorModel::Gray => Kind::Gray { curve: trc },
            _ => Kind::NeedsCms,
        };
        Ok(Profile { bytes: Arc::from(icc), kind, named: info.named.map(NamedSpace::from_lightcraft), description: info.description })
    }

    fn standard(&self, space: NamedSpace, curve: StandardCurve) -> Result<Profile, CodecError> {
        let lc = space.lightcraft();
        let trc = match curve {
            StandardCurve::Native => lc.trc(),
            StandardCurve::Linear => Trc::Linear,
        };
        // Parsed back from the bytes, so that this profile and the one a reader finds in the file are the same.
        self.parse(&icc::write_matrix_trc(&lc.rgb_space(), &trc))
    }

    fn linearize(&self, image: &AnyImage, profile: &Profile) -> Result<Image<f32>, CodecError> {
        let color = color_channels(image.channels());
        let curves = profile.curves(color).ok_or_else(|| CodecError::Profile { source: BackendError::message("its curves don't fit the image") })?;
        let cs = ColorSpace { encoding: Encoding::Linear, icc: Some(profile.bytes.clone()) };
        let out = match image {
            AnyImage::U8(i) => linearize_with_tables(i, &curves, 255, cs)?,
            AnyImage::U16(i) => linearize_with_tables(i, &curves, 65535, cs)?,
            AnyImage::F32(i) => {
                let mut out = i.clone();
                out.set_color_space(cs);
                let channels = out.channels();
                out.par_rows_mut().for_each(|row| {
                    for pixel in row.chunks_exact_mut(channels) {
                        for (v, curve) in pixel.iter_mut().zip(&curves) {
                            *v = curve.to_linear(*v);
                        }
                    }
                });
                out
            }
        };
        Ok(out)
    }

    fn convert(&self, image: &mut Image<f32>, from: &Profile, to: &Profile) -> Result<(), CodecError> {
        if image.channels() < 3 || from == to {
            return Ok(());
        }
        let (Some(src), Some(dst)) = (from.to_xyz_d50(), to.to_xyz_d50()) else {
            return Err(CodecError::Profile { source: BackendError::message("only matrix/curve profiles convert between primaries") });
        };
        let to_rgb =
            dst.inverse().ok_or_else(|| CodecError::Profile { source: BackendError::message("the target profile's colorants are singular") })?;
        let m = to_rgb.mul(src);
        if is_identity(&m) {
            return Ok(());
        }
        let channels = image.channels();
        image.par_rows_mut().for_each(|row| {
            for pixel in row.chunks_exact_mut(channels) {
                if let Some(rgb) = pixel.get_mut(..3) {
                    let out = m.apply_f32([rgb[0], rgb[1], rgb[2]]);
                    rgb.copy_from_slice(&out);
                }
            }
        });
        Ok(())
    }

    fn encode(&self, image: &Image<f32>, profile: &Profile, depth: BitDepth) -> Result<AnyImage, CodecError> {
        let color = color_channels(image.channels());
        let curves = profile.curves(color).ok_or_else(|| CodecError::Profile { source: BackendError::message("its curves don't fit the image") })?;
        let cs = ColorSpace { encoding: profile.encoding().unwrap_or(Encoding::Linear), icc: Some(profile.bytes.clone()) };
        Ok(match depth {
            BitDepth::U8 => AnyImage::U8(encode_with_tables::<u8>(image, &curves, 255, cs)?),
            BitDepth::U16 => AnyImage::U16(encode_with_tables::<u16>(image, &curves, 65535, cs)?),
            BitDepth::F32 => {
                let mut out = image.clone();
                out.set_color_space(cs);
                let channels = out.channels();
                out.par_rows_mut().for_each(|row| {
                    for pixel in row.chunks_exact_mut(channels) {
                        for (v, curve) in pixel.iter_mut().zip(&curves) {
                            *v = curve.from_linear(*v);
                        }
                    }
                });
                AnyImage::F32(out)
            }
        })
    }
}

fn is_identity(m: &Mat3) -> bool {
    m.0.iter().enumerate().all(|(r, row)| row.iter().enumerate().all(|(c, v)| (v - if r == c { 1.0 } else { 0.0 }).abs() < 1e-9))
}

/// The color channels of an image with `channels` channels: 1 for gray (with or without alpha), 3 for RGB(A).
pub(crate) fn color_channels(channels: usize) -> usize {
    if channels >= 3 { 3 } else { 1 }
}

/// The table of `curve`'s linear values for every integer code `0..=max`.
fn forward_table(curve: &Trc, max: u16) -> Vec<f32> {
    let scale = f32::from(max);
    (0..=max).map(|code| curve.to_linear(f32::from(code) / scale)).collect()
}

/// Linearizes integer samples through one table per color channel; alpha is normalized.
fn linearize_with_tables<T: Sample + Into<u32>>(image: &Image<T>, curves: &[Trc], max: u16, cs: ColorSpace) -> Result<Image<f32>, CodecError> {
    let tables: Vec<Vec<f32>> = curves.iter().map(|c| forward_table(c, max)).collect();
    let channels = image.channels();
    let mut out = Image::<f32>::new(image.width(), image.height(), channels, cs)?;
    out.par_rows_mut().zip(image.par_rows()).for_each(|(dst, src)| {
        for (d, s) in dst.chunks_exact_mut(channels).zip(src.chunks_exact(channels)) {
            for (c, (o, &v)) in d.iter_mut().zip(s).enumerate() {
                *o = match tables.get(c) {
                    Some(table) => usize::try_from(v.into()).ok().and_then(|i| table.get(i)).copied().unwrap_or(0.0),
                    // Alpha (and anything past the color channels) is coverage, not light.
                    None => v.to_normalized(),
                };
            }
        }
    });
    Ok(out)
}

/// The integer code whose linear value in `table` (increasing) is nearest to `v`; NaN and values below the first
/// entry give 0, values above the last the largest code.
fn nearest_code(table: &[f32], v: f32) -> usize {
    let last = table.len().saturating_sub(1);
    let (Some(&first), Some(&top)) = (table.first(), table.last()) else { return 0 };
    if v.is_nan() || v <= first {
        return 0;
    }
    if v >= top {
        return last;
    }
    let i = table.partition_point(|&t| t < v).clamp(1, last);
    let (lo, hi) = (table.get(i - 1).copied().unwrap_or(first), table.get(i).copied().unwrap_or(top));
    if v - lo <= hi - v { i - 1 } else { i }
}

/// Encodes linear light with one table per color channel (inverted by binary search, so that every code that was
/// linearized through the same table comes back exactly); alpha is only quantized.
fn encode_with_tables<T: Sample + TryFrom<usize>>(image: &Image<f32>, curves: &[Trc], max: u16, cs: ColorSpace) -> Result<Image<T>, CodecError> {
    let tables: Vec<Vec<f32>> = curves.iter().map(|c| forward_table(c, max)).collect();
    let channels = image.channels();
    let mut out = Image::<T>::new(image.width(), image.height(), channels, cs)?;
    out.par_rows_mut().zip(image.par_rows()).for_each(|(dst, src)| {
        for (d, s) in dst.chunks_exact_mut(channels).zip(src.chunks_exact(channels)) {
            for (c, (o, &v)) in d.iter_mut().zip(s).enumerate() {
                *o = match tables.get(c) {
                    Some(table) => T::try_from(nearest_code(table, v)).unwrap_or_default(),
                    None => T::from_normalized(v),
                };
            }
        }
    });
    Ok(out)
}

/// The profile of an ICC-less file whose container gives a power-law curve and primaries (a PNG `gAMA` and `cHRM`).
pub(crate) fn profile_for_gamma(engine: &dyn ColorEngine, gamma: f32, primaries: Option<&Primaries>, gray: bool) -> Result<Profile, CodecError> {
    if gray {
        return gray_profile(engine, &Trc::Gamma(gamma));
    }
    profile_with_primaries(engine, primaries.unwrap_or(&Primaries::SRGB), &Trc::Gamma(gamma))
}

/// The matrix/curve profile for these chromaticities and `curve`: LightCraft's for a standard space, written with
/// moxcms from the computed colorants for any other.
fn profile_with_primaries(engine: &dyn ColorEngine, primaries: &Primaries, curve: &Trc) -> Result<Profile, CodecError> {
    if let Some(named) = primaries.named() {
        return engine.parse(&icc::write_matrix_trc(&named.lightcraft().rgb_space(), curve));
    }
    let invalid = || CodecError::Profile { source: BackendError::message("its chromaticities don't describe a color space") };
    let m = primaries.colorants_d50().ok_or_else(invalid)?;
    let colorant = |c: usize| moxcms::Xyzd { x: m.0[0][c], y: m.0[1][c], z: m.0[2][c] };
    let mut p = moxcms::ColorProfile::new_srgb();
    p.cicp = None;
    p.red_colorant = colorant(0);
    p.green_colorant = colorant(1);
    p.blue_colorant = colorant(2);
    let trc = tone_curve(curve);
    p.red_trc = Some(trc.clone());
    p.green_trc = Some(trc.clone());
    p.blue_trc = Some(trc);
    let (xw, yw) = primaries.white;
    p.chromatic_adaptation = Some(moxcms::Matrix3d { v: lightcraft_color::bradford(Xy::new(xw, yw), lightcraft_color::D50).0 });
    p.description =
        Some(moxcms::ProfileText::Localizable(vec![moxcms::LocalizableString::new("en".into(), "US".into(), "File chromaticities".into())]));
    engine.parse(&encode_profile(&p)?)
}

/// The linear profile for linear RGB with these primaries, or `None` for sRGB's, which untagged floats already mean.
pub(crate) fn linear_profile_for(engine: &dyn ColorEngine, primaries: &Primaries) -> Result<Option<Profile>, CodecError> {
    if primaries.is_srgb() {
        return Ok(None);
    }
    profile_with_primaries(engine, primaries, &Trc::Linear).map(Some)
}

/// The matrix from linear RGB in `profile`'s primaries (sRGB's when there is none) to CIE XYZ relative to D50, as ICC
/// profiles store their colorants: what a DNG writer's color tags are made from.
pub(crate) fn rgb_to_xyz_d50(profile: Option<&Profile>) -> [[f64; 3]; 3] {
    match profile.and_then(Profile::to_xyz_d50) {
        Some(m) => m.0,
        None => lightcraft_codecs::space::rgb_to_xyz_d50(&lightcraft_color::SRGB).0,
    }
}

/// The xy chromaticities of a standard space's primaries and white, if `profile` matches one: what an OpenEXR file
/// records (its readers expect the space's own white, not the D50 that ICC colorants are adapted to).
pub(crate) fn named_primaries(profile: &Profile) -> Option<Primaries> {
    let space = profile.named?.lightcraft().rgb_space();
    let xy = |p: Xy| (p.x, p.y);
    Some(Primaries { red: xy(space.r), green: xy(space.g), blue: xy(space.b), white: xy(space.white) })
}

/// The xy chromaticities of a matrix profile's primaries and of the white they are relative to (D50, the ICC
/// connection space's: profiles store their colorants adapted to it).
pub(crate) fn primaries_of(profile: &Profile) -> Option<Primaries> {
    let m = profile.to_xyz_d50()?;
    let xy = |c: usize| {
        let (x, y, z) = (m.0[0][c], m.0[1][c], m.0[2][c]);
        let sum = x + y + z;
        (sum.abs() > 1e-12).then(|| (x / sum, y / sum))
    };
    Some(Primaries { red: xy(0)?, green: xy(1)?, blue: xy(2)?, white: (lightcraft_color::D50.x, lightcraft_color::D50.y) })
}

/// A gray profile with `curve`, written with moxcms (LightCraft's codecs write RGB profiles only).
fn gray_profile(engine: &dyn ColorEngine, curve: &Trc) -> Result<Profile, CodecError> {
    let mut p = moxcms::ColorProfile::new_gray_with_gamma(1.0);
    p.gray_trc = Some(tone_curve(curve));
    p.description =
        Some(moxcms::ProfileText::Localizable(vec![moxcms::LocalizableString::new("en".into(), "US".into(), "Gray (FloraStack)".into())]));
    let bytes = encode_profile(&p)?;
    engine.parse(&bytes)
}

/// A copy of a matrix profile with linear curves: what float files written in that profile's primaries carry.
pub(crate) fn relinearized(engine: &dyn ColorEngine, profile: &Profile) -> Result<Profile, CodecError> {
    if let Some(named) = profile.named {
        return engine.standard(named, StandardCurve::Linear);
    }
    if profile.is_gray() {
        return gray_profile(engine, &Trc::Linear);
    }
    let parsed = std::panic::catch_unwind(|| moxcms::ColorProfile::new_from_slice(&profile.bytes)).ok().and_then(Result::ok);
    let Some(mut p) = parsed else {
        return Err(CodecError::Profile { source: BackendError::message("moxcms can't read it") });
    };
    let linear = moxcms::ToneReprCurve::Parametric(vec![1.0]);
    p.red_trc = Some(linear.clone());
    p.green_trc = Some(linear.clone());
    p.blue_trc = Some(linear);
    p.description = Some(moxcms::ProfileText::Localizable(vec![moxcms::LocalizableString::new(
        "en".into(),
        "US".into(),
        format!("{} (linear)", profile.description.as_deref().unwrap_or("Input profile")),
    )]));
    engine.parse(&encode_profile(&p)?)
}

/// `curve` as an ICC tone curve.
fn tone_curve(curve: &Trc) -> moxcms::ToneReprCurve {
    use moxcms::ToneReprCurve as Icc;
    match curve {
        Trc::Linear => Icc::Parametric(vec![1.0]),
        Trc::Srgb => Icc::Parametric(vec![2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.040_45]),
        Trc::Gamma(g) => Icc::Parametric(vec![*g]),
        Trc::Rec709 => Icc::Parametric(vec![1.0 / 0.45, 1.0 / 1.099, 0.099 / 1.099, 1.0 / 4.5, 0.081]),
        Trc::Parametric(p) => Icc::Parametric(p.clone()),
        Trc::Table(t) => Icc::Lut(t.clone()),
    }
}

/// Serializes a profile with moxcms, then makes its bytes depend only on its content and lays its tags out as ICC.1
/// asks: moxcms stamps the current time into the header (bytes 24 to 36, ICC.1 §7.2.8) and packs tag data back to back,
/// while ICC.1 §7.3.1 wants each tag's data on a 4-byte boundary. LightCraft's codecs make the same two corrections to
/// the RGB profiles they write.
fn encode_profile(p: &moxcms::ColorProfile) -> Result<Vec<u8>, CodecError> {
    let bytes = std::panic::catch_unwind(|| p.encode()).ok().and_then(Result::ok);
    let Some(mut bytes) = bytes else {
        return Err(CodecError::Profile { source: BackendError::message("moxcms can't write it") });
    };
    if let Some(date) = bytes.get_mut(24..36) {
        date.fill(0);
    }
    Ok(align_tag_data(&bytes).unwrap_or(bytes))
}

/// The profile with every tag's data starting on a 4-byte boundary (ICC.1 §7.3.1), its size field updated and its
/// profile ID (an MD5 of the old layout) cleared to "not computed"; `None` if the tag table is malformed.
fn align_tag_data(bytes: &[u8]) -> Option<Vec<u8>> {
    let be32 = |at: usize| bytes.get(at..at.checked_add(4)?).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let count = usize::try_from(be32(128)?).ok()?;
    let table_end = count.checked_mul(12)?.checked_add(132)?;
    let mut tags = Vec::with_capacity(count);
    for t in 0..count {
        let entry = 132 + 12 * t;
        let offset = usize::try_from(be32(entry + 4)?).ok()?;
        let size = usize::try_from(be32(entry + 8)?).ok()?;
        let end = offset.checked_add(size)?;
        if offset < table_end || end > bytes.len() {
            return None;
        }
        tags.push((offset, size));
    }
    let mut out = bytes.get(..table_end)?.to_vec();
    let mut placed: Vec<((usize, usize), usize)> = Vec::new();
    for (t, &(offset, size)) in tags.iter().enumerate() {
        let new_offset = if let Some(&(_, at)) = placed.iter().find(|(key, _)| *key == (offset, size)) {
            // Tags that share data keep sharing it.
            at
        } else {
            while out.len() % 4 != 0 {
                out.push(0);
            }
            let at = out.len();
            out.extend_from_slice(bytes.get(offset..offset + size)?);
            placed.push(((offset, size), at));
            at
        };
        let field = 132 + 12 * t + 4;
        out.get_mut(field..field + 4)?.copy_from_slice(&u32::try_from(new_offset).ok()?.to_be_bytes());
    }
    while out.len() % 4 != 0 {
        out.push(0);
    }
    let size = u32::try_from(out.len()).ok()?;
    out.get_mut(0..4)?.copy_from_slice(&size.to_be_bytes());
    out.get_mut(84..100)?.fill(0);
    Some(out)
}

/// What a file says about the color of its samples, as a decoder found it.
#[derive(Clone, Debug, PartialEq)]
pub enum ColorHint {
    /// Nothing: integer samples are sRGB, float samples linear with sRGB primaries.
    Untagged,
    /// An embedded ICC profile.
    Icc(Arc<[u8]>),
    /// The container says sRGB (a PNG `sRGB` chunk).
    Srgb,
    /// A power law with this decoding exponent (2.2 for a PNG `gAMA` of 1/2.2) and these primaries (sRGB's if none).
    Gamma { gamma: f32, primaries: Option<Primaries> },
    /// Linear light with these primaries (sRGB's if none): OpenEXR, a PNG with a `gAMA` of 1.
    Linear { primaries: Option<Primaries> },
    /// No profile, but the EXIF data says Adobe RGB (a DCF option file, `InteropIndex` R03).
    AdobeRgb,
    /// A standard space with its own curve, named by the container (a PNG `cICP` chunk).
    Standard(NamedSpace),
    /// The samples were converted to linear light in this space as they were read (files with a CMYK or LUT-based
    /// profile go through LightCraft's own decoder).
    Converted(NamedSpace),
}

/// The tagged image for a decoder's samples and color hint, by the reading rules in this module's documentation.
/// Returns the image, the profile it ended up with, and whether its samples were linearized.
pub(crate) fn tag(
    engine: &dyn ColorEngine,
    image: AnyImage,
    hint: &ColorHint,
    warnings: &mut Vec<Warning>,
) -> Result<(AnyImage, Option<Profile>, bool), CodecError> {
    let gray = image.channels() < 3;
    let float = matches!(image, AnyImage::F32(_));
    let untagged = |image: AnyImage| {
        let encoding = if float { Encoding::Linear } else { Encoding::Srgb };
        retag(image, ColorSpace { encoding, icc: None })
    };
    match hint {
        ColorHint::Srgb => Ok((retag(image, ColorSpace::SRGB), None, false)),
        ColorHint::Linear { primaries } => {
            // Untagged floats mean linear sRGB already; linear integers get a linear profile, so that they are
            // written back linear (FloraStack never writes linear integer samples untagged) and read back so.
            let profile = match (float, gray) {
                (true, true) => Ok(None),
                (true, false) => primaries.as_ref().map_or(Ok(None), |p| linear_profile_for(engine, p)),
                (false, true) => gray_profile(engine, &Trc::Linear).map(Some),
                (false, false) => profile_with_primaries(engine, primaries.as_ref().unwrap_or(&Primaries::SRGB), &Trc::Linear).map(Some),
            }
            .unwrap_or_else(|_| {
                warnings.push(Warning::ProfileIgnored("its chromaticities don't describe a color space"));
                None
            });
            let icc = profile.as_ref().map(|p| p.bytes.clone());
            Ok((retag(image, ColorSpace { encoding: Encoding::Linear, icc }), profile, false))
        }
        // Linear sRGB is what untagged floats mean already.
        ColorHint::Converted(NamedSpace::Srgb) => Ok((retag(image, ColorSpace::LINEAR), None, true)),
        ColorHint::Converted(space) => {
            let profile = engine.standard(*space, StandardCurve::Linear)?;
            let image = retag(image, ColorSpace { encoding: Encoding::Linear, icc: Some(profile.bytes.clone()) });
            Ok((image, Some(profile), true))
        }
        ColorHint::Gamma { gamma, primaries } => {
            if let Ok(profile) = profile_for_gamma(engine, *gamma, primaries.as_ref(), gray) {
                apply_profile(engine, image, profile)
            } else {
                warnings.push(Warning::ProfileIgnored("its gamma and chromaticities don't describe a color space"));
                Ok((untagged(image), None, false))
            }
        }
        ColorHint::AdobeRgb if !gray => apply_profile(engine, image, engine.standard(NamedSpace::AdobeRgb, StandardCurve::Native)?),
        ColorHint::Standard(space) if !gray => apply_profile(engine, image, engine.standard(*space, StandardCurve::Native)?),
        ColorHint::Untagged | ColorHint::AdobeRgb | ColorHint::Standard(_) => Ok((untagged(image), None, false)),
        ColorHint::Icc(bytes) => match engine.parse(bytes) {
            Err(_) => {
                warnings.push(Warning::ProfileIgnored("it can't be read"));
                Ok((untagged(image), None, false))
            }
            Ok(profile) if profile.needs_cms() => {
                // The decoders send these through LightCraft's own decoder before they get here; a profile that
                // still needs a CMS is one the fallback couldn't apply either.
                warnings.push(Warning::ProfileIgnored("it needs a full color-management system"));
                Ok((untagged(image), None, false))
            }
            Ok(profile) if profile.is_gray() && !gray => {
                warnings.push(Warning::ProfileIgnored("a gray profile on a color image"));
                Ok((untagged(image), None, false))
            }
            Ok(profile) if gray && !profile.is_gray() => {
                // An RGB profile on gray samples: its neutral axis applies, but the file must not carry RGB bytes.
                let curve = profile.curve().cloned().unwrap_or(Trc::Srgb);
                match curve_kind(&curve) {
                    CurveKind::Srgb => Ok((untagged(image), None, false)),
                    CurveKind::Linear => Ok((retag(image, ColorSpace::LINEAR), None, false)),
                    _ => apply_profile(engine, image, gray_profile(engine, &curve)?),
                }
            }
            Ok(profile) => apply_profile(engine, image, profile),
        },
    }
}

/// Tags `image` with `profile` when its curves are sRGB's or linear, and linearizes it through them otherwise.
fn apply_profile(engine: &dyn ColorEngine, image: AnyImage, profile: Profile) -> Result<(AnyImage, Option<Profile>, bool), CodecError> {
    if let Some(encoding) = profile.encoding() {
        Ok((retag(image, ColorSpace { encoding, icc: Some(profile.bytes.clone()) }), Some(profile), false))
    } else {
        let linear = engine.linearize(&image, &profile)?;
        Ok((AnyImage::F32(linear), Some(profile), true))
    }
}

fn retag(mut image: AnyImage, cs: ColorSpace) -> AnyImage {
    match &mut image {
        AnyImage::U8(i) => i.set_color_space(cs),
        AnyImage::U16(i) => i.set_color_space(cs),
        AnyImage::F32(i) => i.set_color_space(cs),
    }
    image
}

/// An image ready for an encoder: its samples in the output depth, encoded for the output profile, and the profile
/// to embed (none for an untagged file).
pub(crate) struct Prepared {
    pub image: AnyImage,
    pub profile: Option<Profile>,
}

/// Converts `image` for writing at `depth` in `output`, by the writing rules in this module's documentation.
/// `linear` asks for linear light whatever the depth (DNG files hold linear 16-bit integers), as float files have it.
pub(crate) fn prepare(
    engine: &dyn ColorEngine,
    image: &AnyImage,
    output: OutputProfile,
    depth: BitDepth,
    extended_range: bool,
    linear: bool,
    warnings: &mut Vec<Warning>,
) -> Result<Prepared, CodecError> {
    if output == OutputProfile::Untagged {
        let encoding = image.color_space().encoding;
        let image = retag(to_depth(image, depth, encoding)?, ColorSpace { encoding, icc: None });
        return Ok(Prepared { image: clip_unless(image, extended_range), profile: None });
    }
    let gray = image.channels() < 3;
    let source = match image.color_space().icc.as_deref() {
        None => None,
        Some(bytes) => match engine.parse(bytes) {
            Ok(p) if !p.needs_cms() && p.is_gray() == gray => Some(p),
            Ok(_) | Err(_) => {
                warnings.push(Warning::ProfileReplaced { reason: "it can't be applied to this image", space: "sRGB" });
                None
            }
        },
    };
    let float = depth == BitDepth::F32 || linear;
    // The target profile, and whether to embed it.
    let target: Option<Profile> = match (output, gray, float) {
        (OutputProfile::Input, _, false) => source.clone(),
        (OutputProfile::Input, _, true) => match &source {
            Some(p) if p.encoding() == Some(Encoding::Linear) => Some(p.clone()),
            Some(p) => {
                if let Ok(linear) = relinearized(engine, p) {
                    Some(linear)
                } else {
                    warnings.push(Warning::ProfileReplaced { reason: "no linear version of it can be made", space: "linear sRGB" });
                    None
                }
            }
            None => None,
        },
        // Gray has no primaries: an explicit space means its curve (sRGB's for every space here) or linear.
        (_, true, _) | (OutputProfile::Untagged, ..) => None,
        (OutputProfile::Srgb, false, f) => Some(engine.standard(NamedSpace::Srgb, if f { StandardCurve::Linear } else { StandardCurve::Native })?),
        (OutputProfile::Wide(space), false, f) => {
            let named = match space {
                WideSpace::ProPhoto => NamedSpace::ProPhoto,
                WideSpace::Rec2020 => NamedSpace::Rec2020,
            };
            Some(engine.standard(named, if f { StandardCurve::Linear } else { StandardCurve::Native })?)
        }
    };
    // Without a target profile the output is sRGB-encoded (integers) or linear (floats), with sRGB primaries.
    let target_encoding = match &target {
        Some(p) => p.encoding(),
        None => Some(if float { Encoding::Linear } else { Encoding::Srgb }),
    };
    let same_primaries = match (&source, &target) {
        (None, None) => true,
        (Some(s), Some(t)) => s == t || gray || primaries_equal(s, t),
        (None, Some(t)) => gray || t.named == Some(NamedSpace::Srgb),
        (Some(s), None) => gray || s.named == Some(NamedSpace::Srgb),
    };
    let icc = target.as_ref().map(|p| p.bytes.clone());
    // Fast path: same primaries and a curve that core's conversions know: only the sample type (and maybe the curve
    // between sRGB and linear) changes, exactly as `florastack_core::convert` does it.
    if same_primaries && let Some(encoding) = target_encoding {
        let out = to_depth(image, depth, encoding)?;
        let out = retag(out, ColorSpace { encoding, icc });
        return Ok(Prepared { image: clip_unless(out, extended_range), profile: target });
    }
    // General path: linear light, then the primaries, then the target's curves.
    let mut linear = match image {
        AnyImage::U8(i) => i.to_linear_f32()?,
        AnyImage::U16(i) => i.to_linear_f32()?,
        AnyImage::F32(i) => i.to_encoding::<f32>(Encoding::Linear)?,
    };
    let srgb = engine.standard(NamedSpace::Srgb, StandardCurve::Linear)?;
    let from = source.as_ref().unwrap_or(&srgb);
    let to = target.as_ref().unwrap_or(&srgb);
    if !same_primaries {
        engine.convert(&mut linear, from, to)?;
    }
    let out = match (target_encoding, &target) {
        (Some(encoding), _) => {
            let mut out = to_depth(&AnyImage::F32(linear), depth, encoding)?;
            out = retag(out, ColorSpace { encoding, icc });
            out
        }
        (None, Some(t)) => engine.encode(&linear, t, depth)?,
        (None, None) => to_depth(&AnyImage::F32(linear), depth, Encoding::Linear)?,
    };
    Ok(Prepared { image: clip_unless(out, extended_range), profile: target })
}

/// Whether two matrix profiles have the same colorants, to the precision ICC stores them with.
fn primaries_equal(a: &Profile, b: &Profile) -> bool {
    match (a.to_xyz_d50(), b.to_xyz_d50()) {
        (Some(m), Some(n)) => m.0.iter().flatten().zip(n.0.iter().flatten()).all(|(x, y)| (x - y).abs() < 2e-4),
        _ => false,
    }
}

/// `image` as samples of `depth` in `encoding` (decoded from or encoded to sRGB as needed), keeping its profile.
fn to_depth(image: &AnyImage, depth: BitDepth, encoding: Encoding) -> Result<AnyImage, CodecError> {
    fn go<T: Sample>(image: &Image<T>, depth: BitDepth, encoding: Encoding) -> Result<AnyImage, CodecError> {
        Ok(match depth {
            BitDepth::U8 => AnyImage::U8(image.to_encoding::<u8>(encoding)?),
            BitDepth::U16 => AnyImage::U16(image.to_encoding::<u16>(encoding)?),
            BitDepth::F32 => AnyImage::F32(image.to_encoding::<f32>(encoding)?),
        })
    }
    match image {
        AnyImage::U8(i) => go(i, depth, encoding),
        AnyImage::U16(i) => go(i, depth, encoding),
        AnyImage::F32(i) => go(i, depth, encoding),
    }
}

/// Float samples clipped to `[0, 1]` unless the caller keeps extended range (integers are clipped by quantization).
/// Alpha is clipped too: coverage outside `[0, 1]` means nothing.
fn clip_unless(mut image: AnyImage, extended_range: bool) -> AnyImage {
    if let AnyImage::F32(i) = &mut image {
        let channels = i.channels();
        let alpha = channels == 2 || channels == 4;
        if !extended_range || alpha {
            i.par_rows_mut().for_each(|row| {
                for pixel in row.chunks_exact_mut(channels) {
                    for (c, v) in pixel.iter_mut().enumerate() {
                        let is_alpha = alpha && c + 1 == channels;
                        if !extended_range || is_alpha {
                            *v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
                        }
                    }
                }
            });
        }
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> LightCraftColor {
        LightCraftColor
    }

    #[test]
    fn standard_profiles_parse_back_with_their_curves() {
        let e = engine();
        for space in [NamedSpace::Srgb, NamedSpace::AdobeRgb, NamedSpace::ProPhoto, NamedSpace::Rec2020, NamedSpace::DisplayP3] {
            let native = e.standard(space, StandardCurve::Native).unwrap();
            assert_eq!(native.named(), Some(space), "{space:?}");
            let linear = e.standard(space, StandardCurve::Linear).unwrap();
            assert_eq!(linear.curve_kind(), CurveKind::Linear);
            assert_eq!(linear.encoding(), Some(Encoding::Linear));
        }
        assert_eq!(e.standard(NamedSpace::Srgb, StandardCurve::Native).unwrap().encoding(), Some(Encoding::Srgb));
        assert!(
            matches!(e.standard(NamedSpace::ProPhoto, StandardCurve::Native).unwrap().curve_kind(), CurveKind::Gamma(g) if (g - 1.8).abs() < 1e-3)
        );
        assert!(e.parse(b"not a profile").is_err());
    }

    #[test]
    fn gray_and_relinearized_profiles_are_valid_and_deterministic() {
        let e = engine();
        let gray = gray_profile(&e, &Trc::Gamma(2.2)).unwrap();
        assert!(gray.is_gray());
        assert!(matches!(gray.curve_kind(), CurveKind::Gamma(g) if (g - 2.2).abs() < 1e-3));
        assert_eq!(gray.bytes(), gray_profile(&e, &Trc::Gamma(2.2)).unwrap().bytes(), "no timestamp in the bytes");
        let bytes = gray.bytes();
        assert_eq!(bytes.len() % 4, 0);
        let count = u32::from_be_bytes(bytes[128..132].try_into().unwrap()) as usize;
        for t in 0..count {
            let offset = u32::from_be_bytes(bytes[136 + 12 * t..140 + 12 * t].try_into().unwrap());
            assert_eq!(offset % 4, 0, "tag {t} is aligned");
        }
        // A matrix profile FloraStack doesn't know by name keeps its colorants and loses its curve.
        let odd = Primaries { red: (0.68, 0.31), green: (0.25, 0.69), blue: (0.15, 0.05), white: (0.3127, 0.329) };
        let custom = profile_for_gamma(&e, 2.0, Some(&odd), false).unwrap();
        assert_eq!(custom.named(), None);
        let linear = relinearized(&e, &custom).unwrap();
        assert_eq!(linear.encoding(), Some(Encoding::Linear));
        assert!(primaries_equal(&custom, &linear));
    }

    #[test]
    fn every_16_bit_code_survives_a_gamma_profile_both_ways() {
        let e = engine();
        let adobe = e.standard(NamedSpace::AdobeRgb, StandardCurve::Native).unwrap();
        let codes: Vec<u16> = (0..=u16::MAX).collect();
        let image = AnyImage::U16(Image::from_vec(256, 256, 1, ColorSpace::SRGB, codes.clone()).unwrap());
        let gray_adobe = e.parse(gray_profile(&e, &Trc::Gamma(563.0 / 256.0)).unwrap().bytes()).unwrap();
        for profile in [&adobe, &gray_adobe] {
            let linear = e.linearize(&image, profile).unwrap();
            assert_eq!(linear.color_space().encoding, Encoding::Linear);
            let back = e.encode(&linear, profile, BitDepth::U16).unwrap();
            assert_eq!(back.as_image::<u16>().unwrap().as_raw(), &codes[..]);
        }
    }

    #[test]
    fn nearest_codes_clip_and_round() {
        let table = [0.0, 0.25, 0.5, 1.0];
        assert_eq!(nearest_code(&table, -1.0), 0);
        assert_eq!(nearest_code(&table, f32::NAN), 0);
        assert_eq!(nearest_code(&table, 0.3), 1);
        assert_eq!(nearest_code(&table, 0.4), 2);
        assert_eq!(nearest_code(&table, 0.75), 2, "ties go to the lower code");
        assert_eq!(nearest_code(&table, 7.0), 3);
        assert_eq!(nearest_code(&[], 0.5), 0);
    }

    #[test]
    fn conversion_between_primaries_keeps_white_and_round_trips() {
        let e = engine();
        let srgb = e.standard(NamedSpace::Srgb, StandardCurve::Linear).unwrap();
        let wide = e.standard(NamedSpace::ProPhoto, StandardCurve::Linear).unwrap();
        let mut image = Image::from_fn(4, 2, 4, ColorSpace::LINEAR, |x, y, c| if c == 3 { 0.5 } else { (x + y + c) as f32 / 10.0 }).unwrap();
        image.pixel_mut(0, 0).unwrap()[..3].copy_from_slice(&[1.0, 1.0, 1.0]);
        let original = image.clone();
        e.convert(&mut image, &srgb, &wide).unwrap();
        let white = image.pixel(0, 0).unwrap();
        assert!(white[..3].iter().all(|v| (v - 1.0).abs() < 1e-4), "white stays white: {white:?}");
        assert_eq!(white[3], 0.5, "alpha is left alone");
        e.convert(&mut image, &wide, &srgb).unwrap();
        assert!(image.as_raw().iter().zip(original.as_raw()).all(|(a, b)| (a - b).abs() < 1e-4));
    }

    #[test]
    fn hints_tag_samples_by_the_reading_rules() {
        let e = engine();
        let mut warnings = Vec::new();
        let u8_image = || AnyImage::U8(Image::from_vec(2, 1, 3, ColorSpace::LINEAR, vec![0, 128, 255, 10, 20, 30]).unwrap());
        let (img, profile, linearized) = tag(&e, u8_image(), &ColorHint::Untagged, &mut warnings).unwrap();
        assert_eq!((img.color_space(), profile.is_none(), linearized), (&ColorSpace::SRGB, true, false));
        let f32_image = AnyImage::F32(Image::from_vec(1, 1, 1, ColorSpace::SRGB, vec![0.5]).unwrap());
        assert_eq!(tag(&e, f32_image, &ColorHint::Untagged, &mut warnings).unwrap().0.color_space(), &ColorSpace::LINEAR);
        // A gamma-2.2 profile is applied as the file is read.
        let adobe = e.standard(NamedSpace::AdobeRgb, StandardCurve::Native).unwrap();
        let (img, profile, linearized) = tag(&e, u8_image(), &ColorHint::Icc(adobe.bytes().clone()), &mut warnings).unwrap();
        assert!(linearized && matches!(img, AnyImage::F32(_)));
        assert_eq!(img.color_space().icc.as_deref(), Some(&adobe.bytes()[..]));
        assert_eq!(profile.unwrap().named(), Some(NamedSpace::AdobeRgb));
        // An sRGB profile keeps the samples, tagged with it.
        let srgb = e.standard(NamedSpace::Srgb, StandardCurve::Native).unwrap();
        let (img, _, linearized) = tag(&e, u8_image(), &ColorHint::Icc(srgb.bytes().clone()), &mut warnings).unwrap();
        assert!(!linearized && img.as_image::<u8>().is_some());
        assert_eq!(img.color_space().encoding, Encoding::Srgb);
        assert_eq!(warnings, []);
        // A broken profile is ignored, with a warning.
        let (img, _, _) = tag(&e, u8_image(), &ColorHint::Icc(Arc::from(&b"junk"[..])), &mut warnings).unwrap();
        assert_eq!(img.color_space(), &ColorSpace::SRGB);
        assert_eq!(warnings, [Warning::ProfileIgnored("it can't be read")]);
    }

    #[test]
    fn preparing_for_output_follows_the_writing_rules() {
        let e = engine();
        let mut w = Vec::new();
        let srgb16 =
            AnyImage::U16(Image::from_fn(3, 2, 3, ColorSpace::SRGB, |x, y, c| u16::try_from(x * 9000 + y * 20000 + c * 3000).unwrap()).unwrap());
        // Untagged sRGB in, input profile out: the samples as they are, and no profile.
        let p = prepare(&e, &srgb16, OutputProfile::Input, BitDepth::U16, false, false, &mut w).unwrap();
        assert_eq!(p.image, srgb16);
        assert!(p.profile.is_none());
        // Float output: linear, no profile for sRGB primaries.
        let p = prepare(&e, &srgb16, OutputProfile::Input, BitDepth::F32, false, false, &mut w).unwrap();
        assert_eq!(p.image.color_space(), &ColorSpace::LINEAR);
        // A wide space: its profile, and the samples converted.
        let p = prepare(&e, &srgb16, OutputProfile::Wide(WideSpace::ProPhoto), BitDepth::U16, false, false, &mut w).unwrap();
        assert_eq!(p.profile.as_ref().unwrap().named(), Some(NamedSpace::ProPhoto));
        assert_ne!(p.image, srgb16);
        // Untagged: the samples alone.
        let p = prepare(&e, &srgb16, OutputProfile::Untagged, BitDepth::U8, false, false, &mut w).unwrap();
        assert!(p.profile.is_none() && p.image.color_space().icc.is_none());
        // Extended range survives only when asked for.
        let hdr = AnyImage::F32(Image::from_vec(1, 1, 3, ColorSpace::LINEAR, vec![-0.25, 0.5, 3.0]).unwrap());
        let kept = prepare(&e, &hdr, OutputProfile::Input, BitDepth::F32, true, false, &mut w).unwrap();
        assert_eq!(kept.image.as_image::<f32>().unwrap().as_raw(), &[-0.25, 0.5, 3.0]);
        let clipped = prepare(&e, &hdr, OutputProfile::Input, BitDepth::F32, false, false, &mut w).unwrap();
        assert_eq!(clipped.image.as_image::<f32>().unwrap().as_raw(), &[0.0, 0.5, 1.0]);
        assert_eq!(w, []);
    }
}
