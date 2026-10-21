//! OpenEXR, written with the exr crate: linear light as 32-bit or half floats, in `Y` (and `A`) or `R`, `G`, `B` (and
//! `A`) channels, with lossless compression. The output profile's primaries go in the `chromaticities` attribute
//! (none for sRGB's, which OpenEXR assumes). OpenEXR has no EXIF or IPTC container: the standard attributes take what
//! they can (the capture date, exposure, aperture, ISO speed, owner, comments and software), and the merged XMP
//! packet, with the frame count, is kept in an `xmp` string attribute.

use std::io::Cursor;

use exr::meta::attribute::{AttributeValue, Chromaticities};
use exr::prelude::{
    AnyChannel, AnyChannels, Blocks, Compression, Encoding, FlatSamples, Image, Layer, LayerAttributes, LineOrder, SmallVec, Text, Vec2,
    WritableImage, f16,
};
use florastack_core::AnyImage;

use crate::codec::{Encoded, ImageEncoder};
use crate::color::{self, Primaries, Profile};
use crate::decode::EXR_XMP_ATTRIBUTE;
use crate::error::{BackendError, CodecError, MetadataKind, Warning};
use crate::format::Format;
use crate::metadata::PreparedMetadata;
use crate::options::{ExrCompression, OutputFormat, OutputSpec};

pub(crate) struct ExrEncoder;

const F: Format = Format::OpenExr;

/// A string attribute; OpenEXR strings are bytes, and the exr crate refuses `&str` with characters past Latin-1, so
/// UTF-8 text goes in as its bytes.
fn text(s: &str) -> Text {
    Text::from_slice_unchecked(s.as_bytes())
}

/// `v` as the `f32` OpenEXR attributes store (chromaticities, exposure, aperture and ISO need no more precision).
#[allow(clippy::cast_possible_truncation)]
fn narrow(v: f64) -> f32 {
    v as f32
}

fn chromaticities(p: &Primaries) -> Chromaticities {
    let v = |(x, y): (f64, f64)| Vec2(narrow(x), narrow(y));
    Chromaticities { red: v(p.red), green: v(p.green), blue: v(p.blue), white: v(p.white) }
}

impl ImageEncoder for ExrEncoder {
    fn format(&self) -> Format {
        F
    }

    fn encode(&self, image: &AnyImage, profile: Option<&Profile>, metadata: &PreparedMetadata, spec: &OutputSpec) -> Result<Encoded, CodecError> {
        let OutputFormat::OpenExr { compression, half } = spec.format else {
            return Err(CodecError::encode(F, BackendError::message("not OpenEXR options")));
        };
        let AnyImage::F32(image) = image else {
            return Err(CodecError::Depth { format: F, depth: spec.depth });
        };
        let mut warnings = Vec::new();
        if metadata.iptc().is_some() {
            warnings.push(Warning::MetadataNotSupported { format: F, kind: MetadataKind::Iptc });
        }
        let channels = image.channels();
        let names: &[&str] = match channels {
            1 => &["Y"],
            2 => &["Y", "A"],
            3 => &["R", "G", "B"],
            _ => &["R", "G", "B", "A"],
        };
        let mut list: SmallVec<[AnyChannel<FlatSamples>; 4]> = SmallVec::new();
        for (c, name) in names.iter().enumerate() {
            let plane = image.rows().flat_map(|row| row.chunks_exact(channels).map(move |px| px.get(c).copied().unwrap_or(0.0)));
            let samples = if half { FlatSamples::F16(plane.map(f16::from_f32).collect()) } else { FlatSamples::F32(plane.collect()) };
            list.push(AnyChannel::new(*name, samples));
        }
        let compression = match compression {
            ExrCompression::None => Compression::Uncompressed,
            ExrCompression::Rle => Compression::RLE,
            ExrCompression::Zips => Compression::ZIP1,
            ExrCompression::Zip => Compression::ZIP16,
            ExrCompression::Piz => Compression::PIZ,
        };
        let encoding = Encoding { compression, blocks: Blocks::ScanLines, line_order: LineOrder::Increasing };
        let summary = metadata.summary();
        let mut attributes = LayerAttributes {
            software_name: Some(text(metadata.software())),
            owner: summary.artist.as_deref().map(text),
            comments: summary.caption.as_deref().map(text),
            capture_date: summary.capture_time.as_deref().map(text),
            exposure: summary.exposure_time.map(narrow),
            aperture: summary.f_number.map(narrow),
            iso_speed: summary.iso.map(|v| narrow(f64::from(v))),
            ..LayerAttributes::default()
        };
        if let Some(xmp) = metadata.xmp(None, &mut warnings) {
            attributes.other.insert(text(EXR_XMP_ATTRIBUTE), AttributeValue::Text(text(&xmp)));
        }
        let layer = Layer::new((image.width(), image.height()), attributes, encoding, AnyChannels::sort(list));
        let mut exr_image = Image::from_layer(layer);
        exr_image.attributes.chromaticities = match profile {
            Some(p) => color::named_primaries(p).or_else(|| color::primaries_of(p)).filter(|p| !p.is_srgb()).map(|p| chromaticities(&p)),
            None => None,
        };
        let mut out = Cursor::new(Vec::new());
        exr_image.write().to_buffered(&mut out).map_err(|e| CodecError::encode(F, e))?;
        Ok(Encoded { bytes: out.into_inner(), warnings })
    }
}
