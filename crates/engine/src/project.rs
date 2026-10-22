//! The project with the types of the crates it can't depend on. florastack-project stores what other crates define as
//! JSON data, and the cache's files as bytes; this module, in the one crate that depends on them all, turns both back
//! into typed values, so that the commands built on it (roadmap 1.5) never handle the raw data:
//!
//! - the develop settings, a preference group ([`develop_settings`], [`set_develop_settings`]), and the stack's
//!   development resolved with them ([`store_develop`], [`develop`]), florastack-raw's types;
//! - each frame's EXIF summary ([`add_frame`], [`exif`]), florastack-codecs';
//! - the cache's images, written losslessly as Deflate-compressed TIFF at their own sample type, color space and
//!   range ([`write_image`], [`read_image`]), and previews, small sRGB JPEGs ([`write_preview`]).
//!
//! The shapes these types take in a project file are pinned by this module's tests: a change to one must raise the
//! project format version and add a migration (`florastack_project::schema`).

use std::path::Path;

use florastack_codecs::{
    BitDepth, ChromaSubsampling, CodecError, ExifSummary, OrientationPolicy, OutputFormat, OutputMetadata, OutputProfile, OutputSpec, ReadOptions,
    TiffCompression,
};
use florastack_core::ops::{OpError, resize};
use florastack_core::{AnyImage, Image, SampleFormat};
use florastack_project::{Area, CacheRef, Develop, Frame, FrameFile, FrameId, Group, Project, ProjectError, ProjectFolder, from_data, to_data};
use florastack_raw::{DevelopSettings, RawDevelop, RawError, Resolved};

/// Why project data can't be read or written.
#[derive(Debug, thiserror::Error)]
pub enum ProjectDataError {
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error(transparent)]
    Raw(#[from] RawError),
    #[error(transparent)]
    Op(#[from] OpError),
}

/// The develop settings the preferences hold, every key they don't set at its default, checked.
pub fn develop_settings(project: &Project) -> Result<DevelopSettings, ProjectDataError> {
    let settings: DevelopSettings = project.preferences.get(Group::Develop)?;
    settings.validate()?;
    Ok(settings)
}

/// Sets the develop preferences, after checking them.
pub fn set_develop_settings(project: &mut Project, settings: &DevelopSettings) -> Result<(), ProjectDataError> {
    settings.validate()?;
    project.preferences.set(Group::Develop, settings)?;
    Ok(())
}

/// Records the stack's development, resolved on the frame `reference`, so that a reopened project develops its frames
/// as before.
pub fn store_develop(project: &mut Project, reference: FrameId, develop: &RawDevelop) -> Result<(), ProjectDataError> {
    let settings = to_data("develop settings", develop.settings())?;
    let resolved = to_data("resolved development", &develop.resolved())?;
    project.develop = Some(Develop { reference, settings, resolved });
    Ok(())
}

/// The stack's development as recorded, if it still holds: resolved with the develop settings the preferences hold
/// now. `None` when nothing is recorded or the settings changed since, and the development must be resolved again.
pub fn develop(project: &Project) -> Result<Option<RawDevelop>, ProjectDataError> {
    let Some(stored) = &project.develop else { return Ok(None) };
    let settings: DevelopSettings = from_data("develop settings", &stored.settings)?;
    if settings != develop_settings(project)? {
        return Ok(None);
    }
    let resolved: Resolved = from_data("resolved development", &stored.resolved)?;
    Ok(Some(RawDevelop::from_resolved(&settings, resolved)?))
}

/// Adds the file at `path` to the project as a frame: reads it once, hashes it and keeps its metadata summary.
pub fn add_frame(project: &mut Project, path: &Path) -> Result<FrameId, ProjectDataError> {
    let bytes = std::fs::read(path).map_err(|source| ProjectError::Read { path: path.to_path_buf(), source })?;
    let file = FrameFile::from_bytes(path, &bytes)?;
    let summary = florastack_codecs::read_metadata(&bytes).summary().clone();
    drop(bytes);
    let exif = if summary == ExifSummary::default() { None } else { Some(to_data("EXIF summary", &summary)?) };
    Ok(project.add_frame(file, exif))
}

/// A frame's EXIF summary, if it has one.
pub fn exif(frame: &Frame) -> Result<Option<ExifSummary>, ProjectDataError> {
    Ok(frame.exif.as_ref().map(|data| from_data("EXIF summary", data)).transpose()?)
}

/// How cache images are written: TIFF with Deflate (and its predictor), at the image's own sample type and in its own
/// profile, float values outside `[0, 1]` kept. Every sample reads back exactly.
fn lossless(format: SampleFormat) -> OutputSpec {
    let depth = match format {
        SampleFormat::U8 => BitDepth::U8,
        SampleFormat::U16 => BitDepth::U16,
        SampleFormat::F32 => BitDepth::F32,
    };
    OutputSpec { format: OutputFormat::Tiff { compression: TiffCompression::Deflate }, depth, profile: OutputProfile::Input, extended_range: true }
}

/// Writes `image` into the project's cache, losslessly, as a file named after `stem` (`output-7`, `frame-3-aligned`),
/// and returns its reference for the project, with `key` (what it was made from).
pub fn write_image(folder: &ProjectFolder, stem: &str, image: &AnyImage, key: Option<String>) -> Result<CacheRef, ProjectDataError> {
    let encoded = florastack_codecs::write(image, &lossless(image.format()), &OutputMetadata::default())?;
    Ok(folder.store(Area::Cache, stem, "tiff", &encoded.bytes, key)?)
}

/// Reads an image from the project's cache, after checking the file against its reference.
pub fn read_image(folder: &ProjectFolder, reference: &CacheRef) -> Result<AnyImage, ProjectDataError> {
    let bytes = folder.read(reference)?;
    // Cache files are written upright; their pixels are used as stored.
    let options = ReadOptions { orientation: OrientationPolicy::Ignore, ..ReadOptions::default() };
    Ok(florastack_codecs::read(&bytes, &options)?.image)
}

/// The long side of a preview, in pixels.
pub const PREVIEW_SIDE: usize = 512;

/// Writes a preview of a linear image into `previews/`: at most [`PREVIEW_SIDE`] pixels on its long side, an sRGB JPEG.
pub fn write_preview(folder: &ProjectFolder, stem: &str, image: &Image<f32>) -> Result<CacheRef, ProjectDataError> {
    let (w, h) = (image.width(), image.height());
    let long = w.max(h);
    let small;
    let image = if long > PREVIEW_SIDE {
        let side = |s: usize| (s.saturating_mul(PREVIEW_SIDE) / long).max(1);
        small = resize(image, side(w), side(h))?;
        &small
    } else {
        image
    };
    let spec = OutputSpec {
        format: OutputFormat::Jpeg { quality: 85, subsampling: ChromaSubsampling::S420 },
        depth: BitDepth::U8,
        profile: OutputProfile::Srgb,
        extended_range: false,
    };
    let encoded = florastack_codecs::write(&AnyImage::F32(image.clone()), &spec, &OutputMetadata::default())?;
    Ok(folder.store(Area::Previews, stem, "jpg", &encoded.bytes, None)?)
}
