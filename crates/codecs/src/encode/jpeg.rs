//! JPEG, written by LightCraft's encoder (jpeg-encoder, baseline, with its own parallel encoder for 4:4:4 and
//! 4:2:0): 8-bit gray or RGB at a quality from 1 to 100. The EXIF goes in APP1, the XMP in another APP1 (at most one
//! segment: 65,504 bytes of packet), the ICC profile in APP2 chunks and the IPTC stream in APP13.

use florastack_core::AnyImage;
use lightcraft_codecs::{EncodeImage, EncodeMeta, Samples};

use super::{dim32, first_channels, packed};
use crate::codec::{Encoded, ImageEncoder};
use crate::color::Profile;
use crate::error::{BackendError, CodecError, MetadataKind, Warning};
use crate::format::Format;
use crate::metadata::{PreparedMetadata, iptc};
use crate::options::{ChromaSubsampling, OutputFormat, OutputSpec};

pub(crate) struct JpegEncoder;

const F: Format = Format::Jpeg;

/// The longest XMP packet one APP1 segment holds, after its namespace header.
const XMP_BUDGET: usize = 65_504;
/// The longest EXIF block one APP1 segment holds, after its `Exif\0\0` header.
const EXIF_BUDGET: usize = 65_527;

impl ImageEncoder for JpegEncoder {
    fn format(&self) -> Format {
        F
    }

    fn encode(&self, image: &AnyImage, profile: Option<&Profile>, metadata: &PreparedMetadata, spec: &OutputSpec) -> Result<Encoded, CodecError> {
        let OutputFormat::Jpeg { quality, subsampling } = spec.format else {
            return Err(CodecError::encode(F, BackendError::message("not JPEG options")));
        };
        let AnyImage::U8(image) = image else {
            return Err(CodecError::Depth { format: F, depth: spec.depth });
        };
        let mut warnings = Vec::new();
        let channels = image.channels();
        let color = if channels >= 3 { 3 } else { 1 };
        let samples = if channels == color {
            packed(image).into_owned()
        } else {
            warnings.push(Warning::AlphaDropped(F));
            first_channels(image, color)
        };
        let exif = metadata.exif().filter(|e| {
            let fits = e.len() <= EXIF_BUDGET;
            if !fits {
                warnings.push(Warning::MetadataDropped { kind: MetadataKind::Exif, reason: "it is larger than a JPEG segment holds" });
            }
            fits
        });
        let xmp = metadata.xmp(Some(XMP_BUDGET), &mut warnings);
        let meta = EncodeMeta { icc: profile.map(|p| &p.bytes()[..]), exif, xmp: xmp.as_deref(), ppi: None };
        let subsampling = match subsampling {
            ChromaSubsampling::S444 => lightcraft_codecs::ChromaSubsampling::S444,
            ChromaSubsampling::S422 => lightcraft_codecs::ChromaSubsampling::S422,
            ChromaSubsampling::S420 => lightcraft_codecs::ChromaSubsampling::S420,
        };
        let input = EncodeImage::new(dim32(image.width()), dim32(image.height()), if color == 3 { 3 } else { 1 }, Samples::U8(&samples));
        let mut bytes = lightcraft_codecs::encode_jpeg(&input, quality.clamp(1, 100), subsampling, &meta).map_err(|e| CodecError::encode(F, e))?;
        if let Some(stream) = metadata.iptc() {
            match iptc::jpeg_app13(stream).and_then(|payload| iptc::insert_app_segment(&bytes, 0xed, &payload)) {
                Some(with) => bytes = with,
                None => warnings.push(Warning::MetadataDropped { kind: MetadataKind::Iptc, reason: "it is larger than a JPEG segment holds" }),
            }
        }
        Ok(Encoded { bytes, warnings })
    }
}
