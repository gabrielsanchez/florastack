//! A decoded RAW file ([`RawFrame`]) and what its headers say ([`RawInfo`]): plain data, so that a decoder other than
//! LightCraft's can produce them and the develop step works the same.
//!
//! The model is the DNG specification's (Adobe, DNG Specification 1.7, chapters 5 and 6): sensor samples from a
//! color filter array (or already demosaiced linear RGB), black-subtracted and scaled so that the white level is 1.0,
//! cropped to the active area; a default crop inside it; and the camera's color model, matrices between CIE XYZ and
//! the camera's raw RGB with the white the camera recorded.

use florastack_core::{Orientation, Rect};
use serde::{Deserialize, Serialize};

use crate::error::RawError;
use crate::format::RawFormat;

/// A CIE 1931 xy chromaticity: here, the color of the light a scene was lit by (its white).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chromaticity {
    pub x: f64,
    pub y: f64,
}

impl Chromaticity {
    /// CIE standard illuminant D65 (daylight, about 6504 K), at the precision the DNG color model uses it: what
    /// "as shot" means for a file that records no white balance.
    pub const D65: Chromaticity = Chromaticity { x: lightcraft_color::D65.x, y: lightcraft_color::D65.y };

    /// The white of a correlated color temperature in kelvin, moved off the Planckian locus by `tint` (positive
    /// toward green, in the units of Lightroom's and LightCraft's tint slider).
    pub fn from_temperature(kelvin: f64, tint: f64) -> Chromaticity {
        let xy = lightcraft_color::cct::temp_tint_to_xy(kelvin, tint);
        Chromaticity { x: xy.x, y: xy.y }
    }

    /// The correlated color temperature (kelvin) and tint of this white, as [`Chromaticity::from_temperature`] takes
    /// them.
    pub fn temperature(self) -> (f64, f64) {
        lightcraft_color::cct::xy_to_temp_tint(self.xy())
    }

    pub(crate) fn xy(self) -> lightcraft_color::Xy {
        lightcraft_color::Xy::new(self.x, self.y)
    }

    pub(crate) fn from_xy(xy: lightcraft_color::Xy) -> Chromaticity {
        Chromaticity { x: xy.x, y: xy.y }
    }

    /// Whether this can be the color of a light: finite, and inside the range of xy the spectrum locus spans.
    pub fn is_plausible(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && (0.05..=0.75).contains(&self.x) && (0.05..=0.85).contains(&self.y)
    }
}

/// How the sensor records color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SensorLayout {
    /// A 2x2 Bayer color filter array (RGGB or one of its shifts).
    Bayer,
    /// Fujifilm's 6x6 X-Trans array, which has its own demosaic.
    XTrans,
    /// Another color filter array (demosaiced like X-Trans).
    OtherCfa,
    /// No color filter: a monochrome sensor.
    Monochrome,
    /// Already demosaiced linear RGB: a DNG `LinearRaw` image, as raw converters write them. Imported unchanged.
    Linear,
}

/// A color filter array: which color each photosite records (0 red, 1 green, 2 blue), as a pattern of `width` by
/// `height` photosites repeated over the sensor from its top-left corner.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Cfa {
    width: usize,
    height: usize,
    pattern: Vec<u8>,
}

impl Cfa {
    /// The largest pattern side (DNG's `CFARepeatPatternDim` allows more; no camera uses it).
    pub const MAX_SIDE: usize = 16;

    /// A pattern of `width` by `height` colors, row by row; `None` unless both sides are 1 to [`Cfa::MAX_SIDE`], the
    /// pattern has that many entries, every one 0, 1 or 2, and all three colors occur.
    pub fn new(width: usize, height: usize, pattern: Vec<u8>) -> Option<Cfa> {
        let sides = (1..=Cfa::MAX_SIDE).contains(&width) && (1..=Cfa::MAX_SIDE).contains(&height);
        let colors = (0..3u8).all(|c| pattern.contains(&c)) && pattern.iter().all(|&c| c <= 2);
        (sides && pattern.len() == width * height && colors).then_some(Cfa { width, height, pattern })
    }

    /// A 2x2 Bayer pattern from its name, such as `"RGGB"`.
    pub fn bayer(name: &str) -> Option<Cfa> {
        let pattern = name
            .chars()
            .map(|c| match c {
                'R' => Some(0),
                'G' => Some(1),
                'B' => Some(2),
                _ => None,
            })
            .collect::<Option<Vec<u8>>>()?;
        Cfa::new(2, 2, pattern).filter(Cfa::is_bayer)
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// The colors, row by row.
    pub fn pattern(&self) -> &[u8] {
        &self.pattern
    }

    /// The color of the photosite at `(x, y)`.
    pub fn color_at(&self, x: usize, y: usize) -> u8 {
        self.pattern.get((y % self.height) * self.width + x % self.width).copied().unwrap_or(1)
    }

    /// The same array seen from photosite `(dx, dy)`: the pattern of a crop that starts there.
    #[must_use]
    pub fn shifted(&self, dx: usize, dy: usize) -> Cfa {
        let pattern = (0..self.height).flat_map(|y| (0..self.width).map(move |x| (x, y))).map(|(x, y)| self.color_at(x + dx, y + dy)).collect();
        Cfa { width: self.width, height: self.height, pattern }
    }

    /// Whether this is a Bayer pattern: 2x2 with one red, two greens and one blue.
    pub fn is_bayer(&self) -> bool {
        let count = |c: u8| self.count(c);
        self.width == 2 && self.height == 2 && (count(0), count(1), count(2)) == (1, 2, 1)
    }

    /// Whether this is Fujifilm's X-Trans layout: 6x6, with 20 greens and five in every 3x3 block.
    pub fn is_xtrans(&self) -> bool {
        let greens = |x0: usize, y0: usize| (0..9).filter(|i| self.color_at(x0 + i % 3, y0 + i / 3) == 1).count();
        self.width == 6 && self.height == 6 && self.count(1) == 20 && greens(0, 0) == 5 && greens(3, 3) == 5
    }

    /// How many photosites of the pattern record `color`.
    fn count(&self, color: u8) -> usize {
        self.pattern.iter().fold(0, |n, &p| n + usize::from(p == color))
    }

    /// The layout this pattern makes.
    pub fn layout(&self) -> SensorLayout {
        if self.is_bayer() {
            SensorLayout::Bayer
        } else if self.is_xtrans() {
            SensorLayout::XTrans
        } else {
            SensorLayout::OtherCfa
        }
    }

    /// The pattern's name, such as `"RGGB"`.
    pub fn name(&self) -> String {
        self.pattern.iter().map(|&c| ['R', 'G', 'B'].get(usize::from(c)).copied().unwrap_or('?')).collect()
    }
}

/// Sensor samples covering the active area, black-subtracted and scaled so that the sensor's white level is 1.0.
/// Noise can take them below zero; above 1.0 they are clipped (except in floating-point files, see
/// [`Mosaic::float`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Mosaic {
    pub width: usize,
    pub height: usize,
    /// 1 for a color filter array or a monochrome sensor, 3 for demosaiced linear RGB.
    pub channels: usize,
    /// `width * height * channels` samples, row by row.
    pub samples: Vec<f32>,
    /// The color filter array, anchored at sample (0, 0); `None` for monochrome and linear RGB data.
    pub cfa: Option<Cfa>,
    /// Whether the file stored floating-point samples, which may exceed 1.0 without being clipped (HDR merges):
    /// highlight reconstruction leaves them alone.
    pub float: bool,
}

impl Mosaic {
    /// The layout these samples have.
    pub fn layout(&self) -> SensorLayout {
        match (&self.cfa, self.channels) {
            (Some(cfa), 1) => cfa.layout(),
            (_, 1) => SensorLayout::Monochrome,
            _ => SensorLayout::Linear,
        }
    }

    /// Checks that the samples fill the size and that a color filter array goes with one channel.
    pub fn validate(&self) -> Result<(), RawError> {
        if self.width == 0 || self.height == 0 {
            return Err(RawError::Frame("the sensor data is empty"));
        }
        if !matches!(self.channels, 1 | 3) {
            return Err(RawError::Frame("sensor data has 1 or 3 samples per pixel"));
        }
        if self.cfa.is_some() && self.channels != 1 {
            return Err(RawError::Frame("a color filter array needs one sample per pixel"));
        }
        let expected = self.width.checked_mul(self.height).and_then(|n| n.checked_mul(self.channels));
        if expected != Some(self.samples.len()) {
            return Err(RawError::Frame("the sample count doesn't match the size"));
        }
        Ok(())
    }
}

/// Where a camera's color model comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorSource {
    /// The file's own matrices (DNG files, and raws that carry them).
    File,
    /// Matrices fitted to the camera model's measured spectral sensitivities (LightCraft's table, from the Academy
    /// Software Foundation's rawtoaces-data).
    Measured,
    /// None known: the camera's RGB is taken for linear sRGB, so colors are approximate (stacking is unaffected).
    #[default]
    Fallback,
}

/// The camera's color model, as the DNG specification (chapter 6, "Mapping Camera Color Space to CIE XYZ Space")
/// describes one: matrices between CIE XYZ and the camera's raw RGB at one or two calibration illuminants, and the
/// white balance the camera recorded. A file with no matrices has none here, and develops with a fallback model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CameraColor {
    /// `CalibrationIlluminant1` and `2`, as EXIF `LightSource` codes (0: unknown).
    pub illuminants: [u16; 2],
    /// `ColorMatrix1` and `2`: CIE XYZ to reference camera RGB, row by row.
    pub color_matrices: [Option<[[f64; 3]; 3]>; 2],
    /// `ForwardMatrix1` and `2`: white-balanced camera RGB to CIE XYZ relative to D50.
    pub forward_matrices: [Option<[[f64; 3]; 3]>; 2],
    /// `CameraCalibration1` and `2`.
    pub calibrations: [Option<[[f64; 3]; 3]>; 2],
    /// `AnalogBalance`.
    pub analog_balance: Option<[f64; 3]>,
    /// `AsShotNeutral`: the camera RGB of a neutral surface under the scene's light.
    pub as_shot_neutral: Option<[f64; 3]>,
    /// `AsShotWhiteXY`.
    pub as_shot_white: Option<Chromaticity>,
    /// White-balance multipliers for red, green and blue that the camera recorded in its maker notes (files other
    /// than DNG).
    pub camera_multipliers: Option<[f32; 3]>,
    /// `BaselineExposure`: the EV that a "normal" rendering adds. Recorded, never applied: developed frames stay
    /// scene-referred, with 1.0 at the sensor's clip level.
    pub baseline_exposure: f64,
    pub source: ColorSource,
}

/// What a RAW file holds, from its headers.
#[derive(Clone, Debug, PartialEq)]
pub struct RawInfo {
    pub format: RawFormat,
    /// The camera maker and model (EXIF `Make` and `Model`).
    pub make: Option<String>,
    pub model: Option<String>,
    /// The size of the developed picture, before orientation: the default crop, rows stretched for photosites that
    /// aren't square.
    pub width: usize,
    pub height: usize,
    pub layout: SensorLayout,
    /// Significant bits of the stored samples.
    pub bits: u32,
    /// The orientation the file records.
    pub orientation: Orientation,
    /// Where the camera's color model comes from.
    pub color: ColorSource,
    /// The white the camera's white balance recorded, if the file records one.
    pub as_shot_white: Option<Chromaticity>,
    /// Whether the file records a lens distortion correction (a DNG `WarpRectilinear` or the camera's own), which
    /// developing applies only when [`crate::DevelopSettings::lens_distortion`] asks for it.
    pub distortion_correction: bool,
}

/// The file's own corrections for after demosaicing (DNG `OpcodeList3`: lens warps and vignetting), in the
/// decoder's terms. Only this crate's decoders make them; a frame built with [`RawFrame::new`] has none.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Corrections(pub(crate) Vec<lightcraft_raw::Opcode>);

/// A decoded RAW file: the sensor's samples, and what developing them needs.
#[derive(Clone, Debug)]
pub struct RawFrame {
    pub info: RawInfo,
    pub sensor: Mosaic,
    pub color: CameraColor,
    /// The default crop: the part of the sensor data the picture shows.
    pub crop: Rect,
    /// The height of a photosite over its width: 1 for square photosites, 2 for the Nikon D1X, whose rows
    /// developing doubles.
    pub pixel_aspect: f32,
    pub(crate) corrections: Corrections,
}

impl RawFrame {
    /// A frame from another decoder's output, checked for consistency. It carries no lens corrections of its own.
    pub fn new(info: RawInfo, sensor: Mosaic, color: CameraColor, crop: Rect, pixel_aspect: f32) -> Result<RawFrame, RawError> {
        let frame = RawFrame { info, sensor, color, crop, pixel_aspect, corrections: Corrections::default() };
        frame.validate()?;
        Ok(frame)
    }

    /// Checks that the parts fit together: the samples fill the sensor size, the crop lies inside it and the pixel
    /// aspect is sane.
    pub fn validate(&self) -> Result<(), RawError> {
        self.sensor.validate()?;
        if self.crop.is_empty() || !self.crop.fits_in(self.sensor.width, self.sensor.height) {
            return Err(RawError::Frame("the default crop lies outside the sensor data"));
        }
        if !(self.pixel_aspect.is_finite() && (0.25..=4.0).contains(&self.pixel_aspect)) {
            return Err(RawError::Frame("the pixel aspect is not between 1/4 and 4"));
        }
        Ok(())
    }

    /// Whether the file was already demosaiced by a raw converter (a `LinearRaw` DNG): it is imported unchanged.
    pub fn is_converted(&self) -> bool {
        self.sensor.layout() == SensorLayout::Linear
    }

    /// The part `rect` of the sensor data (in its coordinates) as a frame of its own, with the color filter array
    /// shifted to match and the whole of it as the crop: a region at full resolution, for a loupe preview or a test.
    /// The file's lens corrections, which describe the whole frame, are left out.
    pub fn region(&self, rect: Rect) -> Result<RawFrame, RawError> {
        let m = &self.sensor;
        m.validate()?;
        if rect.is_empty() || !rect.fits_in(m.width, m.height) {
            return Err(RawError::Frame("the region lies outside the sensor data"));
        }
        let c = m.channels;
        let mut samples = Vec::new();
        samples.try_reserve_exact(rect.width * rect.height * c).map_err(|_| RawError::Frame("the region is too large to copy"))?;
        for y in rect.y..rect.y + rect.height {
            let start = (y * m.width + rect.x) * c;
            let row = m.samples.get(start..start + rect.width * c).ok_or(RawError::Frame("the sample count doesn't match the size"))?;
            samples.extend_from_slice(row);
        }
        let cfa = m.cfa.as_ref().map(|cfa| cfa.shifted(rect.x, rect.y));
        let sensor = Mosaic { width: rect.width, height: rect.height, channels: c, samples, cfa, float: m.float };
        let width = rect.width;
        let height = crate::develop::stretched(rect.height, self.pixel_aspect);
        let info = RawInfo { width, height, distortion_correction: false, ..self.info.clone() };
        let crop = Rect { x: 0, y: 0, width: rect.width, height: rect.height };
        Ok(RawFrame { info, sensor, color: self.color.clone(), crop, pixel_aspect: self.pixel_aspect, corrections: Corrections::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bayer_and_xtrans_patterns_are_recognized() {
        let rggb = Cfa::bayer("RGGB").unwrap();
        assert_eq!(rggb.layout(), SensorLayout::Bayer);
        assert_eq!(rggb.shifted(1, 0).name(), "GRBG");
        assert_eq!(rggb.shifted(1, 1).name(), "BGGR");
        assert_eq!(rggb.color_at(3, 3), 2);
        assert!(Cfa::bayer("RGGX").is_none() && Cfa::bayer("RRGB").is_none() && Cfa::bayer("RGB").is_none());
        // The X-Trans layout as LightCraft writes it, anchored at (0, 0).
        let x = Cfa::new(6, 6, lightcraft_raw::Cfa::xtrans().pattern).unwrap();
        assert_eq!(x.layout(), SensorLayout::XTrans);
        assert_eq!(x.shifted(2, 1).layout(), SensorLayout::XTrans, "every shift of X-Trans is X-Trans");
        let other = Cfa::new(2, 2, vec![0, 1, 2, 2]).unwrap();
        assert_eq!(other.layout(), SensorLayout::OtherCfa);
        assert!(Cfa::new(2, 2, vec![0, 1, 1, 1]).is_none(), "no blue");
        assert!(Cfa::new(17, 1, vec![0; 17]).is_none() && Cfa::new(2, 2, vec![0, 1, 3, 2]).is_none());
    }

    fn mosaic(width: usize, height: usize) -> Mosaic {
        let samples = (0..width * height).map(|i| i as f32 / 100.0).collect();
        Mosaic { width, height, channels: 1, samples, cfa: Cfa::bayer("GRBG"), float: false }
    }

    fn info() -> RawInfo {
        RawInfo {
            format: RawFormat::Dng,
            make: None,
            model: None,
            width: 6,
            height: 4,
            layout: SensorLayout::Bayer,
            bits: 14,
            orientation: Orientation::Normal,
            color: ColorSource::Fallback,
            as_shot_white: None,
            distortion_correction: false,
        }
    }

    #[test]
    fn frames_check_their_parts() {
        let crop = Rect { x: 0, y: 0, width: 6, height: 4 };
        let frame = RawFrame::new(info(), mosaic(6, 4), CameraColor::default(), crop, 1.0).unwrap();
        assert!(!frame.is_converted());
        let mut short = mosaic(6, 4);
        short.samples.pop();
        assert!(matches!(RawFrame::new(info(), short, CameraColor::default(), crop, 1.0), Err(RawError::Frame(_))));
        let outside = Rect { x: 1, y: 0, width: 6, height: 4 };
        assert!(RawFrame::new(info(), mosaic(6, 4), CameraColor::default(), outside, 1.0).is_err());
        assert!(RawFrame::new(info(), mosaic(6, 4), CameraColor::default(), crop, f32::NAN).is_err());
        let mut rgb_cfa = mosaic(2, 2);
        rgb_cfa.channels = 3;
        rgb_cfa.samples = vec![0.0; 12];
        assert!(rgb_cfa.validate().is_err(), "a CFA with three channels");
    }

    #[test]
    fn a_region_keeps_its_samples_and_the_cfa_phase() {
        let crop = Rect { x: 0, y: 0, width: 6, height: 4 };
        let frame = RawFrame::new(info(), mosaic(6, 4), CameraColor::default(), crop, 1.0).unwrap();
        let region = frame.region(Rect { x: 1, y: 1, width: 3, height: 2 }).unwrap();
        assert_eq!(region.sensor.samples, vec![0.07, 0.08, 0.09, 0.13, 0.14, 0.15]);
        assert_eq!(region.sensor.cfa.as_ref().unwrap().name(), Cfa::bayer("GRBG").unwrap().shifted(1, 1).name());
        assert_eq!((region.info.width, region.info.height), (3, 2));
        assert!(frame.region(Rect { x: 4, y: 0, width: 3, height: 1 }).is_err());
        assert!(frame.region(Rect { x: 0, y: 0, width: 0, height: 1 }).is_err());
    }

    #[test]
    fn whites_convert_to_temperatures_and_back() {
        let white = Chromaticity::from_temperature(5000.0, 10.0);
        let (t, tint) = white.temperature();
        assert!((t - 5000.0).abs() < 1.0 && (tint - 10.0).abs() < 0.1, "{t} {tint}");
        let (t65, _) = Chromaticity::D65.temperature();
        assert!((t65 - 6504.0).abs() < 30.0, "{t65}");
        assert!(Chromaticity::D65.is_plausible() && !Chromaticity { x: f64::NAN, y: 0.3 }.is_plausible());
    }
}
