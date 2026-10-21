//! JPEG (ITU-T T.81): 8-bit gray and YCbCr files decoded with zune-jpeg to their native samples; CMYK and YCCK files
//! go through LightCraft's decoder, which converts them with their profile. The metadata segments (EXIF, XMP, ICC in
//! chunks, IPTC) are read by lightcraft-meta.

use std::sync::Arc;

use florastack_core::AnyImage;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;

use super::{check_size, exif_orientation, fallback, image};
use crate::codec::{FileInfo, ImageDecoder, Stored};
use crate::color::ColorHint;
use crate::error::{CodecError, guarded};
use crate::format::Format;
use crate::metadata::{Metadata, exif};

pub(crate) struct JpegDecoder;

const F: Format = Format::Jpeg;

/// The largest side JPEG can describe (16-bit dimensions).
const MAX_SIDE: usize = 65_535;

fn options() -> DecoderOptions {
    DecoderOptions::default().set_max_width(MAX_SIDE).set_max_height(MAX_SIDE)
}

/// The image size, component count and color space, from the headers.
fn headers(bytes: &[u8]) -> Result<(u32, u32, u8, Option<ColorSpace>), CodecError> {
    guarded(
        || {
            let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options());
            d.decode_headers().map_err(|e| CodecError::malformed(F, e))?;
            let info = d.info().ok_or_else(|| CodecError::malformed(F, crate::error::BackendError::message("no frame header")))?;
            Ok((u32::from(info.width), u32::from(info.height), info.components, d.input_colorspace()))
        },
        F,
    )
}

fn metadata(bytes: &[u8]) -> (Metadata, Option<Arc<[u8]>>) {
    let segments = std::panic::catch_unwind(|| lightcraft_meta::jpeg_segments(bytes)).unwrap_or_default();
    let icc = segments.icc.map(Arc::from);
    (Metadata::from_parts(segments.exif, segments.xmp, segments.iptc), icc)
}

fn is_cmyk(space: Option<ColorSpace>) -> bool {
    matches!(space, Some(ColorSpace::CMYK | ColorSpace::YCCK))
}

impl ImageDecoder for JpegDecoder {
    fn format(&self) -> Format {
        F
    }

    fn probe(&self, bytes: &[u8]) -> Result<FileInfo, CodecError> {
        let (width, height, components, space) = headers(bytes)?;
        let (meta, _) = metadata(bytes);
        let channels = if components == 1 { 1 } else { 3 };
        Ok(FileInfo {
            format: F,
            width: usize::try_from(width).unwrap_or(usize::MAX),
            height: usize::try_from(height).unwrap_or(usize::MAX),
            channels: if is_cmyk(space) { 3 } else { channels },
            bits_per_sample: 8,
            float: false,
            alpha: false,
            orientation: exif_orientation(&meta),
        })
    }

    fn decode(&self, bytes: &[u8], max_pixels: u64) -> Result<Stored, CodecError> {
        let (width, height, components, space) = headers(bytes)?;
        check_size(u64::from(width), u64::from(height), max_pixels)?;
        let (meta, icc) = metadata(bytes);
        let orientation = exif_orientation(&meta);
        if is_cmyk(space) {
            let mut stored = fallback::decode(bytes, F, max_pixels)?;
            stored.metadata = meta;
            stored.info.orientation = orientation;
            return Ok(stored);
        }
        let gray = components == 1;
        let out = if gray { ColorSpace::Luma } else { ColorSpace::RGB };
        let pixels = guarded(
            || {
                let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options().jpeg_set_out_colorspace(out));
                d.decode().map_err(|e| CodecError::malformed(F, e))
            },
            F,
        )?;
        let channels = if gray { 1 } else { 3 };
        let image = AnyImage::U8(image(width, height, channels, pixels)?);
        let hint = match icc {
            Some(icc) => ColorHint::Icc(icc),
            None if meta.exif().is_some_and(exif::adobe_rgb_hint) => ColorHint::AdobeRgb,
            None => ColorHint::Untagged,
        };
        let info = FileInfo {
            format: F,
            width: image.width(),
            height: image.height(),
            channels,
            bits_per_sample: 8,
            float: false,
            alpha: false,
            orientation,
        };
        Ok(Stored { image, info, hint, metadata: meta, warnings: Vec::new() })
    }
}
