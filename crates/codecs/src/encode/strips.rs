//! TIFF image data: rows of samples cut into strips, each run through the predictor and compressed, in parallel.
//! Strips are compressed independently and collected in order, so the bytes don't depend on scheduling.
//!
//! - Horizontal differencing (TIFF 6.0, section 14, predictor 2) for integer samples: each sample minus the same
//!   channel of the pixel before it, row by row, wrapping.
//! - The floating-point predictor (Adobe Photoshop TIFF Technical Note 3, predictor 3) for floats: each row's
//!   samples split into byte planes, most significant byte first, then horizontal differencing of those bytes with
//!   the pixel's sample count as the distance.
//! - Deflate as zlib streams (compression 8, "Adobe Deflate") with `miniz_oxide`; LZW with TIFF's code-size switch one
//!   code early (compression 5) with weezl.

use florastack_core::{Image, Sample};
use rayon::prelude::*;

use crate::error::{BackendError, CodecError};
use crate::format::Format;
use crate::options::TiffCompression;

/// The size of a strip before compression: big enough to compress well, small enough to spread over threads.
const STRIP_BYTES: usize = 256 << 10;

/// The zlib level of Deflate strips: 6, the usual balance of size and time.
const DEFLATE_LEVEL: u8 = 6;

/// A sample type as the TIFF writer stores it.
pub(crate) trait TiffSample: Sample {
    const BITS: u16;
    const FLOAT: bool;
    fn put_le(self, out: &mut Vec<u8>);
    fn put_be(self, out: &mut Vec<u8>);
    /// `self - prev`, wrapping: horizontal differencing (integers only; floats use the byte-plane predictor).
    fn minus(self, prev: Self) -> Self;
}

impl TiffSample for u8 {
    const BITS: u16 = 8;
    const FLOAT: bool = false;
    fn put_le(self, out: &mut Vec<u8>) {
        out.push(self);
    }
    fn put_be(self, out: &mut Vec<u8>) {
        out.push(self);
    }
    fn minus(self, prev: u8) -> u8 {
        self.wrapping_sub(prev)
    }
}

impl TiffSample for u16 {
    const BITS: u16 = 16;
    const FLOAT: bool = false;
    fn put_le(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
    fn put_be(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn minus(self, prev: u16) -> u16 {
        self.wrapping_sub(prev)
    }
}

impl TiffSample for f32 {
    const BITS: u16 = 32;
    const FLOAT: bool = true;
    fn put_le(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
    fn put_be(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn minus(self, _prev: f32) -> f32 {
        self
    }
}

/// The predictor tag value for these samples and this compression: 1 (none), 2 (horizontal) or 3 (floating point).
pub(crate) fn predictor<T: TiffSample>(compression: TiffCompression) -> u16 {
    match (compression, T::FLOAT) {
        (TiffCompression::None, _) => 1,
        (_, false) => 2,
        (_, true) => 3,
    }
}

/// The compression tag value.
pub(crate) fn compression_tag(compression: TiffCompression) -> u16 {
    match compression {
        TiffCompression::None => 1,
        TiffCompression::Lzw => 5,
        TiffCompression::Deflate => 8,
    }
}

/// Rows per strip for rows of `row_bytes` bytes, at least one.
pub(crate) fn rows_per_strip(row_bytes: usize, height: usize) -> usize {
    (STRIP_BYTES / row_bytes.max(1)).clamp(1, height.max(1))
}

/// One row's samples as stored, through the predictor, appended to `out`: little-endian for predictors 1 and 2, the
/// byte planes of the big-endian samples for predictor 3.
fn row_bytes<T: TiffSample>(row: &[T], channels: usize, predictor: u16, out: &mut Vec<u8>) {
    match predictor {
        2 => {
            for (i, &v) in row.iter().enumerate() {
                let prev = i.checked_sub(channels).and_then(|j| row.get(j)).copied();
                prev.map_or(v, |p| v.minus(p)).put_le(out);
            }
        }
        3 => {
            let mut be = Vec::with_capacity(std::mem::size_of_val(row));
            for &v in row {
                v.put_be(&mut be);
            }
            let size = std::mem::size_of::<T>();
            let n = row.len();
            let start = out.len();
            for plane in 0..size {
                out.extend(be.iter().skip(plane).step_by(size).take(n));
            }
            for i in (start + channels..out.len()).rev() {
                let prev = out.get(i - channels).copied().unwrap_or(0);
                if let Some(b) = out.get_mut(i) {
                    *b = b.wrapping_sub(prev);
                }
            }
        }
        _ => row.iter().for_each(|&v| v.put_le(out)),
    }
}

/// The image's rows as compressed strips of `rows` rows each.
pub(crate) fn strips<T: TiffSample>(image: &Image<T>, rows: usize, compression: TiffCompression, format: Format) -> Result<Vec<Vec<u8>>, CodecError> {
    let channels = image.channels();
    let predictor = predictor::<T>(compression);
    let height = image.height();
    let rows = rows.max(1);
    (0..height.div_ceil(rows))
        .into_par_iter()
        .map(|s| {
            let first = s * rows;
            let mut raw = Vec::with_capacity(rows * image.width() * channels * std::mem::size_of::<T>());
            for y in first..(first + rows).min(height) {
                if let Some(row) = image.row(y) {
                    row_bytes(row, channels, predictor, &mut raw);
                }
            }
            compress(&raw, compression).ok_or_else(|| CodecError::encode(format, BackendError::message("the LZW encoder failed")))
        })
        .collect()
}

fn compress(raw: &[u8], compression: TiffCompression) -> Option<Vec<u8>> {
    match compression {
        TiffCompression::None => Some(raw.to_vec()),
        TiffCompression::Deflate => Some(miniz_oxide::deflate::compress_to_vec_zlib(raw, DEFLATE_LEVEL)),
        TiffCompression::Lzw => weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8).encode(raw).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use florastack_core::ColorSpace;

    #[test]
    fn horizontal_differences_wrap_per_channel() {
        let mut out = Vec::new();
        row_bytes(&[10u8, 200, 5, 100], 2, 2, &mut out);
        assert_eq!(out, [10, 200, 251, 156]);
        let mut out = Vec::new();
        row_bytes(&[1000u16, 3], 1, 2, &mut out);
        assert_eq!(out, [0xe8, 0x03, 0x1b, 0xfc], "3 - 1000 wraps to 0xfc1b");
    }

    #[test]
    fn the_float_predictor_splits_bytes_into_planes() {
        let mut out = Vec::new();
        // 1.0 = 0x3f800000, 2.0 = 0x40000000: planes [3f 40][80 00][00 00][00 00], then byte differences.
        row_bytes(&[1.0f32, 2.0], 1, 3, &mut out);
        assert_eq!(out, [0x3f, 0x01, 0x40, 0x80, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn strips_cover_every_row_and_decompress() {
        let image = Image::from_fn(300, 1000, 3, ColorSpace::LINEAR, |x, y, c| u16::try_from((x * 7 + y * 3 + c) % 65_536).unwrap()).unwrap();
        let rows = rows_per_strip(300 * 3 * 2, 1000);
        assert_eq!(rows, 145);
        for compression in [TiffCompression::None, TiffCompression::Deflate, TiffCompression::Lzw] {
            let s = strips(&image, rows, compression, Format::Tiff).unwrap();
            assert_eq!(s.len(), 7, "{compression:?}");
            if compression == TiffCompression::Deflate {
                let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&s[6]).unwrap();
                assert_eq!(raw.len(), 130 * 300 * 3 * 2, "the last strip holds the remaining rows");
            }
        }
        assert_eq!(compression_tag(TiffCompression::Lzw), 5);
        assert_eq!(
            (predictor::<u8>(TiffCompression::None), predictor::<u16>(TiffCompression::Lzw), predictor::<f32>(TiffCompression::Deflate)),
            (1, 2, 3)
        );
    }
}
