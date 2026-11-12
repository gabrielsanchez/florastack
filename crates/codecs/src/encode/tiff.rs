//! TIFF, built from lightcraft-tiff's IFDs: 8, 16 or 32-bit float samples, gray, gray and alpha (with
//! `ExtraSamples` 2, unassociated), RGB or RGBA, in strips that are uncompressed or LZW or Deflate compressed with a
//! predictor ([`super::strips`]). IFD0 carries the source frame's descriptive EXIF tags with FloraStack's software
//! tag, the ICC profile (34675), the XMP packet (700) and the IPTC stream (33723), and points to the Exif and GPS
//! IFDs.
//!
//! The file is written a band of rows at a time ([`super::container`]): the strips first, as they are compressed,
//! then the IFDs. Whether it is classic TIFF or BigTIFF is settled before the first strip, from the most its image
//! data can take: an image whose samples could pass 4 GiB is BigTIFF, even if it then compresses to less.

use std::io::Cursor;

use florastack_core::AnyImage;
use lightcraft_tiff::{IfdBuilder, Value, tags as t};

use super::container::{Chunks, Container, Layout};
use super::strips;
use crate::codec::{Encoded, ImageEncoder};
use crate::color::Profile;
use crate::error::{BackendError, CodecError, Warning};
use crate::format::Format;
use crate::metadata::PreparedMetadata;
use crate::options::{BitDepth, OutputFormat, OutputSpec, TiffCompression};
use crate::stream::{self, Bands, Sink, WholeBands, Written};

pub(crate) struct TiffEncoder;

const F: Format = Format::Tiff;

/// The metadata entries of a TIFF-structured file's IFD0, and its Exif and GPS IFDs: what the TIFF and DNG writers
/// share. `icc` is the profile to embed (DNG has none: its color tags say the same).
pub(crate) fn add_metadata(ifd: &mut IfdBuilder, metadata: &PreparedMetadata, icc: Option<&Profile>, warnings: &mut Vec<Warning>) {
    if let Some(parts) = metadata.exif_parts() {
        for (tag, value) in parts.ifd0 {
            ifd.entries.entry(tag).or_insert(value);
        }
        if let Some(exif) = parts.exif {
            ifd.set_child(t::EXIF_IFD, exif);
        }
        if let Some(gps) = parts.gps {
            ifd.set_child(t::GPS_IFD, gps);
        }
    }
    ifd.set(t::SOFTWARE, Value::Ascii(metadata.software().to_owned()));
    ifd.set(t::ORIENTATION, Value::Short(vec![1]));
    for (tag, value) in [
        (t::X_RESOLUTION, Value::Rational(vec![(300, 1)])),
        (t::Y_RESOLUTION, Value::Rational(vec![(300, 1)])),
        (t::RESOLUTION_UNIT, Value::Short(vec![2])),
    ] {
        ifd.entries.entry(tag).or_insert(value);
    }
    if let Some(profile) = icc {
        ifd.set(t::ICC_PROFILE, Value::Undefined(profile.bytes().to_vec()));
    }
    if let Some(xmp) = metadata.xmp(None, warnings) {
        ifd.set(t::XMP, Value::Byte(xmp.into_bytes()));
    }
    if let Some(iptc) = metadata.iptc() {
        ifd.set(t::IPTC_NAA, Value::Undefined(iptc.to_vec()));
    }
}

/// The most `raw` bytes of samples can take once compressed: what decides between classic TIFF and BigTIFF before
/// any strip is written. Deflate stores what it can't shrink, with a few bytes for each block; LZW's codes grow to
/// 12 bits for every byte of data it finds no pattern in.
pub(crate) fn worst_case(raw: u64, compression: TiffCompression) -> u64 {
    match compression {
        TiffCompression::None => raw,
        TiffCompression::Deflate => raw.saturating_add(raw / 64).saturating_add(1 << 16),
        TiffCompression::Lzw => raw.saturating_add(raw / 2).saturating_add(1 << 16),
    }
}

/// The band's rows as compressed strips of `rows` rows each.
pub(crate) fn band_strips(band: &AnyImage, rows: usize, compression: TiffCompression, format: Format) -> Result<Vec<Vec<u8>>, CodecError> {
    match band {
        AnyImage::U8(i) => strips::strips(i, rows, compression, format),
        AnyImage::U16(i) => strips::strips(i, rows, compression, format),
        AnyImage::F32(i) => strips::strips(i, rows, compression, format),
    }
}

impl ImageEncoder for TiffEncoder {
    fn format(&self) -> Format {
        F
    }

    fn encode(&self, image: &AnyImage, profile: Option<&Profile>, metadata: &PreparedMetadata, spec: &OutputSpec) -> Result<Encoded, CodecError> {
        let mut out = Cursor::new(Vec::new());
        let written = self.encode_to(&mut out, &mut WholeBands(image), profile, metadata, spec)?;
        Ok(Encoded { bytes: out.into_inner(), warnings: written.warnings })
    }

    fn streams(&self) -> bool {
        true
    }

    fn encode_to(
        &self,
        sink: &mut dyn Sink,
        bands: &mut dyn Bands,
        profile: Option<&Profile>,
        metadata: &PreparedMetadata,
        spec: &OutputSpec,
    ) -> Result<Written, CodecError> {
        let OutputFormat::Tiff { compression } = spec.format else {
            return Err(CodecError::encode(F, BackendError::message("not TIFF options")));
        };
        let (w, h, c, depth) = (bands.width(), bands.height(), bands.channels(), bands.depth());
        let (bits, float, size) = match depth {
            BitDepth::U8 => (8, false, 1),
            BitDepth::U16 => (16, false, 2),
            BitDepth::F32 => (32, true, 4),
        };
        let row_bytes = w
            .checked_mul(c)
            .and_then(|n| n.checked_mul(size))
            .ok_or_else(|| CodecError::encode(F, BackendError::message("the image is too wide")))?;
        let rows = strips::rows_per_strip(row_bytes, h);
        let raw = (row_bytes as u64).saturating_mul(h as u64);
        let mut container = Container::new(sink, worst_case(raw, compression), F)?;
        let mut chunks = Chunks::default();
        let band_rows = stream::band_rows(row_bytes, rows);
        let mut y = 0;
        while y < h {
            let n = band_rows.min(h - y);
            let band = bands.rows(y, n)?;
            stream::check_band(&band, w, n, c, depth, F)?;
            for strip in band_strips(&band, rows, compression, F)? {
                container.chunk(&strip, &mut chunks)?;
            }
            y += n;
        }
        let mut ifd = IfdBuilder::new();
        let samples = u16::try_from(c).unwrap_or(4);
        ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![super::dim32(w)]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![super::dim32(h)]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![bits; c]));
        ifd.set(t::COMPRESSION, Value::Short(vec![strips::compression_tag(compression)]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![if c >= 3 { 2 } else { 1 }]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![samples]));
        ifd.set(t::PLANAR_CONFIGURATION, Value::Short(vec![1]));
        let predictor = strips::predictor_for(compression, float);
        if predictor != 1 {
            ifd.set(t::PREDICTOR, Value::Short(vec![predictor]));
        }
        ifd.set(t::SAMPLE_FORMAT, Value::Short(vec![if float { 3 } else { 1 }; c]));
        if c == 2 || c == 4 {
            ifd.set(t::EXTRA_SAMPLES, Value::Short(vec![2]));
        }
        container.place(&mut ifd, Layout::Strips { rows: super::dim32(rows) }, chunks)?;
        let mut warnings = Vec::new();
        add_metadata(&mut ifd, metadata, profile, &mut warnings);
        let (_, bytes) = container.finish(&[ifd])?;
        Ok(Written { bytes, warnings })
    }
}
