//! Camera RAW files: a thin adapter over LightCraft's pure-Rust RAW decoders, kept behind FloraStack's own
//! [`RawDecoder`] trait so that the dependency can be bumped, vendored or swapped without touching other crates, and a
//! develop step that turns every frame of a stack, with identical settings, into linear scene-referred RGB for
//! stacking. It may depend only on `florastack-core`.
//!
//! - **Decoding** ([`RawDecoder`], [`decode`]): what LightCraft decodes at the pinned commit (its
//!   `docs/raw-parity.md`): DNG (uncompressed, lossless and lossy JPEG, Deflate, JPEG XL; CFA and `LinearRaw`),
//!   Canon CR2 and CR3 (lossless CRX and C-RAW), Nikon NEF (compressed included) and NRW, Sony ARW, Fujifilm RAF
//!   (Bayer and X-Trans), Panasonic RW2 and Leica RWL, Pentax PEF, uncompressed Olympus ORF and Samsung SRW, and Epson
//!   ERF, Mamiya MEF and Hasselblad 3FR and FFF. Anything else, a variant inside those formats included, is a
//!   [`RawError::Unsupported`] that says to convert the file to DNG or to a 16-bit TIFF first. A decoded file is a
//!   [`RawFrame`] of plain data (normalized sensor samples, the color filter array, the camera's color model), so that
//!   another decoder can stand in for LightCraft's and the develop step works the same.
//! - **Developing** ([`DevelopSettings`], [`RawDevelop`], [`Developed`]): white balance (as shot, automatic or a
//!   temperature and tint), a fast demosaic for previews and a high-quality one for output (X-Trans with its own),
//!   highlight reconstruction, exposure, lateral chromatic-aberration correction and light noise reduction, into
//!   linear Rec. 2020: no tone curve, filmic shoulder or look, which belong after stacking. Lens distortion
//!   correction is off unless asked for. The settings are chosen once per stack and resolved once, on a reference
//!   frame, so that every frame gets the same white and the same corrections; the `develop` module documents each
//!   step and where its algorithm comes from.
//! - **Already-converted files**: a DNG that a raw converter has already demosaiced (`LinearRaw`) is imported
//!   unchanged: the develop settings don't apply to it, only its own color model at its own white. A 16-bit TIFF from
//!   a raw converter is no RAW file: `florastack-codecs` reads it, and `florastack-engine` sends each file to the crate
//!   that reads it (this crate's [`probe`] says which files are RAW).
//!
//! LightCraft's crates, pinned in the workspace manifest (`docs/lightcraft.md`), stay behind this crate's own types:
//! `lightcraft-raw` decodes, demosaics, rebuilds highlights and holds the DNG color model, and `lightcraft-pipeline`
//! reduces noise and estimates chromatic aberration and white balance. None of their types is part of this crate's
//! API.
//!
//! Every reader treats its input as hostile: sizes are checked from the headers before the samples are decoded, and
//! LightCraft's code runs behind a panic guard, so that a damaged file is an error ([`RawError`]), never a crash.

mod color;
pub mod decoder;
pub mod develop;
pub mod error;
pub mod format;
pub mod frame;

pub use color::working_space_icc;
pub use decoder::{LightCraftRaw, RawDecoder};
pub use develop::{
    ChromaticAberration, Demosaic, DevelopSettings, DevelopWarning, Developed, Highlights, NoiseReduction, Quality, RawDevelop, Resolved,
    WhiteBalance,
};
pub use error::{BackendError, RawError};
pub use format::{RawFormat, extensions};
pub use frame::{CameraColor, Cfa, Chromaticity, ColorSource, Mosaic, RawFrame, RawInfo, SensorLayout};

/// The most photosites a file may have by default: larger ones are refused before their samples are decoded (the
/// same limit as `florastack-codecs`' default).
pub const DEFAULT_MAX_PIXELS: u64 = 1 << 30;

/// The built-in decoder, LightCraft's.
pub fn decoder() -> &'static dyn RawDecoder {
    &LightCraftRaw
}

/// The RAW format of a file from its bytes, or `None` when it isn't a camera RAW or DNG file.
pub fn probe(bytes: &[u8]) -> Option<RawFormat> {
    decoder().probe(bytes)
}

/// What a RAW file holds, from its headers.
pub fn info(bytes: &[u8]) -> Result<RawInfo, RawError> {
    decoder().info(bytes)
}

/// Decodes a RAW file from its bytes, refusing files of more than `max_pixels` photosites.
pub fn decode(bytes: &[u8], max_pixels: u64) -> Result<RawFrame, RawError> {
    decoder().decode(bytes, max_pixels)
}

/// Decodes a RAW file.
#[cfg(not(target_arch = "wasm32"))]
pub fn decode_file(path: &std::path::Path, max_pixels: u64) -> Result<RawFrame, RawError> {
    let bytes = std::fs::read(path).map_err(|source| RawError::Read { path: path.to_path_buf(), source })?;
    decode(&bytes, max_pixels)
}
