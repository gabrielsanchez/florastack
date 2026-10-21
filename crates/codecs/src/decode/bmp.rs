//! BMP (the Windows and OS/2 bitmap formats), decoded with the image crate: 1 to 32 bits per pixel, palettes, bit
//! fields and run-length compression, bottom-up or top-down. BMP files carry no metadata this crate reads.

use std::io::Cursor;

use florastack_core::{AnyImage, Orientation};
use image::ImageDecoder as _;

use super::{byte_budget, check_size, image};
use crate::codec::{FileInfo, ImageDecoder, Stored};
use crate::color::ColorHint;
use crate::error::{CodecError, guarded};
use crate::format::Format;
use crate::metadata::Metadata;

pub(crate) struct BmpDecoder;

const F: Format = Format::Bmp;

fn channels(color: image::ColorType) -> Result<usize, CodecError> {
    match color {
        image::ColorType::L8 => Ok(1),
        image::ColorType::La8 => Ok(2),
        image::ColorType::Rgb8 => Ok(3),
        image::ColorType::Rgba8 => Ok(4),
        _ => Err(CodecError::Unsupported { format: F, what: "this pixel format" }),
    }
}

fn open(bytes: &[u8]) -> Result<image::codecs::bmp::BmpDecoder<Cursor<&[u8]>>, CodecError> {
    image::codecs::bmp::BmpDecoder::new(Cursor::new(bytes)).map_err(|e| CodecError::malformed(F, e))
}

impl ImageDecoder for BmpDecoder {
    fn format(&self) -> Format {
        F
    }

    fn probe(&self, bytes: &[u8]) -> Result<FileInfo, CodecError> {
        guarded(
            || {
                let d = open(bytes)?;
                let (w, h) = d.dimensions();
                let channels = channels(d.color_type())?;
                Ok(FileInfo {
                    format: F,
                    width: usize::try_from(w).unwrap_or(usize::MAX),
                    height: usize::try_from(h).unwrap_or(usize::MAX),
                    channels,
                    bits_per_sample: 8,
                    float: false,
                    alpha: channels == 2 || channels == 4,
                    orientation: Orientation::Normal,
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
                let mut limits = image::Limits::default();
                limits.max_alloc = Some(u64::try_from(byte_budget(max_pixels)).unwrap_or(u64::MAX));
                d.set_limits(limits).map_err(|e| CodecError::malformed(F, e))?;
                let channels = channels(d.color_type())?;
                let size = usize::try_from(d.total_bytes()).map_err(|_| CodecError::TooLarge {
                    width: u64::from(w),
                    height: u64::from(h),
                    max: max_pixels,
                })?;
                let mut buf = Vec::new();
                buf.try_reserve_exact(size).map_err(|_| CodecError::TooLarge { width: u64::from(w), height: u64::from(h), max: max_pixels })?;
                buf.resize(size, 0);
                d.read_image(&mut buf).map_err(|e| CodecError::malformed(F, e))?;
                let image = AnyImage::U8(image(w, h, channels, buf)?);
                let info = FileInfo {
                    format: F,
                    width: image.width(),
                    height: image.height(),
                    channels,
                    bits_per_sample: 8,
                    float: false,
                    alpha: channels == 2 || channels == 4,
                    orientation: Orientation::Normal,
                };
                Ok(Stored { image, info, hint: ColorHint::Untagged, metadata: Metadata::default(), warnings: Vec::new() })
            },
            F,
        )
    }
}
