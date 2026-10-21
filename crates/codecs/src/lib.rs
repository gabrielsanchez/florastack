//! Reading and writing image files with their metadata and color profiles, in pure Rust. It may depend only on
//! `florastack-core`.
//!
//! - **Formats** ([`Format`]): JPEG, PNG, TIFF, OpenEXR, WebP and BMP in; JPEG, PNG, TIFF, OpenEXR and linear 16-bit
//!   DNG out. Integer samples come back exactly as stored, at their own depth; [`write`] converts to the output's.
//! - **Metadata** ([`Metadata`], [`OutputMetadata`]): EXIF, IPTC and XMP are read from every format that carries them
//!   and copied into outputs from a chosen source frame, with FloraStack's software tag and the stack's frame count
//!   in XMP.
//! - **Color** ([`color`]): embedded ICC profiles are kept, sRGB is assumed without one, and files whose profile has
//!   another curve than sRGB's or a linear one are linearized as they are read. Outputs go in the input's profile,
//!   sRGB or a wide working space ([`OutputProfile`]).
//! - **Orientation** ([`OrientationPolicy`]): the recorded orientation is applied, ignored or forced on reading;
//!   outputs are always written upright.
//!
//! The work is done by LightCraft's low-level crates (lightcraft-tiff, -codecs, -meta, -color and -raw, pinned in
//! the workspace manifest; see `docs/lightcraft.md`) and by the image-rs and exr crates, behind this crate's own
//! types and traits ([`ImageDecoder`], [`ImageEncoder`], [`ColorEngine`]): none of their types is part of its API,
//! so any of them can be replaced without touching the rest of FloraStack. `ATTRIBUTION.md` says which crate does
//! what.
//!
//! Every reader treats its input as hostile: sizes are checked before anything is allocated, and the libraries
//! underneath run behind a panic guard, so that a damaged file is an error ([`CodecError`]), never a crash.

pub mod codec;
pub mod color;
mod decode;
mod encode;
pub mod error;
pub mod format;
pub mod metadata;
pub mod options;

pub use codec::{Codecs, Decoded, Encoded, FileInfo, ImageDecoder, ImageEncoder, ProfileInfo, Stored};
pub use color::{ColorEngine, ColorHint, CurveKind, LightCraftColor, NamedSpace, Primaries, Profile};
pub use error::{BackendError, CodecError, MetadataKind, Warning};
pub use format::{Format, sniff};
pub use metadata::{ExifSummary, Metadata, OutputMetadata, PreparedMetadata, Timestamp, read_metadata};
pub use options::{
    BitDepth, ChromaSubsampling, DngCompression, DngLook, ExrCompression, OrientationPolicy, OutputFormat, OutputProfile, OutputSpec, ReadOptions,
    TiffCompression, WideSpace,
};

use florastack_core::AnyImage;

/// Reads an image file from its bytes.
pub fn read(bytes: &[u8], options: &ReadOptions) -> Result<Decoded, CodecError> {
    Codecs::shared().read(bytes, options)
}

/// What an image file holds, from its headers, without decoding its pixels.
pub fn probe(bytes: &[u8]) -> Result<FileInfo, CodecError> {
    Codecs::shared().probe(bytes)
}

/// Writes `image` as `spec` says, with `metadata`, and returns the file's bytes.
pub fn write(image: &AnyImage, spec: &OutputSpec, metadata: &OutputMetadata) -> Result<Encoded, CodecError> {
    Codecs::shared().write(image, spec, metadata)
}

/// Reads an image file.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_file(path: &std::path::Path, options: &ReadOptions) -> Result<Decoded, CodecError> {
    let bytes = std::fs::read(path).map_err(|source| CodecError::Read { path: path.to_path_buf(), source })?;
    read(&bytes, options)
}

/// Writes `image` to `path`, through a temporary file in the same folder that replaces `path` only once it is
/// complete, so that a failed write (a full disk) never leaves a damaged file. Returns the write's warnings.
#[cfg(not(target_arch = "wasm32"))]
pub fn write_file(path: &std::path::Path, image: &AnyImage, spec: &OutputSpec, metadata: &OutputMetadata) -> Result<Vec<Warning>, CodecError> {
    use std::io::Write as _;
    let encoded = write(image, spec, metadata)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.{}.part", std::process::id()));
    let failed = |source: std::io::Error| {
        let _ = std::fs::remove_file(&temp);
        CodecError::Write { path: path.to_path_buf(), source }
    };
    let mut file = std::fs::File::create(&temp).map_err(|source| CodecError::Write { path: path.to_path_buf(), source })?;
    file.write_all(&encoded.bytes).map_err(failed)?;
    file.sync_all().map_err(failed)?;
    drop(file);
    std::fs::rename(&temp, path).map_err(failed)?;
    Ok(encoded.warnings)
}
