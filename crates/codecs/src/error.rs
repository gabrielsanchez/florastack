//! What can go wrong reading and writing image files ([`CodecError`]), and what a read or write left out or changed
//! without failing ([`Warning`]).
//!
//! The libraries underneath (png, tiff, exr, zune-jpeg, image-webp, image and LightCraft's crates) keep their own
//! error types. [`BackendError`] wraps them, so that the error stays available as the source of a [`CodecError`]
//! while none of their types appears in this crate's API.

use std::fmt;
use std::path::PathBuf;

use florastack_core::ImageError;

use crate::format::Format;
use crate::options::BitDepth;

/// Why a file can't be read or written.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("not an image file FloraStack can read: the format isn't recognized")]
    UnknownFormat,
    #[error("this is a {0}: open it with the RAW importer, or convert it to 16-bit TIFF first")]
    RawFile(&'static str),
    #[error("{0} files are not supported: convert them to TIFF or PNG first")]
    UnsupportedFormat(&'static str),
    #[error("{format} files with {what} are not supported")]
    Unsupported { format: Format, what: &'static str },
    /// A valid file that uses a feature the decoder lacks, such as 12-bit TIFF samples or DWA-compressed OpenEXR.
    #[error("this kind of {format} file is not supported: {advice}")]
    UnsupportedVariant {
        format: Format,
        /// What the user can do instead.
        advice: &'static str,
        #[source]
        source: BackendError,
    },
    #[error("the {format} file is damaged or not a valid {format} file")]
    Malformed {
        format: Format,
        #[source]
        source: BackendError,
    },
    #[error("the image is {width}x{height}, more than the {max} pixels FloraStack reads at once")]
    TooLarge { width: u64, height: u64, max: u64 },
    #[error("{format} can't store {depth} samples")]
    Depth { format: Format, depth: BitDepth },
    #[error("cannot write the {format} file")]
    Encode {
        format: Format,
        #[source]
        source: BackendError,
    },
    #[error("the ICC profile can't be used")]
    Profile {
        #[source]
        source: BackendError,
    },
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error("cannot read {}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {}", .path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl CodecError {
    pub(crate) fn malformed(format: Format, source: impl Into<BackendError>) -> CodecError {
        CodecError::Malformed { format, source: source.into() }
    }

    pub(crate) fn encode(format: Format, source: impl Into<BackendError>) -> CodecError {
        CodecError::Encode { format, source: source.into() }
    }
}

/// An error from one of the libraries this crate is built on, kept as the source of a [`CodecError`]. Its message is
/// the library's; its own source is the library's error.
pub struct BackendError(Backend);

/// The library errors a [`BackendError`] can hold.
pub(crate) enum Backend {
    Png(png::DecodingError),
    PngEncode(png::EncodingError),
    Tiff(tiff::TiffError),
    Jpeg(zune_jpeg::errors::DecodeErrors),
    WebP(image_webp::DecodingError),
    Bmp(image::ImageError),
    Exr(exr::error::Error),
    LightCraftTiff(lightcraft_tiff::TiffError),
    LightCraftCodecs(lightcraft_codecs::Error),
    LightCraftMeta(lightcraft_meta::XmpError),
    Io(std::io::Error),
    /// A library panicked on the file (caught on targets that unwind).
    Panicked,
    /// A problem this crate found itself.
    Message(&'static str),
}

impl BackendError {
    pub(crate) fn message(text: &'static str) -> BackendError {
        BackendError(Backend::Message(text))
    }

    pub(crate) fn panicked() -> BackendError {
        BackendError(Backend::Panicked)
    }
}

macro_rules! backend_from {
    ($($variant:ident($ty:ty)),* $(,)?) => {
        $(impl From<$ty> for BackendError {
            fn from(e: $ty) -> BackendError {
                BackendError(Backend::$variant(e))
            }
        })*
    };
}

backend_from!(
    Png(png::DecodingError),
    PngEncode(png::EncodingError),
    Tiff(tiff::TiffError),
    Jpeg(zune_jpeg::errors::DecodeErrors),
    WebP(image_webp::DecodingError),
    Bmp(image::ImageError),
    Exr(exr::error::Error),
    LightCraftTiff(lightcraft_tiff::TiffError),
    LightCraftCodecs(lightcraft_codecs::Error),
    LightCraftMeta(lightcraft_meta::XmpError),
    Io(std::io::Error),
);

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Backend::Png(e) => write!(f, "png: {e}"),
            Backend::PngEncode(e) => write!(f, "png: {e}"),
            Backend::Tiff(e) => write!(f, "tiff: {e}"),
            Backend::Jpeg(e) => write!(f, "jpeg: {e}"),
            Backend::WebP(e) => write!(f, "webp: {e}"),
            Backend::Bmp(e) => write!(f, "bmp: {e}"),
            Backend::Exr(e) => write!(f, "exr: {e}"),
            Backend::LightCraftTiff(e) => write!(f, "tiff: {e}"),
            Backend::LightCraftCodecs(e) => write!(f, "{e}"),
            Backend::LightCraftMeta(e) => write!(f, "xmp: {e}"),
            Backend::Io(e) => write!(f, "{e}"),
            Backend::Panicked => f.write_str("the decoder failed on malformed data"),
            Backend::Message(text) => f.write_str(text),
        }
    }
}

impl fmt::Debug for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BackendError({self})")
    }
}

impl std::error::Error for BackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0 {
            Backend::Png(e) => Some(e),
            Backend::PngEncode(e) => Some(e),
            Backend::Tiff(e) => Some(e),
            Backend::Jpeg(e) => Some(e),
            Backend::WebP(e) => Some(e),
            Backend::Bmp(e) => Some(e),
            Backend::Exr(e) => Some(e),
            Backend::LightCraftTiff(e) => Some(e),
            Backend::LightCraftCodecs(e) => Some(e),
            Backend::LightCraftMeta(e) => Some(e),
            Backend::Io(e) => Some(e),
            Backend::Panicked | Backend::Message(_) => None,
        }
    }
}

/// A kind of metadata a [`Warning`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MetadataKind {
    Exif,
    Iptc,
    Xmp,
    Icc,
}

impl fmt::Display for MetadataKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MetadataKind::Exif => "EXIF",
            MetadataKind::Iptc => "IPTC",
            MetadataKind::Xmp => "XMP",
            MetadataKind::Icc => "ICC profile",
        })
    }
}

/// Something a read or write did differently from what was asked, without failing: the file was read or written,
/// and the caller may show these to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    /// The file's metadata of this kind couldn't be read, and was left out.
    UnreadableMetadata(MetadataKind),
    /// The output format has no place for this kind of metadata, so the file doesn't carry it.
    MetadataNotSupported { format: Format, kind: MetadataKind },
    /// The metadata didn't fit the format (a JPEG segment holds 64 KiB) and was shortened: `removed` says what went.
    MetadataTrimmed { kind: MetadataKind, removed: &'static str },
    /// The metadata didn't fit the format even shortened, and was left out.
    MetadataDropped { kind: MetadataKind, reason: &'static str },
    /// The format can't store an alpha channel: the file was written without it.
    AlphaDropped(Format),
    /// The format stores color images only: the gray image was written as RGB.
    GrayExpanded(Format),
    /// The embedded ICC profile couldn't be used: sRGB was assumed.
    ProfileIgnored(&'static str),
    /// The input's ICC profile can't describe the output: it was written in this space instead.
    ProfileReplaced { reason: &'static str, space: &'static str },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::UnreadableMetadata(kind) => write!(f, "the {kind} metadata couldn't be read and was left out"),
            Warning::MetadataNotSupported { format, kind } => write!(f, "{format} files can't carry {kind} metadata: it was left out"),
            Warning::MetadataTrimmed { kind, removed } => write!(f, "the {kind} metadata was too large and was shortened: {removed} removed"),
            Warning::MetadataDropped { kind, reason } => write!(f, "the {kind} metadata was left out: {reason}"),
            Warning::AlphaDropped(format) => write!(f, "{format} files have no alpha channel: the transparency was left out"),
            Warning::GrayExpanded(format) => write!(f, "{format} files are written in color: the gray image was written as RGB"),
            Warning::ProfileIgnored(reason) => write!(f, "the embedded ICC profile was ignored ({reason}): sRGB was assumed"),
            Warning::ProfileReplaced { reason, space } => {
                write!(f, "the input's ICC profile can't be used for the output ({reason}): written in {space}")
            }
        }
    }
}

/// Runs a third-party decoder or encoder, turning a panic into [`BackendError::panicked`]. Decoders must not panic on
/// malformed files, but a crash loses work (AGENTS.md, Never crash), so this guards against the ones that do. On
/// targets that abort on panic (wasm32), it can't help.
pub(crate) fn guarded<T>(f: impl FnOnce() -> Result<T, CodecError>, format: Format) -> Result<T, CodecError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(CodecError::malformed(format, BackendError::panicked())))
}

impl fmt::Display for BitDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BitDepth::U8 => "8-bit",
            BitDepth::U16 => "16-bit",
            BitDepth::F32 => "32-bit float",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_errors_keep_the_library_error_as_their_source() {
        let e = CodecError::malformed(Format::Png, png::DecodingError::LimitsExceeded);
        let source = std::error::Error::source(&e).expect("a backend error");
        assert!(source.to_string().starts_with("png: "), "{source}");
        assert!(std::error::Error::source(source).is_some(), "the png error itself");
        assert_eq!(e.to_string(), "the PNG file is damaged or not a valid PNG file");
        let panicked = CodecError::malformed(Format::Tiff, BackendError::panicked());
        assert!(std::error::Error::source(std::error::Error::source(&panicked).unwrap()).is_none());
    }

    #[test]
    fn a_panic_in_a_library_becomes_an_error() {
        let result: Result<(), CodecError> = guarded(|| panic!("hostile file"), Format::Bmp);
        assert!(matches!(result, Err(CodecError::Malformed { format: Format::Bmp, .. })));
        assert_eq!(guarded(|| Ok(7), Format::Bmp).unwrap(), 7);
    }

    #[test]
    fn warnings_read_as_sentences() {
        let w = Warning::MetadataNotSupported { format: Format::Png, kind: MetadataKind::Iptc };
        assert_eq!(w.to_string(), "PNG files can't carry IPTC metadata: it was left out");
        assert_eq!(BitDepth::F32.to_string(), "32-bit float");
    }
}
