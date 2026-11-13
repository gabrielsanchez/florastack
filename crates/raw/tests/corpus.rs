//! One real RAW file per format florastack-raw decodes: the `raw` fixture set of `corpus/manifest.toml` (CC0 files
//! from raw.pixls.us, which `cargo xtask corpus --fixtures` fetches into `corpus/fixtures/raw/`; CI does so before
//! the tests). Without them the test records a skip, so `cargo xtask ci` lists it.
//!
//! The files are decoded one after another inside one test, and a lock keeps any other test of this binary from
//! running beside it: the largest (26 megapixels) needs about 160 MB as it decodes, and the develop steps work on a
//! 256 x 256 region of each, so the test never needs more than that at once.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Mutex;

use florastack_core::{Image, Orientation, Rect};
use florastack_raw::{
    ColorSource, DEFAULT_MAX_PIXELS, DevelopSettings, NoiseReduction, Quality, RawDevelop, RawFormat, RawFrame, SensorLayout, WhiteBalance, decode,
    info,
};
use florastack_testkit::{fixture, skip};

/// Serializes the tests that decode corpus files.
static HEAVY: Mutex<()> = Mutex::new(());

/// What a file must decode to: its format, camera, developed size (before orientation) and layout, as the pinned
/// LightCraft commit reads them. A pin bump that changes one of these shows here first.
struct Expected {
    file: &'static str,
    format: RawFormat,
    model: &'static str,
    size: (usize, usize),
    layout: SensorLayout,
    color: ColorSource,
    orientation: Orientation,
}

const fn file(
    file: &'static str,
    format: RawFormat,
    model: &'static str,
    size: (usize, usize),
    layout: SensorLayout,
    color: ColorSource,
    orientation: Orientation,
) -> Expected {
    Expected { file, format, model, size, layout, color, orientation }
}

use ColorSource::{Fallback, File, Measured};
use Orientation::{Normal, Rotate270};
use SensorLayout::{Bayer, Linear, XTrans};

const FILES: &[Expected] = &[
    file("dng-canon-eos-5d-mark-iii.dng", RawFormat::Dng, "Canon EOS 5D Mark III", (1920, 818), Bayer, File, Normal),
    file("dng-adobe-converter-linear.dng", RawFormat::Dng, "Canon EOS 5D Mark III", (3960, 2640), Linear, File, Normal),
    file("cr2-canon-eos-digital-rebel-xt.cr2", RawFormat::Cr2, "Canon EOS DIGITAL REBEL XT", (3456, 2304), Bayer, Fallback, Rotate270),
    file("cr3-canon-eos-r6.cr3", RawFormat::Cr3, "Canon EOS R6", (3408, 2272), Bayer, Measured, Normal),
    file("arw-sony-ilce-7s.arw", RawFormat::Arw, "ILCE-7S", (2768, 1848), Bayer, Fallback, Normal),
    file("nef-nikon-d2h.nef", RawFormat::Nef, "NIKON D2H", (2496, 1648), Bayer, Fallback, Normal),
    file("raf-fujifilm-x-s10-xtrans.raf", RawFormat::Raf, "X-S10", (6240, 4160), XTrans, Measured, Normal),
    file("raf-fujifilm-finepix-x100-bayer.raf", RawFormat::Raf, "FinePix X100", (4288, 2848), Bayer, Fallback, Normal),
    file("rw2-panasonic-dmc-lx7.rw2", RawFormat::Rw2, "DMC-LX7", (1368, 1368), Bayer, Fallback, Normal),
    file("pef-pentax-k10d.pef", RawFormat::Pef, "PENTAX K10D", (3872, 2592), Bayer, Fallback, Normal),
    file("orf-olympus-e-10.orf", RawFormat::Orf, "E-10", (2256, 1684), Bayer, Fallback, Normal),
    file("srw-samsung-ex1.srw", RawFormat::Srw, "EX1", (3648, 2736), Bayer, Fallback, Normal),
    // Paths the files above don't reach: lossless CRX (the R6 file above is C-RAW), Canon's CIFF files of 2000 to
    // 2004, Minolta's MRW, Epson's ERF, an uncompressed Nikon NRW (which LightCraft reads as the NEF it is inside)
    // and Leica's RWL (Panasonic's RW2 under another name).
    file("cr3-canon-eos-r6-raw.cr3", RawFormat::Cr3, "Canon EOS R6", (5472, 3648), Bayer, Measured, Normal),
    file("crw-canon-eos-d30.crw", RawFormat::Crw, "Canon EOS D30", (2160, 1440), Bayer, Fallback, Normal),
    file("mrw-minolta-dimage-5.mrw", RawFormat::Mrw, "DiMAGE 5", (2048, 1544), Bayer, Fallback, Normal),
    file("erf-epson-r-d1.erf", RawFormat::Erf, "R-D1", (3008, 2000), Bayer, Fallback, Normal),
    file("nrw-nikon-coolpix-p1000.nrw", RawFormat::Nef, "COOLPIX P1000", (4624, 3470), Bayer, Fallback, Normal),
    file("rwl-leica-d-lux-5.rwl", RawFormat::Rw2, "D-LUX 5", (2736, 2736), Bayer, Fallback, Normal),
];

/// The 256 x 256 region at the center of the frame's crop.
fn center(frame: &RawFrame) -> RawFrame {
    let c = frame.crop;
    let side = 256.min(c.width).min(c.height);
    let rect = Rect::new(c.x + (c.width - side) / 2, c.y + (c.height - side) / 2, side, side);
    frame.region(rect).unwrap()
}

fn samples(image: &Image<f32>) -> impl Iterator<Item = f32> + '_ {
    image.rows().flatten().copied()
}

/// The largest difference between the 16 x 16 block means of two images, relative to the block's level, over the
/// smoothest quarter of the blocks (by mean neighbor difference, relative to the level). The outer ring of blocks,
/// where each demosaic mirrors the region's edge its own way, is left out.
fn smooth_block_difference(preview: &Image<f32>, output: &Image<f32>) -> f64 {
    let blocks = (preview.width() / 16, preview.height() / 16);
    let mut stats = Vec::new();
    for by in 1..blocks.1.saturating_sub(1) {
        for bx in 1..blocks.0.saturating_sub(1) {
            let (mut sp, mut so, mut rough) = ([0f64; 3], [0f64; 3], 0f64);
            for y in by * 16..by * 16 + 16 {
                for x in bx * 16..(bx * 16 + 16).min(output.width() - 1) {
                    let (p, o, next) = (preview.pixel(x, y).unwrap(), output.pixel(x, y).unwrap(), output.pixel(x + 1, y).unwrap());
                    for c in 0..3 {
                        sp[c] += f64::from(p[c]);
                        so[c] += f64::from(o[c]);
                        rough += f64::from((next[c] - o[c]).abs());
                    }
                }
            }
            let level = (so[0] + so[1] + so[2]).abs().max(1e-9);
            let diff = (0..3).map(|c| (sp[c] - so[c]).abs() * 3.0 / level).fold(0.0, f64::max);
            stats.push((rough / level, diff));
        }
    }
    stats.sort_by(|a, b| a.0.total_cmp(&b.0));
    stats[..stats.len() / 4].iter().map(|s| s.1).fold(0.0, f64::max)
}

#[test]
fn every_format_decodes_and_develops() {
    let _heavy = HEAVY.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if florastack_testkit::fixtures("raw").is_empty() {
        skip!(Corpus, "the RAW fixtures are missing: run `cargo xtask corpus --fixtures`");
        return;
    }
    for expected in FILES {
        let Some(path) = fixture("raw", expected.file) else {
            skip!(Corpus, "corpus/fixtures/raw/{} is missing: run `cargo xtask corpus --fixtures`", expected.file);
            continue;
        };
        let name = expected.file;
        let bytes = std::fs::read(&path).unwrap();
        // The headers alone, and the whole file: they agree.
        let header = info(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let frame = decode(&bytes, DEFAULT_MAX_PIXELS).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(frame.info, header, "{name}");
        assert_eq!(header.format, expected.format, "{name}");
        assert!(header.model.as_deref().is_some_and(|m| m.contains(expected.model)), "{name}: {:?}", header.model);
        assert_eq!((header.width, header.height), expected.size, "{name}");
        assert_eq!(header.layout, expected.layout, "{name}");
        assert_eq!(header.color, expected.color, "{name}");
        assert_eq!(header.orientation, expected.orientation, "{name}");
        assert!(header.as_shot_white.is_none_or(florastack_raw::Chromaticity::is_plausible), "{name}: {:?}", header.as_shot_white);
        let finite = frame.sensor.samples.iter().filter(|v| v.is_finite()).count();
        assert_eq!(finite, frame.sensor.samples.len(), "{name}: every sample is a number");

        // The whole frame at a fraction of its size, straight from the mosaic: at least 256 pixels on its long side,
        // no more than half the frame's, every sample a number. Files with corrections of their own to apply after
        // demosaicing, and those a raw converter demosaiced, come whole instead.
        let quick = RawDevelop::new(&DevelopSettings::default(), &center(&frame)).unwrap().develop_reduced(&frame, 256).unwrap();
        let (long, full) = (quick.image.width().max(quick.image.height()), header.width.max(header.height));
        if expected.layout == Linear {
            assert_eq!(long, full, "{name}");
        } else {
            assert!(long == full || (256..=full / 2).contains(&long), "{name}: reduced to {long} of {full}");
        }
        assert!(samples(&quick.image).all(f32::is_finite), "{name}");
        drop(quick);

        // A region at full resolution, developed as a stack's reference with automatic white balance and
        // chromatic-aberration estimate: the preview and the output agree on its smooth parts, and a second run gives
        // the same pixels.
        let region = center(&frame);
        drop(frame);
        let settings = DevelopSettings { white_balance: WhiteBalance::Auto, ..DevelopSettings::default() };
        let develop = RawDevelop::new(&settings, &region).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(develop.resolved().white.is_plausible(), "{name}: {:?}", develop.resolved());
        let preview = develop.develop(&region, Quality::Preview).unwrap();
        let output = develop.develop(&region, Quality::Output).unwrap();
        assert_eq!(output.converted, expected.layout == Linear, "{name}");
        assert!(samples(&output.image).all(f32::is_finite), "{name}");
        let mean = samples(&output.image).map(f64::from).sum::<f64>() / (output.image.pixel_count() * 3) as f64;
        assert!((0.002..2.0).contains(&mean), "{name}: mean level {mean}");
        // Real sensors add noise and texture at the photosite scale, which the two demosaics resolve differently, but
        // over a smooth 16 x 16 block they find the same color: within 3 % of the block's level (the finest-grained
        // region here, the LX7's, comes to 2.5 %; the others to 1.4 % or less). A wrong CFA phase or a missing step
        // would be off by tens of percent.
        let diff = smooth_block_difference(&preview.image, &output.image);
        assert!(diff < 0.03, "{name}: the preview and the output differ by {diff} on smooth blocks");
        let again = develop.develop(&region, Quality::Output).unwrap();
        assert!(
            samples(&again.image).zip(samples(&output.image)).all(|(a, b)| a.to_bits() == b.to_bits()),
            "{name}: identical settings, identical output"
        );
        // An already-demosaiced DNG ignores the settings.
        if expected.layout == Linear {
            let wild = DevelopSettings { exposure: 2.0, white_balance: WhiteBalance::Custom { temperature: 3000.0, tint: 20.0 }, ..settings };
            let other = RawDevelop::new(&wild, &region).unwrap().develop(&region, Quality::Output).unwrap();
            assert!(samples(&other.image).zip(samples(&output.image)).all(|(a, b)| a.to_bits() == b.to_bits()), "{name}: imported unchanged");
        }
    }
}

#[test]
fn a_whole_frame_develops_at_full_size() {
    // The smallest file, end to end: every step on the whole frame, the default crop and the automatic
    // chromatic-aberration estimate included, twice, bit for bit.
    let _heavy = HEAVY.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(path) = fixture("raw", "dng-canon-eos-5d-mark-iii.dng") else {
        skip!(Corpus, "corpus/fixtures/raw/dng-canon-eos-5d-mark-iii.dng is missing: run `cargo xtask corpus --fixtures`");
        return;
    };
    let frame = decode(&std::fs::read(path).unwrap(), DEFAULT_MAX_PIXELS).unwrap();
    let settings = DevelopSettings { noise_reduction: NoiseReduction { luminance: 0.1, color: 0.25 }, ..DevelopSettings::default() };
    let develop = RawDevelop::new(&settings, &frame).unwrap();
    let first = develop.develop(&frame, Quality::Output).unwrap();
    assert_eq!((first.image.width(), first.image.height()), (frame.info.width, frame.info.height));
    assert_eq!(first.image.color_space().icc.as_deref(), Some(&florastack_raw::working_space_icc()[..]));
    let second = RawDevelop::new(&settings, &frame).unwrap().develop(&frame, Quality::Output).unwrap();
    assert!(samples(&first.image).zip(samples(&second.image)).all(|(a, b)| a.to_bits() == b.to_bits()));
    assert_eq!(first.fingerprint, second.fingerprint);
    // Reduced straight from the mosaic, the frame holds the same light: each channel's mean over the picture is
    // within 1 % of the full development's (they differ by what highlight reconstruction and the crop's last
    // partial blocks see).
    let reduced = develop.develop_reduced(&frame, 400).unwrap();
    assert_eq!((reduced.image.width(), reduced.image.height()), (frame.info.width / 4, frame.info.height / 4));
    let mean = |image: &Image<f32>, c: usize| samples(image).skip(c).step_by(3).map(f64::from).sum::<f64>() / image.pixel_count() as f64;
    for c in 0..3 {
        let (small, whole) = (mean(&reduced.image, c), mean(&first.image, c));
        assert!((small - whole).abs() <= whole * 0.01, "channel {c}: {small} reduced, {whole} whole");
    }
}
