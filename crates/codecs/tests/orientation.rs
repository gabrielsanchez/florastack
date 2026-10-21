//! Orientation: each of the eight EXIF orientations, recorded in JPEG, TIFF, PNG and WebP files, applied, ignored
//! or forced as the read options say; and outputs always written upright.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{camera_exif, max_diff_u8, pattern_u8, spec};
use florastack_codecs::{BitDepth, OrientationPolicy, OutputFormat, OutputMetadata, ReadOptions, TiffCompression, read, write};
use florastack_core::{AnyImage, ColorSpace, Image, Orientation};
use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples};
use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value, tags as t};

/// An upright picture of flat 8x8 blocks, so that even JPEG keeps each block's color: 5 blocks across, 3 down.
fn upright() -> Image<u8> {
    Image::from_fn(40, 24, 3, ColorSpace::SRGB, |x, y, c| {
        let block = (x / 8) + 5 * (y / 8);
        u8::try_from((block * 17 + c * 80) % 256).unwrap()
    })
    .unwrap()
}

fn dims(image: &Image<u8>) -> (u32, u32) {
    (u32::try_from(image.width()).unwrap(), u32::try_from(image.height()).unwrap())
}

/// A file of `stored` that records `orientation`, in `format`.
fn file(format: &str, stored: &Image<u8>, orientation: Orientation) -> Vec<u8> {
    let exif = camera_exif(orientation.to_exif());
    let (w, h) = dims(stored);
    let input = EncodeImage::new(w, h, 3, Samples::U8(stored.as_raw()));
    let meta = EncodeMeta { exif: Some(&exif), ..EncodeMeta::default() };
    match format {
        "jpeg" => lightcraft_codecs::encode_jpeg(&input, 100, ChromaSubsampling::S444, &meta).unwrap(),
        "png" => lightcraft_codecs::encode_png(&input, &meta).unwrap(),
        "webp" => lightcraft_codecs::encode_webp_lossless(&input, &meta).unwrap(),
        "tiff" => {
            // TIFF records its orientation in its own IFD0.
            let mut ifd = IfdBuilder::new();
            ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
            ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
            ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![8; 3]));
            ifd.set(t::COMPRESSION, Value::Short(vec![1]));
            ifd.set(t::PHOTOMETRIC, Value::Short(vec![2]));
            ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
            ifd.set(t::ORIENTATION, Value::Short(vec![orientation.to_exif()]));
            ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![stored.as_raw().to_vec()] });
            TiffWriter::new(ByteOrder::Little, false).write(&[ifd]).unwrap()
        }
        other => panic!("{other}"),
    }
}

fn pixels(decoded: &florastack_codecs::Decoded) -> Image<u8> {
    decoded.image.as_image::<u8>().unwrap().clone()
}

#[test]
fn every_orientation_is_applied_ignored_or_forced() {
    let upright = upright();
    for format in ["jpeg", "png", "webp", "tiff"] {
        // JPEG keeps flat blocks to within a few levels; the others exactly.
        let tolerance = if format == "jpeg" { 6 } else { 0 };
        for o in Orientation::ALL {
            let stored = upright.oriented(o.inverse()).unwrap();
            let bytes = file(format, &stored, o);
            let applied = read(&bytes, &ReadOptions::default()).unwrap();
            assert_eq!((applied.info.orientation, applied.orientation_applied), (o, o), "{format} {o:?}");
            assert!(max_diff_u8(&pixels(&applied), &upright) <= tolerance, "{format} {o:?}: upright");
            let ignored = read(&bytes, &ReadOptions { orientation: OrientationPolicy::Ignore, ..ReadOptions::default() }).unwrap();
            assert_eq!((ignored.info.orientation, ignored.orientation_applied), (o, Orientation::Normal));
            assert!(max_diff_u8(&pixels(&ignored), &stored) <= tolerance, "{format} {o:?}: as stored");
            let forced =
                read(&bytes, &ReadOptions { orientation: OrientationPolicy::Force(Orientation::Rotate90), ..ReadOptions::default() }).unwrap();
            assert_eq!(forced.orientation_applied, Orientation::Rotate90);
            assert!(max_diff_u8(&pixels(&forced), &stored.oriented(Orientation::Rotate90).unwrap()) <= tolerance, "{format} {o:?}: forced");
        }
    }
}

#[test]
fn a_vertical_rig_s_random_orientations_can_be_ignored() {
    // Pointing down, the camera's gravity sensor records a different orientation frame by frame.
    let frame = pattern_u8(40, 24, 3);
    let tags = [Orientation::Normal, Orientation::Rotate90, Orientation::Rotate270, Orientation::Rotate180, Orientation::Rotate90];
    let ignore = ReadOptions { orientation: OrientationPolicy::Ignore, ..ReadOptions::default() };
    for o in tags {
        let decoded = read(&file("png", &frame, o), &ignore).unwrap();
        assert_eq!(pixels(&decoded), frame, "{o:?}: every frame as stored, the same way up");
    }
    // Honoring the tags would turn the stack's frames against each other.
    let widths: Vec<usize> = tags.iter().map(|&o| read(&file("png", &frame, o), &ReadOptions::default()).unwrap().image.width()).collect();
    assert!(widths.contains(&24) && widths.contains(&40), "{widths:?}");
}

#[test]
fn outputs_are_written_upright() {
    let upright = upright();
    let stored = upright.oriented(Orientation::Rotate90.inverse()).unwrap();
    let decoded = read(&file("png", &stored, Orientation::Rotate90), &ReadOptions::default()).unwrap();
    let out = OutputMetadata { source: Some(decoded.metadata.clone()), ..OutputMetadata::default() };
    let encoded = write(&decoded.image, &spec(OutputFormat::Tiff { compression: TiffCompression::Lzw }, BitDepth::U8), &out).unwrap();
    let again = read(&encoded.bytes, &ReadOptions::default()).unwrap();
    assert_eq!(again.info.orientation, Orientation::Normal, "the TIFF tag and the EXIF both say 1");
    assert_eq!(again.metadata.orientation(), Some(Orientation::Normal));
    assert_eq!(again.image, AnyImage::U8(upright));
}
