//! DNG files written here with LightCraft's DNG writer, read back through `florastack_raw::decode`: the samples come
//! back as stored, a DNG that a raw converter already demosaiced comes out unchanged whatever the settings, the file's
//! lens warp applies only when asked, and damaged or oversized files are errors, never crashes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{dng, identical, lightcraft_raw as raw_image, px, rec2020_tags, smooth_scene};
use florastack_core::{Encoding, Orientation};
use florastack_raw::{
    Cfa, ChromaticAberration, DEFAULT_MAX_PIXELS, DevelopSettings, NoiseReduction, Quality, RawDevelop, RawError, RawFormat, SensorLayout,
    WhiteBalance, decode, info, probe,
};

/// The quantization step of the files written here: 16-bit codes from 512 (black) to 16383 (white).
const STEP: f32 = 1.0 / (16383.0 - 512.0);

fn settings() -> DevelopSettings {
    DevelopSettings { chromatic_aberration: ChromaticAberration::Off, ..DevelopSettings::default() }
}

#[test]
fn a_cfa_dng_decodes_to_its_samples() {
    let cfa = Cfa::bayer("GRBG").unwrap();
    let bytes = dng(&raw_image(64, 48, Some(&cfa), rec2020_tags(), smooth_scene));
    assert_eq!(probe(&bytes), Some(RawFormat::Dng));
    let header = info(&bytes).unwrap();
    assert_eq!((header.width, header.height, header.layout), (64, 48, SensorLayout::Bayer));
    assert_eq!(header.make.as_deref(), Some("Synthetic"));
    let frame = decode(&bytes, DEFAULT_MAX_PIXELS).unwrap();
    assert_eq!(frame.info, header, "the header and the decoded file agree");
    assert!(!frame.is_converted());
    assert_eq!(frame.sensor.cfa.as_ref().unwrap().name(), "GRBG");
    for y in 0..48 {
        for x in 0..64 {
            let want = smooth_scene(x, y)[usize::from(cfa.color_at(x, y))];
            let got = frame.sensor.samples[y * 64 + x];
            assert!((got - want).abs() <= STEP, "({x}, {y}): {got} for {want}");
        }
    }
}

#[test]
fn a_converted_dng_is_imported_unchanged() {
    // Linear Rec. 2020 samples, as a raw converter (or FloraStack's own DNG writer) stores them, demosaiced and white
    // balanced: they come back as stored, to the file's 16-bit precision and the color tags' rational precision, and
    // no develop setting changes them.
    let bytes = dng(&raw_image(40, 30, None, rec2020_tags(), smooth_scene));
    let frame = decode(&bytes, DEFAULT_MAX_PIXELS).unwrap();
    assert!(frame.is_converted());
    assert_eq!(frame.info.layout, SensorLayout::Linear);
    let plain = RawDevelop::new(&settings(), &frame).unwrap().develop(&frame, Quality::Output).unwrap();
    assert!(plain.converted);
    assert_eq!(plain.image.color_space().encoding, Encoding::Linear);
    for y in 0..30 {
        for x in 0..40 {
            for (got, want) in px(&plain.image, x, y).iter().zip(smooth_scene(x, y)) {
                assert!((got - want).abs() < STEP + 1e-4, "({x}, {y}): {got} for {want}");
            }
        }
    }
    let wild = DevelopSettings {
        white_balance: WhiteBalance::Custom { temperature: 2500.0, tint: 40.0 },
        exposure: 3.0,
        chromatic_aberration: ChromaticAberration::Manual { red: 0.005, blue: -0.005 },
        noise_reduction: NoiseReduction { luminance: 1.0, color: 1.0 },
        ..settings()
    };
    for quality in [Quality::Preview, Quality::Output] {
        let other = RawDevelop::new(&wild, &frame).unwrap().develop(&frame, quality).unwrap();
        assert!(identical(&plain.image, &other.image), "{quality:?}");
    }
}

#[test]
fn a_floating_point_dng_keeps_values_above_white() {
    // An HDR merge stores floats that may exceed 1: nothing clips them.
    let mut hdr = raw_image(16, 16, None, rec2020_tags(), |x, _| [x as f32 / 4.0; 3]);
    hdr.data = lightcraft_raw::RawData::F32((0..16 * 16).flat_map(|i| [(i % 16) as f32 / 4.0; 3]).collect());
    hdr.black = lightcraft_raw::BlackLevel::uniform(0.0);
    hdr.white = vec![1.0];
    let options =
        lightcraft_raw::DngWriteOptions { compression: lightcraft_raw::DngCompression::Deflate { tile: 16, half: false }, ..Default::default() };
    let bytes = lightcraft_raw::write_dng(&hdr, &options).unwrap();
    let frame = decode(&bytes, DEFAULT_MAX_PIXELS).unwrap();
    assert!(frame.sensor.float);
    let out = RawDevelop::new(&settings(), &frame).unwrap().develop(&frame, Quality::Output).unwrap();
    let p = px(&out.image, 15, 3);
    assert!(p.iter().all(|v| (v - 3.75).abs() < 1e-3), "{p:?}");
}

/// A DNG `WarpRectilinear` that magnifies every plane by 3 % about the center.
fn magnify() -> lightcraft_raw::Opcode {
    lightcraft_raw::Opcode::WarpRectilinear { planes: vec![[1.03, 0.0, 0.0, 0.0, 0.0, 0.0]], center: [0.5, 0.5] }
}

#[test]
fn the_files_lens_warp_applies_only_when_asked() {
    let cfa = Cfa::bayer("RGGB").unwrap();
    let plain = raw_image(96, 64, Some(&cfa), rec2020_tags(), smooth_scene);
    let mut warped = plain.clone();
    warped.opcodes.list3 = vec![magnify()];
    let (plain, warped) = (decode(&dng(&plain), DEFAULT_MAX_PIXELS).unwrap(), decode(&dng(&warped), DEFAULT_MAX_PIXELS).unwrap());
    assert!(warped.info.distortion_correction && !plain.info.distortion_correction);
    let develop = |frame: &florastack_raw::RawFrame, lens_distortion: bool| {
        let s = DevelopSettings { lens_distortion, ..settings() };
        RawDevelop::new(&s, frame).unwrap().develop(frame, Quality::Output).unwrap().image
    };
    // Off by default: the warp is ignored and the frame keeps its geometry, pixel for pixel.
    assert!(!DevelopSettings::default().lens_distortion);
    assert!(identical(&develop(&warped, false), &develop(&plain, false)));
    // On: the picture moves.
    assert!(!identical(&develop(&warped, true), &develop(&plain, true)));
    assert!(identical(&develop(&plain, true), &develop(&plain, false)), "nothing to apply");
}

#[test]
fn the_files_vignetting_correction_always_applies() {
    // FixVignetteRadial is a gain, not a move: it doesn't change the geometry, so it applies with lens distortion
    // correction off.
    let cfa = Cfa::bayer("RGGB").unwrap();
    let plain = raw_image(96, 64, Some(&cfa), rec2020_tags(), |_, _| [0.3; 3]);
    let mut vignetted = plain.clone();
    vignetted.opcodes.list3 = vec![lightcraft_raw::Opcode::FixVignetteRadial { k: [0.5, 0.0, 0.0, 0.0, 0.0], center: [0.5, 0.5] }];
    let develop = |raw: &lightcraft_raw::RawImage| {
        let frame = decode(&dng(raw), DEFAULT_MAX_PIXELS).unwrap();
        RawDevelop::new(&settings(), &frame).unwrap().develop(&frame, Quality::Output).unwrap().image
    };
    let (a, b) = (develop(&plain), develop(&vignetted));
    let (corner, middle) = ((px(&b, 0, 0)[1], px(&a, 0, 0)[1]), (px(&b, 48, 32)[1], px(&a, 48, 32)[1]));
    assert!(corner.0 > corner.1 * 1.3, "the corner is brightened: {corner:?}");
    assert!((middle.0 / middle.1 - 1.0).abs() < 0.01, "the center hardly: {middle:?}");
}

#[test]
fn the_recorded_orientation_is_reported_not_applied() {
    let cfa = Cfa::bayer("RGGB").unwrap();
    let mut raw = raw_image(32, 20, Some(&cfa), rec2020_tags(), smooth_scene);
    raw.orientation = lightcraft_raw::Orientation::Rotate90;
    let frame = decode(&dng(&raw), DEFAULT_MAX_PIXELS).unwrap();
    assert_eq!(frame.info.orientation, Orientation::Rotate90);
    let out = RawDevelop::new(&settings(), &frame).unwrap().develop(&frame, Quality::Preview).unwrap();
    assert_eq!((out.image.width(), out.image.height(), out.orientation), (32, 20, Orientation::Rotate90));
}

#[test]
fn oversized_files_are_refused_before_decoding() {
    let cfa = Cfa::bayer("RGGB").unwrap();
    let bytes = dng(&raw_image(32, 20, Some(&cfa), rec2020_tags(), smooth_scene));
    assert!(matches!(decode(&bytes, 32 * 20 - 1), Err(RawError::TooLarge { width: 32, height: 20, .. })));
    assert!(decode(&bytes, 32 * 20).is_ok());
}

#[test]
fn damaged_files_are_errors_not_crashes() {
    let cfa = Cfa::bayer("RGGB").unwrap();
    let bytes = dng(&raw_image(48, 32, Some(&cfa), rec2020_tags(), smooth_scene));
    // Every truncation, and the file with its bytes flipped one region at a time: each decodes or fails cleanly, and
    // what decodes develops.
    let develop =
        RawDevelop::from_resolved(&settings(), florastack_raw::Resolved { white: florastack_raw::Chromaticity::D65, chromatic_aberration: [0.0; 2] })
            .unwrap();
    let check = |damaged: &[u8]| {
        if let Ok(frame) = decode(damaged, DEFAULT_MAX_PIXELS) {
            develop.develop(&frame, Quality::Preview).unwrap();
        }
    };
    for len in (0..bytes.len()).step_by(97) {
        check(&bytes[..len]);
    }
    for start in (8..bytes.len()).step_by(61) {
        let mut damaged = bytes.clone();
        for b in damaged.iter_mut().skip(start).take(16) {
            *b ^= 0xa5;
        }
        check(&damaged);
    }
    assert!(matches!(decode(b"not a raw file at all", DEFAULT_MAX_PIXELS), Err(RawError::NotRaw)));
    #[cfg(not(target_arch = "wasm32"))]
    assert!(matches!(florastack_raw::decode_file(std::path::Path::new("no-such-file.dng"), DEFAULT_MAX_PIXELS), Err(RawError::Read { .. })));
}

mod properties {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// Arbitrary bytes written over a DNG's header and directory never crash the decoder.
        #[test]
        fn corrupted_headers_never_crash(offset in 0usize..512, garbage in proptest::collection::vec(any::<u8>(), 1..32)) {
            let cfa = Cfa::bayer("RGGB").unwrap();
            let mut bytes = dng(&raw_image(24, 16, Some(&cfa), rec2020_tags(), smooth_scene));
            let end = (offset + garbage.len()).min(bytes.len());
            if offset < end {
                bytes[offset..end].copy_from_slice(&garbage[..end - offset]);
            }
            let _ = decode(&bytes, DEFAULT_MAX_PIXELS);
            let _ = info(&bytes);
        }
    }
}
