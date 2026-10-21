//! What can go wrong reading and developing RAW files ([`RawError`]).
//!
//! LightCraft's decoders keep their own error type. [`BackendError`] wraps it, so that the decoder's explanation stays
//! available as the source of a [`RawError`] while none of LightCraft's types appears in this crate's API.

use std::fmt;
use std::path::PathBuf;

use florastack_core::ImageError;
use florastack_core::ops::OpError;

use crate::format::RawFormat;

/// Why a RAW file can't be read or developed.
#[derive(Debug, thiserror::Error)]
pub enum RawError {
    /// The bytes are not a camera RAW or DNG file (16-bit TIFF and other images are read by `florastack-codecs`).
    #[error("not a camera RAW or DNG file")]
    NotRaw,
    /// A format, or a variant of one (compressed ORF, NEF High Efficiency), that the decoder can't read yet. The
    /// source says what it found.
    #[error(
        "{format} files of this kind can't be decoded yet: convert them to DNG (Adobe DNG Converter does) or to a 16-bit TIFF in your raw converter first"
    )]
    Unsupported {
        format: RawFormat,
        #[source]
        source: BackendError,
    },
    #[error("the {format} file is damaged or not a valid {format} file")]
    Malformed {
        format: RawFormat,
        #[source]
        source: BackendError,
    },
    #[error("the image is {width}x{height}, more than the {max} pixels FloraStack reads at once")]
    TooLarge { width: u64, height: u64, max: u64 },
    /// A develop setting outside its range, or not a number.
    #[error("the develop setting {name} is {value}, but it must be {expected}")]
    Setting { name: &'static str, value: f64, expected: &'static str },
    /// A frame whose parts don't fit together (its sample count against its size, a crop outside it), which a decoder
    /// other than the built-in one can produce.
    #[error("the RAW frame is inconsistent: {0}")]
    Frame(&'static str),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Op(#[from] OpError),
    #[error("cannot read {}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl RawError {
    /// The error for what LightCraft's decoder reported on a file it recognized as `format`: one that then turns out
    /// not to be a RAW file after all is a damaged one.
    pub(crate) fn from_lightcraft(format: RawFormat, e: lightcraft_raw::RawError) -> RawError {
        use lightcraft_raw::RawError as Lc;
        match e {
            Lc::Unsupported(_) => RawError::Unsupported { format, source: BackendError(Backend::LightCraft(e)) },
            Lc::NotRaw | Lc::Corrupt(_) | Lc::Limit(_) | Lc::Tiff(_) => RawError::Malformed { format, source: BackendError(Backend::LightCraft(e)) },
        }
    }

    /// A format the decoder recognizes but doesn't decode at all.
    pub(crate) fn unsupported(format: RawFormat, why: &'static str) -> RawError {
        RawError::Unsupported { format, source: BackendError(Backend::Message(why)) }
    }
}

/// An error from the decoder this crate is built on, kept as the source of a [`RawError`]. Its message is the
/// decoder's; its own source is the decoder's error.
pub struct BackendError(Backend);

/// What a [`BackendError`] can hold.
enum Backend {
    LightCraft(lightcraft_raw::RawError),
    /// The decoder panicked on the file (caught on targets that unwind).
    Panicked,
    /// A problem this crate found itself.
    Message(&'static str),
}

impl BackendError {
    pub(crate) fn panicked() -> BackendError {
        BackendError(Backend::Panicked)
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Backend::LightCraft(e) => write!(f, "LightCraft: {e}"),
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
            Backend::LightCraft(e) => Some(e),
            Backend::Panicked | Backend::Message(_) => None,
        }
    }
}

/// Runs LightCraft's code on data from a file, turning a panic into [`RawError::Malformed`]. The decoders and the
/// develop code must not panic on malformed files, but a crash loses work (AGENTS.md, Never crash), so this guards
/// against the ones that do. On targets that abort on panic (wasm32), it can't help.
pub(crate) fn guarded<T>(format: RawFormat, f: impl FnOnce() -> Result<T, RawError>) -> Result<T, RawError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(RawError::Malformed { format, source: BackendError::panicked() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lightcraft_errors_keep_their_message_as_the_source() {
        let e = RawError::from_lightcraft(RawFormat::Orf, lightcraft_raw::RawError::Unsupported("compressed ORF".into()));
        assert!(matches!(e, RawError::Unsupported { format: RawFormat::Orf, .. }));
        let text = e.to_string();
        assert!(text.contains("Olympus ORF") && text.contains("DNG") && text.contains("16-bit TIFF"), "{text}");
        let source = std::error::Error::source(&e).unwrap();
        assert_eq!(source.to_string(), "LightCraft: unsupported raw variant: compressed ORF");
        assert!(std::error::Error::source(source).is_some(), "LightCraft's error itself");
        let corrupt = RawError::from_lightcraft(RawFormat::Nef, lightcraft_raw::RawError::Corrupt("truncated strip".into()));
        assert_eq!(corrupt.to_string(), "the Nikon NEF file is damaged or not a valid Nikon NEF file");
        let not_raw = RawError::from_lightcraft(RawFormat::Raf, lightcraft_raw::RawError::NotRaw);
        assert!(matches!(not_raw, RawError::Malformed { format: RawFormat::Raf, .. }), "a file that looked like a RAF: {not_raw:?}");
    }

    #[test]
    fn a_panic_in_the_decoder_becomes_an_error() {
        let result: Result<(), RawError> = guarded(RawFormat::Cr3, || panic!("hostile file"));
        let Err(RawError::Malformed { format: RawFormat::Cr3, source }) = result else { panic!("{result:?}") };
        assert!(std::error::Error::source(&source).is_none());
        assert_eq!(guarded(RawFormat::Cr3, || Ok(7)).unwrap(), 7);
    }
}
