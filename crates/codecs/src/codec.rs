//! The traits every format implements ([`ImageDecoder`], [`ImageEncoder`]), what they produce, and [`Codecs`], the
//! registry that runs the shared steps of reading (color tagging, orientation) and writing (profile conversion,
//! metadata) around them.

use std::sync::OnceLock;

use florastack_core::{AnyImage, Orientation};

use crate::color::{self, ColorEngine, ColorHint, CurveKind, LightCraftColor, NamedSpace, Profile};
use crate::error::{CodecError, Warning};
use crate::format::{Format, sniff};
use crate::metadata::{Metadata, OutputMetadata, PreparedMetadata};
use crate::options::{OrientationPolicy, OutputSpec, ReadOptions};
use crate::{decode, encode};

/// What a file holds, as its headers say.
#[derive(Clone, Debug, PartialEq)]
pub struct FileInfo {
    pub format: Format,
    /// The stored size, before any orientation was applied.
    pub width: usize,
    pub height: usize,
    /// Channels after palettes and transparency chunks are expanded: 1 gray, 2 gray and alpha, 3 RGB, 4 RGBA.
    pub channels: usize,
    /// Bits per sample as stored (1 to 64).
    pub bits_per_sample: u8,
    /// Whether the samples are floating point.
    pub float: bool,
    pub alpha: bool,
    /// The orientation the file records ([`Orientation::Normal`] when it records none).
    pub orientation: Orientation,
}

/// The pixels of a file as stored, with what the file says about their color and its metadata: what an
/// [`ImageDecoder`] produces, before the shared steps of a read.
#[derive(Clone, Debug)]
pub struct Stored {
    /// The samples, with any color space tag: the read replaces it.
    pub image: AnyImage,
    pub info: FileInfo,
    pub hint: ColorHint,
    pub metadata: Metadata,
    pub warnings: Vec<Warning>,
}

/// The color profile a read ended up with.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileInfo {
    /// The profile's name, as it describes itself.
    pub description: Option<String>,
    /// The standard space its primaries match, if one does.
    pub named: Option<NamedSpace>,
    /// The curve the file's samples were encoded with.
    pub curve: CurveKind,
}

/// A file, read: the image, what the file said about it, and its metadata.
#[derive(Clone, Debug)]
pub struct Decoded {
    /// The pixels, upright as the options asked, tagged with their color space.
    pub image: AnyImage,
    pub info: FileInfo,
    /// The profile the image is tagged with, if any.
    pub profile: Option<ProfileInfo>,
    /// Whether the samples were linearized through the profile as they were read (their curve was neither sRGB's
    /// nor linear), so that `image` holds linear `f32` rather than the stored sample type.
    pub linearized: bool,
    /// The orientation the read applied.
    pub orientation_applied: Orientation,
    pub metadata: Metadata,
    pub warnings: Vec<Warning>,
}

/// A file, written.
#[derive(Clone, Debug, PartialEq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub warnings: Vec<Warning>,
}

/// Reads one file format into FloraStack's types.
pub trait ImageDecoder: Send + Sync {
    fn format(&self) -> Format;

    /// What the file holds, from its headers, without decoding the pixels.
    fn probe(&self, bytes: &[u8]) -> Result<FileInfo, CodecError>;

    /// The pixels as stored, refusing images of more than `max_pixels` before allocating them.
    fn decode(&self, bytes: &[u8], max_pixels: u64) -> Result<Stored, CodecError>;
}

/// Writes one file format.
pub trait ImageEncoder: Send + Sync {
    fn format(&self) -> Format;

    /// Writes `image`, whose samples are already in the output's depth and encoded for its profile; `profile` is the
    /// one to embed (none for an untagged file).
    fn encode(&self, image: &AnyImage, profile: Option<&Profile>, metadata: &PreparedMetadata, spec: &OutputSpec) -> Result<Encoded, CodecError>;
}

/// The decoders, encoders and color engine that reading and writing use.
pub struct Codecs {
    decoders: Vec<Box<dyn ImageDecoder>>,
    encoders: Vec<Box<dyn ImageEncoder>>,
    color: Box<dyn ColorEngine>,
}

impl Codecs {
    /// The built-in formats and the LightCraft color engine.
    pub fn builtin() -> Codecs {
        Codecs { decoders: decode::builtin(), encoders: encode::builtin(), color: Box::new(LightCraftColor) }
    }

    /// The registry shared by [`crate::read`] and [`crate::write`].
    pub fn shared() -> &'static Codecs {
        static SHARED: OnceLock<Codecs> = OnceLock::new();
        SHARED.get_or_init(Codecs::builtin)
    }

    /// This registry with `decoder` reading its format instead of the one registered.
    #[must_use]
    pub fn with_decoder(mut self, decoder: Box<dyn ImageDecoder>) -> Codecs {
        self.decoders.retain(|d| d.format() != decoder.format());
        self.decoders.push(decoder);
        self
    }

    /// This registry with `encoder` writing its format instead of the one registered.
    #[must_use]
    pub fn with_encoder(mut self, encoder: Box<dyn ImageEncoder>) -> Codecs {
        self.encoders.retain(|e| e.format() != encoder.format());
        self.encoders.push(encoder);
        self
    }

    /// This registry with another color engine.
    #[must_use]
    pub fn with_color_engine(mut self, color: Box<dyn ColorEngine>) -> Codecs {
        self.color = color;
        self
    }

    pub fn color_engine(&self) -> &dyn ColorEngine {
        self.color.as_ref()
    }

    fn decoder(&self, format: Format) -> Result<&dyn ImageDecoder, CodecError> {
        self.decoders.iter().find(|d| d.format() == format).map(AsRef::as_ref).ok_or(CodecError::UnsupportedFormat(format.name()))
    }

    fn encoder(&self, format: Format) -> Result<&dyn ImageEncoder, CodecError> {
        self.encoders.iter().find(|e| e.format() == format).map(AsRef::as_ref).ok_or(CodecError::UnsupportedFormat(format.name()))
    }

    /// What a file holds, from its headers.
    pub fn probe(&self, bytes: &[u8]) -> Result<FileInfo, CodecError> {
        self.decoder(sniff(bytes)?)?.probe(bytes)
    }

    /// Reads a file: decodes it, tags its samples with their color space (linearizing them when the profile's curve
    /// is neither sRGB's nor linear) and turns them upright as `options` say.
    pub fn read(&self, bytes: &[u8], options: &ReadOptions) -> Result<Decoded, CodecError> {
        let format = sniff(bytes)?;
        let mut stored = self.decoder(format)?.decode(bytes, options.max_pixels)?;
        // A profile that only a full color-management system can apply goes through LightCraft's own decoder, which
        // converts the samples with moxcms.
        if let ColorHint::Icc(icc) = &stored.hint
            && self.color.parse(icc).is_ok_and(|p| p.needs_cms())
        {
            let converted = decode::fallback::decode(bytes, format, options.max_pixels)?;
            stored = Stored { metadata: stored.metadata, warnings: stored.warnings, ..converted };
        }
        let mut warnings = stored.warnings;
        let (image, profile, linearized) = color::tag(self.color.as_ref(), stored.image, &stored.hint, &mut warnings)?;
        let applied = match options.orientation {
            OrientationPolicy::Apply => stored.info.orientation,
            OrientationPolicy::Ignore => Orientation::Normal,
            OrientationPolicy::Force(o) => o,
        };
        let image = if applied == Orientation::Normal { image } else { orient(&image, applied)? };
        let profile = profile.map(|p| ProfileInfo { description: p.description().map(str::to_owned), named: p.named(), curve: p.curve_kind() });
        Ok(Decoded { image, info: stored.info, profile, linearized, orientation_applied: applied, metadata: stored.metadata, warnings })
    }

    /// Writes `image` as `spec` says, with `metadata`.
    pub fn write(&self, image: &AnyImage, spec: &OutputSpec, metadata: &OutputMetadata) -> Result<Encoded, CodecError> {
        spec.validate()?;
        let format = spec.format.format();
        let encoder = self.encoder(format)?;
        let mut warnings = Vec::new();
        // DNG and OpenEXR hold linear light whatever the depth.
        let linear = matches!(format, Format::Dng | Format::OpenExr);
        let prepared = color::prepare(self.color.as_ref(), image, spec.profile, spec.depth, spec.extended_range, linear, &mut warnings)?;
        let srgb = match &prepared.profile {
            None => prepared.image.color_space().encoding == florastack_core::Encoding::Srgb,
            Some(p) => p.named() == Some(NamedSpace::Srgb) && p.curve_kind() == CurveKind::Srgb,
        };
        let meta = PreparedMetadata::new(metadata, prepared.image.width(), prepared.image.height(), srgb, &mut warnings);
        let mut written = encoder.encode(&prepared.image, prepared.profile.as_ref(), &meta, spec)?;
        warnings.append(&mut written.warnings);
        written.warnings = warnings;
        Ok(written)
    }
}

/// `image` turned upright for stored pixels with `orientation`.
fn orient(image: &AnyImage, orientation: Orientation) -> Result<AnyImage, CodecError> {
    Ok(match image {
        AnyImage::U8(i) => AnyImage::U8(i.oriented(orientation)?),
        AnyImage::U16(i) => AnyImage::U16(i.oriented(orientation)?),
        AnyImage::F32(i) => AnyImage::F32(i.oriented(orientation)?),
    })
}
