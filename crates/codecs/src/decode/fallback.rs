//! Files whose color only a full color-management system can apply (CMYK JPEG and TIFF files, and profiles built
//! from lookup tables) are decoded by LightCraft's own decoder, which converts them with moxcms to linear light: in
//! Rec. 2020 when it applied the profile, in sRGB when there was none to apply.

use florastack_core::{AnyImage, Orientation};
use lightcraft_codecs::{DecodeOptions, SpaceOrigin};

use super::image;
use crate::codec::{FileInfo, Stored};
use crate::color::{ColorHint, NamedSpace};
use crate::error::{CodecError, guarded};
use crate::format::Format;
use crate::metadata::Metadata;

/// The image as LightCraft decodes it: linear `f32`, RGB (or gray, when the source was gray) with straight alpha
/// when the file has one. The read sets the metadata and orientation from its own parsing.
pub(crate) fn decode(bytes: &[u8], format: Format, max_pixels: u64) -> Result<Stored, CodecError> {
    let options = DecodeOptions { max_size: None, max_pixels };
    let d = guarded(|| lightcraft_codecs::decode(bytes, options).map_err(|e| CodecError::malformed(format, e)), format)?;
    let gray = d.grayscale;
    let alpha = d.alpha.as_ref().filter(|a| a.width == d.image.width && a.height == d.image.height);
    let channels = usize::from(!gray) * 2 + 1 + usize::from(alpha.is_some());
    let pixels = d.image.data.len();
    let mut samples = Vec::new();
    samples.try_reserve_exact(pixels.saturating_mul(channels)).map_err(|_| CodecError::TooLarge {
        width: u64::from(d.width),
        height: u64::from(d.height),
        max: max_pixels,
    })?;
    for (i, rgb) in d.image.data.iter().enumerate() {
        if gray {
            samples.push(rgb[1]);
        } else {
            samples.extend_from_slice(rgb);
        }
        if let Some(a) = alpha {
            samples.push(a.data.get(i).copied().unwrap_or(1.0));
        }
    }
    let image = AnyImage::F32(image(d.width, d.height, channels, samples)?);
    let space = if d.space.origin == SpaceOrigin::IccCms || d.space.named == Some(lightcraft_codecs::NamedSpace::Rec2020) {
        NamedSpace::Rec2020
    } else {
        NamedSpace::Srgb
    };
    let info = FileInfo {
        format,
        width: image.width(),
        height: image.height(),
        channels,
        bits_per_sample: d.bit_depth,
        float: d.float,
        alpha: alpha.is_some(),
        orientation: Orientation::Normal,
    };
    Ok(Stored { image, info, hint: ColorHint::Converted(space), metadata: Metadata::default(), warnings: Vec::new() })
}
