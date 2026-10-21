//! TIFF, written with lightcraft-tiff's IFD writer: 8, 16 or 32-bit float samples, gray, gray and alpha (with
//! `ExtraSamples` 2, unassociated), RGB or RGBA, in strips that are uncompressed or LZW or Deflate compressed with a
//! predictor ([`super::strips`]). IFD0 carries the source frame's descriptive EXIF tags with FloraStack's software
//! tag, the ICC profile (34675), the XMP packet (700) and the IPTC stream (33723), and points to the Exif and GPS
//! IFDs. Files past 4 GiB are written as BigTIFF.

use florastack_core::{AnyImage, Image};
use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value, tags as t};

use super::strips::{self, TiffSample};
use crate::codec::{Encoded, ImageEncoder};
use crate::color::Profile;
use crate::error::{BackendError, CodecError, Warning};
use crate::format::Format;
use crate::metadata::PreparedMetadata;
use crate::options::{OutputFormat, OutputSpec, TiffCompression};

pub(crate) struct TiffEncoder;

const F: Format = Format::Tiff;

/// Classic TIFF's 32-bit offsets reach 4 GiB; past this much image data (leaving room for the IFDs), BigTIFF.
const CLASSIC_LIMIT: u64 = (1 << 32) - (64 << 20);

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

/// Serializes `ifd` as a TIFF stream, as BigTIFF when its data is too large for classic TIFF.
pub(crate) fn write(ifd: IfdBuilder, data_bytes: u64, format: Format) -> Result<Vec<u8>, CodecError> {
    let bigtiff = data_bytes > CLASSIC_LIMIT;
    TiffWriter::new(ByteOrder::Little, bigtiff).write(&[ifd]).map_err(|e| CodecError::encode(format, e))
}

fn ifd_for<T: TiffSample>(image: &Image<T>, compression: TiffCompression) -> Result<(IfdBuilder, u64), CodecError> {
    let (w, h, c) = (image.width(), image.height(), image.channels());
    let row_bytes = w
        .checked_mul(c)
        .and_then(|n| n.checked_mul(std::mem::size_of::<T>()))
        .ok_or_else(|| CodecError::encode(F, BackendError::message("the image is too wide")))?;
    let rows = strips::rows_per_strip(row_bytes, h);
    let data = strips::strips(image, rows, compression, F)?;
    let data_bytes: u64 = data.iter().map(|s| s.len() as u64).sum();
    let mut ifd = IfdBuilder::new();
    let samples = u16::try_from(c).unwrap_or(4);
    ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
    ifd.set(t::IMAGE_WIDTH, Value::Long(vec![super::dim32(w)]));
    ifd.set(t::IMAGE_LENGTH, Value::Long(vec![super::dim32(h)]));
    ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![T::BITS; c]));
    ifd.set(t::COMPRESSION, Value::Short(vec![strips::compression_tag(compression)]));
    ifd.set(t::PHOTOMETRIC, Value::Short(vec![if c >= 3 { 2 } else { 1 }]));
    ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![samples]));
    ifd.set(t::PLANAR_CONFIGURATION, Value::Short(vec![1]));
    let predictor = strips::predictor::<T>(compression);
    if predictor != 1 {
        ifd.set(t::PREDICTOR, Value::Short(vec![predictor]));
    }
    ifd.set(t::SAMPLE_FORMAT, Value::Short(vec![if T::FLOAT { 3 } else { 1 }; c]));
    if c == 2 || c == 4 {
        ifd.set(t::EXTRA_SAMPLES, Value::Short(vec![2]));
    }
    ifd.set_image(ImageData::Strips { rows_per_strip: super::dim32(rows), strips: data });
    Ok((ifd, data_bytes))
}

impl ImageEncoder for TiffEncoder {
    fn format(&self) -> Format {
        F
    }

    fn encode(&self, image: &AnyImage, profile: Option<&Profile>, metadata: &PreparedMetadata, spec: &OutputSpec) -> Result<Encoded, CodecError> {
        let OutputFormat::Tiff { compression } = spec.format else {
            return Err(CodecError::encode(F, BackendError::message("not TIFF options")));
        };
        let (mut ifd, data_bytes) = match image {
            AnyImage::U8(i) => ifd_for(i, compression)?,
            AnyImage::U16(i) => ifd_for(i, compression)?,
            AnyImage::F32(i) => ifd_for(i, compression)?,
        };
        let mut warnings = Vec::new();
        add_metadata(&mut ifd, metadata, profile, &mut warnings);
        let bytes = write(ifd, data_bytes, F)?;
        Ok(Encoded { bytes, warnings })
    }
}
