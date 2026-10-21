//! Hostile input: truncated and corrupted files, absurd sizes and random bytes must give errors, never panics or huge
//! allocations (AGENTS.md, Never crash).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{camera_jpeg, pattern_f32, pattern_u8, pattern_u16, spec};
use florastack_codecs::{
    BitDepth, CodecError, ExrCompression, OutputFormat, OutputMetadata, ReadOptions, TiffCompression, probe, read, read_metadata, write,
};
use florastack_core::AnyImage;
use proptest::prelude::*;

/// One small file of every readable format.
fn samples() -> Vec<(&'static str, Vec<u8>)> {
    let rgb = AnyImage::U8(pattern_u8(29, 17, 3));
    let none = OutputMetadata::default();
    let out = |image: &AnyImage, format, depth| write(image, &spec(format, depth), &none).unwrap().bytes;
    let webp = lightcraft_codecs::encode_webp_lossless(
        &lightcraft_codecs::EncodeImage::new(29, 17, 3, lightcraft_codecs::Samples::U8(pattern_u8(29, 17, 3).as_raw())),
        &lightcraft_codecs::EncodeMeta::default(),
    )
    .unwrap();
    let mut bmp = Vec::new();
    image::codecs::bmp::BmpEncoder::new(&mut bmp).encode(pattern_u8(29, 17, 3).as_raw(), 29, 17, image::ExtendedColorType::Rgb8).unwrap();
    vec![
        ("jpeg", camera_jpeg(&pattern_u8(29, 17, 3), 6)),
        ("png", out(&AnyImage::U16(pattern_u16(29, 17, 4)), OutputFormat::Png, BitDepth::U16)),
        ("tiff", out(&rgb, OutputFormat::Tiff { compression: TiffCompression::Lzw }, BitDepth::U8)),
        ("exr", out(&AnyImage::F32(pattern_f32(29, 17, 3)), OutputFormat::OpenExr { compression: ExrCompression::Piz, half: false }, BitDepth::F32)),
        ("webp", webp),
        ("bmp", bmp),
    ]
}

#[test]
fn truncated_files_never_panic() {
    for (name, bytes) in samples() {
        assert!(read(&bytes, &ReadOptions::default()).is_ok(), "{name} reads whole");
        let len = bytes.len();
        let mut cuts: Vec<usize> = (0..64.min(len)).collect();
        cuts.extend((1..20).map(|i| len * i / 20));
        cuts.push(len - 1);
        for cut in cuts {
            // An error, or for formats that tolerate a short tail (JPEG) a partial image: never a panic.
            let _ = read(&bytes[..cut], &ReadOptions::default());
            let _ = probe(&bytes[..cut]);
            let _ = read_metadata(&bytes[..cut]);
        }
    }
}

#[test]
fn corrupted_bytes_never_panic() {
    for (name, bytes) in samples() {
        // Flip one byte at a time in the first 512 bytes (headers and metadata), then every 97th byte after.
        let positions = (0..bytes.len().min(512)).chain((512..bytes.len()).step_by(97));
        for i in positions {
            let mut damaged = bytes.clone();
            damaged[i] ^= 0x5a;
            let _ = read(&damaged, &ReadOptions::default());
        }
        let _ = name;
    }
}

#[test]
fn images_beyond_the_pixel_limit_are_refused_before_decoding() {
    let small = ReadOptions { max_pixels: 100, ..ReadOptions::default() };
    for (name, bytes) in samples() {
        assert!(matches!(read(&bytes, &small), Err(CodecError::TooLarge { width: 29, height: 17, max: 100 })), "{name}");
    }
}

#[test]
fn absurd_declared_sizes_are_errors() {
    // A PNG header of 2^31 - 1 x 2^31 - 1 pixels, and nothing else of the image.
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\x7f\xff\xff\xff\x7f\xff\xff\xff\x08\x02\0\0\0".to_vec();
    let crc = crc32(&png[12..29]);
    png.extend_from_slice(&crc.to_be_bytes());
    assert!(read(&png, &ReadOptions::default()).is_err());
    // A TIFF whose IFD claims 4 billion columns.
    let mut ifd = lightcraft_tiff::IfdBuilder::new();
    ifd.set(lightcraft_tiff::tags::IMAGE_WIDTH, lightcraft_tiff::Value::Long(vec![u32::MAX]));
    ifd.set(lightcraft_tiff::tags::IMAGE_LENGTH, lightcraft_tiff::Value::Long(vec![u32::MAX]));
    ifd.set(lightcraft_tiff::tags::BITS_PER_SAMPLE, lightcraft_tiff::Value::Short(vec![16; 3]));
    ifd.set(lightcraft_tiff::tags::SAMPLES_PER_PIXEL, lightcraft_tiff::Value::Short(vec![3]));
    ifd.set(lightcraft_tiff::tags::PHOTOMETRIC, lightcraft_tiff::Value::Short(vec![2]));
    ifd.set_image(lightcraft_tiff::ImageData::Strips { rows_per_strip: 1, strips: vec![vec![0; 6]] });
    let tiff = lightcraft_tiff::TiffWriter::new(lightcraft_tiff::ByteOrder::Little, false).write(&[ifd]).unwrap();
    assert!(read(&tiff, &ReadOptions::default()).is_err());
}

/// CRC-32 as PNG chunks use it (ISO 3309), for the test's hand-made header.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn random_bytes_behind_every_magic_number_never_panic(tail in proptest::collection::vec(any::<u8>(), 0..2048), which in 0usize..6) {
        let magic: [&[u8]; 6] = [b"\xff\xd8\xff\xe0", b"\x89PNG\r\n\x1a\n", b"II*\0", b"\x76\x2f\x31\x01", b"RIFF\0\0\0\0WEBPVP8L", b"BM"];
        let mut bytes = magic[which].to_vec();
        bytes.extend_from_slice(&tail);
        let _ = read(&bytes, &ReadOptions::default());
        let _ = probe(&bytes);
        let _ = read_metadata(&bytes);
    }
}
