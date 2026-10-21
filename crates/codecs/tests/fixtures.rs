//! Files from other writers, fetched by `cargo xtask corpus` into `corpus/fixtures/` (see `corpus/manifest.toml`):
//! PngSuite, TIFF variants written by libtiff (image-tiff's test images), WebP files with their reference PNGs
//! (image-webp's) and OpenEXR sample and damaged files (openexr-images). Each test skips, recording why, when its
//! set hasn't been fetched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use florastack_codecs::{CurveKind, Format, OutputFormat, OutputMetadata, OutputProfile, OutputSpec, ReadOptions, TiffCompression, read, write};
use florastack_core::{AnyImage, Encoding};
use florastack_testkit::{fixtures, skip};

fn set(name: &str) -> Option<Vec<PathBuf>> {
    let files = fixtures(name);
    if files.is_empty() { None } else { Some(files) }
}

fn name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

#[test]
fn pngsuite_files_decode_and_damaged_ones_fail() {
    let Some(files) = set("pngsuite") else {
        skip!(Corpus, "the PngSuite fixtures are missing: run `cargo xtask corpus`");
        return;
    };
    let pngs: Vec<&PathBuf> = files.iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))).collect();
    assert!(pngs.len() >= 170, "{} files", pngs.len());
    let mut decoded = BTreeMap::new();
    for path in pngs {
        let file = name(path);
        let result = read(&bytes(path), &ReadOptions::default());
        if file.starts_with('x') {
            assert!(result.is_err(), "{file} is damaged on purpose");
            continue;
        }
        let d = result.unwrap_or_else(|e| panic!("{file}: {e}"));
        // basn0g16: b = basic, n = not interlaced, 0 = gray, g = depth 16.
        if let Some(code) = file.get(4..6) {
            let channels = match code {
                "0g" => Some(1),
                "2c" | "3p" => Some(3),
                "4a" => Some(2),
                "6a" => Some(4),
                _ => None,
            };
            if file.starts_with("bas")
                && let Some(channels) = channels
            {
                assert_eq!(d.info.channels, channels, "{file}");
                let bits: u8 = file[6..8].parse().unwrap();
                assert_eq!(d.info.bits_per_sample, bits, "{file}");
            }
        }
        decoded.insert(file, d);
    }
    // Interlaced and plain, every zlib level and every IDAT split of one picture decode alike.
    let same = |a: &str, b: &str| assert_eq!(decoded[a].image, decoded[b].image, "{a} = {b}");
    for (n, i) in
        [("basn0g16.png", "basi0g16.png"), ("basn2c16.png", "basi2c16.png"), ("basn6a08.png", "basi6a08.png"), ("basn3p08.png", "basi3p08.png")]
    {
        same(n, i);
    }
    for level in ["z00n2c08.png", "z03n2c08.png", "z06n2c08.png", "z09n2c08.png"] {
        same("z00n2c08.png", level);
    }
    for split in ["oi1n0g16.png", "oi2n0g16.png", "oi4n0g16.png", "oi9n0g16.png"] {
        same("basn0g16.png", split);
    }
    // A gAMA chunk is a curve: these files come back as linear light through it.
    let g = &decoded["g10n0g16.png"];
    assert!(!g.linearized && g.image.color_space().encoding == Encoding::Linear, "gamma 1.0 is linear already");
    let g25 = &decoded["g25n2c08.png"];
    assert!(g25.linearized && matches!(g25.profile.as_ref().unwrap().curve, CurveKind::Gamma(_)));
    assert!(decoded["exif2c08.png"].metadata.exif().is_some(), "the eXIf chunk");
    // Every picture that keeps its samples comes back from FloraStack's own PNG writer exactly.
    let mut checked = 0;
    for (file, d) in &decoded {
        if d.linearized {
            continue;
        }
        let depth = match d.image {
            AnyImage::U8(_) => florastack_codecs::BitDepth::U8,
            AnyImage::U16(_) => florastack_codecs::BitDepth::U16,
            AnyImage::F32(_) => continue,
        };
        let spec = OutputSpec { format: OutputFormat::Png, depth, profile: OutputProfile::Input, extended_range: false };
        let again = read(&write(&d.image, &spec, &OutputMetadata::default()).unwrap().bytes, &ReadOptions::default()).unwrap();
        assert_eq!(again.image, d.image, "{file}");
        checked += 1;
    }
    assert!(checked > 120, "{checked}");
}

#[test]
fn tiff_variants_from_libtiff_decode_identically() {
    let Some(files) = set("tiff") else {
        skip!(Corpus, "the TIFF fixtures are missing: run `cargo xtask corpus`");
        return;
    };
    // seq-<layout>-<variant>-<crc>.tiff: every variant of one picture carries its CRC in the name.
    let mut groups: BTreeMap<String, Vec<(String, AnyImage)>> = BTreeMap::new();
    for path in &files {
        let file = name(path);
        let d = read(&bytes(path), &ReadOptions::default()).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert_eq!(d.info.format, Format::Tiff);
        // Each one comes back from FloraStack's TIFF writer exactly too, in every compression.
        for compression in [TiffCompression::None, TiffCompression::Lzw, TiffCompression::Deflate] {
            let depth = match d.image {
                AnyImage::U8(_) => florastack_codecs::BitDepth::U8,
                AnyImage::U16(_) => florastack_codecs::BitDepth::U16,
                AnyImage::F32(_) => florastack_codecs::BitDepth::F32,
            };
            let spec = OutputSpec { format: OutputFormat::Tiff { compression }, depth, profile: OutputProfile::Input, extended_range: true };
            let again = read(&write(&d.image, &spec, &OutputMetadata::default()).unwrap().bytes, &ReadOptions::default()).unwrap();
            assert_eq!(again.image, d.image, "{file} {compression:?}");
        }
        if let Some(stem) = file.strip_prefix("seq-").and_then(|f| f.strip_suffix(".tiff")) {
            let crc = stem.rsplit('-').next().unwrap().to_owned();
            // The associated-alpha variant stores the same bytes premultiplied: FloraStack reads it as straight alpha.
            let key = if file.contains("-rgba-") && !file.contains("-unassoc-") { format!("{crc} (associated)") } else { crc };
            groups.entry(key).or_default().push((file.clone(), d.image));
        }
    }
    assert!(groups.values().any(|g| g.len() >= 5), "the 16-bit gray group has its variants");
    for (crc, members) in &groups {
        for (file, image) in members {
            assert_eq!(image, &members[0].1, "{file} decodes like {} ({crc})", members[0].0);
        }
    }
    // Floating-point predictor files, and half floats read as 32-bit floats.
    for file in ["predictor-3-rgb-f32.tif", "predictor-3-gray-f32.tif", "random-fp16-pred3.tiff"] {
        let d = read(&bytes(&files.iter().find(|p| name(p) == file).unwrap().clone()), &ReadOptions::default()).unwrap();
        assert!(d.info.float && matches!(d.image, AnyImage::F32(_)), "{file}");
    }
}

#[test]
fn webp_files_decode_like_their_reference_pngs() {
    let Some(files) = set("webp") else {
        skip!(Corpus, "the WebP fixtures are missing: run `cargo xtask corpus`");
        return;
    };
    let find = |n: &str| files.iter().find(|p| name(p) == n).unwrap_or_else(|| panic!("{n}"));
    for (webp, reference, lossy) in [
        ("random_lossless.webp", "random_lossless-1.png", false),
        ("lossless_indexed_1bit_palette.webp", "lossless_indexed_1bit_palette.png", false),
        ("lossless_indexed_2bit_palette.webp", "lossless_indexed_2bit_palette.png", false),
        ("lossless_indexed_4bit_palette.webp", "lossless_indexed_4bit_palette.png", false),
        ("random_lossy.webp", "random_lossy-1.png", true),
    ] {
        let w = read(&bytes(find(webp)), &ReadOptions::default()).unwrap();
        let r = read(&bytes(find(reference)), &ReadOptions::default()).unwrap();
        assert_eq!(w.info.format, Format::WebP);
        let (a, b) = (w.image.as_image::<u8>().unwrap(), r.image.as_image::<u8>().unwrap());
        assert_eq!((a.width(), a.height()), (b.width(), b.height()), "{webp}");
        // The references are RGB or RGBA as the converter wrote them: compare the color channels.
        let color = |img: &florastack_core::Image<u8>| -> Vec<u8> {
            img.rows().flat_map(|row| row.chunks(img.channels()).flat_map(|px| px[..3].to_vec())).collect()
        };
        let diff = color(a).iter().zip(color(b).iter()).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        // libwebp's own decoder made the lossy reference; image-webp matches it to rounding.
        assert!(diff <= if lossy { 3 } else { 0 }, "{webp}: {diff}");
    }
}

#[test]
fn openexr_samples_decode_and_damaged_files_fail_cleanly() {
    let Some(files) = set("openexr") else {
        skip!(Corpus, "the OpenEXR fixtures are missing: run `cargo xtask corpus`");
        return;
    };
    let find = |n: &str| files.iter().find(|p| name(p) == n).unwrap_or_else(|| panic!("{n}"));
    let gray = read(&bytes(find("GrayRampsHorizontal.exr")), &ReadOptions::default()).unwrap();
    assert_eq!((gray.info.channels, gray.info.float), (1, true));
    let halves = read(&bytes(find("AllHalfValues.exr")), &ReadOptions::default()).unwrap();
    assert_eq!(halves.info.bits_per_sample, 16);
    let values = halves.image.as_image::<f32>().unwrap().as_raw();
    assert!(values.iter().any(|v| v.is_nan()) && values.iter().any(|v| v.is_infinite()), "non-finite halves survive");
    let xyz = read(&bytes(find("XYZ.exr")), &ReadOptions::default()).unwrap();
    assert!(xyz.profile.is_some(), "XYZ primaries need a profile");
    assert!(xyz.profile.unwrap().named.is_none());
    let rec709 = read(&bytes(find("Rec709.exr")), &ReadOptions::default()).unwrap();
    assert!(rec709.profile.is_none(), "sRGB primaries need none");
    let mut damaged = 0;
    for path in files.iter().filter(|p| name(p).starts_with("damaged-")) {
        // Fuzzer-found crash cases for OpenEXR's C++ library: an error, or an image, but never a panic or a hang.
        let _ = read(&bytes(path), &ReadOptions::default());
        damaged += 1;
    }
    assert!(damaged >= 5, "{damaged}");
}
