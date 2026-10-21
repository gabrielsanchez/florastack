//! WebP (RFC 9649), decoded with image-webp: lossy and lossless, with or without alpha; the first frame of an
//! animation. The `ICCP`, `EXIF` and `XMP ` chunks come with it.

use std::io::Cursor;
use std::sync::Arc;

use florastack_core::AnyImage;

use super::{byte_budget, check_size, exif_orientation, image};
use crate::codec::{FileInfo, ImageDecoder, Stored};
use crate::color::ColorHint;
use crate::error::{BackendError, CodecError, guarded};
use crate::format::Format;
use crate::metadata::Metadata;

pub(crate) struct WebPDecoder;

const F: Format = Format::WebP;

fn open(bytes: &[u8]) -> Result<image_webp::WebPDecoder<Cursor<&[u8]>>, CodecError> {
    image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|e| CodecError::malformed(F, e))
}

fn metadata(d: &mut image_webp::WebPDecoder<Cursor<&[u8]>>) -> (Metadata, Option<Arc<[u8]>>) {
    let exif = d.exif_metadata().ok().flatten();
    let xmp = d.xmp_metadata().ok().flatten().map(|b| String::from_utf8_lossy(&b).into_owned());
    let icc = d.icc_profile().ok().flatten().map(Arc::from);
    (Metadata::from_parts(exif, xmp, None), icc)
}

impl ImageDecoder for WebPDecoder {
    fn format(&self) -> Format {
        F
    }

    fn probe(&self, bytes: &[u8]) -> Result<FileInfo, CodecError> {
        guarded(
            || {
                let mut d = open(bytes)?;
                let (w, h) = d.dimensions();
                let alpha = d.has_alpha();
                let (meta, _) = metadata(&mut d);
                Ok(FileInfo {
                    format: F,
                    width: usize::try_from(w).unwrap_or(usize::MAX),
                    height: usize::try_from(h).unwrap_or(usize::MAX),
                    channels: if alpha { 4 } else { 3 },
                    bits_per_sample: 8,
                    float: false,
                    alpha,
                    orientation: exif_orientation(&meta),
                })
            },
            F,
        )
    }

    fn decode(&self, bytes: &[u8], max_pixels: u64) -> Result<Stored, CodecError> {
        guarded(
            || {
                let mut d = open(bytes)?;
                let (w, h) = d.dimensions();
                check_size(u64::from(w), u64::from(h), max_pixels)?;
                d.set_memory_limit(byte_budget(max_pixels));
                let alpha = d.has_alpha();
                let (meta, icc) = metadata(&mut d);
                let size =
                    d.output_buffer_size().ok_or_else(|| CodecError::malformed(F, BackendError::message("the image is too large to address")))?;
                let mut buf = Vec::new();
                buf.try_reserve_exact(size).map_err(|_| CodecError::TooLarge { width: u64::from(w), height: u64::from(h), max: max_pixels })?;
                buf.resize(size, 0);
                d.read_image(&mut buf).map_err(|e| CodecError::malformed(F, e))?;
                let channels = if alpha { 4 } else { 3 };
                let image = AnyImage::U8(image(w, h, channels, buf)?);
                let orientation = exif_orientation(&meta);
                let info = FileInfo {
                    format: F,
                    width: image.width(),
                    height: image.height(),
                    channels,
                    bits_per_sample: 8,
                    float: false,
                    alpha,
                    orientation,
                };
                let hint = icc.map_or(ColorHint::Untagged, ColorHint::Icc);
                Ok(Stored { image, info, hint, metadata: meta, warnings: Vec::new() })
            },
            F,
        )
    }
}
