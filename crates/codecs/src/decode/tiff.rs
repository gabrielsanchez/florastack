//! TIFF (TIFF 6.0, with BigTIFF): the samples decoded by the tiff crate (uncompressed, LZW, Deflate, PackBits, JPEG
//! and CCITT; strips or tiles; contiguous or planar; horizontal and floating-point predictors), the metadata and the
//! profile read from the IFDs by lightcraft-tiff. The first image of the file is read.
//!
//! Gray, gray and alpha, RGB and RGBA files keep their samples: 8 and 16 bits as they are, 1 to 4 bits widened to 8,
//! 9 to 15 bits to 16, 32 and 64-bit integers narrowed to 16, 16 and 64-bit floats converted to 32. Associated alpha becomes straight
//! alpha. CMYK files go through LightCraft's decoder, which converts them with their profile.

use std::io::Cursor;
use std::sync::Arc;

use florastack_core::{AnyImage, Orientation};
use lightcraft_tiff::{Tiff, tags as t};
use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

use super::{byte_budget, check_size, fallback, image, interleave, unpremultiply};
use crate::codec::{FileInfo, ImageDecoder, Stored};
use crate::color::ColorHint;
use crate::error::{BackendError, CodecError, guarded};
use crate::format::Format;
use crate::metadata::{self, Metadata};

pub(crate) struct TiffDecoder;

const F: Format = Format::Tiff;

/// A tiff crate error: what it doesn't support said so, the rest a damaged file.
fn tiff_error(e: tiff::TiffError) -> CodecError {
    match e {
        tiff::TiffError::UnsupportedError(_) => {
            CodecError::UnsupportedVariant { format: F, advice: "convert it to an 8-bit, 16-bit or 32-bit float TIFF first", source: e.into() }
        }
        other => CodecError::malformed(F, other),
    }
}

fn decoder(bytes: &[u8], max_pixels: u64) -> Result<Decoder<Cursor<&[u8]>>, CodecError> {
    let mut limits = Limits::default();
    limits.decoding_buffer_size = byte_budget(max_pixels);
    // A strip or tile may be as large as the image it holds (an uncompressed, single-strip 16-bit export).
    limits.intermediate_buffer_size = limits.intermediate_buffer_size.max(limits.decoding_buffer_size);
    limits.ifd_value_size = 32 << 20;
    Ok(Decoder::new(Cursor::new(bytes)).map_err(tiff_error)?.with_limits(limits))
}

/// The layout of a color type: (color samples, samples per pixel, bits per sample), or why it isn't read here.
enum Layout {
    Samples { color: usize, samples: usize, bits: u8 },
    Cmyk,
}

fn layout(ct: ColorType, compression: Option<u32>) -> Result<Layout, CodecError> {
    Ok(match ct {
        ColorType::Gray(b) => Layout::Samples { color: 1, samples: 1, bits: b },
        ColorType::GrayA(b) => Layout::Samples { color: 1, samples: 2, bits: b },
        ColorType::RGB(b) => Layout::Samples { color: 3, samples: 3, bits: b },
        ColorType::RGBA(b) => Layout::Samples { color: 3, samples: 4, bits: b },
        // JPEG-compressed YCbCr comes out of the JPEG decoder as RGB.
        ColorType::YCbCr(b) if compression == Some(7) => Layout::Samples { color: 3, samples: 3, bits: b },
        ColorType::Multiband { bit_depth, num_samples } if num_samples >= 3 => {
            Layout::Samples { color: 3, samples: usize::from(num_samples), bits: bit_depth }
        }
        ColorType::Multiband { bit_depth, num_samples } if num_samples >= 1 => {
            Layout::Samples { color: 1, samples: usize::from(num_samples), bits: bit_depth }
        }
        ColorType::CMYK(_) | ColorType::CMYKA(_) => Layout::Cmyk,
        ColorType::Palette(_) => return Err(CodecError::Unsupported { format: F, what: "a color palette" }),
        ColorType::Lab(_) => return Err(CodecError::Unsupported { format: F, what: "CIE L*a*b* samples" }),
        _ => return Err(CodecError::Unsupported { format: F, what: "this color type" }),
    })
}

/// The decoded samples, as the sample types an image holds.
enum Samples {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Narrowing shifts and float conversions.
fn samples(result: DecodingResult, bits: u8, width: usize, height: usize, per_pixel: usize) -> Result<Samples, CodecError> {
    Ok(match result {
        DecodingResult::U8(v) if bits < 8 => Samples::U8(unpack(&v, width, height, per_pixel, bits)?),
        DecodingResult::U8(v) => Samples::U8(v),
        // 9 to 15-bit samples (scientific and some camera files) are fractions of their own maximum, as every sample
        // is: scaled to 16 bits, rounding to nearest.
        DecodingResult::U16(v) if (9..16).contains(&bits) => {
            let max = (1u32 << bits) - 1;
            Samples::U16(v.into_iter().map(|x| ((u32::from(x).min(max) * 65_535 + max / 2) / max) as u16).collect())
        }
        DecodingResult::U16(v) => Samples::U16(v),
        // Wider integers keep their 16 most significant bits.
        DecodingResult::U32(v) => Samples::U16(v.into_iter().map(|x| (x >> 16) as u16).collect()),
        DecodingResult::U64(v) => Samples::U16(v.into_iter().map(|x| (x >> 48) as u16).collect()),
        // Signed integers: the negative half is black.
        DecodingResult::I8(v) => Samples::U8(v.into_iter().map(|x| (x.max(0) as u8) << 1).collect()),
        DecodingResult::I16(v) => Samples::U16(v.into_iter().map(|x| (x.max(0) as u16) << 1).collect()),
        DecodingResult::I32(v) => Samples::U16(v.into_iter().map(|x| (x.max(0) >> 15) as u16).collect()),
        DecodingResult::I64(v) => Samples::U16(v.into_iter().map(|x| (x.max(0) >> 47) as u16).collect()),
        // The `half` crate's `f16`, which this crate doesn't name: a closure rather than a method path.
        #[allow(clippy::redundant_closure_for_method_calls)]
        DecodingResult::F16(v) => Samples::F32(v.into_iter().map(|x| x.to_f32()).collect()),
        DecodingResult::F32(v) => Samples::F32(v),
        DecodingResult::F64(v) => Samples::F32(v.into_iter().map(|x| x as f32).collect()),
    })
}

/// Samples narrower than a byte arrive packed, most significant bits first, each row padded to a whole byte: one
/// per byte, scaled to 0..=255.
fn unpack(packed: &[u8], width: usize, height: usize, per_pixel: usize, bits: u8) -> Result<Vec<u8>, CodecError> {
    let unsupported = CodecError::Unsupported { format: F, what: "samples of 3, 5, 6 or 7 bits" };
    if !matches!(bits, 1 | 2 | 4) {
        return Err(unsupported);
    }
    let bits = usize::from(bits);
    let per_row = width.checked_mul(per_pixel).ok_or(CodecError::Malformed { format: F, source: BackendError::message("row too wide") })?;
    let row_bytes = per_row.checked_mul(bits).map_or(0, |b| b.div_ceil(8));
    if row_bytes == 0 || packed.len() / row_bytes < height {
        return Err(CodecError::malformed(F, BackendError::message("the image data is cut short")));
    }
    let max = u8::MAX >> (8 - bits);
    let scale = 255 / max;
    let mut out = Vec::with_capacity(per_row.saturating_mul(height));
    for row in packed.chunks_exact(row_bytes).take(height) {
        let values = row.iter().flat_map(|&b| (1..=8 / bits).map(move |i| (b >> (8 - bits * i)) & max));
        out.extend(values.take(per_row).map(|v| v * scale));
    }
    Ok(out)
}

/// The first `keep` of every `per_pixel` samples.
fn select<T: Copy>(v: Vec<T>, per_pixel: usize, keep: usize) -> Vec<T> {
    if per_pixel == keep {
        return v;
    }
    v.chunks_exact(per_pixel).flat_map(|px| px.iter().take(keep).copied()).collect()
}

/// The file's metadata and embedded profile, from its IFD0.
fn metadata(bytes: &[u8]) -> (Metadata, Option<Arc<[u8]>>, Orientation) {
    let Some(tiff) = std::panic::catch_unwind(|| Tiff::parse(bytes).ok()).ok().flatten() else {
        return (Metadata::default(), None, Orientation::Normal);
    };
    let ifd0 = tiff.ifds.first();
    let icc = ifd0.and_then(|i| i.bytes(t::ICC_PROFILE)).map(Arc::from);
    let orientation = ifd0.and_then(|i| i.u16(t::ORIENTATION)).and_then(Orientation::from_exif).unwrap_or_default();
    (metadata::from_tiff(&tiff), icc, orientation)
}

impl ImageDecoder for TiffDecoder {
    fn format(&self) -> Format {
        F
    }

    fn probe(&self, bytes: &[u8]) -> Result<FileInfo, CodecError> {
        guarded(
            || {
                let mut d = decoder(bytes, u64::MAX >> 8)?;
                let (w, h) = d.dimensions().map_err(tiff_error)?;
                let ct = d.colortype().map_err(tiff_error)?;
                let compression = d.get_tag_u32(Tag::Compression).ok();
                let float = d.get_tag_u16_vec(Tag::SampleFormat).ok().and_then(|v| v.first().copied()) == Some(3);
                let (channels, bits) = match layout(ct, compression)? {
                    Layout::Samples { color, samples, bits } => (color + usize::from(samples > color), bits),
                    Layout::Cmyk => (3, 8),
                };
                let (_, _, orientation) = metadata(bytes);
                Ok(FileInfo {
                    format: F,
                    width: usize::try_from(w).unwrap_or(usize::MAX),
                    height: usize::try_from(h).unwrap_or(usize::MAX),
                    channels,
                    bits_per_sample: bits,
                    float,
                    alpha: channels == 2 || channels == 4,
                    orientation,
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
    let mut d = decoder(bytes, max_pixels)?;
    let (w, h) = d.dimensions().map_err(tiff_error)?;
    check_size(u64::from(w), u64::from(h), max_pixels)?;
    let ct = d.colortype().map_err(tiff_error)?;
    let compression = d.get_tag_u32(Tag::Compression).ok();
    let planar = d.get_tag_u32(Tag::PlanarConfiguration).ok() == Some(2);
    let extra = d.get_tag_u16_vec(Tag::ExtraSamples).ok().unwrap_or_default();
    let float = d.get_tag_u16_vec(Tag::SampleFormat).ok().and_then(|v| v.first().copied()) == Some(3);
    let (meta, icc, orientation) = metadata(bytes);
    let (color, per_pixel, bits) = match layout(ct, compression)? {
        Layout::Samples { color, samples, bits } => (color, samples, bits),
        Layout::Cmyk => {
            let mut stored = fallback::decode(bytes, F, max_pixels)?;
            stored.metadata = meta;
            stored.info.orientation = orientation;
            return Ok(stored);
        }
    };
    if bits < 8 && planar && per_pixel > 1 {
        return Err(CodecError::Unsupported { format: F, what: "planar samples narrower than a byte" });
    }
    let mut result = DecodingResult::U8(Vec::new());
    let layout_pref = d.read_image_to_buffer(&mut result).map_err(tiff_error)?;
    // A planar image whose planes don't all fit the decoder's buffer comes back with its first plane only.
    if planar && per_pixel > 1 && result.as_buffer(0).as_bytes().len() < layout_pref.complete_len {
        return Err(CodecError::Unsupported { format: F, what: "planar images too large to decode in one buffer" });
    }
    let (width, height) = (usize::try_from(w).unwrap_or(usize::MAX), usize::try_from(h).unwrap_or(usize::MAX));
    let pixels = width.saturating_mul(height);
    let samples = samples(result, bits, width, height, per_pixel)?;
    let has_alpha = per_pixel > color;
    let keep = color + usize::from(has_alpha);
    let premultiplied = has_alpha && extra.first() == Some(&1);
    macro_rules! finish {
        ($v:expr) => {{
            let mut v = $v;
            if planar && per_pixel > 1 {
                v = interleave(&v, pixels, per_pixel);
            }
            let mut v = select(v, per_pixel, keep);
            if premultiplied {
                unpremultiply(&mut v, keep);
            }
            image(w, h, keep, v)?
        }};
    }
    let image = match samples {
        Samples::U8(v) => AnyImage::U8(finish!(v)),
        Samples::U16(v) => AnyImage::U16(finish!(v)),
        Samples::F32(v) => AnyImage::F32(finish!(v)),
    };
    let hint = icc.map_or(ColorHint::Untagged, ColorHint::Icc);
    let info = FileInfo { format: F, width, height, channels: keep, bits_per_sample: bits, float, alpha: has_alpha, orientation };
    Ok(Stored { image, info, hint, metadata: meta, warnings: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_of_9_to_15_bits_scale_to_16() {
        let Samples::U16(v) = samples(DecodingResult::U16(vec![0, 2048, 4095, 9999]), 12, 4, 1, 1).unwrap() else { panic!("u16") };
        assert_eq!(v, [0, 32_776, 65_535, 65_535], "white is white, and values past the maximum clip");
        let Samples::U16(v) = samples(DecodingResult::U16(vec![1023]), 10, 1, 1, 1).unwrap() else { panic!("u16") };
        assert_eq!(v, [65_535]);
    }

    #[test]
    fn packed_samples_unpack_to_bytes() {
        assert_eq!(unpack(&[0b1010_0000], 3, 1, 1, 1).unwrap(), [255, 0, 255]);
        assert_eq!(unpack(&[0b1100_0110], 2, 1, 1, 4).unwrap(), [204, 102]);
        assert!(unpack(&[0], 9, 1, 1, 1).is_err(), "rows cut short");
        assert!(unpack(&[0], 1, 1, 1, 3).is_err(), "3-bit samples");
    }
}
