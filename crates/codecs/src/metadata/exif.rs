//! EXIF: TIFF-structured blocks (CIPA DC-008 Exif 2.32), read and rebuilt with lightcraft-tiff.
//!
//! A stacked output carries the EXIF of one source frame, rebuilt rather than copied byte for byte: the tags that
//! describe the scene, the camera and the capture stay; the ones that describe the source file's pixels (its size,
//! compression, thumbnail) or that would be wrong after stacking go; and FloraStack sets its own. The `MakerNote` is
//! dropped: its vendor formats hold offsets into the original block, which a rebuilt block would break.

use std::collections::BTreeMap;

use lightcraft_tiff::tags as t;
use lightcraft_tiff::{ByteOrder, Ifd, IfdBuilder, Tiff, TiffWriter, Value};

/// IFD0 tags that describe the picture rather than the file, kept from the source frame.
const IFD0_KEPT: &[u16] = &[
    t::IMAGE_DESCRIPTION,
    t::MAKE,
    t::MODEL,
    t::X_RESOLUTION,
    t::Y_RESOLUTION,
    t::RESOLUTION_UNIT,
    t::DATE_TIME,
    t::ARTIST,
    316, // HostComputer
    t::COPYRIGHT,
    t::RATING,
    t::RATING_PERCENT,
    // Windows' XPTitle, XPComment, XPAuthor, XPKeywords and XPSubject.
    0x9c9b,
    0x9c9c,
    0x9c9d,
    0x9c9e,
    0x9c9f,
];

/// Exif-IFD tags dropped from the source frame: the `MakerNote` (vendor offsets), the source's own identity and
/// compression, and the pointer to the interoperability IFD (it describes the source file's DCF color space).
const EXIF_DROPPED: &[u16] = &[
    t::MAKER_NOTE,
    t::INTEROP_IFD,
    37122, // CompressedBitsPerPixel
    42016, // ImageUniqueID
    t::PIXEL_X_DIMENSION,
    t::PIXEL_Y_DIMENSION,
    COLOR_SPACE,
];

const COLOR_SPACE: u16 = 40961;
const EXIF_VERSION: u16 = 36864;
/// The interoperability IFD's `InteropIndex` (tag 1): "R98" for sRGB, "R03" for Adobe RGB DCF files.
const INTEROP_INDEX: u16 = 1;

/// What the output's EXIF says about itself.
pub(crate) struct ExifOutput<'a> {
    pub software: &'a str,
    pub width: u32,
    pub height: u32,
    /// Whether the output is sRGB (EXIF `ColorSpace` 1) rather than another space (0xFFFF, uncalibrated).
    pub srgb: bool,
    /// EXIF `DateTime` ("YYYY:MM:DD HH:MM:SS"); without one the source frame's is kept.
    pub modified: Option<&'a str>,
}

/// The entries of `ifd` as a builder, without pointers to other IFDs (which the writer sets from the children).
fn entries(ifd: &Ifd, keep: impl Fn(u16) -> bool) -> BTreeMap<u16, Value> {
    ifd.entries
        .iter()
        .filter(|e| keep(e.tag) && !matches!(e.value, Value::Ifd(_) | Value::Ifd8(_)))
        .filter(|e| !matches!(e.tag, t::EXIF_IFD | t::GPS_IFD | t::INTEROP_IFD | t::SUB_IFDS))
        .map(|e| (e.tag, e.value.clone()))
        .collect()
}

fn builder(entries: BTreeMap<u16, Value>) -> IfdBuilder {
    IfdBuilder { entries, ..IfdBuilder::default() }
}

/// A compact EXIF block from a whole TIFF-structured file (a TIFF, DNG or camera RAW): its IFD0's descriptive tags
/// and its Exif and GPS IFDs, without image data, thumbnails or the `MakerNote`.
pub(crate) fn compact(tiff: &Tiff) -> Option<Vec<u8>> {
    let ifd0 = tiff.ifds.first()?;
    let mut out = builder(entries(ifd0, |tag| IFD0_KEPT.contains(&tag) || tag == t::ORIENTATION || tag == t::SOFTWARE));
    if let Some(exif) = &ifd0.exif {
        out.set_child(t::EXIF_IFD, builder(entries(exif, |tag| tag != t::MAKER_NOTE && tag != t::INTEROP_IFD)));
    }
    if let Some(gps) = &ifd0.gps {
        out.set_child(t::GPS_IFD, builder(entries(gps, |_| true)));
    }
    TiffWriter::new(ByteOrder::Little, false).write(&[out]).ok()
}

/// The output's EXIF block: the source frame's (if it parses), rebuilt as this module's documentation says.
/// Returns the block and whether a source was given that couldn't be read.
pub(crate) fn for_output(source: Option<&[u8]>, out: &ExifOutput<'_>) -> (Option<Vec<u8>>, bool) {
    let parsed = source.map(|bytes| Tiff::parse(lightcraft_meta::strip_exif_header(bytes)).ok());
    let unreadable = matches!(parsed, Some(None));
    let tiff = parsed.flatten();
    let ifd0 = tiff.as_ref().and_then(|t| t.ifds.first());
    let mut root = builder(ifd0.map(|ifd| entries(ifd, |tag| IFD0_KEPT.contains(&tag))).unwrap_or_default());
    root.set(t::ORIENTATION, Value::Short(vec![1]));
    root.set(t::SOFTWARE, Value::Ascii(out.software.to_owned()));
    if let Some(when) = out.modified {
        root.set(t::DATE_TIME, Value::Ascii(when.to_owned()));
    }
    let mut exif = builder(ifd0.and_then(|ifd| ifd.exif.as_deref()).map(|e| entries(e, |tag| !EXIF_DROPPED.contains(&tag))).unwrap_or_default());
    exif.entries.entry(EXIF_VERSION).or_insert_with(|| Value::Undefined(b"0232".to_vec()));
    exif.set(t::PIXEL_X_DIMENSION, Value::Long(vec![out.width]));
    exif.set(t::PIXEL_Y_DIMENSION, Value::Long(vec![out.height]));
    exif.set(COLOR_SPACE, Value::Short(vec![if out.srgb { 1 } else { 0xffff }]));
    root.set_child(t::EXIF_IFD, exif);
    if let Some(gps) = ifd0.and_then(|ifd| ifd.gps.as_deref()) {
        root.set_child(t::GPS_IFD, builder(entries(gps, |_| true)));
    }
    (TiffWriter::new(ByteOrder::Little, false).write(&[root]).ok(), unreadable)
}

/// The parts of a (normalized) EXIF block that a TIFF or DNG writer puts in its own IFD0: the descriptive entries and
/// the Exif and GPS IFDs.
pub(crate) struct Parts {
    pub ifd0: BTreeMap<u16, Value>,
    pub exif: Option<IfdBuilder>,
    pub gps: Option<IfdBuilder>,
}

/// Splits an EXIF block into [`Parts`].
pub(crate) fn parts(block: &[u8]) -> Option<Parts> {
    let tiff = Tiff::parse(block).ok()?;
    let ifd0 = tiff.ifds.first()?;
    Some(Parts {
        ifd0: entries(ifd0, |tag| IFD0_KEPT.contains(&tag) || tag == t::ORIENTATION || tag == t::SOFTWARE),
        exif: ifd0.exif.as_deref().map(|e| builder(entries(e, |_| true))),
        gps: ifd0.gps.as_deref().map(|g| builder(entries(g, |_| true))),
    })
}

/// Whether the EXIF says the file is Adobe RGB without embedding a profile: a DCF "option file", whose Exif
/// `ColorSpace` is 0xFFFF (uncalibrated) and whose interoperability index is "R03" (DCF 2.0).
pub(crate) fn adobe_rgb_hint(block: &[u8]) -> bool {
    let Ok(tiff) = Tiff::parse(lightcraft_meta::strip_exif_header(block)) else { return false };
    let Some(exif) = tiff.ifds.first().and_then(|ifd| ifd.exif.as_deref()) else { return false };
    exif.u16(COLOR_SPACE) == Some(0xffff) && exif.interop.as_deref().and_then(|i| i.string(INTEROP_INDEX)).as_deref() == Some("R03")
}

/// The orientation an EXIF block records, if it records one that is valid.
pub(crate) fn orientation(block: &[u8]) -> Option<florastack_core::Orientation> {
    let tiff = Tiff::parse(lightcraft_meta::strip_exif_header(block)).ok()?;
    florastack_core::Orientation::from_exif(tiff.ifds.first()?.u16(t::ORIENTATION)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera-like EXIF block: IFD0 with orientation 6 and an IFD1 thumbnail pointer, an Exif IFD with a `MakerNote`
    /// and an interoperability IFD, and a GPS IFD.
    fn camera_exif() -> Vec<u8> {
        let mut interop = IfdBuilder::new();
        interop.set(INTEROP_INDEX, Value::Ascii("R03".into()));
        let mut exif = IfdBuilder::new();
        exif.set(t::EXPOSURE_TIME, Value::Rational(vec![(1, 250)]));
        exif.set(t::F_NUMBER, Value::Rational(vec![(56, 10)]));
        exif.set(t::ISO_SPEED, Value::Short(vec![200]));
        exif.set(t::DATE_TIME_ORIGINAL, Value::Ascii("2026:10:20 09:15:00".into()));
        exif.set(t::MAKER_NOTE, Value::Undefined(b"vendor data with offsets".to_vec()));
        exif.set(t::PIXEL_X_DIMENSION, Value::Long(vec![6000]));
        exif.set(COLOR_SPACE, Value::Short(vec![0xffff]));
        exif.set_child(t::INTEROP_IFD, interop);
        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE_REF, Value::Ascii("N".into()));
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(47, 1), (22, 1), (0, 1)]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("Canon".into()));
        ifd0.set(t::MODEL, Value::Ascii("EOS R5".into()));
        ifd0.set(t::ORIENTATION, Value::Short(vec![6]));
        ifd0.set(t::SOFTWARE, Value::Ascii("Firmware 1.8".into()));
        ifd0.set(t::ARTIST, Value::Ascii("A. Photographer".into()));
        ifd0.set(t::IMAGE_WIDTH, Value::Long(vec![6000]));
        ifd0.set_child(t::EXIF_IFD, exif);
        ifd0.set_child(t::GPS_IFD, gps);
        let mut ifd1 = IfdBuilder::new();
        ifd1.set(t::JPEG_INTERCHANGE_FORMAT_LENGTH, Value::Long(vec![0]));
        TiffWriter::new(ByteOrder::Big, false).write(&[ifd0, ifd1]).unwrap()
    }

    #[test]
    fn the_output_block_keeps_the_scene_and_replaces_the_file() {
        let out = ExifOutput { software: "FloraStack 0.1.0", width: 4000, height: 3000, srgb: false, modified: Some("2026:10:21 12:00:00") };
        let (block, unreadable) = for_output(Some(&camera_exif()), &out);
        assert!(!unreadable);
        let tiff = Tiff::parse(&block.unwrap()).unwrap();
        assert_eq!(tiff.ifds.len(), 1, "no thumbnail IFD");
        let ifd0 = &tiff.ifds[0];
        assert_eq!(ifd0.string(t::MAKE).as_deref(), Some("Canon"));
        assert_eq!(ifd0.string(t::ARTIST).as_deref(), Some("A. Photographer"));
        assert_eq!(ifd0.string(t::SOFTWARE).as_deref(), Some("FloraStack 0.1.0"));
        assert_eq!(ifd0.string(t::DATE_TIME).as_deref(), Some("2026:10:21 12:00:00"));
        assert_eq!(ifd0.u16(t::ORIENTATION), Some(1));
        assert!(!ifd0.contains(t::IMAGE_WIDTH), "structure tags go");
        let exif = ifd0.exif.as_deref().unwrap();
        assert_eq!(exif.f64(t::EXPOSURE_TIME), Some(0.004));
        assert_eq!(exif.u32(t::PIXEL_X_DIMENSION), Some(4000));
        assert_eq!(exif.u32(t::PIXEL_Y_DIMENSION), Some(3000));
        assert_eq!(exif.u16(COLOR_SPACE), Some(0xffff));
        assert!(!exif.contains(t::MAKER_NOTE) && exif.interop.is_none());
        assert!(exif.contains(EXIF_VERSION));
        assert!(ifd0.gps.as_deref().unwrap().contains(t::GPS_LATITUDE));
    }

    #[test]
    fn without_a_source_the_block_says_what_florastack_knows() {
        let out = ExifOutput { software: "FloraStack 0.1.0", width: 10, height: 20, srgb: true, modified: None };
        for source in [None, Some(&b"not exif"[..])] {
            let (block, unreadable) = for_output(source, &out);
            assert_eq!(unreadable, source.is_some());
            let tiff = Tiff::parse(&block.unwrap()).unwrap();
            let ifd0 = &tiff.ifds[0];
            assert_eq!(ifd0.string(t::SOFTWARE).as_deref(), Some("FloraStack 0.1.0"));
            assert!(!ifd0.contains(t::DATE_TIME), "no time unless one is given");
            assert_eq!(ifd0.exif.as_deref().unwrap().u16(COLOR_SPACE), Some(1));
        }
    }

    #[test]
    fn hints_orientation_and_compaction() {
        let block = camera_exif();
        assert!(adobe_rgb_hint(&block));
        assert_eq!(orientation(&block), Some(florastack_core::Orientation::Rotate90));
        let mut jpeg_payload = b"Exif\0\0".to_vec();
        jpeg_payload.extend_from_slice(&block);
        assert_eq!(orientation(&jpeg_payload), Some(florastack_core::Orientation::Rotate90), "with the APP1 prefix too");
        let compacted = compact(&Tiff::parse(&block).unwrap()).unwrap();
        let tiff = Tiff::parse(&compacted).unwrap();
        assert_eq!(tiff.ifds[0].u16(t::ORIENTATION), Some(6));
        assert!(!tiff.ifds[0].exif.as_deref().unwrap().contains(t::MAKER_NOTE));
        assert!(!adobe_rgb_hint(&compacted), "the interoperability IFD is gone");
        assert!(!adobe_rgb_hint(b"junk") && orientation(b"junk").is_none());
    }
}
