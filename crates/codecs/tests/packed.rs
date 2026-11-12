//! TIFF files with 9 to 15 bits per sample, which the tiff crate refuses: 12-bit scans, and 10 and 14-bit scientific
//! files. No downloadable sample is pinned for them, so the files are made here from the specification (TIFF 6.0:
//! samples packed most significant bit first, rows starting on a byte) with lightcraft-tiff's writer.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Bit packing: the casts keep the low bits on purpose, and the pattern's coordinates are small.
#![allow(clippy::cast_possible_truncation, clippy::single_match_else)]

use florastack_codecs::{CodecError, Format, ReadOptions, probe, read};
use florastack_core::AnyImage;
use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value, tags as t};

/// The sample at (`x`, `y`, `c`), below `1 << bits`, with the maximum and zero among them.
fn value(x: usize, y: usize, c: usize, bits: u16) -> u16 {
    let max = (1u32 << bits) - 1;
    match (x, y, c) {
        (0, 0, _) => u16::try_from(max).unwrap(),
        (1, 0, _) => 0,
        _ => u16::try_from((x as u32 * 977 + y as u32 * 131 + c as u32 * 53) % (max + 1)).unwrap(),
    }
}

/// Rows `y0..y0 + rows` of columns `x0..x0 + width`, packed: columns past `image_width` are padding.
fn pack(x0: usize, y0: usize, width: usize, rows: usize, channels: usize, bits: u16, image_width: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for y in y0..y0 + rows {
        let (mut buffer, mut held) = (0u64, 0u32);
        for x in x0..x0 + width {
            for c in 0..channels {
                let v = if x < image_width { value(x, y, c, bits) } else { 0 };
                buffer = (buffer << bits) | u64::from(v);
                held += u32::from(bits);
                while held >= 8 {
                    held -= 8;
                    out.push((buffer >> held) as u8);
                }
            }
        }
        if held > 0 {
            out.push((buffer << (8 - held)) as u8);
        }
    }
    out
}

fn compress(raw: Vec<u8>, compression: u16) -> Vec<u8> {
    match compression {
        5 => weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8).encode(&raw).unwrap(),
        8 => miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6),
        // PackBits, as literal runs of up to 128 bytes.
        32_773 => raw.chunks(128).flat_map(|run| std::iter::once(u8::try_from(run.len() - 1).unwrap()).chain(run.iter().copied())).collect(),
        _ => raw,
    }
}

struct File {
    width: usize,
    height: usize,
    channels: usize,
    bits: u16,
    compression: u16,
    /// A tile's side, or strips of 5 rows.
    tile: Option<usize>,
    photometric: u16,
}

impl File {
    fn ifd(&self) -> IfdBuilder {
        let mut ifd = IfdBuilder::new();
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![u32::try_from(self.width).unwrap()]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![u32::try_from(self.height).unwrap()]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![self.bits; self.channels]));
        ifd.set(t::COMPRESSION, Value::Short(vec![self.compression]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![self.photometric]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![u16::try_from(self.channels).unwrap()]));
        ifd.set(t::PLANAR_CONFIGURATION, Value::Short(vec![1]));
        if self.channels == 2 || self.channels == 4 {
            ifd.set(t::EXTRA_SAMPLES, Value::Short(vec![2]));
        }
        let (w, h, c) = (self.width, self.height, self.channels);
        match self.tile {
            Some(side) => {
                let mut tiles = Vec::new();
                for ty in (0..h).step_by(side) {
                    for tx in (0..w).step_by(side) {
                        // Rows past the image repeat the pattern: their content means nothing.
                        tiles.push(compress(pack(tx, ty, side, side, c, self.bits, w), self.compression));
                    }
                }
                let side = u32::try_from(side).unwrap();
                ifd.set_image(ImageData::Tiles { tile_width: side, tile_height: side, tiles });
            }
            None => {
                let strips = (0..h).step_by(5).map(|y| compress(pack(0, y, w, 5.min(h - y), c, self.bits, w), self.compression)).collect();
                ifd.set_image(ImageData::Strips { rows_per_strip: 5, strips });
            }
        }
        ifd
    }

    fn bytes(&self) -> Vec<u8> {
        TiffWriter::new(ByteOrder::Little, false).write(&[self.ifd()]).unwrap()
    }

    /// What the reader must give: fractions of the samples' maximum, in 16 bits.
    fn expected(&self) -> Vec<u16> {
        let max = (1u32 << self.bits) - 1;
        let mut out = Vec::new();
        for y in 0..self.height {
            for x in 0..self.width {
                for c in 0..self.channels {
                    let v = u32::from(value(x, y, c, self.bits));
                    let v = if self.photometric == 0 { max - v } else { v };
                    out.push(u16::try_from((v * 65_535 + max / 2) / max).unwrap());
                }
            }
        }
        out
    }
}

#[test]
fn samples_of_9_to_15_bits_read_at_16_bits() {
    // Widths that leave padding bits at the end of a row, and heights that leave a short last strip and tile.
    for bits in [9, 10, 12, 14, 15] {
        for (channels, photometric) in [(1, 1), (2, 1), (3, 2), (4, 2)] {
            for (compression, tile) in [(1, None), (5, None), (8, None), (32_773, None), (1, Some(16)), (8, Some(16))] {
                let file = File { width: 37, height: 23, channels, bits, compression, tile, photometric };
                let bytes = file.bytes();
                let what = format!("{bits} bits, {channels} channels, compression {compression}, tile {tile:?}");
                let info = probe(&bytes).unwrap_or_else(|e| panic!("{what}: {e}"));
                assert_eq!((info.width, info.height, info.channels, u16::from(info.bits_per_sample)), (37, 23, channels, bits), "{what}");
                assert_eq!((info.float, info.alpha), (false, channels % 2 == 0), "{what}");
                let decoded = read(&bytes, &ReadOptions::default()).unwrap_or_else(|e| panic!("{what}: {e}"));
                let AnyImage::U16(image) = &decoded.image else { panic!("{what}: 16-bit samples") };
                assert_eq!((image.width(), image.height(), image.channels()), (37, 23, channels), "{what}");
                assert!(image.as_raw() == file.expected().as_slice(), "{what}");
                assert_eq!(image.pixel(0, 0).unwrap()[0], 65_535, "{what}: the samples' maximum is white");
                assert_eq!(image.pixel(1, 0).unwrap()[0], 0, "{what}");
                assert_eq!(u16::from(decoded.info.bits_per_sample), bits);
            }
        }
    }
}

#[test]
fn white_is_zero_is_inverted_and_metadata_comes_along() {
    let file = File { width: 9, height: 7, channels: 1, bits: 12, compression: 1, tile: None, photometric: 0 };
    let mut ifd = file.ifd();
    ifd.set(t::ORIENTATION, Value::Short(vec![6]));
    ifd.set(t::MAKE, Value::Ascii("Acme".to_owned()));
    let bytes = TiffWriter::new(ByteOrder::Big, false).write(&[ifd]).unwrap();
    let decoded = read(&bytes, &ReadOptions { orientation: florastack_codecs::OrientationPolicy::Ignore, ..ReadOptions::default() }).unwrap();
    let AnyImage::U16(image) = &decoded.image else { panic!("16-bit samples") };
    assert_eq!(image.as_raw(), file.expected().as_slice(), "the bytes' order doesn't change packed samples");
    assert_eq!(image.pixel(0, 0).unwrap(), [0], "the maximum is black");
    assert_eq!(decoded.info.orientation, florastack_core::Orientation::Rotate90);
    assert_eq!(decoded.metadata.summary().make.as_deref(), Some("Acme"));
    // Applied, the orientation turns the image.
    let upright = read(&bytes, &ReadOptions::default()).unwrap();
    assert_eq!((upright.image.width(), upright.image.height()), (7, 9));
}

#[test]
fn damaged_and_unsupported_12_bit_files_are_errors() {
    let file = File { width: 37, height: 23, channels: 3, bits: 12, compression: 8, tile: None, photometric: 2 };
    let good = file.bytes();
    // Cut short anywhere: an error, never a panic or a partial image.
    for len in [good.len() / 4, good.len() / 2, good.len() - 200] {
        if let Ok(decoded) = read(&good[..len], &ReadOptions::default()) {
            panic!("{len} of {} bytes read as {}x{}", good.len(), decoded.image.width(), decoded.image.height());
        }
    }
    // Too large for the limit: refused before anything is decompressed.
    let small = ReadOptions { max_pixels: 100, ..ReadOptions::default() };
    assert!(matches!(read(&good, &small), Err(CodecError::TooLarge { width: 37, height: 23, max: 100 })));
    // What the reader leaves out says so.
    let tagged = |tag: u16, value: Value| {
        let mut ifd = file.ifd();
        ifd.set(tag, value);
        TiffWriter::new(ByteOrder::Little, false).write(&[ifd]).unwrap()
    };
    for bytes in [
        tagged(t::PREDICTOR, Value::Short(vec![2])),
        tagged(t::PLANAR_CONFIGURATION, Value::Short(vec![2])),
        tagged(t::SAMPLE_FORMAT, Value::Short(vec![2; 3])),
        tagged(t::PHOTOMETRIC, Value::Short(vec![5])),
        tagged(266, Value::Short(vec![2])),
        tagged(t::COMPRESSION, Value::Short(vec![7])),
    ] {
        let error = read(&bytes, &ReadOptions::default()).unwrap_err();
        assert!(matches!(error, CodecError::Unsupported { format: Format::Tiff, .. }), "{error:?}");
        assert!(matches!(probe(&bytes), Err(CodecError::Unsupported { .. })));
    }
    // A strip that points outside the file, and a tile far larger than the image.
    let mut ifd = file.ifd();
    ifd.image = None;
    ifd.set(t::ROWS_PER_STRIP, Value::Long(vec![23]));
    ifd.set(t::STRIP_OFFSETS, Value::Long(vec![4_000_000]));
    ifd.set(t::STRIP_BYTE_COUNTS, Value::Long(vec![100]));
    let outside = TiffWriter::new(ByteOrder::Little, false).write(&[ifd]).unwrap();
    assert!(matches!(read(&outside, &ReadOptions::default()), Err(CodecError::Malformed { .. })));
    let mut ifd = file.ifd();
    ifd.image = None;
    ifd.set(t::TILE_WIDTH, Value::Long(vec![1 << 30]));
    ifd.set(t::TILE_LENGTH, Value::Long(vec![1 << 30]));
    ifd.set(t::TILE_OFFSETS, Value::Long(vec![8]));
    ifd.set(t::TILE_BYTE_COUNTS, Value::Long(vec![16]));
    let huge = TiffWriter::new(ByteOrder::Little, false).write(&[ifd]).unwrap();
    assert!(matches!(read(&huge, &ReadOptions::default()), Err(CodecError::Malformed { .. })));
}
