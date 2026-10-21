//! OpenEXR, decoded with the exr crate: the first layer with gray (`Y`, optionally `A`) or color (`R`, `G`, `B`,
//! optionally `A`) channels, at its largest resolution level. Half, float and integer channels all become `f32`;
//! the samples are linear light, in the primaries of the `chromaticities` attribute (sRGB's without one). The
//! header is read and checked before any pixel is allocated.

use std::io::Cursor;

use exr::prelude::{FlatSamples, ImageAttributes, LayerAttributes, ReadChannels, ReadLayers, Text};
use florastack_core::{AnyImage, Orientation};

use super::{check_size, image};
use crate::codec::{FileInfo, ImageDecoder, Stored};
use crate::color::{ColorHint, Primaries};
use crate::error::{BackendError, CodecError, guarded};
use crate::format::Format;
use crate::metadata::{ExifSummary, Metadata};

pub(crate) struct ExrDecoder;

const F: Format = Format::OpenExr;

/// The custom attribute FloraStack stores its XMP packet in (OpenEXR has no standard one).
pub(crate) const XMP_ATTRIBUTE: &str = "xmp";

/// An exr crate error: what it doesn't support (DWA and HTJ2K compression, subsampled channels, newer file versions)
/// said so, the rest a damaged file.
fn exr_error(e: exr::error::Error) -> CodecError {
    match e {
        exr::error::Error::NotSupported(_) => {
            CodecError::UnsupportedVariant { format: F, advice: "save it again with ZIP or PIZ compression first", source: e.into() }
        }
        other => CodecError::malformed(F, other),
    }
}

/// The size, channel names and sample bits of the first header.
fn header(bytes: &[u8]) -> Result<(usize, usize, Vec<String>, u8), CodecError> {
    let meta = exr::meta::MetaData::read_from_buffered(Cursor::new(bytes), false).map_err(exr_error)?;
    let header = meta.headers.first().ok_or_else(|| CodecError::malformed(F, BackendError::message("no header")))?;
    if header.deep {
        return Err(CodecError::Unsupported { format: F, what: "deep data" });
    }
    let names: Vec<String> = header.channels.list.iter().map(|c| String::from_utf8_lossy(c.name.bytes()).into_owned()).collect();
    let bits = if header.channels.list.iter().all(|c| c.sample_type == exr::meta::attribute::SampleType::F16) { 16 } else { 32 };
    Ok((header.layer_size.width(), header.layer_size.height(), names, bits))
}

/// The channels to read, by their names' last component: `Y`, `Y A`, `R G B` or `R G B A`, as indices into the list.
fn pick(names: &[String]) -> Result<Vec<usize>, CodecError> {
    let base = |n: &String| n.rsplit('.').next().unwrap_or(n).to_owned();
    let find = |wanted: &str| names.iter().position(|n| base(n) == wanted);
    let alpha = find("A");
    let picked: Vec<usize> = match (find("R"), find("G"), find("B"), find("Y")) {
        (Some(r), Some(g), Some(b), _) => [Some(r), Some(g), Some(b), alpha].into_iter().flatten().collect(),
        (_, _, _, Some(y)) => [Some(y), alpha].into_iter().flatten().collect(),
        // A lone channel of any name (a depth or a mask) reads as gray.
        _ if names.len() == 1 => vec![0],
        _ => return Err(CodecError::Unsupported { format: F, what: "channels other than R, G, B, Y and A" }),
    };
    Ok(picked)
}

#[allow(clippy::cast_precision_loss)] // Integer channels (IDs) become floats; that is what a picture of them means.
fn sample(data: &FlatSamples, i: usize) -> f32 {
    match data {
        FlatSamples::F16(v) => v.get(i).map_or(0.0, |x| x.to_f32()),
        FlatSamples::F32(v) => v.get(i).copied().unwrap_or(0.0),
        FlatSamples::U32(v) => v.get(i).map_or(0.0, |&x| x as f32),
    }
}

/// An ISO speed as a whole number, if it is one that makes sense.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Checked to be in range first.
fn iso(v: f32) -> Option<u32> {
    (v.is_finite() && (0.0..=1e7).contains(&v)).then(|| v.round() as u32)
}

/// The metadata of an OpenEXR file: its XMP packet (in FloraStack's `xmp` attribute), and without one a summary
/// from the standard attributes.
fn metadata(layer: &LayerAttributes, image: &ImageAttributes) -> Metadata {
    let custom = |name: &str| {
        let key = Text::new_or_none(name)?;
        match layer.other.get(&key).or_else(|| image.other.get(&key))? {
            exr::meta::attribute::AttributeValue::Text(t) => Some(text(t)),
            _ => None,
        }
    };
    let metadata = Metadata::from_parts(None, custom(XMP_ATTRIBUTE), None);
    if metadata.summary() != &ExifSummary::default() {
        return metadata;
    }
    metadata.with_summary(ExifSummary {
        software: layer.software_name.as_ref().map(text),
        capture_time: layer.capture_date.as_ref().map(text),
        exposure_time: layer.exposure.map(f64::from),
        f_number: layer.aperture.map(f64::from),
        iso: layer.iso_speed.and_then(iso),
        artist: layer.owner.as_ref().map(text),
        caption: layer.comments.as_ref().map(text),
        ..ExifSummary::default()
    })
}

/// The metadata of an OpenEXR file from its first header alone.
pub(crate) fn read_metadata(bytes: &[u8]) -> Option<Metadata> {
    let meta = std::panic::catch_unwind(|| exr::meta::MetaData::read_from_buffered(Cursor::new(bytes), false).ok()).ok().flatten()?;
    let header = meta.headers.first()?;
    Some(metadata(&header.own_attributes, &header.shared_attributes))
}

fn text(t: &Text) -> String {
    String::from_utf8_lossy(t.bytes()).into_owned()
}

impl ImageDecoder for ExrDecoder {
    fn format(&self) -> Format {
        F
    }

    fn probe(&self, bytes: &[u8]) -> Result<FileInfo, CodecError> {
        guarded(
            || {
                let (width, height, names, bits) = header(bytes)?;
                let channels = pick(&names)?.len();
                Ok(FileInfo {
                    format: F,
                    width,
                    height,
                    channels,
                    bits_per_sample: bits,
                    float: true,
                    alpha: channels == 2 || channels == 4,
                    orientation: Orientation::Normal,
                })
            },
            F,
        )
    }

    fn decode(&self, bytes: &[u8], max_pixels: u64) -> Result<Stored, CodecError> {
        guarded(|| decode(bytes, max_pixels), F)
    }
}

fn decode(bytes: &[u8], max_pixels: u64) -> Result<Stored, CodecError> {
    let (width, height, _, bits) = header(bytes)?;
    check_size(width as u64, height as u64, max_pixels)?;
    let read = exr::prelude::read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .from_buffered(Cursor::new(bytes))
        .map_err(exr_error)?;
    let layer = &read.layer_data;
    let list = &layer.channel_data.list;
    let names: Vec<String> = list.iter().map(|c| text(&c.name)).collect();
    let picked = pick(&names)?;
    let (w, h) = (layer.size.width(), layer.size.height());
    let pixels = w.checked_mul(h).ok_or(CodecError::TooLarge { width: w as u64, height: h as u64, max: max_pixels })?;
    let channels = picked.len();
    let mut samples = Vec::new();
    samples.try_reserve_exact(pixels.saturating_mul(channels)).map_err(|_| CodecError::TooLarge {
        width: w as u64,
        height: h as u64,
        max: max_pixels,
    })?;
    for i in 0..pixels {
        for &c in &picked {
            samples.push(list.get(c).map_or(0.0, |ch| sample(&ch.sample_data, i)));
        }
    }
    let image = AnyImage::F32(image(u32::try_from(w).unwrap_or(u32::MAX), u32::try_from(h).unwrap_or(u32::MAX), channels, samples)?);
    let primaries = read.attributes.chromaticities.map(|c| Primaries {
        red: (f64::from(c.red.x()), f64::from(c.red.y())),
        green: (f64::from(c.green.x()), f64::from(c.green.y())),
        blue: (f64::from(c.blue.x()), f64::from(c.blue.y())),
        white: (f64::from(c.white.x()), f64::from(c.white.y())),
    });
    let metadata = metadata(&layer.attributes, &read.attributes);
    let info = FileInfo {
        format: F,
        width: w,
        height: h,
        channels,
        bits_per_sample: bits,
        float: true,
        alpha: channels == 2 || channels == 4,
        orientation: Orientation::Normal,
    };
    Ok(Stored { image, info, hint: ColorHint::Linear { primaries }, metadata, warnings: Vec::new() })
}

#[cfg(test)]
mod tests {
    use florastack_core::{ColorSpace, Image};

    use super::*;
    use crate::{BitDepth, ExrCompression, OutputFormat, OutputMetadata, OutputProfile, OutputSpec, ReadOptions};

    #[test]
    fn a_compression_exr_lacks_is_unsupported_not_damaged() {
        // A ZIP file relabeled as DWAA (compression 8), which the exr crate can't decompress. One flat gray block of
        // 16 rows compresses below its raw size, so the reader must decompress it.
        let gray = AnyImage::F32(Image::from_vec(16, 16, 3, ColorSpace::LINEAR, vec![0.5; 16 * 16 * 3]).unwrap());
        let spec = OutputSpec {
            format: OutputFormat::OpenExr { compression: ExrCompression::Zip, half: false },
            depth: BitDepth::F32,
            profile: OutputProfile::Input,
            extended_range: false,
        };
        let mut bytes = crate::write(&gray, &spec, &OutputMetadata::default()).unwrap().bytes;
        let attribute = b"compression\0compression\0\x01\0\0\0\x03";
        let at = bytes.windows(attribute.len()).position(|w| w == attribute).expect("the compression attribute, ZIP16");
        bytes[at + attribute.len() - 1] = 8;
        let error = crate::read(&bytes, &ReadOptions::default()).unwrap_err();
        assert!(matches!(error, CodecError::UnsupportedVariant { format: Format::OpenExr, .. }), "{error:?}");
        assert_eq!(error.to_string(), "this kind of OpenEXR file is not supported: save it again with ZIP or PIZ compression first");
        assert!(matches!(exr_error(exr::error::Error::Invalid("bad".into())), CodecError::Malformed { format: Format::OpenExr, .. }));
    }
}
