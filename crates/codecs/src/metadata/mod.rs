//! Metadata: EXIF, IPTC and XMP read from image files ([`Metadata`]), and carried into outputs
//! ([`OutputMetadata`]), copied from a chosen source frame, with the software tag set to FloraStack and the stack's
//! frame count in XMP.
//!
//! The containers are walked by lightcraft-meta (JPEG, PNG, WebP, Canon CR3, Fujifilm RAF) and lightcraft-tiff
//! (TIFF, DNG and the TIFF-based RAW formats); the decoders add what their own libraries find (PNG text chunks,
//! WebP chunks). [`ExifSummary`] is what a project lists for each frame, from EXIF, then IPTC, then XMP, with
//! lightcraft-meta's precedence rules.

pub(crate) mod exif;
pub(crate) mod iptc;
pub(crate) mod xmp;

use std::sync::Arc;

use florastack_core::{Orientation, branding};
use lightcraft_tiff::{Tiff, Value, tags};
use serde::{Deserialize, Serialize};

use crate::error::{MetadataKind, Warning};

/// The metadata of a file: its EXIF block, XMP packet and IPTC-IIM stream, and a summary of what they say.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    exif: Option<Arc<[u8]>>,
    xmp: Option<Arc<str>>,
    iptc: Option<Arc<[u8]>>,
    summary: ExifSummary,
}

impl Metadata {
    /// Metadata from its parts: a TIFF-structured EXIF block (with or without JPEG's `Exif\0\0` prefix), an XMP
    /// packet and an IPTC-IIM stream.
    pub fn from_parts(exif: Option<Vec<u8>>, xmp: Option<String>, iptc: Option<Vec<u8>>) -> Metadata {
        let exif = exif.map(|e| {
            let stripped = lightcraft_meta::strip_exif_header(&e);
            if stripped.len() == e.len() { e } else { stripped.to_vec() }
        });
        let base = exif.as_deref().map(|e| guarded_meta(|| lightcraft_meta::read_exif(e))).unwrap_or_default();
        let summary = summarize(base, iptc.as_deref(), xmp.as_deref());
        Metadata { exif: exif.map(Arc::from), xmp: xmp.map(Arc::from), iptc: iptc.map(Arc::from), summary }
    }

    /// The EXIF block, TIFF-structured (no `Exif\0\0` prefix). For a TIFF-based file, a compact copy of the file's
    /// descriptive tags and its Exif and GPS IFDs.
    pub fn exif(&self) -> Option<&[u8]> {
        self.exif.as_deref()
    }

    /// The XMP packet.
    pub fn xmp(&self) -> Option<&str> {
        self.xmp.as_deref()
    }

    /// The IPTC-IIM stream.
    pub fn iptc(&self) -> Option<&[u8]> {
        self.iptc.as_deref()
    }

    /// What the metadata says about the photo.
    pub fn summary(&self) -> &ExifSummary {
        &self.summary
    }

    /// The orientation the metadata records, if any.
    pub fn orientation(&self) -> Option<Orientation> {
        self.summary.orientation
    }

    /// Whether there is no metadata at all.
    pub fn is_empty(&self) -> bool {
        self.exif.is_none() && self.xmp.is_none() && self.iptc.is_none()
    }

    /// This metadata with another summary, for formats whose own fields say what EXIF would (OpenEXR attributes).
    pub(crate) fn with_summary(mut self, summary: ExifSummary) -> Metadata {
        self.summary = summary;
        self
    }
}

/// What the metadata says about a photo, for listing frames: the camera, the lens, the exposure and the people's
/// fields.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExifSummary {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub software: Option<String>,
    /// When the photo was taken, ISO 8601 (with milliseconds and the UTC offset when the file has them).
    pub capture_time: Option<String>,
    /// Seconds.
    pub exposure_time: Option<f64>,
    pub f_number: Option<f64>,
    pub iso: Option<u32>,
    /// Millimetres.
    pub focal_length: Option<f64>,
    pub focal_length_35mm: Option<f64>,
    #[serde(skip)]
    pub orientation: Option<Orientation>,
    pub artist: Option<String>,
    pub copyright: Option<String>,
    pub title: Option<String>,
    pub caption: Option<String>,
    pub keywords: Vec<String>,
    /// Latitude and longitude in degrees (north and east positive), and altitude in metres.
    pub gps: Option<(f64, f64, Option<f64>)>,
}

/// Runs a lightcraft-meta reader, which never panics by design, behind a panic guard all the same: metadata comes
/// from untrusted files, and a crash would lose work.
fn guarded_meta<T: Default>(f: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_default()
}

/// The summary of EXIF metadata, with IPTC filling gaps and XMP overriding the people's fields and filling the rest
/// (lightcraft-meta's `extract` rules).
fn summarize(mut m: lightcraft_meta::Metadata, iptc: Option<&[u8]>, xmp: Option<&str>) -> ExifSummary {
    if let Some(iptc) = iptc {
        m.fill_missing(&guarded_meta(|| lightcraft_meta::parse_iptc(iptc)));
    }
    if let Some(parsed) = xmp.and_then(|x| guarded_meta(|| lightcraft_meta::parse_xmp(x).ok())) {
        m.overlay_user_fields(&parsed.metadata);
        m.fill_missing(&parsed.metadata);
    }
    let lens = match (&m.lens_make, &m.lens_model) {
        (_, Some(model)) => Some(model.clone()),
        (Some(make), None) => Some(make.clone()),
        (None, None) => m.lens_spec.and_then(lightcraft_meta::lens_spec_name),
    };
    ExifSummary {
        make: m.make,
        model: m.model,
        lens,
        software: m.software,
        capture_time: m.capture_time.map(|t| t.to_iso()),
        exposure_time: m.exposure_time,
        f_number: m.f_number,
        iso: m.iso,
        focal_length: m.focal_length,
        focal_length_35mm: m.focal_length_35mm,
        orientation: m.orientation.and_then(|o| Orientation::from_exif(o.to_exif())),
        artist: m.artist,
        copyright: m.copyright,
        title: m.title,
        caption: m.caption,
        keywords: m.keywords,
        gps: m.gps.map(|g| (g.latitude, g.longitude, g.altitude)),
    }
}

/// Whether `bytes` start like a TIFF-structured file: TIFF, BigTIFF, DNG and most camera RAW formats, including the
/// private magic numbers of Olympus ORF and Panasonic RW2.
fn is_tiff_like(bytes: &[u8]) -> bool {
    [&b"II*\0"[..], b"MM\0*", b"II+\0", b"MM\0+", b"IIRO", b"IIRS", b"MMOR", b"IIU\0"].iter().any(|m| bytes.starts_with(m))
}

/// The metadata of a whole file in any container lightcraft-meta and lightcraft-tiff know (JPEG, PNG, WebP, TIFF,
/// DNG, TIFF-based RAW, CR3, RAF), or OpenEXR. An unknown container has none. For a TIFF-based file, the EXIF kept is a compact
/// copy of its descriptive tags, so that a large file isn't copied.
pub fn read_metadata(bytes: &[u8]) -> Metadata {
    if bytes.starts_with(&[0x76, 0x2f, 0x31, 0x01]) {
        return crate::decode::exr_metadata(bytes).unwrap_or_default();
    }
    if is_tiff_like(bytes) {
        return match guarded_meta(|| Tiff::parse(bytes).ok()) {
            Some(tiff) => from_tiff(&tiff),
            None => Metadata::default(),
        };
    }
    let e = guarded_meta(|| lightcraft_meta::embedded(bytes));
    Metadata::from_parts(e.exif, e.xmp, e.iptc)
}

/// The metadata of a parsed TIFF-structured file: the summary from all of it, the EXIF block compacted.
pub(crate) fn from_tiff(tiff: &Tiff) -> Metadata {
    let Some(ifd0) = tiff.ifds.first() else { return Metadata::default() };
    let xmp = ifd0.bytes(tags::XMP).map(|b| String::from_utf8_lossy(b).trim_end_matches('\0').to_owned());
    let iptc = ifd0.value(tags::IPTC_NAA).map(|v| match v {
        Value::Byte(b) | Value::Undefined(b) => b.clone(),
        // Some writers store the stream as LONGs; its bytes are the same in the file's byte order.
        other => lightcraft_tiff::writer::encode(tiff.order, other),
    });
    let base = guarded_meta(|| lightcraft_meta::from_tiff(tiff));
    let summary = summarize(base, iptc.as_deref(), xmp.as_deref());
    Metadata { exif: exif::compact(tiff).map(Arc::from), xmp: xmp.map(Arc::from), iptc: iptc.map(Arc::from), summary }
}

/// A date and time, for the outputs' EXIF `DateTime` and XMP `ModifyDate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Timestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Timestamp {
    fn is_valid(self) -> bool {
        (1..=9999).contains(&self.year)
            && (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.second < 61
    }

    /// EXIF's form: `2026:10:21 12:30:00`.
    pub fn exif(&self) -> String {
        format!("{:04}:{:02}:{:02} {:02}:{:02}:{:02}", self.year, self.month, self.day, self.hour, self.minute, self.second)
    }

    /// ISO 8601, as XMP writes dates: `2026-10-21T12:30:00`.
    pub fn iso(&self) -> String {
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", self.year, self.month, self.day, self.hour, self.minute, self.second)
    }
}

/// The metadata to write into an output file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputMetadata {
    /// The metadata to carry over: the chosen source frame's.
    pub source: Option<Metadata>,
    /// The number of frames stacked, recorded in XMP.
    pub frame_count: Option<u32>,
    /// When the output was made. Without it, two writes of the same image give the same bytes.
    pub modified: Option<Timestamp>,
}

/// The name and version every output's software tag carries.
pub(crate) fn software() -> String {
    format!("{} {}", branding::DISPLAY_NAME, env!("CARGO_PKG_VERSION"))
}

/// An output's metadata, made ready for its encoder: the EXIF block rebuilt for the output, the XMP merged on
/// demand (each format has its own size limit), the IPTC stream as it was.
#[derive(Clone, Debug)]
pub struct PreparedMetadata {
    exif: Option<Vec<u8>>,
    source_xmp: Option<Arc<str>>,
    iptc: Option<Arc<[u8]>>,
    summary: ExifSummary,
    software: String,
    frame_count: Option<u32>,
    modified: Option<Timestamp>,
    width: u32,
    height: u32,
}

impl PreparedMetadata {
    /// The metadata for an output of `width` x `height`, whose profile is sRGB (`srgb`) or another space.
    pub(crate) fn new(out: &OutputMetadata, width: usize, height: usize, srgb: bool, warnings: &mut Vec<Warning>) -> PreparedMetadata {
        let software = software();
        let (width, height) = (u32::try_from(width).unwrap_or(u32::MAX), u32::try_from(height).unwrap_or(u32::MAX));
        let modified = out.modified.filter(|t| t.is_valid());
        let exif_when = modified.map(|t| t.exif());
        let source = out.source.as_ref();
        let (exif, unreadable) = exif::for_output(
            source.and_then(Metadata::exif),
            &exif::ExifOutput { software: &software, width, height, srgb, modified: exif_when.as_deref() },
        );
        if unreadable {
            warnings.push(Warning::UnreadableMetadata(MetadataKind::Exif));
        }
        PreparedMetadata {
            exif,
            source_xmp: source.and_then(|s| s.xmp.clone()),
            iptc: source.and_then(|s| s.iptc.clone()),
            summary: source.map(|s| s.summary.clone()).unwrap_or_default(),
            software,
            frame_count: out.frame_count,
            modified,
            width,
            height,
        }
    }

    /// The output's EXIF block (TIFF-structured, no `Exif\0\0` prefix): the source frame's descriptive tags and its
    /// Exif and GPS IFDs, with FloraStack's software tag, orientation 1 and the output's size.
    pub fn exif(&self) -> Option<&[u8]> {
        self.exif.as_deref()
    }

    /// The output's XMP packet, at most `budget` bytes long when there is a budget. What had to go is added to
    /// `warnings`.
    pub fn xmp(&self, budget: Option<usize>, warnings: &mut Vec<Warning>) -> Option<String> {
        let modified = self.modified.map(|t| t.iso());
        let out = xmp::XmpOutput {
            creator_tool: &self.software,
            frame_count: self.frame_count,
            modified: modified.as_deref(),
            width: self.width,
            height: self.height,
        };
        xmp::for_output(self.source_xmp.as_deref(), &out, budget, warnings)
    }

    /// The source frame's IPTC-IIM stream.
    pub fn iptc(&self) -> Option<&[u8]> {
        self.iptc.as_deref()
    }

    /// What the source frame's metadata says about the photo.
    pub fn summary(&self) -> &ExifSummary {
        &self.summary
    }

    /// The software tag: FloraStack's name and version.
    pub fn software(&self) -> &str {
        &self.software
    }

    /// The EXIF block split into what a TIFF-structured file puts in its own IFD0.
    pub(crate) fn exif_parts(&self) -> Option<exif::Parts> {
        self.exif.as_deref().and_then(exif::parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_format_for_exif_and_xmp() {
        let t = Timestamp { year: 2026, month: 10, day: 21, hour: 9, minute: 5, second: 0 };
        assert_eq!(t.exif(), "2026:10:21 09:05:00");
        assert_eq!(t.iso(), "2026-10-21T09:05:00");
        assert!(t.is_valid());
        assert!(!Timestamp { month: 13, ..t }.is_valid());
    }

    #[test]
    fn unknown_containers_and_garbage_have_no_metadata() {
        assert!(read_metadata(b"").is_empty());
        assert!(read_metadata(b"not an image at all").is_empty());
        assert!(read_metadata(b"II*\0garbage").is_empty());
        let m = Metadata::from_parts(Some(b"Exif\0\0junk".to_vec()), Some("<not xmp".into()), Some(vec![0x1c, 2]));
        assert_eq!(m.exif(), Some(&b"junk"[..]), "the JPEG prefix goes");
        assert_eq!(m.summary(), &ExifSummary::default());
    }

    #[test]
    fn prepared_metadata_always_names_the_software() {
        let mut w = Vec::new();
        let p = PreparedMetadata::new(&OutputMetadata::default(), 8, 6, true, &mut w);
        assert_eq!(w, []);
        assert!(p.software().starts_with(branding::DISPLAY_NAME));
        let parts = p.exif_parts().unwrap();
        assert_eq!(parts.ifd0.get(&tags::SOFTWARE), Some(&Value::Ascii(p.software().to_owned())));
        let xmp = p.xmp(None, &mut w).unwrap();
        assert!(xmp.contains(p.software()) && !xmp.contains("FrameCount"));
        let with_count = PreparedMetadata::new(&OutputMetadata { frame_count: Some(3), ..OutputMetadata::default() }, 8, 6, true, &mut w);
        assert!(with_count.xmp(None, &mut w).unwrap().contains(&format!("<{p}:FrameCount>3</{p}:FrameCount>", p = branding::XMP_PREFIX)));
    }
}
