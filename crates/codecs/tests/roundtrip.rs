//! Every format at every bit depth it stores, written and read back: lossless formats exactly, JPEG within its
//! quality, and files from other encoders (the tiff crate's, image-webp's, image's) read as they were written.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::Cursor;

use common::{pattern_f32, pattern_u8, pattern_u16, picture_u8, spec};
use florastack_codecs::{
    BitDepth, ChromaSubsampling, CodecError, DngCompression, DngLook, ExrCompression, Format, OutputFormat, OutputMetadata, OutputProfile,
    OutputSpec, ReadOptions, RowSource, TiffCompression, Warning, read, write, write_to,
};
use florastack_core::{AnyImage, ColorSpace, Image};
use lightcraft_tiff::tags as t;

fn round_trip(image: &AnyImage, spec: OutputSpec) -> florastack_codecs::Decoded {
    let encoded = write(image, &spec, &OutputMetadata::default()).unwrap();
    read(&encoded.bytes, &ReadOptions::default()).unwrap()
}

#[test]
fn png_keeps_every_sample_at_8_and_16_bits() {
    for channels in 1..=4 {
        for (image, depth, bits) in
            [(AnyImage::U8(pattern_u8(37, 23, channels)), BitDepth::U8, 8), (AnyImage::U16(pattern_u16(37, 23, channels)), BitDepth::U16, 16)]
        {
            let back = round_trip(&image, spec(OutputFormat::Png, depth));
            assert_eq!(back.image, image, "{channels} channels, {bits} bits");
            assert_eq!((back.info.format, back.info.bits_per_sample, back.info.channels), (Format::Png, bits, channels));
            assert!(!back.linearized && back.profile.is_none());
        }
    }
}

#[test]
fn tiff_keeps_every_sample_at_every_depth_and_compression() {
    for compression in [TiffCompression::None, TiffCompression::Lzw, TiffCompression::Deflate] {
        for channels in 1..=4 {
            let images = [
                (AnyImage::U8(pattern_u8(37, 23, channels)), BitDepth::U8),
                (AnyImage::U16(pattern_u16(37, 23, channels)), BitDepth::U16),
                (AnyImage::F32(pattern_f32(37, 23, channels)), BitDepth::F32),
            ];
            for (image, depth) in images {
                let encoded = write(&image, &spec(OutputFormat::Tiff { compression }, depth), &OutputMetadata::default()).unwrap();
                let back = read(&encoded.bytes, &ReadOptions::default()).unwrap();
                assert_eq!(back.image, image, "{compression:?}, {channels} channels, {depth}");
                assert_eq!(back.info.float, depth == BitDepth::F32);
                // The tiff crate reads the file too, on its own.
                let mut d = tiff::decoder::Decoder::new(Cursor::new(&encoded.bytes)).unwrap();
                assert_eq!(d.dimensions().unwrap(), (37, 23));
                d.read_image().unwrap();
            }
        }
    }
}

#[test]
fn float_files_keep_extended_range_and_special_values_bit_for_bit() {
    let special = [0.0f32, -0.0, -3.5, 1.5e6, f32::MIN_POSITIVE, f32::INFINITY, f32::NEG_INFINITY, f32::NAN, 1.0, 0.5, -1e-30, 7.25];
    let image = AnyImage::F32(Image::from_vec(4, 1, 3, ColorSpace::LINEAR, special.to_vec()).unwrap());
    let bits = |img: &AnyImage| img.as_image::<f32>().unwrap().as_raw().iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    for format in
        [OutputFormat::Tiff { compression: TiffCompression::Deflate }, OutputFormat::OpenExr { compression: ExrCompression::Zip, half: false }]
    {
        let back = round_trip(&image, spec(format, BitDepth::F32));
        assert_eq!(bits(&back.image), bits(&image), "{format:?}");
    }
    // Without "retain extended range", float output is clipped to [0, 1].
    let clipped =
        round_trip(&image, OutputSpec { extended_range: false, ..spec(OutputFormat::Tiff { compression: TiffCompression::None }, BitDepth::F32) });
    assert!(clipped.image.as_image::<f32>().unwrap().as_raw().iter().all(|v| (0.0..=1.0).contains(v)));
}

#[test]
fn tiffs_from_the_tiff_crates_own_encoder_read_exactly() {
    use tiff::encoder::{Compression, DeflateLevel, TiffEncoder, colortype};
    let image = pattern_u16(37, 23, 3);
    let gray = pattern_u8(37, 23, 1);
    for (label, compression) in [
        ("none", Compression::Uncompressed),
        ("LZW", Compression::Lzw),
        ("Deflate", Compression::Deflate(DeflateLevel::Balanced)),
        ("PackBits", Compression::Packbits),
    ] {
        let mut out = Cursor::new(Vec::new());
        let mut encoder = TiffEncoder::new(&mut out).unwrap().with_compression(compression).with_predictor(tiff::tags::Predictor::Horizontal);
        encoder.write_image::<colortype::RGB16>(37, 23, image.as_raw()).unwrap();
        let back = read(out.get_ref(), &ReadOptions::default()).unwrap();
        assert_eq!(back.image.as_image::<u16>().unwrap().as_raw(), image.as_raw(), "16-bit {label}");
        let mut out = Cursor::new(Vec::new());
        TiffEncoder::new(&mut out).unwrap().with_compression(compression).write_image::<colortype::Gray8>(37, 23, gray.as_raw()).unwrap();
        let back = read(out.get_ref(), &ReadOptions::default()).unwrap();
        assert_eq!(back.image.as_image::<u8>().unwrap().as_raw(), gray.as_raw(), "8-bit gray {label}");
    }
}

#[test]
fn jpeg_quality_trades_size_for_fidelity() {
    let picture = AnyImage::U8(picture_u8(160, 120));
    let jpeg =
        |quality, subsampling| write(&picture, &spec(OutputFormat::Jpeg { quality, subsampling }, BitDepth::U8), &OutputMetadata::default()).unwrap();
    let (best, small) = (jpeg(98, ChromaSubsampling::S444), jpeg(40, ChromaSubsampling::S420));
    assert!(small.bytes.len() < best.bytes.len());
    let back = read(&best.bytes, &ReadOptions::default()).unwrap();
    assert_eq!((back.info.format, back.info.channels, back.info.bits_per_sample), (Format::Jpeg, 3, 8));
    let as_f32 = |img: &AnyImage| img.as_image::<u8>().unwrap().convert::<f32>().unwrap();
    let psnr = florastack_metrics::psnr(&as_f32(&back.image), &as_f32(&picture)).unwrap();
    // Quality 98 without chroma subsampling: well above the 40 dB at which differences stop being visible.
    assert!(psnr > 40.0, "{psnr} dB");
    // Gray stays gray; alpha has no place in a JPEG file.
    let gray = AnyImage::U8(pattern_u8(16, 16, 1));
    assert_eq!(read(&jpeg_of(&gray).bytes, &ReadOptions::default()).unwrap().info.channels, 1);
    let rgba = AnyImage::U8(pattern_u8(16, 16, 4));
    let encoded = jpeg_of(&rgba);
    assert_eq!(encoded.warnings, [Warning::AlphaDropped(Format::Jpeg)]);
    assert_eq!(read(&encoded.bytes, &ReadOptions::default()).unwrap().info.channels, 3);
}

fn jpeg_of(image: &AnyImage) -> florastack_codecs::Encoded {
    write(image, &spec(OutputFormat::Jpeg { quality: 90, subsampling: ChromaSubsampling::S444 }, BitDepth::U8), &OutputMetadata::default()).unwrap()
}

#[test]
fn openexr_keeps_floats_exactly_and_halves_to_their_precision() {
    for channels in 1..=4 {
        let image = AnyImage::F32(pattern_f32(37, 23, channels));
        for compression in [ExrCompression::None, ExrCompression::Rle, ExrCompression::Zips, ExrCompression::Zip, ExrCompression::Piz] {
            let back = round_trip(&image, spec(OutputFormat::OpenExr { compression, half: false }, BitDepth::F32));
            assert_eq!(back.image, image, "{channels} channels, {compression:?}");
            assert_eq!(back.info.bits_per_sample, 32);
        }
        let half = round_trip(&image, spec(OutputFormat::OpenExr { compression: ExrCompression::Zip, half: true }, BitDepth::F32));
        assert_eq!(half.info.bits_per_sample, 16);
        let (a, b) = (half.image.as_image::<f32>().unwrap(), image.as_image::<f32>().unwrap());
        // Half floats keep 11 significant bits: a relative error below 2^-11, with a floor for values near zero.
        assert!(a.as_raw().iter().zip(b.as_raw()).all(|(x, y)| (x - y).abs() <= y.abs() / 2048.0 + 1e-4), "{channels} channels");
    }
}

#[test]
fn dng_holds_linear_16_bit_samples_that_lightcraft_raw_reads_back() {
    let image = Image::from_vec(37, 23, 3, ColorSpace::LINEAR, pattern_u16(37, 23, 3).into_raw()).unwrap();
    for (compression, preview) in [DngCompression::Uncompressed, DngCompression::LosslessJpeg].into_iter().flat_map(|c| [(c, false), (c, true)]) {
        let spec = OutputSpec {
            format: OutputFormat::Dng { compression, look: DngLook::Rendered, preview },
            depth: BitDepth::U16,
            profile: OutputProfile::Input,
            extended_range: false,
        };
        let encoded = write(&AnyImage::U16(image.clone()), &spec, &OutputMetadata::default()).unwrap();
        assert_eq!(encoded.warnings, []);
        let raw = lightcraft_raw::decode(&encoded.bytes).unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp), (37, 23, 3), "{compression:?}");
        let lightcraft_raw::RawData::U16(samples) = &raw.data else { panic!("16-bit samples") };
        assert_eq!(samples, image.as_raw(), "{compression:?}, preview {preview}");
        // With a preview, IFD0 is a picture any TIFF reader shows and the raw image is its sub-IFD.
        let tiff = lightcraft_tiff::Tiff::parse(&encoded.bytes).unwrap();
        let ifd0 = &tiff.ifds[0];
        assert_eq!(ifd0.u32(t::NEW_SUBFILE_TYPE), Some(u32::from(preview)));
        assert_eq!(ifd0.sub_ifds.len(), usize::from(preview));
        assert_eq!(ifd0.bytes(t::DNG_VERSION), Some(&[1u8, 4, 0, 0][..]));
        assert_eq!(ifd0.bytes(t::DNG_BACKWARD_VERSION), Some(&[1u8, 3, 0, 0][..]));
        if preview {
            assert_eq!((ifd0.u32(t::IMAGE_WIDTH), ifd0.u32(t::IMAGE_LENGTH), ifd0.u16(t::PHOTOMETRIC)), (Some(37), Some(23), Some(2)));
            assert_eq!(ifd0.u32(t::STRIP_BYTE_COUNTS), Some(37 * 23 * 3));
            let raw_ifd = &ifd0.sub_ifds[0];
            assert_eq!((raw_ifd.u32(t::NEW_SUBFILE_TYPE), raw_ifd.u16(t::PHOTOMETRIC)), (Some(0), Some(t::photometric::LINEAR_RAW)));
        }
        assert!(raw.color.color_matrix[0].is_some() && raw.color.forward_matrix[0].is_some());
        assert_eq!(raw.metadata.software.as_deref(), Some(format!("FloraStack {}", env!("CARGO_PKG_VERSION")).as_str()));
        // A DNG is a RAW file: the RAW importer reads it, not this crate.
        assert!(matches!(read(&encoded.bytes, &ReadOptions::default()), Err(CodecError::RawFile(_))));
    }
    // Gray is written as RGB, without alpha.
    let gray = AnyImage::U16(Image::from_vec(5, 4, 2, ColorSpace::LINEAR, vec![1000; 40]).unwrap());
    let spec = OutputSpec::new(Format::Dng).unwrap();
    let encoded = write(&gray, &spec, &OutputMetadata::default()).unwrap();
    assert_eq!(encoded.warnings, [Warning::AlphaDropped(Format::Dng), Warning::GrayExpanded(Format::Dng)]);
    assert_eq!(lightcraft_raw::decode(&encoded.bytes).unwrap().cpp, 3);
}

#[test]
fn webp_and_bmp_files_from_other_encoders_read_exactly() {
    for channels in [3u8, 4] {
        let image = pattern_u8(37, 23, usize::from(channels));
        let webp = lightcraft_codecs::encode_webp_lossless(
            &lightcraft_codecs::EncodeImage::new(37, 23, channels, lightcraft_codecs::Samples::U8(image.as_raw())),
            &lightcraft_codecs::EncodeMeta::default(),
        )
        .unwrap();
        let back = read(&webp, &ReadOptions::default()).unwrap();
        assert_eq!(back.image, AnyImage::U8(image.clone()), "WebP, {channels} channels");
        assert_eq!(back.info.format, Format::WebP);
        let mut bmp = Vec::new();
        let color = if channels == 3 { image::ExtendedColorType::Rgb8 } else { image::ExtendedColorType::Rgba8 };
        image::codecs::bmp::BmpEncoder::new(&mut bmp).encode(image.as_raw(), 37, 23, color).unwrap();
        let back = read(&bmp, &ReadOptions::default()).unwrap();
        assert_eq!(back.image, AnyImage::U8(image), "BMP, {channels} channels");
        assert_eq!(back.info.format, Format::Bmp);
    }
}

#[test]
fn a_chain_of_formats_keeps_16_bit_samples() {
    let original = AnyImage::U16(pattern_u16(37, 23, 3));
    let png = round_trip(&original, spec(OutputFormat::Png, BitDepth::U16));
    let tiff = round_trip(&png.image, spec(OutputFormat::Tiff { compression: TiffCompression::Deflate }, BitDepth::U16));
    // Linear light from here on: every 16-bit sRGB value survives linear f32 and back (florastack_core::convert).
    let exr = round_trip(&tiff.image, spec(OutputFormat::OpenExr { compression: ExrCompression::Piz, half: false }, BitDepth::F32));
    let float_tiff = round_trip(&exr.image, spec(OutputFormat::Tiff { compression: TiffCompression::Lzw }, BitDepth::F32));
    let back = round_trip(&float_tiff.image, spec(OutputFormat::Png, BitDepth::U16));
    assert_eq!(back.image, original);
}

#[test]
fn float_dng_keeps_32_bit_samples_and_values_above_white() {
    // Values from -0.5 to 2.6: negative ones and those above white survive with extended range.
    let image = Image::from_vec(41, 29, 3, ColorSpace::LINEAR, pattern_f32(41, 29, 3).into_raw()).unwrap();
    for (compression, extended_range) in [DngCompression::Uncompressed, DngCompression::Deflate].into_iter().flat_map(|c| [(c, false), (c, true)]) {
        let spec = OutputSpec {
            format: OutputFormat::Dng { compression, look: DngLook::Raw, preview: true },
            depth: BitDepth::F32,
            profile: OutputProfile::Input,
            extended_range,
        };
        let encoded = write(&AnyImage::F32(image.clone()), &spec, &OutputMetadata::default()).unwrap();
        assert_eq!(encoded.warnings, []);
        let tiff = lightcraft_tiff::Tiff::parse(&encoded.bytes).unwrap();
        let raw_ifd = &tiff.ifds[0].sub_ifds[0];
        assert_eq!((raw_ifd.u16(t::BITS_PER_SAMPLE), raw_ifd.u16(t::SAMPLE_FORMAT), raw_ifd.u32(t::WHITE_LEVEL)), (Some(32), Some(3), Some(1)));
        assert_eq!(raw_ifd.u16(t::PREDICTOR), (compression == DngCompression::Deflate).then_some(3));
        assert_eq!(tiff.ifds[0].bytes(t::DNG_BACKWARD_VERSION), Some(&[1u8, 4, 0, 0][..]), "float DNG needs a 1.4 reader");
        let raw = lightcraft_raw::decode(&encoded.bytes).unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp), (41, 29, 3), "{compression:?}");
        let lightcraft_raw::RawData::F32(samples) = &raw.data else { panic!("float samples") };
        let expected: Vec<f32> = image.as_raw().iter().map(|&v| if extended_range { v } else { v.clamp(0.0, 1.0) }).collect();
        assert!(samples == &expected, "{compression:?}, extended range {extended_range}");
        assert_eq!(samples.iter().any(|&v| v > 1.0), extended_range);
    }
    // Each compression stores one kind of sample.
    for (compression, depth) in [(DngCompression::LosslessJpeg, BitDepth::F32), (DngCompression::Deflate, BitDepth::U16)] {
        let spec = spec(OutputFormat::Dng { compression, look: DngLook::Raw, preview: false }, depth);
        let error = write(&AnyImage::F32(image.clone()), &spec, &OutputMetadata::default()).unwrap_err();
        assert!(matches!(error, CodecError::Unsupported { format: Format::Dng, .. }), "{error:?}");
    }
}

/// A source that makes its rows as they are asked for, and remembers the most it was asked for at once.
struct Generated {
    width: usize,
    height: usize,
    channels: usize,
    most_rows: usize,
    calls: usize,
}

impl Generated {
    fn value(x: usize, y: usize, c: usize) -> u16 {
        u16::try_from((x * 2_654_435 + y * 40_503 + c * 21_845) % 65_536).unwrap()
    }

    fn whole(&self) -> AnyImage {
        AnyImage::U16(Image::from_fn(self.width, self.height, self.channels, ColorSpace::SRGB, Generated::value).unwrap())
    }
}

impl RowSource for Generated {
    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn rows(&mut self, y: usize, rows: usize) -> Result<AnyImage, CodecError> {
        self.most_rows = self.most_rows.max(rows);
        self.calls += 1;
        Ok(AnyImage::U16(Image::from_fn(self.width, rows, self.channels, ColorSpace::SRGB, |x, dy, c| Generated::value(x, y + dy, c)).unwrap()))
    }
}

#[test]
fn files_written_in_bands_are_the_files_written_whole() {
    // 40 MB of 16-bit samples: more than one band, whose last strips and tiles are partial.
    let mut source = Generated { width: 3001, height: 2203, channels: 3, most_rows: 0, calls: 0 };
    let whole = source.whole();
    for (format, depth, profile) in [
        (OutputFormat::Tiff { compression: TiffCompression::Deflate }, BitDepth::U16, OutputProfile::Input),
        (OutputFormat::Tiff { compression: TiffCompression::None }, BitDepth::U16, OutputProfile::Srgb),
        // A conversion of primaries and to linear float, band by band.
        (OutputFormat::Tiff { compression: TiffCompression::Lzw }, BitDepth::F32, OutputProfile::Wide(florastack_codecs::WideSpace::ProPhoto)),
        (
            OutputFormat::Dng { compression: DngCompression::LosslessJpeg, look: DngLook::Rendered, preview: true },
            BitDepth::U16,
            OutputProfile::Input,
        ),
        (OutputFormat::Dng { compression: DngCompression::Deflate, look: DngLook::Raw, preview: false }, BitDepth::F32, OutputProfile::Input),
    ] {
        let spec = OutputSpec { format, depth, profile, extended_range: false };
        let expected = write(&whole, &spec, &OutputMetadata::default()).unwrap();
        (source.most_rows, source.calls) = (0, 0);
        let mut sink = Cursor::new(Vec::new());
        let written = write_to(&mut sink, &mut source, &spec, &OutputMetadata::default()).unwrap();
        assert!(sink.get_ref() == &expected.bytes, "{format:?} {depth}");
        assert_eq!((written.bytes, &written.warnings), (expected.bytes.len() as u64, &expected.warnings));
        assert_eq!(sink.position(), written.bytes, "the sink is left at the file's end");
        assert!(
            // One call for the first row, then bands: never the whole image.
            source.calls >= 3 && source.most_rows < source.height,
            "{format:?}: {} rows at once in {} calls",
            source.most_rows,
            source.calls
        );
    }
    // A format that doesn't stream takes every row at once, and writes the same file.
    let mut small = Generated { width: 64, height: 48, channels: 3, most_rows: 0, calls: 0 };
    let spec = OutputSpec::new(Format::Png).unwrap();
    let mut sink = Cursor::new(Vec::new());
    write_to(&mut sink, &mut small, &spec, &OutputMetadata::default()).unwrap();
    assert_eq!(sink.into_inner(), write(&small.whole(), &spec, &OutputMetadata::default()).unwrap().bytes);
    assert_eq!((small.most_rows, small.calls), (48, 1));
}

#[test]
fn a_source_that_changes_shape_is_an_error_not_a_damaged_file() {
    struct Short(Generated);
    impl RowSource for Short {
        fn width(&self) -> usize {
            self.0.width
        }
        fn height(&self) -> usize {
            self.0.height
        }
        fn rows(&mut self, y: usize, rows: usize) -> Result<AnyImage, CodecError> {
            // One row short, from the second request on.
            self.0.rows(y, if y == 0 && rows == 1 { 1 } else { rows - 1 })
        }
    }
    let mut source = Short(Generated { width: 16, height: 9, channels: 3, most_rows: 0, calls: 0 });
    let error = write_to(&mut Cursor::new(Vec::new()), &mut source, &OutputSpec::new(Format::Tiff).unwrap(), &OutputMetadata::default()).unwrap_err();
    assert!(matches!(error, CodecError::Encode { format: Format::Tiff, .. }), "{error:?}");
}

#[test]
fn every_writable_format_rejects_depths_it_cannot_store() {
    let image = AnyImage::U8(pattern_u8(4, 4, 3));
    for (format, depth) in [
        (OutputFormat::Jpeg { quality: 90, subsampling: ChromaSubsampling::S444 }, BitDepth::U16),
        (OutputFormat::Png, BitDepth::F32),
        (OutputFormat::OpenExr { compression: ExrCompression::Zip, half: false }, BitDepth::U16),
        (OutputFormat::Dng { compression: DngCompression::Uncompressed, look: DngLook::Raw, preview: true }, BitDepth::U8),
    ] {
        assert!(matches!(write(&image, &spec(format, depth), &OutputMetadata::default()), Err(CodecError::Depth { .. })), "{format:?} {depth}");
    }
}
