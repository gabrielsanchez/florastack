//! TIFF images with 9 to 15 bits per sample: 12-bit scans and microscope cameras, and the 10 and 14-bit files of
//! scientific software. The tiff crate refuses them, so they are read here: the layout from lightcraft-tiff's IFDs,
//! the strips or tiles decompressed (uncompressed, LZW, Deflate or PackBits) and their samples unpacked.
//!
//! The samples are packed most significant bit first, each row starting on a byte (TIFF 6.0, section 2: rows begin
//! on byte boundaries), and come out scaled to 16 bits as fractions of their own maximum. Gray
//! (`BlackIsZero`, or `WhiteIsZero` inverted) and RGB, with extra samples, chunky. What isn't read is said to be
//! unsupported: planar files, predictors (libtiff has none for these depths either), signed and float samples and
//! bit-reversed bytes (`FillOrder` 2).
//!
//! Everything is checked as hostile input: sizes before allocating, every chunk against the file's length, and what
//! a chunk decompresses to against what its rows hold.

use std::io::Write;

use lightcraft_tiff::image::chunk_bytes;
use lightcraft_tiff::{Tiff, tags as t};

use super::check_size;
use crate::error::{BackendError, CodecError};
use crate::format::Format;

const F: Format = Format::Tiff;

/// `FillOrder`, which lightcraft-tiff's tags don't name.
const FILL_ORDER: u16 = 266;

/// More samples per pixel than any image FloraStack keeps: a bound for what a header can make this allocate.
const MAX_SAMPLES: usize = 16;

/// What a file of 9 to 15-bit samples holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Info {
    pub width: u32,
    pub height: u32,
    /// Color samples: 1 or 3.
    pub color: usize,
    /// Samples per pixel, extra ones included.
    pub per_pixel: usize,
    pub bits: u8,
    /// `ExtraSamples`: 1 for associated alpha.
    pub extra: Vec<u16>,
}

fn malformed(text: &'static str) -> CodecError {
    CodecError::malformed(F, BackendError::message(text))
}

/// The layout of the first image, if its samples have 9 to 15 bits: `None` for every other file, which the tiff
/// crate reads, and for one whose IFDs can't be parsed, which it reports.
fn layout(bytes: &[u8]) -> Option<Result<(Info, lightcraft_tiff::image::ImageInfo, bool), CodecError>> {
    let tiff = Tiff::parse(bytes).ok()?;
    let ifd = tiff.ifds.first()?;
    let image = ifd.image().ok()?;
    let bits = *image.bits_per_sample.first()?;
    if !(9..16).contains(&bits) || image.bits_per_sample.iter().any(|&b| b != bits) {
        return None;
    }
    let unsupported = |what| Some(Err(CodecError::Unsupported { format: F, what }));
    let per_pixel = usize::from(image.samples_per_pixel);
    let (color, invert) = match image.photometric {
        0 => (1, true),
        1 => (1, false),
        2 => (3, false),
        _ => return unsupported("palette, CMYK or other color samples of 9 to 15 bits"),
    };
    if per_pixel < color || per_pixel > MAX_SAMPLES {
        return Some(Err(malformed("the samples per pixel don't fit the color type")));
    }
    if image.sample_format != 1 {
        return unsupported("signed or float samples of 9 to 15 bits");
    }
    if image.planar != 1 && per_pixel > 1 {
        return unsupported("planar samples of 9 to 15 bits");
    }
    if image.predictor != 1 {
        return unsupported("a predictor on samples of 9 to 15 bits");
    }
    if ifd.u16(FILL_ORDER).unwrap_or(1) != 1 {
        return unsupported("bit-reversed samples of 9 to 15 bits");
    }
    if !matches!(image.compression, 1 | 5 | 8 | 32_946 | 32_773) {
        return unsupported("samples of 9 to 15 bits with this compression");
    }
    let extra = ifd.u64s(t::EXTRA_SAMPLES).unwrap_or_default().into_iter().map(|v| u16::try_from(v).unwrap_or(0)).collect();
    // 9 to 15, checked above.
    let info = Info { width: image.width, height: image.height, color, per_pixel, bits: u8::try_from(bits).unwrap_or(15), extra };
    Some(Ok((info, image, invert)))
}

/// What the file holds, from its headers, if its samples have 9 to 15 bits.
pub(crate) fn probe(bytes: &[u8]) -> Option<Result<Info, CodecError>> {
    layout(bytes).map(|r| r.map(|(info, _, _)| info))
}

/// The samples of a file of 9 to 15 bits per sample, chunky and scaled to 16 bits; `None` for every other file.
pub(crate) fn decode(bytes: &[u8], max_pixels: u64) -> Option<Result<(Info, Vec<u16>), CodecError>> {
    let (info, image, invert) = match layout(bytes)? {
        Ok(found) => found,
        Err(e) => return Some(Err(e)),
    };
    Some(samples(bytes, &info, &image, invert, max_pixels).map(|samples| (info, samples)))
}

fn samples(bytes: &[u8], info: &Info, image: &lightcraft_tiff::image::ImageInfo, invert: bool, max_pixels: u64) -> Result<Vec<u16>, CodecError> {
    check_size(u64::from(info.width), u64::from(info.height), max_pixels)?;
    let too_large = || malformed("the image's size overflows");
    let (width, height) = (usize::try_from(info.width).map_err(|_| too_large())?, usize::try_from(info.height).map_err(|_| too_large())?);
    let per_row = width.checked_mul(info.per_pixel).ok_or_else(too_large)?;
    let total = per_row.checked_mul(height).ok_or_else(too_large)?;
    let bits = usize::from(info.bits);
    // A tile may reach past the image, but not hold more than a few images' worth: a header can't ask for more.
    let chunk_budget = total.checked_mul(2).and_then(|n| n.checked_mul(4)).ok_or_else(too_large)?.max(1 << 16);
    let (chunk_width, _) = image.chunk_size();
    let chunk_width = usize::try_from(chunk_width).map_err(|_| too_large())?;
    let row_bytes = chunk_width.checked_mul(info.per_pixel).and_then(|n| n.checked_mul(bits)).ok_or_else(too_large)?.div_ceil(8);
    let max = (1u32 << bits) - 1;
    let mut out = vec![0u16; total];
    // Every chunk has a place of its own, so the samples placed count what is covered.
    let mut covered = 0usize;
    for chunk in image.chunks(bytes.len() as u64) {
        let (x0, y0) = (usize::try_from(chunk.x).map_err(|_| too_large())?, usize::try_from(chunk.y).map_err(|_| too_large())?);
        if chunk.plane != 0 || x0 >= width || y0 >= height {
            continue;
        }
        let rows = usize::try_from(chunk.height).map_err(|_| too_large())?;
        let expected =
            row_bytes.checked_mul(rows).filter(|&n| n <= chunk_budget).ok_or_else(|| malformed("a strip or tile is larger than the image"))?;
        let stored = chunk_bytes(bytes, &chunk).ok_or_else(|| malformed("a strip or tile lies outside the file"))?;
        let raw = decompress(stored, image.compression, expected)?;
        if raw.len() < expected {
            return Err(malformed("the image data is cut short"));
        }
        // The samples of this chunk that fall inside the image.
        let visible = (chunk_width.min(width - x0)).saturating_mul(info.per_pixel);
        for (dy, row) in raw.chunks_exact(row_bytes.max(1)).take(rows.min(height - y0)).enumerate() {
            let start = (y0 + dy).checked_mul(per_row).and_then(|n| n.checked_add(x0.checked_mul(info.per_pixel)?)).ok_or_else(too_large)?;
            let Some(target) = start.checked_add(visible).and_then(|end| out.get_mut(start..end)) else {
                return Err(malformed("a strip or tile lies outside the image"));
            };
            unpack_row(row, bits, target);
            covered = covered.saturating_add(visible);
        }
    }
    if covered < total {
        return Err(malformed("the image data is cut short"));
    }
    // Fractions of the samples' own maximum, as every sample is: scaled to 16 bits, rounding to nearest.
    #[allow(clippy::cast_possible_truncation)] // At most 65535 by construction.
    for v in &mut out {
        let x = u32::from(*v).min(max);
        let x = if invert { max - x } else { x };
        *v = ((x * 65_535 + max / 2) / max) as u16;
    }
    Ok(out)
}

/// One row's samples of `bits` bits each, most significant bit first, into `out`.
fn unpack_row(row: &[u8], bits: usize, out: &mut [u16]) {
    let (mut buffer, mut held) = (0u32, 0usize);
    let mut bytes = row.iter();
    let mask = (1u32 << bits) - 1;
    for v in out {
        while held < bits {
            buffer = (buffer << 8) | u32::from(bytes.next().copied().unwrap_or(0));
            held += 8;
        }
        held -= bits;
        // At most 15 bits, by the mask.
        *v = u16::try_from((buffer >> held) & mask).unwrap_or(u16::MAX);
    }
}

/// A sink that takes `limit` bytes and no more: what a decompressor writes to, so that a strip can't inflate past what
/// its rows hold.
struct Capped {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for Capped {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let room = self.limit.saturating_sub(self.bytes.len());
        let take = room.min(data.len());
        self.bytes.extend_from_slice(data.get(..take).unwrap_or(data));
        // Whatever doesn't fit is dropped: the rows are all there, and trailing bytes mean nothing.
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A chunk's bytes, decompressed, `expected` of them at most.
fn decompress(stored: &[u8], compression: u16, expected: usize) -> Result<std::borrow::Cow<'_, [u8]>, CodecError> {
    use std::borrow::Cow;
    Ok(match compression {
        1 => Cow::Borrowed(stored),
        5 => {
            let mut sink = Capped { bytes: Vec::new(), limit: expected };
            let mut decoder = weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8);
            // A damaged tail is an error only if rows are missing, which the caller checks.
            let _ = decoder.into_stream(&mut sink).decode_all(stored).status;
            Cow::Owned(sink.bytes)
        }
        8 | 32_946 => match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(stored, expected) {
            Ok(bytes) => Cow::Owned(bytes),
            // More than the rows hold: the first `expected` bytes are the rows.
            Err(e) if matches!(e.status, miniz_oxide::inflate::TINFLStatus::HasMoreOutput) => Cow::Owned(e.output),
            Err(_) => return Err(malformed("a Deflate strip or tile is damaged")),
        },
        _ => Cow::Owned(unpack_bits(stored, expected)),
    })
}

/// PackBits (TIFF 6.0, section 9): a count byte `n`, then `n + 1` literal bytes (0 to 127) or one byte repeated
/// `1 - n` times (-127 to -1); -128 does nothing.
fn unpack_bits(stored: &[u8], expected: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(expected.min(stored.len().saturating_mul(128)));
    let mut bytes = stored.iter().copied();
    while out.len() < expected {
        let Some(n) = bytes.next() else { break };
        if n < 128 {
            out.extend(bytes.by_ref().take(usize::from(n) + 1));
        } else if n > 128 {
            let Some(value) = bytes.next() else { break };
            out.extend(std::iter::repeat_n(value, 257 - usize::from(n)));
        }
    }
    out.truncate(expected);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_unpack_most_significant_bit_first() {
        // 12 bits: 0xABC, 0x123, 0xFFF packed as AB C1 23 FF F0.
        let mut out = [0u16; 3];
        unpack_row(&[0xAB, 0xC1, 0x23, 0xFF, 0xF0], 12, &mut out);
        assert_eq!(out, [0xABC, 0x123, 0xFFF]);
        // 10 bits: 1023, 0, 513 = 1111111111 0000000000 1000000001.
        let mut out = [0u16; 3];
        unpack_row(&[0xFF, 0xC0, 0x08, 0x04], 10, &mut out);
        assert_eq!(out, [1023, 0, 513]);
        // A row cut short reads zeros, never past its end.
        let mut out = [7u16; 2];
        unpack_row(&[0xFF], 12, &mut out);
        assert_eq!(out, [0xFF0, 0]);
    }

    #[test]
    fn packbits_runs_and_literals() {
        assert_eq!(unpack_bits(&[2, 1, 2, 3, 254, 9, 128, 0, 5], 16), [1, 2, 3, 9, 9, 9, 5]);
        assert_eq!(unpack_bits(&[129, 7], 4), [7; 4], "a run longer than the rows hold is cut");
        assert_eq!(unpack_bits(&[5, 1], 8), [1], "literals cut short");
    }

    #[test]
    fn decompression_stops_at_what_the_rows_hold() {
        let raw = vec![3u8; 10_000];
        let lzw = weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8).encode(&raw).unwrap();
        assert_eq!(decompress(&lzw, 5, 100).unwrap().len(), 100);
        assert_eq!(decompress(&lzw, 5, 20_000).unwrap().len(), 10_000);
        let zlib = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        assert_eq!(decompress(&zlib, 8, 100).unwrap().len(), 100);
        assert_eq!(decompress(&zlib, 32_946, 20_000).unwrap().len(), 10_000);
        assert!(decompress(&[1, 2, 3], 8, 100).is_err());
    }
}
