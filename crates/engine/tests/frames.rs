//! Reading frames through the engine: 16-bit TIFF and other image files come through florastack-codecs unchanged,
//! RAW and DNG files through florastack-raw, developed alike, and FloraStack's own linear DNG output reads back as it
//! was written.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use florastack_codecs::{
    BitDepth, DngCompression, DngLook, Format, OutputFormat, OutputMetadata, OutputProfile, OutputSpec, TiffCompression, WideSpace, write,
};
use florastack_core::{AnyImage, ColorSpace, Encoding, Image, Orientation};
use florastack_engine::frames::{FrameError, FrameReader, FrameSource, source};
use florastack_engine::{DevelopSettings, OrientationPolicy, Quality, ReadOptions, WhiteBalance};
use florastack_raw::RawFormat;

/// A 16-bit picture with every channel different and values over the whole range.
fn picture(width: usize, height: usize) -> Image<u16> {
    Image::from_fn(width, height, 3, ColorSpace::SRGB, |x, y, c| u16::try_from((x * 997 + y * 6151 + c * 21_841) % 65_536).unwrap()).unwrap()
}

fn tiff(image: &Image<u16>, profile: OutputProfile) -> Vec<u8> {
    let spec = OutputSpec { format: OutputFormat::Tiff { compression: TiffCompression::Lzw }, depth: BitDepth::U16, profile, extended_range: false };
    write(&AnyImage::U16(image.clone()), &spec, &OutputMetadata::default()).unwrap().bytes
}

/// Settings no image file may feel.
fn wild() -> DevelopSettings {
    DevelopSettings { exposure: 3.0, white_balance: WhiteBalance::Custom { temperature: 2500.0, tint: 30.0 }, ..DevelopSettings::default() }
}

#[test]
fn a_16_bit_tiff_is_imported_unchanged() {
    let stored = picture(40, 24);
    let bytes = tiff(&stored, OutputProfile::Input);
    assert_eq!(source(&bytes).unwrap(), FrameSource::Image(Format::Tiff));
    let mut reader = FrameReader::new(wild(), ReadOptions::default());
    let frame = reader.read(&bytes, Quality::Output).unwrap();
    assert_eq!(frame.source, FrameSource::Image(Format::Tiff));
    assert!(frame.unchanged);
    assert!(reader.develop().is_none(), "no RAW frame, nothing resolved");
    // The samples as stored, only decoded from sRGB to linear: exactly what florastack-core's conversion gives.
    let want = stored.to_linear_f32().unwrap();
    assert_eq!(frame.image.color_space().encoding, Encoding::Linear);
    assert!(frame.image.rows().zip(want.rows()).all(|(a, b)| a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())));
    // And back: encoded again they are the stored values, every one.
    let again = frame.image.to_encoding::<u16>(Encoding::Srgb).unwrap();
    assert!(again.rows().zip(stored.rows()).all(|(a, b)| a == b));
}

#[test]
fn a_wide_gamut_tiff_keeps_its_profile() {
    // ProPhoto's 1.8 gamma: linearized through the profile, which the frame keeps, as florastack-codecs reads it.
    let bytes = tiff(&picture(16, 16), OutputProfile::Wide(WideSpace::ProPhoto));
    let frame = FrameReader::new(DevelopSettings::default(), ReadOptions::default()).read(&bytes, Quality::Preview).unwrap();
    let codecs = florastack_codecs::read(&bytes, &ReadOptions::default()).unwrap();
    let AnyImage::F32(want) = codecs.image else { panic!("a linearized read") };
    assert!(frame.image.color_space().icc.is_some());
    assert_eq!(frame.image.color_space(), want.color_space());
    assert!(frame.image.rows().zip(want.rows()).all(|(a, b)| a == b));
}

#[test]
fn florastacks_own_linear_dng_reads_back_as_written() {
    // A stacked result written as linear DNG in Rec. 2020 is a `LinearRaw` DNG: the RAW path imports it unchanged,
    // whatever the stack's settings, to the file's 16-bit precision (and the rational precision of its color tags).
    let stored = picture(32, 20).to_linear_f32().unwrap();
    let mut linear = stored.clone();
    linear.set_color_space(ColorSpace { encoding: Encoding::Linear, icc: Some(florastack_raw::working_space_icc()) });
    let spec = OutputSpec {
        format: OutputFormat::Dng { compression: DngCompression::LosslessJpeg, look: DngLook::Raw },
        depth: BitDepth::U16,
        profile: OutputProfile::Input,
        extended_range: false,
    };
    let bytes = write(&AnyImage::F32(linear), &spec, &OutputMetadata::default()).unwrap().bytes;
    assert_eq!(source(&bytes).unwrap(), FrameSource::Raw(RawFormat::Dng));
    let mut reader = FrameReader::new(wild(), ReadOptions::default());
    let frame = reader.read(&bytes, Quality::Output).unwrap();
    assert!(frame.unchanged && reader.develop().is_some());
    for (a, b) in frame.image.rows().flatten().zip(stored.rows().flatten()) {
        assert!((a - b).abs() < 1.0 / 65535.0 + 1e-4, "{a} for {b}");
    }
}

#[test]
fn the_orientation_policy_applies_to_every_kind() {
    let stored = picture(6, 4);
    let bytes = tiff(&stored, OutputProfile::Input);
    let forced = ReadOptions { orientation: OrientationPolicy::Force(Orientation::Rotate90), ..ReadOptions::default() };
    let frame = FrameReader::new(DevelopSettings::default(), forced).read(&bytes, Quality::Output).unwrap();
    assert_eq!((frame.image.width(), frame.image.height(), frame.orientation), (4, 6, Orientation::Rotate90));
}

#[test]
fn a_real_raw_frame_is_developed_and_the_stack_resolved_on_it() {
    let Some(path) = florastack_testkit::fixture("raw", "dng-canon-eos-5d-mark-iii.dng") else {
        florastack_testkit::skip!(Corpus, "corpus/fixtures/raw/dng-canon-eos-5d-mark-iii.dng is missing: run `cargo xtask corpus --fixtures`");
        return;
    };
    let bytes = std::fs::read(path).unwrap();
    let mut reader = FrameReader::new(DevelopSettings::default(), ReadOptions::default());
    let frame = reader.read(&bytes, Quality::Preview).unwrap();
    assert_eq!(frame.source, FrameSource::Raw(RawFormat::Dng));
    assert!(!frame.unchanged);
    assert_eq!((frame.image.width(), frame.image.height()), (1920, 818));
    let develop = reader.develop().unwrap().clone();
    // A second frame (the same file here) gets the same development; a reader built from the stored resolution too.
    let again = reader.read(&bytes, Quality::Preview).unwrap();
    let mut reopened = FrameReader::with_develop(develop, ReadOptions::default());
    let third = reopened.read(&bytes, Quality::Preview).unwrap();
    for other in [&again, &third] {
        assert!(other.image.rows().zip(frame.image.rows()).all(|(a, b)| a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())));
    }
}

#[test]
fn files_nothing_reads_are_errors() {
    let mut reader = FrameReader::new(DevelopSettings::default(), ReadOptions::default());
    assert!(matches!(reader.read(b"plain text", Quality::Output), Err(FrameError::Codec(_))));
    let mrw = b"\0MRM\0\0\0\0";
    let e = reader.read(mrw, Quality::Output).unwrap_err();
    assert!(matches!(e, FrameError::Raw(florastack_raw::RawError::Unsupported { .. })), "{e:?}");
    assert!(e.to_string().contains("16-bit TIFF"), "{e}");
}
