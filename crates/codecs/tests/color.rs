//! Color through a read and a write: sRGB assumed without a profile, profiles kept byte for byte, files with another
//! curve linearized and encoded back exactly, and conversion to and from the wide working spaces.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::Cursor;

use common::{pattern_f32, pattern_u16, picture_u8, spec};
use florastack_codecs::{
    BitDepth, CurveKind, ExrCompression, NamedSpace, OutputFormat, OutputMetadata, OutputProfile, OutputSpec, ReadOptions, TiffCompression,
    WideSpace, read, write,
};
use florastack_core::{AnyImage, ColorSpace, Encoding, Image};
use lightcraft_codecs::{EncodeImage, EncodeMeta, Samples};

/// A PNG of raw 16-bit RGB samples with `icc` embedded, written by LightCraft's encoder.
fn png_with_profile(samples: &[u16], w: u32, h: u32, icc: &[u8]) -> Vec<u8> {
    let meta = EncodeMeta { icc: Some(icc), ..EncodeMeta::default() };
    lightcraft_codecs::encode_png(&EncodeImage::new(w, h, 3, Samples::U16(samples)), &meta).unwrap()
}

/// The raw 16-bit samples and the embedded profile of a PNG, read by the png crate.
fn raw_png(bytes: &[u8]) -> (Vec<u16>, Option<Vec<u8>>) {
    let mut reader = png::Decoder::new(Cursor::new(bytes)).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    let samples = buf[..info.buffer_size()].as_chunks::<2>().0.iter().map(|b| u16::from_be_bytes(*b)).collect();
    (samples, reader.info().icc_profile.as_ref().map(|p| p.to_vec()))
}

#[test]
fn without_a_profile_integers_are_srgb_and_floats_linear() {
    let png = write(&AnyImage::U16(pattern_u16(8, 8, 3)), &spec(OutputFormat::Png, BitDepth::U16), &OutputMetadata::default()).unwrap();
    assert_eq!(read(&png.bytes, &ReadOptions::default()).unwrap().image.color_space(), &ColorSpace::SRGB);
    let tiff = write(
        &AnyImage::F32(pattern_f32(8, 8, 3)),
        &spec(OutputFormat::Tiff { compression: TiffCompression::None }, BitDepth::F32),
        &OutputMetadata::default(),
    )
    .unwrap();
    assert_eq!(read(&tiff.bytes, &ReadOptions::default()).unwrap().image.color_space(), &ColorSpace::LINEAR);
}

#[test]
fn a_gamma_profile_is_applied_on_reading_and_again_on_writing() {
    // Adobe RGB (1998): a 563/256 gamma, not sRGB's curve.
    let adobe = lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::AdobeRgb);
    let samples = pattern_u16(37, 23, 3).into_raw();
    let file = png_with_profile(&samples, 37, 23, &adobe);
    let decoded = read(&file, &ReadOptions::default()).unwrap();
    assert!(decoded.linearized, "the samples come back as linear light");
    let profile = decoded.profile.clone().unwrap();
    assert_eq!(profile.named, Some(NamedSpace::AdobeRgb));
    assert!(matches!(profile.curve, CurveKind::Gamma(g) if (g - 563.0 / 256.0).abs() < 1e-3), "{:?}", profile.curve);
    let linear = decoded.image.as_image::<f32>().unwrap();
    assert_eq!(linear.color_space().encoding, Encoding::Linear);
    assert_eq!(linear.color_space().icc.as_deref(), Some(&adobe[..]), "the profile is kept for its primaries and curve");
    // Written in the input's profile, every 16-bit code comes back, and so do the profile's bytes.
    let out = write(&decoded.image, &spec(OutputFormat::Png, BitDepth::U16), &OutputMetadata::default()).unwrap();
    let (back, icc) = raw_png(&out.bytes);
    assert_eq!(back, samples);
    assert_eq!(icc.as_deref(), Some(&adobe[..]));
}

#[test]
fn a_profile_with_srgb_s_curve_keeps_the_samples_and_its_bytes() {
    // Display P3 uses sRGB's curve: no linearization, the samples as stored.
    let p3 = lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::DisplayP3);
    let samples = pattern_u16(16, 9, 3).into_raw();
    let decoded = read(&png_with_profile(&samples, 16, 9, &p3), &ReadOptions::default()).unwrap();
    assert!(!decoded.linearized);
    let image = decoded.image.as_image::<u16>().unwrap();
    assert_eq!(image.as_raw(), &samples[..]);
    assert_eq!(image.color_space().encoding, Encoding::Srgb);
    assert_eq!(decoded.profile.unwrap().named, Some(NamedSpace::DisplayP3));
    for format in [OutputFormat::Png, OutputFormat::Tiff { compression: TiffCompression::Lzw }] {
        let out = write(&decoded.image, &spec(format, BitDepth::U16), &OutputMetadata::default()).unwrap();
        let again = read(&out.bytes, &ReadOptions::default()).unwrap();
        assert_eq!(again.image, decoded.image, "{format:?}: same samples, same profile bytes");
    }
}

/// The mean and largest CIEDE2000 difference between two 16-bit sRGB images.
fn delta_e(a: &AnyImage, b: &AnyImage) -> (f64, f64) {
    let linear = |i: &AnyImage| i.as_image::<u16>().unwrap().to_linear_f32().unwrap();
    let d = florastack_metrics::color_shift(&linear(a), &linear(b)).unwrap();
    (d.mean, d.max)
}

#[test]
fn wide_spaces_hold_srgb_colors_and_give_them_back() {
    let picture = AnyImage::U16(picture_u8(64, 48).convert::<u16>().unwrap());
    for (space, named) in [(WideSpace::ProPhoto, NamedSpace::ProPhoto), (WideSpace::Rec2020, NamedSpace::Rec2020)] {
        let wide =
            OutputSpec { profile: OutputProfile::Wide(space), ..spec(OutputFormat::Tiff { compression: TiffCompression::Deflate }, BitDepth::U16) };
        let encoded = write(&picture, &wide, &OutputMetadata::default()).unwrap();
        let decoded = read(&encoded.bytes, &ReadOptions::default()).unwrap();
        assert_eq!(decoded.profile.as_ref().unwrap().named, Some(named), "{space:?}");
        // Back to sRGB: the same colors, to well under a just-noticeable difference (about 1).
        let srgb = OutputSpec { profile: OutputProfile::Srgb, ..spec(OutputFormat::Png, BitDepth::U16) };
        let back = read(&write(&decoded.image, &srgb, &OutputMetadata::default()).unwrap().bytes, &ReadOptions::default()).unwrap();
        let (mean, max) = delta_e(&back.image, &picture);
        assert!(mean < 0.01 && max < 0.05, "{space:?}: mean {mean}, max {max}");
        // Float output in a wide space is linear, with a linear profile of that space.
        let float =
            OutputSpec { profile: OutputProfile::Wide(space), ..spec(OutputFormat::Tiff { compression: TiffCompression::None }, BitDepth::F32) };
        let linear = read(&write(&picture, &float, &OutputMetadata::default()).unwrap().bytes, &ReadOptions::default()).unwrap();
        let profile = linear.profile.unwrap();
        assert_eq!((profile.named, profile.curve), (Some(named), CurveKind::Linear));
        assert!(!linear.linearized);
    }
}

#[test]
fn openexr_records_the_primaries_of_a_wide_output() {
    let picture = AnyImage::U16(picture_u8(32, 24).convert::<u16>().unwrap());
    let exr = |profile| OutputSpec { profile, ..spec(OutputFormat::OpenExr { compression: ExrCompression::Zip, half: false }, BitDepth::F32) };
    let wide =
        read(&write(&picture, &exr(OutputProfile::Wide(WideSpace::Rec2020)), &OutputMetadata::default()).unwrap().bytes, &ReadOptions::default())
            .unwrap();
    assert_eq!(wide.profile.unwrap().named, Some(NamedSpace::Rec2020));
    let srgb = read(&write(&picture, &exr(OutputProfile::Input), &OutputMetadata::default()).unwrap().bytes, &ReadOptions::default()).unwrap();
    assert!(srgb.profile.is_none(), "sRGB primaries need no chromaticities");
    assert_eq!(srgb.image.color_space(), &ColorSpace::LINEAR);
}

#[test]
fn untagged_output_writes_data_as_it_is() {
    // A depth map: 16-bit codes that aren't light at all.
    let depth = Image::from_fn(16, 8, 1, ColorSpace::LINEAR, |x, y, _| u16::try_from(x * 4000 + y).unwrap()).unwrap();
    let untagged =
        OutputSpec { profile: OutputProfile::Untagged, ..spec(OutputFormat::Tiff { compression: TiffCompression::Deflate }, BitDepth::U16) };
    let encoded = write(&AnyImage::U16(depth.clone()), &untagged, &OutputMetadata::default()).unwrap();
    let back = read(&encoded.bytes, &ReadOptions::default()).unwrap();
    assert_eq!(back.image.as_image::<u16>().unwrap().as_raw(), depth.as_raw());
    assert!(back.profile.is_none());
}
