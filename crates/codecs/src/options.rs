//! Options for reading ([`ReadOptions`]) and writing ([`OutputSpec`]) image files. They are plain data with serde
//! support and JSON schemas, so that commands can take them as parameters.

use florastack_core::Orientation;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::CodecError;
use crate::format::Format;

/// How [`crate::read`] reads a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ReadOptions {
    /// What to do with the orientation the file records.
    pub orientation: OrientationPolicy,
    /// The most pixels an image may have; larger files are refused before anything is allocated.
    pub max_pixels: u64,
}

impl Default for ReadOptions {
    fn default() -> ReadOptions {
        ReadOptions { orientation: OrientationPolicy::Apply, max_pixels: 1 << 30 }
    }
}

/// What a read does with the orientation a file records (the EXIF or TIFF `Orientation` tag).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OrientationPolicy {
    /// Turn the pixels upright as the tag says, as image viewers do.
    #[default]
    Apply,
    /// Keep the pixels as stored. Cameras on a vertical rig record an orientation at random, frame by frame, so that
    /// honoring it would turn some frames of a stack against the others.
    Ignore,
    /// Turn every file the same way, whatever it records (for example as the first frame of a stack says).
    Force(
        #[serde(with = "exif_orientation")]
        #[schemars(with = "u16", range(min = 1, max = 8))]
        Orientation,
    ),
}

/// Serializes an [`Orientation`] as its tag value, 1 to 8.
mod exif_orientation {
    use florastack_core::Orientation;
    use serde::{Deserialize, Deserializer, Serializer};

    #[allow(clippy::trivially_copy_pass_by_ref)] // serde's `with` passes a reference.
    pub fn serialize<S: Serializer>(orientation: &Orientation, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u16(orientation.to_exif())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Orientation, D::Error> {
        let value = u16::deserialize(deserializer)?;
        Orientation::from_exif(value).ok_or_else(|| serde::de::Error::custom(format!("orientation {value} is not between 1 and 8")))
    }
}

/// The sample type of an output file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BitDepth {
    /// 8-bit integers.
    U8,
    /// 16-bit integers.
    U16,
    /// 32-bit floats (or 16-bit floats, for OpenEXR with `half`).
    F32,
}

/// The color profile of an output file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OutputProfile {
    /// The profile of the input: the one embedded in the source frames, or sRGB when they had none. Integer files are
    /// encoded with its curve and carry the profile itself; float files are linear, with a linear profile of the same
    /// primaries.
    #[default]
    Input,
    /// sRGB.
    Srgb,
    /// A wide working space, which keeps colors that sRGB can't hold.
    Wide(WideSpace),
    /// No profile and no conversion: the samples are written as they are. For data rather than pictures, such as a
    /// depth map or a confidence map.
    Untagged,
}

/// A wide-gamut working space for [`OutputProfile::Wide`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WideSpace {
    /// ProPhoto RGB (ROMM RGB, D50 white): integer files use its 1.8 gamma, float files are linear.
    #[default]
    ProPhoto,
    /// ITU-R BT.2020 primaries with a D65 white: integer files use its curve (that of BT.709), float files are
    /// linear.
    Rec2020,
}

/// How a TIFF file's image data is compressed. Every option is lossless; LZW and Deflate use a predictor (horizontal
/// differences for integers, the floating-point predictor for floats).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TiffCompression {
    None,
    Lzw,
    #[default]
    Deflate,
}

/// How an OpenEXR file is compressed; every option here is lossless.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ExrCompression {
    None,
    Rle,
    /// Deflate, one scan line at a time.
    Zips,
    /// Deflate, 16 scan lines at a time.
    #[default]
    Zip,
    /// Wavelet compression, good for noisy images.
    Piz,
}

/// How a DNG file's image data is stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DngCompression {
    /// Every DNG reader reads it.
    #[default]
    Uncompressed,
    /// Lossless JPEG tiles (ITU-T T.81 process 14), about half the size.
    LosslessJpeg,
}

/// What a raw converter should do with a DNG file's tones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DngLook {
    /// The image is already rendered (stacked from JPEG or TIFF files): an identity tone curve keeps the converter's
    /// default curve off.
    #[default]
    Rendered,
    /// The image is scene-referred (stacked from RAW files): the converter applies its default look, as to a raw.
    Raw,
}

/// The chroma subsampling of a JPEG file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ChromaSubsampling {
    /// None: full color resolution.
    #[default]
    S444,
    S422,
    S420,
}

/// An output format with its options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", tag = "format", deny_unknown_fields)]
pub enum OutputFormat {
    /// `quality` from 1 to 100.
    Jpeg {
        #[schemars(range(min = 1, max = 100))]
        quality: u8,
        subsampling: ChromaSubsampling,
    },
    Png,
    Tiff {
        compression: TiffCompression,
    },
    /// `half`: 16-bit floats, half the size, with about three decimal digits.
    OpenExr {
        compression: ExrCompression,
        half: bool,
    },
    Dng {
        compression: DngCompression,
        look: DngLook,
    },
}

impl OutputFormat {
    pub fn format(self) -> Format {
        match self {
            OutputFormat::Jpeg { .. } => Format::Jpeg,
            OutputFormat::Png => Format::Png,
            OutputFormat::Tiff { .. } => Format::Tiff,
            OutputFormat::OpenExr { .. } => Format::OpenExr,
            OutputFormat::Dng { .. } => Format::Dng,
        }
    }

    /// The bit depths the format stores.
    pub fn depths(self) -> &'static [BitDepth] {
        match self {
            OutputFormat::Jpeg { .. } => &[BitDepth::U8],
            OutputFormat::Png => &[BitDepth::U8, BitDepth::U16],
            OutputFormat::Tiff { .. } => &[BitDepth::U8, BitDepth::U16, BitDepth::F32],
            OutputFormat::OpenExr { .. } => &[BitDepth::F32],
            OutputFormat::Dng { .. } => &[BitDepth::U16],
        }
    }
}

/// What [`crate::write`] writes: the format and its options, the bit depth and the color profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputSpec {
    pub format: OutputFormat,
    pub depth: BitDepth,
    #[serde(default)]
    pub profile: OutputProfile,
    /// Keep float values outside `[0, 1]` (the "retain extended range" option, for people who tone-map themselves).
    /// Integer files always clip.
    #[serde(default)]
    pub extended_range: bool,
}

impl OutputSpec {
    /// The usual settings for a format: the deepest integer depth it stores (8-bit for JPEG, 32-bit float for
    /// OpenEXR), the input's profile and its default options (JPEG quality 95).
    pub fn new(format: Format) -> Result<OutputSpec, CodecError> {
        let (format, depth) = match format {
            Format::Jpeg => (OutputFormat::Jpeg { quality: 95, subsampling: ChromaSubsampling::default() }, BitDepth::U8),
            Format::Png => (OutputFormat::Png, BitDepth::U16),
            Format::Tiff => (OutputFormat::Tiff { compression: TiffCompression::default() }, BitDepth::U16),
            Format::OpenExr => (OutputFormat::OpenExr { compression: ExrCompression::default(), half: false }, BitDepth::F32),
            Format::Dng => (OutputFormat::Dng { compression: DngCompression::default(), look: DngLook::default() }, BitDepth::U16),
            Format::WebP | Format::Bmp => {
                return Err(CodecError::UnsupportedFormat(if format == Format::WebP { "Writing WebP" } else { "Writing BMP" }));
            }
        };
        Ok(OutputSpec { format, depth, profile: OutputProfile::default(), extended_range: false })
    }

    /// Checks that the format stores this depth and that its options are in range.
    pub fn validate(&self) -> Result<(), CodecError> {
        let format = self.format.format();
        if !self.format.depths().contains(&self.depth) {
            return Err(CodecError::Depth { format, depth: self.depth });
        }
        if let OutputFormat::Jpeg { quality, .. } = self.format
            && !(1..=100).contains(&quality)
        {
            return Err(CodecError::Unsupported { format, what: "a quality outside 1 to 100" });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_format_has_usable_defaults() {
        for format in Format::ALL.into_iter().filter(|f| f.can_write()) {
            let spec = OutputSpec::new(format).unwrap();
            spec.validate().unwrap();
            assert_eq!(spec.format.format(), format);
        }
        assert!(OutputSpec::new(Format::WebP).is_err());
        let mut jpeg = OutputSpec::new(Format::Jpeg).unwrap();
        jpeg.depth = BitDepth::U16;
        assert!(matches!(jpeg.validate(), Err(CodecError::Depth { format: Format::Jpeg, depth: BitDepth::U16 })));
        jpeg.depth = BitDepth::U8;
        jpeg.format = OutputFormat::Jpeg { quality: 0, subsampling: ChromaSubsampling::S420 };
        assert!(jpeg.validate().is_err());
    }

    #[test]
    fn options_round_trip_through_serde() {
        let read = ReadOptions { orientation: OrientationPolicy::Force(Orientation::Rotate270), max_pixels: 100 };
        let text = toml::to_string(&read).unwrap();
        assert!(text.contains("force = 8"), "the orientation is its tag value: {text}");
        let spec = OutputSpec {
            format: OutputFormat::Tiff { compression: TiffCompression::Lzw },
            depth: BitDepth::F32,
            profile: OutputProfile::Wide(WideSpace::Rec2020),
            extended_range: true,
        };
        assert_eq!(toml::from_str::<OutputSpec>(&toml::to_string(&spec).unwrap()).unwrap(), spec);
        assert_eq!(toml::from_str::<ReadOptions>(&toml::to_string(&read).unwrap()).unwrap(), read);
        assert!(toml::from_str::<ReadOptions>("orientation = { force = 9 }").is_err(), "no such orientation");
        // A misspelled option of a format is an error, not ignored (it was before 1.5).
        let typo = r#"{ "format": { "format": "jpeg", "quality": 90, "subsampling": "s444", "qualty": 10 }, "depth": "u8" }"#;
        assert!(serde_json::from_str::<OutputSpec>(typo).unwrap_err().to_string().contains("unknown field"));
        let ok = r#"{ "format": { "format": "jpeg", "quality": 90, "subsampling": "s444" }, "depth": "u8" }"#;
        assert_eq!(serde_json::from_str::<OutputSpec>(ok).unwrap().format, OutputFormat::Jpeg { quality: 90, subsampling: ChromaSubsampling::S444 });
        assert_eq!(toml::from_str::<ReadOptions>("").unwrap(), ReadOptions::default());
    }
}
