//! Developing RAW frames into linear, scene-referred RGB for stacking: [`DevelopSettings`] (chosen once per stack and
//! stored in the project), [`RawDevelop`] (those settings resolved for the stack) and [`Developed`] (a frame's
//! result).
//!
//! # The steps
//!
//! For a frame from a color filter array or a monochrome sensor:
//!
//! 1. **Demosaic**: bilinear interpolation for previews ([`Quality::Preview`]); for output, AHD (Adaptive
//!    Homogeneity-Directed demosaicing: K. Hirakawa and T. W. Parks, IEEE Transactions on Image Processing 14(3),
//!    2005) on Bayer sensors and LightCraft's own edge-weighted color-difference interpolation on X-Trans and other
//!    arrays. Both are LightCraft's implementations (`lightcraft_raw::demosaic`), written from the paper and from its
//!    own design; FloraStack takes no demosaic code from elsewhere.
//! 2. **The file's own corrections** after demosaicing (DNG `OpcodeList3`): vignetting and other per-pixel gains
//!    always, lens warps only with [`DevelopSettings::lens_distortion`]; then the default crop, and the rows
//!    stretched for photosites that aren't square.
//! 3. **Lateral chromatic aberration**: the red and blue planes scaled about the image center to meet green
//!    ([`ChromaticAberration`]), with FloraStack's bicubic warp (`florastack_core::ops::warp`). The automatic amount
//!    is LightCraft's estimate (`lightcraft_pipeline::optics::estimate_lateral_ca`), made once per stack.
//! 4. **Highlight reconstruction**: channels the sensor clipped rebuilt from the unclipped ones and the color of
//!    reliable content nearby (`lightcraft_raw::highlight::reconstruct`); pixels with no clipped channel are left
//!    exactly as they were.
//! 5. **White balance and color**: the camera's color model (DNG specification 1.7, chapter 6, in
//!    `lightcraft_raw::color`) evaluated at the stack's white gives white-balance multipliers and the matrix to linear
//!    Rec. 2020, with no camera profile look, hue map or tone curve.
//! 6. **Exposure**: a gain of `2^EV`.
//! 7. **Noise reduction**, when asked for: LightCraft's edge-aware guided filter on log luminance and its chroma
//!    blur (`lightcraft_pipeline::local::apply_nr`).
//!
//! Nothing else: no tone curve, filmic shoulder, contrast, saturation or look. Stacking needs linear data, so the
//! output is proportional to scene light, with 1.0 at the brightest neutral the sensor records without clipping at
//! exposure 0 (`BaselineExposure` is recorded, not applied), and values above 1 kept.
//!
//! A frame that a raw converter already demosaiced (a `LinearRaw` DNG) is imported unchanged: only its own color
//! model at its own recorded white applies, with its corrections (lens warps again only when asked).
//!
//! # One stack, one development
//!
//! A stack's frames must develop alike, or color and brightness flicker from frame to frame and show in the stack.
//! [`RawDevelop::new`] therefore resolves the settings once, from a reference frame (the first, say): "as shot" and
//! "auto" white balance become one white, and automatic chromatic-aberration correction one pair of scales, that
//! every frame then gets, whatever its own white balance says. [`RawDevelop::resolved`] is what to store next to the
//! settings, and [`RawDevelop::from_resolved`] rebuilds the same development without the reference.

use florastack_core::ops::{Border, Interpolation, warp};
use florastack_core::{Affine, ColorSpace, Encoding, Image, Orientation};
use lightcraft_raw::Rgb32f;
use serde::{Deserialize, Serialize};

use crate::color::{self, Transform};
use crate::decoder::is_warp;
use crate::error::{RawError, guarded};
use crate::frame::{Chromaticity, Mosaic, RawFrame, SensorLayout};

/// How a stack's RAW frames are developed: chosen once per stack, stored in its project, applied to every frame
/// alike through [`RawDevelop`]. Every field has a default, so a preset can name only some.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DevelopSettings {
    pub white_balance: WhiteBalance,
    /// The demosaic of output frames; previews always use [`Demosaic::Fast`].
    pub demosaic: Demosaic,
    pub highlights: Highlights,
    /// Exposure in stops (EV), from -10 to 10: a gain of `2^exposure`, linear like everything else.
    pub exposure: f32,
    pub chromatic_aberration: ChromaticAberration,
    pub noise_reduction: NoiseReduction,
    /// Apply the lens distortion correction the file records (DNG `WarpRectilinear`, or a camera's own). Off by
    /// default: distortion changes with focus distance, so correcting it changes each frame's geometry differently,
    /// which fights alignment. Alignment's scale and shift absorb the focus breathing instead.
    pub lens_distortion: bool,
}

/// Which white the frames are balanced for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhiteBalance {
    /// The white the camera recorded for the reference frame (D65 if it recorded none).
    #[default]
    AsShot,
    /// Estimated from the reference frame's content: a gray world weighted toward mid-tone, low-chroma pixels
    /// (LightCraft's `auto_wb`).
    Auto,
    /// A correlated color temperature in kelvin (2000 to 50000) and a tint (-150 to 150, positive toward green),
    /// on the camera's own scale when it has color matrices and relative to the as-shot rendering otherwise.
    Custom { temperature: f64, tint: f64 },
}

/// The demosaic method.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Demosaic {
    /// Bilinear interpolation, on any color filter array: fast, a little soft, with color fringes on the finest
    /// detail. What previews use.
    Fast,
    /// AHD on Bayer sensors, LightCraft's edge-weighted directional interpolation on X-Trans and other arrays.
    #[default]
    HighQuality,
}

/// What happens to highlights the sensor clipped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Highlights {
    /// Rebuild clipped channels from the unclipped ones (fully clipped pixels become neutral), so that clipped
    /// highlights don't turn magenta. Pixels with no clipped channel are left exactly as they are.
    #[default]
    Reconstruct,
    /// Leave the samples as the sensor clipped them: clipped highlights take the white balance's tint.
    Off,
}

/// Lateral chromatic aberration correction: the red and blue planes magnified relative to green about the image's
/// center, by a factor of `1 + scale`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChromaticAberration {
    Off,
    /// Measured once on the reference frame, from the radial offsets of strong edges between the planes; no
    /// correction where it finds too few edges.
    #[default]
    Auto,
    /// The red and blue planes' radial scales relative to green, each between -0.01 and 0.01 (a red edge at radius
    /// `r` sits at `r * (1 + red)`).
    Manual {
        red: f32,
        blue: f32,
    },
}

/// Light noise reduction: strengths from 0 (off, the default) to 1, as LightCraft's (and Lightroom's) 0 to 100
/// sliders. Off by default because both filters are nonlinear: the luminance filter smooths fine texture, which
/// stacking reads as focus, and the chroma filter keeps every sample at zero or above, which lifts the noise floor of
/// the darkest areas a little. Stacking averages noise away anyway; turn it on for frames shot at high ISO.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoiseReduction {
    /// Edge-aware smoothing of luminance. It smooths fine texture too, which stacking reads as focus, so keep it low.
    pub luminance: f32,
    /// Smoothing of color (chroma) noise, keeping luminance.
    pub color: f32,
}

impl NoiseReduction {
    /// Whether both strengths are zero.
    pub fn is_off(self) -> bool {
        self.luminance <= 0.0 && self.color <= 0.0
    }
}

/// Which demosaic a development uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Quality {
    /// The fast demosaic, for previews; every other step is the same as for output.
    Preview,
    /// The settings' demosaic.
    #[default]
    Output,
}

/// The largest exposure correction, in stops.
const MAX_EXPOSURE: f32 = 10.0;
/// The temperatures and tints that custom white balance takes (LightCraft's automatic white balance stays in them too).
const TEMPERATURES: std::ops::RangeInclusive<f64> = 2000.0..=50000.0;
const TINTS: std::ops::RangeInclusive<f64> = -150.0..=150.0;
/// The largest chromatic-aberration scale: 1 %, ten times what real lenses show.
const MAX_CA: f32 = 0.01;
/// The sensor clip level for highlight reconstruction, in normalized units: slightly below the white level, as
/// LightCraft uses it.
const HIGHLIGHT_CLIP: f32 = 0.99;
/// The long side of the reduced copy that automatic white balance looks at.
const AUTO_WB_SIDE: usize = 512;

fn setting(name: &'static str, value: f64, expected: &'static str) -> RawError {
    RawError::Setting { name, value, expected }
}

impl DevelopSettings {
    /// Checks that every number is finite and in its range.
    pub fn validate(&self) -> Result<(), RawError> {
        if !(self.exposure.is_finite() && (-MAX_EXPOSURE..=MAX_EXPOSURE).contains(&self.exposure)) {
            return Err(setting("exposure", f64::from(self.exposure), "between -10 and 10 EV"));
        }
        if let WhiteBalance::Custom { temperature, tint } = self.white_balance {
            if !TEMPERATURES.contains(&temperature) {
                return Err(setting("white_balance.temperature", temperature, "between 2000 and 50000 K"));
            }
            if !TINTS.contains(&tint) {
                return Err(setting("white_balance.tint", tint, "between -150 and 150"));
            }
        }
        if let ChromaticAberration::Manual { red, blue } = self.chromatic_aberration {
            for (name, v) in [("chromatic_aberration.red", red), ("chromatic_aberration.blue", blue)] {
                if !(v.is_finite() && v.abs() <= MAX_CA) {
                    return Err(setting(name, f64::from(v), "between -0.01 and 0.01"));
                }
            }
        }
        let nr = self.noise_reduction;
        for (name, v) in [("noise_reduction.luminance", nr.luminance), ("noise_reduction.color", nr.color)] {
            if !(0.0..=1.0).contains(&v) {
                return Err(setting(name, f64::from(v), "between 0 and 1"));
            }
        }
        Ok(())
    }
}

/// What a stack's [`DevelopSettings`] come to once resolved on its reference frame: the white every frame is balanced
/// for, and the chromatic-aberration scales every frame is corrected by. Store it with the settings, so that a
/// reopened project develops its frames exactly as before.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolved {
    pub white: Chromaticity,
    /// The red and blue planes' radial scales relative to green (0 for none).
    pub chromatic_aberration: [f32; 2],
}

/// Something a development did that the user may want to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DevelopWarning {
    /// The camera has no color matrix FloraStack knows: its RGB was taken for sRGB, so colors are approximate.
    /// Stacking is unaffected.
    ApproximateColor,
    /// The reference frame recorded no white balance: "as shot" used D65.
    NoRecordedWhite,
}

/// A developed frame.
#[derive(Clone, Debug)]
pub struct Developed {
    /// Linear, scene-referred RGB in Rec. 2020 primaries with a D65 white, tagged with a linear Rec. 2020 ICC profile
    /// ([`crate::working_space_icc`]); 1.0 is the brightest neutral the sensor records without clipping at exposure
    /// 0, and brighter values are kept. Not yet oriented: see `orientation`.
    pub image: Image<f32>,
    /// The orientation the file records, for the caller to apply or ignore as the stack's orientation policy says
    /// (`Image::oriented`).
    pub orientation: Orientation,
    /// Whether the file was already demosaiced by a raw converter and so imported unchanged.
    pub converted: bool,
    /// The [`RawDevelop::fingerprint`] of the development that made it, with its quality.
    pub fingerprint: u64,
    pub warnings: Vec<DevelopWarning>,
}

/// A stack's development: its [`DevelopSettings`], resolved once ([`Resolved`]), applied to each frame in the same
/// way. Developing is deterministic: the same frame gives the same pixels, bit for bit, whatever the thread count.
#[derive(Clone, Debug, PartialEq)]
pub struct RawDevelop {
    settings: DevelopSettings,
    resolved: Resolved,
    warnings: Vec<DevelopWarning>,
}

impl RawDevelop {
    /// The development of a stack with these settings, resolved on its `reference` frame. Automatic chromatic
    /// aberration costs one full development of the reference; automatic white balance a reduced one.
    pub fn new(settings: &DevelopSettings, reference: &RawFrame) -> Result<RawDevelop, RawError> {
        settings.validate()?;
        reference.validate()?;
        guarded(reference.info.format, || {
            let mut warnings = Vec::new();
            let white = match settings.white_balance {
                WhiteBalance::AsShot => {
                    let balanced = !matches!(reference.sensor.layout(), SensorLayout::Monochrome | SensorLayout::Linear);
                    if balanced
                        && reference.color.as_shot_neutral.is_none()
                        && reference.color.as_shot_white.is_none()
                        && reference.color.camera_multipliers.is_none()
                    {
                        warnings.push(DevelopWarning::NoRecordedWhite);
                    }
                    color::as_shot_white(&reference.color)
                }
                WhiteBalance::Auto => auto_white(reference),
                WhiteBalance::Custom { temperature, tint } => Chromaticity::from_temperature(temperature, tint),
            };
            let chromatic_aberration = match settings.chromatic_aberration {
                ChromaticAberration::Off => [0.0; 2],
                ChromaticAberration::Manual { red, blue } => [red, blue],
                ChromaticAberration::Auto => estimate_ca(reference, settings)?,
            };
            tracing::debug!(?white, ?chromatic_aberration, "resolved the stack's development");
            Ok(RawDevelop { settings: settings.clone(), resolved: Resolved { white, chromatic_aberration }, warnings })
        })
    }

    /// The development with these settings, resolved before (with [`RawDevelop::new`], then stored).
    pub fn from_resolved(settings: &DevelopSettings, resolved: Resolved) -> Result<RawDevelop, RawError> {
        settings.validate()?;
        if !resolved.white.is_plausible() {
            return Err(setting("resolved.white", resolved.white.x, "a white's chromaticity"));
        }
        for v in resolved.chromatic_aberration {
            if !(v.is_finite() && v.abs() <= MAX_CA) {
                return Err(setting("resolved.chromatic_aberration", f64::from(v), "between -0.01 and 0.01"));
            }
        }
        Ok(RawDevelop { settings: settings.clone(), resolved, warnings: Vec::new() })
    }

    pub fn settings(&self) -> &DevelopSettings {
        &self.settings
    }

    pub fn resolved(&self) -> Resolved {
        self.resolved
    }

    /// What resolving found worth telling (it is repeated in every frame's warnings).
    pub fn warnings(&self) -> &[DevelopWarning] {
        &self.warnings
    }

    /// A 64-bit fingerprint of everything that decides a frame's pixels: the settings, the resolved values, the
    /// quality and the version of the steps. Equal fingerprints mean identical development, so a cache of developed
    /// frames can key on it with the file's content hash.
    pub fn fingerprint(&self, quality: Quality) -> u64 {
        let s = &self.settings;
        let mut h = Fnv::new();
        h.bytes(b"florastack-raw develop 1");
        match s.white_balance {
            WhiteBalance::AsShot => h.u8(0),
            WhiteBalance::Auto => h.u8(1),
            WhiteBalance::Custom { temperature, tint } => {
                h.u8(2);
                h.f64(temperature);
                h.f64(tint);
            }
        }
        h.u8(match s.demosaic {
            Demosaic::Fast => 0,
            Demosaic::HighQuality => 1,
        });
        h.u8(match s.highlights {
            Highlights::Reconstruct => 0,
            Highlights::Off => 1,
        });
        h.f32(s.exposure);
        match s.chromatic_aberration {
            ChromaticAberration::Off => h.u8(0),
            ChromaticAberration::Auto => h.u8(1),
            ChromaticAberration::Manual { red, blue } => {
                h.u8(2);
                h.f32(red);
                h.f32(blue);
            }
        }
        h.f32(s.noise_reduction.luminance);
        h.f32(s.noise_reduction.color);
        h.u8(u8::from(s.lens_distortion));
        h.f64(self.resolved.white.x);
        h.f64(self.resolved.white.y);
        h.f32(self.resolved.chromatic_aberration[0]);
        h.f32(self.resolved.chromatic_aberration[1]);
        h.u8(match quality {
            Quality::Preview => 0,
            Quality::Output => 1,
        });
        h.0
    }

    /// Develops one frame of the stack.
    pub fn develop(&self, frame: &RawFrame, quality: Quality) -> Result<Developed, RawError> {
        frame.validate()?;
        guarded(frame.info.format, || self.develop_checked(frame, quality))
    }

    fn develop_checked(&self, frame: &RawFrame, quality: Quality) -> Result<Developed, RawError> {
        let s = &self.settings;
        let layout = frame.sensor.layout();
        let converted = layout == SensorLayout::Linear;
        let demosaic = match quality {
            Quality::Preview => Demosaic::Fast,
            Quality::Output => s.demosaic,
        };
        let mut rgb = camera_rgb(frame, demosaic, s.lens_distortion)?;
        let mut warnings = self.warnings.clone();
        if frame.color.source == crate::ColorSource::Fallback && layout != SensorLayout::Monochrome {
            warnings.push(DevelopWarning::ApproximateColor);
        }
        let (transform, gain) = if converted {
            // Unchanged: the converter's own white balance, no exposure.
            (color::transform(&frame.color, color::as_shot_white(&frame.color)), 1.0)
        } else if layout == SensorLayout::Monochrome {
            (Transform::NEUTRAL, s.exposure.exp2())
        } else {
            (color::transform(&frame.color, self.resolved.white), s.exposure.exp2())
        };
        if !converted {
            correct_lateral_ca(&mut rgb, self.resolved.chromatic_aberration)?;
            if s.highlights == Highlights::Reconstruct && !frame.sensor.float && layout != SensorLayout::Monochrome {
                lightcraft_raw::highlight::reconstruct(&mut rgb, transform.multipliers, HIGHLIGHT_CLIP);
            }
        }
        apply_color(&mut rgb, &transform, gain);
        if !converted && !s.noise_reduction.is_off() {
            reduce_noise(&mut rgb, s.noise_reduction);
        }
        Ok(Developed { image: to_image(rgb)?, orientation: frame.info.orientation, converted, fingerprint: self.fingerprint(quality), warnings })
    }
}

/// The FNV-1a 64-bit hash (Fowler, Noll and Vo), over little-endian bytes.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn u8(&mut self, v: u8) {
        self.bytes(&[v]);
    }

    fn f32(&mut self, v: f32) {
        self.bytes(&v.to_bits().to_le_bytes());
    }

    fn f64(&mut self, v: f64) {
        self.bytes(&v.to_bits().to_le_bytes());
    }
}

/// The number of rows a picture `height` rows tall has once photosites `aspect` times taller than wide are made
/// square.
pub(crate) fn stretched(height: usize, aspect: f32) -> usize {
    lightcraft_raw::stretched_height(height, aspect)
}

/// LightCraft's color filter array for FloraStack's.
fn lc_cfa(cfa: &crate::Cfa) -> lightcraft_raw::Cfa {
    lightcraft_raw::Cfa { width: cfa.width(), height: cfa.height(), pattern: cfa.pattern().to_vec() }
}

/// Camera RGB (white = 1.0, not white balanced) of the default crop: demosaiced, with the file's corrections.
fn camera_rgb(frame: &RawFrame, demosaic: Demosaic, lens_distortion: bool) -> Result<Rgb32f, RawError> {
    let m = &frame.sensor;
    // A sample that isn't a number (another decoder's, or a float file's) reads as black, as LightCraft's own
    // normalization makes it, so that every step after sees numbers.
    let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
    let mut rgb = if m.channels == 3 {
        let data = m.samples.as_chunks::<3>().0.iter().map(|p| p.map(finite)).collect();
        Rgb32f { width: m.width, height: m.height, data }
    } else {
        let method = match demosaic {
            Demosaic::Fast => lightcraft_raw::Method::Bilinear,
            Demosaic::HighQuality => lightcraft_raw::Method::Ahd,
        };
        let data = m.samples.iter().map(|&v| finite(v)).collect();
        let normalized = lightcraft_raw::Normalized { width: m.width, height: m.height, cpp: 1, data, cfa: m.cfa.as_ref().map(lc_cfa) };
        lightcraft_raw::demosaic(&normalized, method)
    };
    if rgb.data.len() != m.width * m.height {
        return Err(RawError::Frame("the demosaic returned another size"));
    }
    let ops: Vec<lightcraft_raw::Opcode> = frame.corrections.0.iter().filter(|op| lens_distortion || !is_warp(op)).cloned().collect();
    lightcraft_raw::opcodes::apply_list3(&ops, &mut rgb);
    let c = frame.crop;
    if (c.x, c.y, c.width, c.height) != (0, 0, rgb.width, rgb.height) {
        rgb = rgb.into_crop(c.x, c.y, c.width, c.height);
    }
    let height = stretched(rgb.height, frame.pixel_aspect);
    if height != rgb.height {
        rgb = lightcraft_raster::resample::resize(&rgb, rgb.width, height, lightcraft_raster::resample::Filter::Mitchell);
    }
    Ok(rgb)
}

/// Corrects lateral chromatic aberration: the red and blue planes resampled (bicubic) from where their edges really
/// are, `center + (1 + scale) (p - center)` for output pixel `p`.
fn correct_lateral_ca(rgb: &mut Rgb32f, scale: [f32; 2]) -> Result<(), RawError> {
    let (w, h) = (rgb.width, rgb.height);
    let center = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    for (channel, s) in [(0, scale[0]), (2, scale[1])] {
        // Below a billionth the planes move by less than a thousandth of a pixel even at the corners of a 100 MP frame.
        if s.abs() < 1e-9 {
            continue;
        }
        let plane = Image::from_vec(w, h, 1, ColorSpace::LINEAR, rgb.data.iter().map(|p| p[channel]).collect())?;
        let to_source = Affine::similarity(center, 1.0 + f64::from(s), 0.0, (0.0, 0.0));
        let corrected = warp(&plane, &to_source, Interpolation::Bicubic, Border::Clamp)?;
        for (p, v) in rgb.data.iter_mut().zip(corrected.rows().flatten()) {
            p[channel] = *v;
        }
    }
    Ok(())
}

/// `gain * matrix * (multipliers ⊙ p)` for every pixel, in place.
fn apply_color(rgb: &mut Rgb32f, t: &Transform, gain: f32) {
    let (wb, m) = (t.multipliers, t.matrix);
    lightcraft_raster::par_rows(&mut rgb.data, rgb.width, |_, row| {
        for p in row {
            let c = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
            *p = std::array::from_fn(|i| (m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2]) * gain);
        }
    });
}

/// LightCraft's noise reduction, its parameters read from LightCraft's 0 to 100 sliders at full size.
fn reduce_noise(rgb: &mut Rgb32f, nr: NoiseReduction) {
    let mut s = lightcraft_develop::DevelopSettings::default();
    s.detail.nr_luminance = f64::from(nr.luminance) * 100.0;
    s.detail.nr_color = f64::from(nr.color) * 100.0;
    let (luminance, color) = lightcraft_pipeline::local::nr_params(&s, 1.0);
    lightcraft_pipeline::local::apply_nr(rgb, luminance, color);
}

/// The developed image as FloraStack's, tagged linear Rec. 2020. No copy: the pixels are reinterpreted.
fn to_image(rgb: Rgb32f) -> Result<Image<f32>, RawError> {
    let (w, h) = (rgb.width, rgb.height);
    let color = ColorSpace { encoding: Encoding::Linear, icc: Some(color::working_space_icc()) };
    Ok(Image::from_vec(w, h, 3, color, rgb.data.into_flattened())?)
}

/// The red and blue scales that correct the reference frame's lateral chromatic aberration (none on data without a
/// color filter array).
fn estimate_ca(reference: &RawFrame, settings: &DevelopSettings) -> Result<[f32; 2], RawError> {
    if !matches!(reference.sensor.layout(), SensorLayout::Bayer | SensorLayout::XTrans | SensorLayout::OtherCfa) {
        return Ok([0.0; 2]);
    }
    let rgb = camera_rgb(reference, settings.demosaic, settings.lens_distortion)?;
    let [red, blue] = lightcraft_pipeline::optics::estimate_lateral_ca(&rgb);
    #[allow(clippy::cast_possible_truncation)] // Scales of at most 1 %, kept in range below.
    let clamp = |v: f64| if v.is_finite() { (v as f32).clamp(-MAX_CA, MAX_CA) } else { 0.0 };
    Ok([clamp(red), clamp(blue)])
}

/// The reference frame's white by LightCraft's automatic white balance, on a reduced copy developed for its
/// recorded white.
fn auto_white(reference: &RawFrame) -> Chromaticity {
    let as_shot = color::as_shot_white(&reference.color);
    if matches!(reference.sensor.layout(), SensorLayout::Monochrome | SensorLayout::Linear) {
        return as_shot;
    }
    let Some(mut small) = binned(&reference.sensor, reference.crop, AUTO_WB_SIDE) else { return as_shot };
    apply_color(&mut small, &color::transform(&reference.color, as_shot), 1.0);
    let has_matrix = reference.color.source != crate::ColorSource::Fallback;
    let (temp, tint) = as_shot.temperature();
    let info = lightcraft_pipeline::SourceInfo {
        raw: true,
        relative_wb: !has_matrix,
        as_shot_temp: temp,
        as_shot_tint: tint,
        camera_color: has_matrix.then(|| {
            std::sync::Arc::new(lightcraft_pipeline::CameraColor { tags: color::color_data(&reference.color), developed_for: as_shot.xy() })
        }),
        ..lightcraft_pipeline::SourceInfo::default()
    };
    let (temp, tint) = lightcraft_pipeline::auto::auto_wb(&small, &info);
    let white = Chromaticity::from_temperature(temp, tint);
    if white.is_plausible() { white } else { as_shot }
}

/// A reduced camera-RGB copy of `crop` of a color filter array's samples, at most `max_side` pixels on its long
/// side: every `k` x `k` block becomes one pixel whose channels are the means of the block's samples of each color
/// ("superpixel" binning, the idea of LightCraft's binned RAW previews, `RawImage::develop_binned`, written anew here
/// for FloraStack's frames; `k` a multiple of the pattern's size, so that every block holds all three colors, and at
/// least 4, so that each color's samples spread over the block and fine detail doesn't turn into false color).
/// Samples that aren't numbers count as black, as everywhere else.
fn binned(m: &Mosaic, crop: florastack_core::Rect, max_side: usize) -> Option<Rgb32f> {
    let cfa = m.cfa.as_ref()?;
    let period = lcm(cfa.width(), cfa.height());
    let long = crop.width.max(crop.height);
    let k = long.div_ceil(max_side.max(1)).max(4).div_ceil(period) * period;
    let (bw, bh) = (crop.width / k, crop.height / k);
    if bw == 0 || bh == 0 {
        return None;
    }
    let mut out = Rgb32f::new(bw, bh);
    lightcraft_raster::par_rows(&mut out.data, bw, |by, row| {
        for (bx, px) in row.iter_mut().enumerate() {
            let (mut sum, mut n) = ([0.0f32; 3], [0u32; 3]);
            for y in crop.y + by * k..crop.y + (by + 1) * k {
                for x in crop.x + bx * k..crop.x + (bx + 1) * k {
                    let c = usize::from(cfa.color_at(x, y)).min(2);
                    if let Some(&v) = m.samples.get(y * m.width + x) {
                        sum[c] += if v.is_finite() { v } else { 0.0 };
                        n[c] += 1;
                    }
                }
            }
            #[allow(clippy::cast_precision_loss)] // At most a few thousand samples per block.
            let mean = |c: usize| if n[c] > 0 { sum[c] / n[c] as f32 } else { 0.0 };
            *px = [mean(0), mean(1), mean(2)];
        }
    });
    Some(out)
}

fn lcm(a: usize, b: usize) -> usize {
    let gcd = |mut a: usize, mut b: usize| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a.max(1)
    };
    a / gcd(a, b) * b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_ones() {
        let s = DevelopSettings::default();
        assert_eq!(s.white_balance, WhiteBalance::AsShot);
        assert_eq!(s.demosaic, Demosaic::HighQuality);
        assert_eq!(s.highlights, Highlights::Reconstruct);
        assert_eq!(s.chromatic_aberration, ChromaticAberration::Auto);
        assert!(s.noise_reduction.is_off());
        assert!(!s.lens_distortion, "lens distortion correction is off by default");
        assert_eq!(s.exposure.to_bits(), 0.0f32.to_bits());
        s.validate().unwrap();
    }

    #[test]
    fn out_of_range_settings_are_refused() {
        let bad = [
            DevelopSettings { exposure: f32::NAN, ..DevelopSettings::default() },
            DevelopSettings { exposure: 11.0, ..DevelopSettings::default() },
            DevelopSettings { white_balance: WhiteBalance::Custom { temperature: 1000.0, tint: 0.0 }, ..DevelopSettings::default() },
            DevelopSettings { white_balance: WhiteBalance::Custom { temperature: 5000.0, tint: f64::INFINITY }, ..DevelopSettings::default() },
            DevelopSettings { chromatic_aberration: ChromaticAberration::Manual { red: 0.02, blue: 0.0 }, ..DevelopSettings::default() },
            DevelopSettings { noise_reduction: NoiseReduction { luminance: -0.1, color: 0.0 }, ..DevelopSettings::default() },
            DevelopSettings { noise_reduction: NoiseReduction { luminance: 0.0, color: f32::NAN }, ..DevelopSettings::default() },
        ];
        for s in bad {
            assert!(matches!(s.validate(), Err(RawError::Setting { .. })), "{s:?}");
        }
    }

    #[test]
    fn settings_round_trip_through_json_and_toml_and_partial_presets_fill_in_defaults() {
        let s = DevelopSettings {
            white_balance: WhiteBalance::Custom { temperature: 5200.0, tint: -4.0 },
            demosaic: Demosaic::Fast,
            highlights: Highlights::Off,
            exposure: 0.5,
            chromatic_aberration: ChromaticAberration::Manual { red: 0.0004, blue: -0.0003 },
            noise_reduction: NoiseReduction { luminance: 0.1, color: 0.3 },
            lens_distortion: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<DevelopSettings>(&json).unwrap(), s);
        let toml_text = toml::to_string(&s).unwrap();
        assert_eq!(toml::from_str::<DevelopSettings>(&toml_text).unwrap(), s);
        let partial: DevelopSettings = toml::from_str("white_balance = \"auto\"\nexposure = -1.0\n").unwrap();
        assert_eq!(partial, DevelopSettings { white_balance: WhiteBalance::Auto, exposure: -1.0, ..DevelopSettings::default() });
        assert!(toml::from_str::<DevelopSettings>("whitebalance = \"auto\"").is_err(), "a misspelled key is an error, not ignored");
        let resolved = Resolved { white: Chromaticity { x: 0.33, y: 0.34 }, chromatic_aberration: [0.0002, -0.0001] };
        assert_eq!(serde_json::from_str::<Resolved>(&serde_json::to_string(&resolved).unwrap()).unwrap(), resolved);
    }

    #[test]
    fn fingerprints_tell_developments_apart() {
        let resolved = Resolved { white: Chromaticity::D65, chromatic_aberration: [0.0; 2] };
        let a = RawDevelop::from_resolved(&DevelopSettings::default(), resolved).unwrap();
        let b = RawDevelop::from_resolved(&DevelopSettings { exposure: 0.1, ..DevelopSettings::default() }, resolved).unwrap();
        let c = RawDevelop::from_resolved(&DevelopSettings::default(), Resolved { chromatic_aberration: [1e-4, 0.0], ..resolved }).unwrap();
        assert_eq!(a.fingerprint(Quality::Output), a.clone().fingerprint(Quality::Output));
        assert_ne!(a.fingerprint(Quality::Output), a.fingerprint(Quality::Preview));
        assert_ne!(a.fingerprint(Quality::Output), b.fingerprint(Quality::Output));
        assert_ne!(a.fingerprint(Quality::Output), c.fingerprint(Quality::Output));
        assert!(RawDevelop::from_resolved(&DevelopSettings::default(), Resolved { white: Chromaticity { x: 2.0, y: 0.3 }, ..resolved }).is_err());
        assert!(RawDevelop::from_resolved(&DevelopSettings::default(), Resolved { chromatic_aberration: [f32::NAN, 0.0], ..resolved }).is_err());
    }

    #[test]
    fn binning_averages_each_color_of_a_block() {
        let cfa = crate::Cfa::bayer("RGGB").unwrap();
        let (w, h) = (8, 4);
        let samples = (0..w * h).map(|i| f32::from(cfa.color_at(i % w, i / w)) / 2.0 + 0.1).collect();
        let m = Mosaic { width: w, height: h, channels: 1, samples, cfa: Some(cfa), float: false };
        let out = binned(&m, florastack_core::Rect::new(0, 0, w, h), 4).unwrap();
        assert_eq!((out.width, out.height), (2, 1), "blocks of at least 4 photosites");
        assert!(out.data.iter().all(|p| (p[0] - 0.1).abs() < 1e-6 && (p[1] - 0.6).abs() < 1e-6 && (p[2] - 1.1).abs() < 1e-6), "{:?}", out.data);
        assert_eq!(lcm(6, 6), 6);
        assert_eq!(lcm(2, 3), 6);
    }
}
