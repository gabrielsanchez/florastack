//! Metadata through a stack's output: EXIF, IPTC and XMP read from a camera JPEG and carried into every output
//! format, with FloraStack's software tag, the frame count in XMP, the output's own size and orientation, and what no
//! longer applies (the `MakerNote`, the thumbnail, develop settings) left out.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fmt::Write as _;

use common::{camera_jpeg, iim, picture_u8, spec};
use florastack_codecs::{
    BitDepth, ChromaSubsampling, DngCompression, DngLook, ExrCompression, Format, Metadata, MetadataKind, OutputFormat, OutputMetadata, ReadOptions,
    TiffCompression, Timestamp, Warning, read, read_metadata, write,
};
use florastack_core::{AnyImage, branding};
use lightcraft_tiff::{Tiff, tags as t};

fn software() -> String {
    format!("{} {}", branding::DISPLAY_NAME, env!("CARGO_PKG_VERSION"))
}

fn frame_count_in(xmp: &str) -> Option<String> {
    let parsed = lightcraft_meta::parse_xmp(xmp).unwrap();
    parsed.values.get(&format!("{}:FrameCount", branding::XMP_PREFIX)).and_then(lightcraft_meta::XmpValue::text).map(str::to_owned)
}

/// The camera frame, read: its metadata is what the outputs copy.
fn source() -> (AnyImage, Metadata) {
    let decoded = read(&camera_jpeg(&picture_u8(64, 48), 1), &ReadOptions::default()).unwrap();
    (decoded.image, decoded.metadata)
}

#[test]
fn a_camera_jpeg_s_metadata_is_read() {
    let (_, m) = source();
    let s = m.summary();
    assert_eq!((s.make.as_deref(), s.model.as_deref()), (Some("Canon"), Some("Canon EOS R5")));
    assert_eq!(s.exposure_time, Some(0.004));
    assert_eq!(s.f_number, Some(5.6));
    assert_eq!(s.iso, Some(200));
    assert_eq!(s.lens.as_deref(), Some("RF100mm F2.8 L MACRO IS USM"));
    assert!(s.capture_time.as_deref().unwrap().starts_with("2026-10-20T09:15:00"), "{:?}", s.capture_time);
    assert_eq!(s.title.as_deref(), Some("Weevil on moss"), "XMP's title wins over IPTC's object name");
    assert!(s.keywords.iter().any(|k| k == "macro"));
    assert!(s.gps.is_some());
    assert_eq!(m.iptc(), Some(&iim()[..]));
    assert!(m.xmp().unwrap().contains("Weevil on moss"));
}

/// The outputs in every format, with the camera frame's metadata, 57 frames and a time.
fn outputs() -> Vec<(OutputFormat, BitDepth, Vec<u8>, Vec<Warning>)> {
    let (image, metadata) = source();
    let out = OutputMetadata {
        source: Some(metadata),
        frame_count: Some(57),
        modified: Some(Timestamp { year: 2026, month: 10, day: 21, hour: 12, minute: 30, second: 0 }),
    };
    [
        (OutputFormat::Jpeg { quality: 90, subsampling: ChromaSubsampling::S444 }, BitDepth::U8),
        (OutputFormat::Png, BitDepth::U16),
        (OutputFormat::Tiff { compression: TiffCompression::Deflate }, BitDepth::U16),
        (OutputFormat::OpenExr { compression: ExrCompression::Zip, half: false }, BitDepth::F32),
        (OutputFormat::Dng { compression: DngCompression::Uncompressed, look: DngLook::Rendered }, BitDepth::U16),
    ]
    .into_iter()
    .map(|(format, depth)| {
        let encoded = write(&image, &spec(format, depth), &out).unwrap();
        (format, depth, encoded.bytes, encoded.warnings)
    })
    .collect()
}

#[test]
fn every_output_carries_the_source_frame_s_metadata_and_florastack_s_own() {
    for (format, _, bytes, warnings) in outputs() {
        let name = format.format();
        // DNG files are RAW: read their metadata alone (the RAW importer reads the pixels).
        let metadata = if name == Format::Dng { read_metadata(&bytes) } else { read(&bytes, &ReadOptions::default()).unwrap().metadata };
        let xmp = metadata.xmp().unwrap_or_else(|| panic!("{name}: XMP"));
        assert_eq!(frame_count_in(xmp).as_deref(), Some("57"), "{name}");
        let parsed = lightcraft_meta::parse_xmp(xmp).unwrap();
        let text = |k: &str| parsed.values.get(k).and_then(lightcraft_meta::XmpValue::text).map(str::to_owned);
        assert_eq!(text("xmp:CreatorTool"), Some(software()), "{name}");
        assert_eq!(text("xmp:ModifyDate").as_deref(), Some("2026-10-21T12:30:00"), "{name}");
        assert_eq!(text("dc:title").as_deref(), Some("Weevil on moss"), "{name}: the source's own XMP stays");
        assert_eq!(text("xmp:Rating").as_deref(), Some("4"), "{name}");
        assert!(!parsed.values.keys().any(|k| k.starts_with("crs:")), "{name}: develop settings go");
        assert!(!parsed.values.contains_key("xmpMM:DocumentID"), "{name}");
        let s = metadata.summary();
        assert_eq!(s.title.as_deref(), Some("Weevil on moss"), "{name}");
        if name == Format::OpenExr {
            // OpenEXR has no EXIF or IPTC; its XMP and standard attributes carry the rest.
            assert!(metadata.exif().is_none() && metadata.iptc().is_none());
            assert_eq!(warnings, [Warning::MetadataNotSupported { format: name, kind: MetadataKind::Iptc }]);
            continue;
        }
        assert_eq!(s.software.as_deref(), Some(software().as_str()), "{name}");
        assert_eq!((s.make.as_deref(), s.model.as_deref()), (Some("Canon"), Some("Canon EOS R5")), "{name}");
        assert_eq!((s.exposure_time, s.f_number, s.iso), (Some(0.004), Some(5.6), Some(200)), "{name}");
        assert_eq!(s.artist.as_deref(), Some("A. Photographer"), "{name}");
        assert!(s.gps.is_some(), "{name}");
        let exif = Tiff::parse(metadata.exif().unwrap()).unwrap();
        let ifd0 = &exif.ifds[0];
        assert_eq!(ifd0.u16(t::ORIENTATION), Some(1), "{name}");
        assert_eq!(ifd0.string(t::DATE_TIME).as_deref(), Some("2026:10:21 12:30:00"), "{name}");
        let exif_ifd = ifd0.exif.as_deref().unwrap();
        assert!(!exif_ifd.contains(t::MAKER_NOTE), "{name}: the MakerNote goes");
        assert!(exif_ifd.interop.is_none(), "{name}");
        assert_eq!((exif_ifd.u32(t::PIXEL_X_DIMENSION), exif_ifd.u32(t::PIXEL_Y_DIMENSION)), (Some(64), Some(48)), "{name}");
        if name == Format::Png {
            assert_eq!(metadata.iptc(), None);
            assert_eq!(warnings, [Warning::MetadataNotSupported { format: name, kind: MetadataKind::Iptc }]);
        } else {
            assert_eq!(metadata.iptc(), Some(&iim()[..]), "{name}: the IPTC stream byte for byte");
            assert_eq!(warnings, [], "{name}");
        }
    }
}

#[test]
fn without_a_source_the_output_still_says_who_made_it() {
    let image = AnyImage::U8(picture_u8(16, 16));
    let out = OutputMetadata { frame_count: Some(3), ..OutputMetadata::default() };
    let encoded = write(&image, &spec(OutputFormat::Tiff { compression: TiffCompression::None }, BitDepth::U8), &out).unwrap();
    let m = read(&encoded.bytes, &ReadOptions::default()).unwrap().metadata;
    assert_eq!(m.summary().software.as_deref(), Some(software().as_str()));
    assert_eq!(frame_count_in(m.xmp().unwrap()).as_deref(), Some("3"));
    assert!(m.iptc().is_none());
    // Two writes without a time give the same bytes.
    let again = write(&image, &spec(OutputFormat::Tiff { compression: TiffCompression::None }, BitDepth::U8), &out).unwrap();
    assert_eq!(again.bytes, encoded.bytes);
}

#[test]
fn a_jpeg_output_trims_a_source_packet_too_large_for_its_segment() {
    let (image, metadata) = source();
    let mut history = String::new();
    for i in 0..4000 {
        writeln!(history, "<rdf:li stEvt:action=\"saved\" stEvt:instanceID=\"xmp.iid:{i:08}\"/>").unwrap();
    }
    let big = metadata.xmp().unwrap().replace(
        "</rdf:Description>",
        &format!("<xmpMM:History xmlns:stEvt=\"http://ns.adobe.com/xap/1.0/sType/ResourceEvent#\"><rdf:Seq>{history}</rdf:Seq></xmpMM:History></rdf:Description>"),
    );
    assert!(big.len() > 65_504);
    let source = Metadata::from_parts(metadata.exif().map(<[u8]>::to_vec), Some(big), metadata.iptc().map(<[u8]>::to_vec));
    let out = OutputMetadata { source: Some(source), frame_count: Some(9), modified: None };
    let encoded = write(&image, &spec(OutputFormat::Jpeg { quality: 85, subsampling: ChromaSubsampling::S420 }, BitDepth::U8), &out).unwrap();
    assert_eq!(encoded.warnings, [Warning::MetadataTrimmed { kind: MetadataKind::Xmp, removed: "the document history" }]);
    let m = read(&encoded.bytes, &ReadOptions::default()).unwrap().metadata;
    assert_eq!(frame_count_in(m.xmp().unwrap()).as_deref(), Some("9"));
    assert_eq!(m.summary().title.as_deref(), Some("Weevil on moss"));
}

#[test]
fn damaged_source_metadata_is_left_out_with_a_warning() {
    let image = AnyImage::U8(picture_u8(16, 16));
    let broken = Metadata::from_parts(Some(b"II*\0 not an IFD".to_vec()), Some("<x:xmpmeta><rdf:RDF".into()), None);
    let out = OutputMetadata { source: Some(broken), frame_count: Some(2), modified: None };
    let encoded = write(&image, &spec(OutputFormat::Png, BitDepth::U8), &out).unwrap();
    assert_eq!(encoded.warnings, [Warning::UnreadableMetadata(MetadataKind::Exif), Warning::UnreadableMetadata(MetadataKind::Xmp)]);
    let m = read(&encoded.bytes, &ReadOptions::default()).unwrap().metadata;
    assert_eq!(m.summary().software.as_deref(), Some(software().as_str()));
    assert_eq!(frame_count_in(m.xmp().unwrap()).as_deref(), Some("2"));
}

#[test]
fn metadata_reads_from_any_container() {
    for (format, _, bytes, _) in outputs() {
        let m = read_metadata(&bytes);
        assert!(m.xmp().is_some(), "{format:?}");
    }
    assert!(read_metadata(b"not an image").is_empty());
}
