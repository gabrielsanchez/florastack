//! The decoder trait ([`RawDecoder`]) and its built-in implementation over LightCraft's `lightcraft-raw`
//! ([`LightCraftRaw`]).
//!
//! The trait is the seam that keeps LightCraft replaceable: the rest of FloraStack decodes through it, and what it
//! returns ([`RawFrame`], [`RawInfo`]) is FloraStack's own plain data. Bumping the LightCraft pin touches this file
//! (and the develop steps in `develop.rs` that call LightCraft's algorithms); vendoring it touches nothing; another
//! decoder implements the trait.

use florastack_core::{Orientation, Rect};

use crate::error::{RawError, guarded};
use crate::format::RawFormat;
use crate::frame::{CameraColor, Cfa, Chromaticity, ColorSource, Corrections, Mosaic, RawFrame, RawInfo, SensorLayout};

/// Decodes camera RAW and DNG files.
pub trait RawDecoder: Send + Sync {
    /// The decoder's name and version, for logs and bug reports.
    fn name(&self) -> &'static str;

    /// The RAW format of `bytes` (a whole file, or at least its first megabyte), from their first bytes and their
    /// TIFF structure; `None` when they aren't a camera RAW or DNG file. A format may be recognized and still not
    /// decoded ([`RawFormat::is_decoded`]).
    fn probe(&self, bytes: &[u8]) -> Option<RawFormat>;

    /// What the file holds, from its headers, without decoding the sensor data.
    fn info(&self, bytes: &[u8]) -> Result<RawInfo, RawError>;

    /// The file's sensor data, normalized, and what developing it needs. Files of more than `max_pixels` photosites
    /// are refused from their headers, before the decode proper allocates their samples.
    fn decode(&self, bytes: &[u8], max_pixels: u64) -> Result<RawFrame, RawError>;
}

/// The decoder built on LightCraft's `lightcraft-raw` (pinned in the workspace manifest; `docs/lightcraft.md`).
#[derive(Clone, Copy, Debug, Default)]
pub struct LightCraftRaw;

impl RawDecoder for LightCraftRaw {
    fn name(&self) -> &'static str {
        "LightCraft lightcraft-raw 0.10.0 (a2a4138)"
    }

    fn probe(&self, bytes: &[u8]) -> Option<RawFormat> {
        // The probe parses the file's TIFF structure; guard it like everything else that reads the file.
        std::panic::catch_unwind(|| lightcraft_raw::probe(bytes)).ok().flatten().map(RawFormat::from_lightcraft)
    }

    fn info(&self, bytes: &[u8]) -> Result<RawInfo, RawError> {
        let format = self.probe(bytes).ok_or(RawError::NotRaw)?;
        if !format.is_decoded() {
            return Err(RawError::unsupported(format, "this format is recognized but not decoded"));
        }
        guarded(format, || {
            let lc = lightcraft_raw::probe_info(bytes).map_err(|e| RawError::from_lightcraft(format, e))?;
            Ok(info_of(format, &lc))
        })
    }

    fn decode(&self, bytes: &[u8], max_pixels: u64) -> Result<RawFrame, RawError> {
        let format = self.probe(bytes).ok_or(RawError::NotRaw)?;
        if !format.is_decoded() {
            return Err(RawError::unsupported(format, "this format is recognized but not decoded"));
        }
        guarded(format, || {
            // The headers first, so that an oversized file is refused before its samples are allocated. (For
            // uncompressed ORF and PEF files LightCraft's header read unpacks the samples itself, within its own cap of
            // 2^30 samples, to find their bit depth and borders.)
            let header = lightcraft_raw::probe_info(bytes).map_err(|e| RawError::from_lightcraft(format, e))?;
            check_size(header.width, header.height, max_pixels)?;
            let raw = lightcraft_raw::decode(bytes).map_err(|e| RawError::from_lightcraft(format, e))?;
            check_size(raw.width, raw.height, max_pixels)?;
            frame_of(format, raw)
        })
    }
}

fn check_size(width: usize, height: usize, max: u64) -> Result<(), RawError> {
    let (w, h) = (width as u64, height as u64);
    if w.saturating_mul(h) > max {
        return Err(RawError::TooLarge { width: w, height: h, max });
    }
    Ok(())
}

/// Whether an opcode moves pixels: the lens warps, which FloraStack applies only when asked (lens distortion
/// correction is off by default, because it changes with focus distance and fights alignment).
pub(crate) fn is_warp(op: &lightcraft_raw::Opcode) -> bool {
    use lightcraft_raw::Opcode;
    matches!(op, Opcode::WarpRectilinear { .. } | Opcode::WarpRectilinear2 { .. } | Opcode::WarpFisheye { .. })
}

/// The layout of LightCraft's data: its CFA, or none with one or three samples per pixel.
fn layout_of(cfa: Option<&lightcraft_raw::Cfa>, cpp: usize) -> SensorLayout {
    match (cfa.and_then(cfa_of), cpp) {
        (Some(cfa), 1) => cfa.layout(),
        (None, 1) => SensorLayout::Monochrome,
        // A CFA on multi-sample data, or several samples per pixel: LightCraft develops the first three as RGB.
        _ => SensorLayout::Linear,
    }
}

fn cfa_of(cfa: &lightcraft_raw::Cfa) -> Option<Cfa> {
    Cfa::new(cfa.width, cfa.height, cfa.pattern.clone())
}

/// The camera's color model: the file's matrices, else the measured ones for its model, else none (the fallback).
fn color_model(color: &lightcraft_raw::ColorData, multipliers: Option<[f32; 3]>, make: Option<&str>, model: Option<&str>) -> CameraColor {
    let mut color = color.clone();
    let source = if lightcraft_raw::color::has_matrix(&color) {
        ColorSource::File
    } else if let Some(camera) = model.and_then(|model| lightcraft_raw::spectral::find(make, model)) {
        camera.fill(&mut color);
        ColorSource::Measured
    } else {
        ColorSource::Fallback
    };
    let m = |m: &Option<lightcraft_raw::Mat3>| m.map(|m| m.0);
    CameraColor {
        illuminants: color.illuminant,
        color_matrices: [m(&color.color_matrix[0]), m(&color.color_matrix[1])],
        forward_matrices: [m(&color.forward_matrix[0]), m(&color.forward_matrix[1])],
        calibrations: [m(&color.camera_calibration[0]), m(&color.camera_calibration[1])],
        analog_balance: color.analog_balance,
        as_shot_neutral: color.as_shot_neutral,
        as_shot_white: color.as_shot_white_xy.map(Chromaticity::from_xy),
        camera_multipliers: multipliers.filter(|m| m.iter().all(|v| v.is_finite() && *v > 0.0)),
        baseline_exposure: color.baseline_exposure,
        source,
    }
}

/// The white the camera recorded, through `color`'s model, if it recorded one.
pub(crate) fn recorded_white(color: &CameraColor) -> Option<Chromaticity> {
    let recorded = color.as_shot_white.is_some() || color.as_shot_neutral.is_some() || color.camera_multipliers.is_some();
    recorded.then(|| Chromaticity::from_xy(lightcraft_raw::color::as_shot_white_of(&crate::color::color_data(color), color.camera_multipliers)))
}

fn info_of(format: RawFormat, lc: &lightcraft_raw::RawInfo) -> RawInfo {
    let (make, model) = (lc.metadata.make.clone(), lc.metadata.model.clone());
    let color = color_model(&lc.color, lc.wb_multipliers, make.as_deref(), model.as_deref());
    let (width, height) = lc.developed_size();
    RawInfo {
        format,
        width,
        height,
        layout: layout_of(lc.cfa.as_ref(), lc.cpp),
        bits: lc.bits,
        orientation: Orientation::from_exif(lc.orientation.to_exif()).unwrap_or_default(),
        color: color.source,
        as_shot_white: recorded_white(&color),
        distortion_correction: lc.opcodes.list3.iter().any(is_warp),
        make,
        model,
    }
}

/// FloraStack's frame for a file LightCraft decoded: the samples normalized (black level, white level, the DNG
/// opcode lists that apply to the raw samples, the active area), the corrections after demosaicing kept for the
/// develop step.
fn frame_of(format: RawFormat, mut raw: lightcraft_raw::RawImage) -> Result<RawFrame, RawError> {
    let lc = |e| RawError::from_lightcraft(format, e);
    raw.validate().map_err(lc)?;
    // Lens warps belong after demosaicing (OpcodeList3); one in the lists that run on the mosaic can't be switched
    // off with the other lens corrections, and is left out.
    raw.opcodes.list1.retain(|op| !is_warp(op));
    raw.opcodes.list2.retain(|op| !is_warp(op));
    let corrections = Corrections(std::mem::take(&mut raw.opcodes.list3));
    let float = matches!(raw.data, lightcraft_raw::RawData::F32(_));
    let normalized = raw.normalized().map_err(lc)?;
    // The samples are in `normalized` now: free the stored ones before the rest is built.
    raw.data = lightcraft_raw::RawData::U16(Vec::new());
    let info = raw.info();
    let crop = raw.develop_crop(normalized.width, normalized.height);
    let layout = layout_of(normalized.cfa.as_ref(), normalized.cpp);
    let mut info = info_of(format, &info);
    info.distortion_correction = corrections.0.iter().any(is_warp);
    info.layout = layout;
    let (channels, samples) = match normalized.cpp {
        1 | 3 => (normalized.cpp, normalized.data),
        // Four samples a pixel (some DNGs): LightCraft develops the first three as RGB, and so does FloraStack.
        cpp => (3, normalized.data.chunks_exact(cpp).flat_map(|px| px.iter().take(3).copied()).collect()),
    };
    let cfa = if channels == 1 { normalized.cfa.as_ref().and_then(cfa_of) } else { None };
    let sensor = Mosaic { width: normalized.width, height: normalized.height, channels, samples, cfa, float };
    let make = info.make.clone();
    let model = info.model.clone();
    let color = color_model(&raw.color, raw.wb_multipliers, make.as_deref(), model.as_deref());
    let crop = Rect { x: crop.x, y: crop.y, width: crop.width, height: crop.height };
    let frame = RawFrame { info, sensor, color, crop, pixel_aspect: raw.pixel_aspect, corrections };
    frame.validate()?;
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_that_are_not_raw_are_told_apart() {
        let d = LightCraftRaw;
        for bytes in [&b""[..], b"hello", b"\x89PNG\r\n\x1a\n\0\0\0\x0d", &[0xff, 0xd8, 0xff, 0xe0, 0, 0]] {
            assert_eq!(d.probe(bytes), None);
            assert!(matches!(d.info(bytes), Err(RawError::NotRaw)));
            assert!(matches!(d.decode(bytes, u64::MAX), Err(RawError::NotRaw)));
        }
        assert_eq!(d.probe(b"FUJIFILMCCD-RAW 0201FF383501"), Some(RawFormat::Raf));
        assert!(matches!(d.decode(b"FUJIFILMCCD-RAW 0201FF383501", u64::MAX), Err(RawError::Malformed { format: RawFormat::Raf, .. })));
        // Recognized, never decoded: the advice to convert comes with it.
        let mrw = b"\0MRM\0\0\0\0";
        assert_eq!(d.probe(mrw), Some(RawFormat::Mrw));
        let e = d.decode(mrw, u64::MAX).unwrap_err();
        assert!(matches!(e, RawError::Unsupported { format: RawFormat::Mrw, .. }), "{e:?}");
        assert!(e.to_string().contains("DNG") && e.to_string().contains("16-bit TIFF"), "{e}");
    }
}
