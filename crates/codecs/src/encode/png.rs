//! PNG, written by LightCraft's encoder (the png crate): 8 or 16 bits, gray, gray and alpha, RGB or RGBA, with the
//! ICC profile in `iCCP`, the EXIF in `eXIf` and the XMP in an `iTXt` chunk. PNG has no IPTC chunk; its XMP carries
//! the same fields.

use florastack_core::AnyImage;
use lightcraft_codecs::{EncodeImage, EncodeMeta, Samples};

use super::{dim32, packed};
use crate::codec::{Encoded, ImageEncoder};
use crate::color::Profile;
use crate::error::{CodecError, MetadataKind, Warning};
use crate::format::Format;
use crate::metadata::PreparedMetadata;
use crate::options::OutputSpec;

pub(crate) struct PngEncoder;

const F: Format = Format::Png;

impl ImageEncoder for PngEncoder {
    fn format(&self) -> Format {
        F
    }

    fn encode(&self, image: &AnyImage, profile: Option<&Profile>, metadata: &PreparedMetadata, spec: &OutputSpec) -> Result<Encoded, CodecError> {
        let mut warnings = Vec::new();
        if metadata.iptc().is_some() {
            warnings.push(Warning::MetadataNotSupported { format: F, kind: MetadataKind::Iptc });
        }
        let xmp = metadata.xmp(None, &mut warnings);
        let meta = EncodeMeta { icc: profile.map(|p| &p.bytes()[..]), exif: metadata.exif(), xmp: xmp.as_deref(), ppi: None };
        let channels = u8::try_from(image.channels()).unwrap_or(4);
        let (w, h) = (dim32(image.width()), dim32(image.height()));
        let bytes = match image {
            AnyImage::U8(i) => lightcraft_codecs::encode_png(&EncodeImage::new(w, h, channels, Samples::U8(&packed(i))), &meta),
            AnyImage::U16(i) => lightcraft_codecs::encode_png(&EncodeImage::new(w, h, channels, Samples::U16(&packed(i))), &meta),
            AnyImage::F32(_) => return Err(CodecError::Depth { format: F, depth: spec.depth }),
        }
        .map_err(|e| CodecError::encode(F, e))?;
        Ok(Encoded { bytes, warnings })
    }
}
