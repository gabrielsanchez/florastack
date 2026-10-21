//! PNG (PNG Specification, 3rd edition): every bit depth and color type, decoded with the png crate. Palettes,
//! transparency chunks and depths below 8 bits are expanded to 8 bits; 16-bit samples stay 16-bit. The color comes
//! from the first of `cICP`, `iCCP`, `sRGB` and `gAMA`/`cHRM`, the order the specification gives them; the EXIF from
//! `eXIf`, the XMP from the `XML:com.adobe.xmp` text chunk.

use std::io::Cursor;
use std::sync::Arc;

use florastack_core::AnyImage;

use super::{byte_budget, check_size, exif_orientation, image};
use crate::codec::{FileInfo, ImageDecoder, Stored};
use crate::color::{ColorHint, NamedSpace, Primaries};
use crate::error::{BackendError, CodecError, Warning, guarded};
use crate::format::Format;
use crate::metadata::Metadata;

pub(crate) struct PngDecoder;

const F: Format = Format::Png;
const XMP_KEYWORD: &str = "XML:com.adobe.xmp";

fn reader(bytes: &[u8], max_pixels: u64) -> Result<png::Reader<Cursor<&[u8]>>, CodecError> {
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: byte_budget(max_pixels) });
    decoder.set_transformations(png::Transformations::EXPAND);
    decoder.read_info().map_err(|e| CodecError::malformed(F, e))
}

fn channels(color: png::ColorType) -> usize {
    match color {
        png::ColorType::Grayscale | png::ColorType::Indexed => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
    }
}

/// The channels after expansion: a palette becomes RGB, a `tRNS` chunk adds alpha.
fn expanded_channels(info: &png::Info<'_>) -> usize {
    let base = match info.color_type {
        png::ColorType::Indexed => 3,
        other => channels(other),
    };
    if info.trns.is_some() && (base == 1 || base == 3) { base + 1 } else { base }
}

fn primaries(c: &png::SourceChromaticities) -> Primaries {
    let xy = |(x, y): (png::ScaledFloat, png::ScaledFloat)| (f64::from(x.into_value()), f64::from(y.into_value()));
    Primaries { red: xy(c.red), green: xy(c.green), blue: xy(c.blue), white: xy(c.white) }
}

/// What the `cICP` chunk (ITU-T H.273 code points) says, if it is something FloraStack models.
fn cicp_hint(primaries: u8, transfer: u8) -> Option<ColorHint> {
    let space = match primaries {
        1 => NamedSpace::Srgb,
        9 => NamedSpace::Rec2020,
        12 => NamedSpace::DisplayP3,
        _ => return None,
    };
    let chromaticities = |space| match space {
        NamedSpace::Rec2020 => Primaries { red: (0.708, 0.292), green: (0.170, 0.797), blue: (0.131, 0.046), white: (0.3127, 0.3290) },
        NamedSpace::DisplayP3 => Primaries { red: (0.680, 0.320), green: (0.265, 0.690), blue: (0.150, 0.060), white: (0.3127, 0.3290) },
        _ => Primaries::SRGB,
    };
    match (space, transfer) {
        (NamedSpace::Srgb, 13) => Some(ColorHint::Srgb),
        (_, 8) => Some(ColorHint::Linear { primaries: Some(chromaticities(space)) }),
        (NamedSpace::DisplayP3, 13) => Some(ColorHint::Standard(NamedSpace::DisplayP3)),
        (NamedSpace::Rec2020, 1 | 6 | 14 | 15) => Some(ColorHint::Standard(NamedSpace::Rec2020)),
        _ => None,
    }
}

fn color_hint(info: &png::Info<'_>, warnings: &mut Vec<Warning>) -> ColorHint {
    if let Some(cicp) = &info.coding_independent_code_points {
        match cicp_hint(cicp.color_primaries, cicp.transfer_function) {
            Some(hint) => return hint,
            None => warnings.push(Warning::ProfileIgnored("its cICP chunk names a color space FloraStack doesn't read, such as HDR")),
        }
    }
    if let Some(icc) = &info.icc_profile {
        return ColorHint::Icc(Arc::from(icc.as_ref()));
    }
    if info.srgb.is_some() {
        return ColorHint::Srgb;
    }
    if let Some(gama) = info.gama_chunk {
        let encoding = gama.into_value();
        let primaries = info.chrm_chunk.as_ref().map(primaries);
        if encoding > 0.0 && encoding.is_finite() {
            let gamma = 1.0 / encoding;
            if (gamma - 1.0).abs() < 1e-3 {
                return ColorHint::Linear { primaries };
            }
            return ColorHint::Gamma { gamma, primaries };
        }
    }
    ColorHint::Untagged
}

fn xmp(info: &png::Info<'_>) -> Option<String> {
    info.utf8_text
        .iter()
        .find(|t| t.keyword == XMP_KEYWORD)
        .and_then(|t| t.get_text().ok())
        .or_else(|| info.uncompressed_latin1_text.iter().find(|t| t.keyword == XMP_KEYWORD).map(|t| t.text.clone()))
}

impl ImageDecoder for PngDecoder {
    fn format(&self) -> Format {
        F
    }

    fn probe(&self, bytes: &[u8]) -> Result<FileInfo, CodecError> {
        guarded(
            || {
                let reader = reader(bytes, u64::MAX >> 8)?;
                let info = reader.info();
                let meta = Metadata::from_parts(info.exif_metadata.as_ref().map(|e| e.to_vec()), None, None);
                let channels = expanded_channels(info);
                Ok(FileInfo {
                    format: F,
                    width: usize::try_from(info.width).unwrap_or(usize::MAX),
                    height: usize::try_from(info.height).unwrap_or(usize::MAX),
                    channels,
                    bits_per_sample: info.bit_depth as u8,
                    float: false,
                    alpha: channels == 2 || channels == 4,
                    orientation: exif_orientation(&meta),
                })
            },
            F,
        )
    }

    fn decode(&self, bytes: &[u8], max_pixels: u64) -> Result<Stored, CodecError> {
        guarded(|| decode(bytes, max_pixels), F)
    }
}

fn decode(bytes: &[u8], max_pixels: u64) -> Result<Stored, CodecError> {
    let mut reader = reader(bytes, max_pixels)?;
    let (width, height) = (reader.info().width, reader.info().height);
    check_size(u64::from(width), u64::from(height), max_pixels)?;
    let bits = reader.info().bit_depth as u8;
    let size = reader.output_buffer_size().ok_or_else(|| CodecError::malformed(F, BackendError::message("the image is too large to address")))?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(size).map_err(|_| CodecError::TooLarge { width: u64::from(width), height: u64::from(height), max: max_pixels })?;
    buf.resize(size, 0);
    let frame = reader.next_frame(&mut buf).map_err(|e| CodecError::malformed(F, e))?;
    buf.truncate(frame.buffer_size());
    // Text and EXIF chunks may follow the image data.
    let _ = reader.finish();
    let info = reader.info();
    let channels = channels(frame.color_type);
    let image = match frame.bit_depth {
        png::BitDepth::Sixteen => {
            let samples = buf.as_chunks::<2>().0.iter().map(|b| u16::from_be_bytes(*b)).collect();
            AnyImage::U16(image(frame.width, frame.height, channels, samples)?)
        }
        _ => AnyImage::U8(image(frame.width, frame.height, channels, buf)?),
    };
    let mut warnings = Vec::new();
    let hint = color_hint(info, &mut warnings);
    let metadata = Metadata::from_parts(info.exif_metadata.as_ref().map(|e| e.to_vec()), xmp(info), None);
    let info = FileInfo {
        format: F,
        width: image.width(),
        height: image.height(),
        channels,
        bits_per_sample: bits,
        float: false,
        alpha: channels == 2 || channels == 4,
        orientation: exif_orientation(&metadata),
    };
    Ok(Stored { image, info, hint, metadata, warnings })
}
