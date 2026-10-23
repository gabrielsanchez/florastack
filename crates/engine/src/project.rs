//! The project with the types of the crates it can't depend on. florastack-project stores what other crates define as
//! JSON data, and the cache's files as bytes; this module, in the one crate that depends on them all, turns both back
//! into typed values, so that the commands built on it (roadmap 1.5) never handle the raw data:
//!
//! - the develop settings, a preference group ([`develop_settings`], [`set_develop_settings`]), and the stack's
//!   development resolved with them ([`store_develop`], [`develop`]), florastack-raw's types;
//! - the alignment settings, a preference group ([`align_settings`]), and an alignment run with what it found about
//!   each frame and about the stack ([`alignment_edit`], [`stored_alignment`]; [`FrameReport`] and [`RunReport`] are
//!   the shapes the reports take, with the motion found in each frame, [`FrameMotion`]), florastack-align's types;
//! - the stack's defect mask, one of the project's maps, stored as a lossless image of black spots on white with a
//!   summary of it as its cache key ([`store_defect_mask`], [`read_defect_mask`], [`MaskSummary`]);
//! - the frames' previews for the filmstrip, as shot and aligned, keyed by what they were made from
//!   ([`write_frame_preview`], [`write_aligned_preview`], [`preview_key`], [`aligned_preview_key`]);
//! - each frame's EXIF summary ([`add_frame`], [`read_frame_file`], [`exif`]), florastack-codecs';
//! - the cache's images, written losslessly as Deflate-compressed TIFF at their own sample type, color space and
//!   range ([`write_image`], [`read_image`]), and previews, small sRGB JPEGs ([`write_preview`]). They take a
//!   project folder or its [`CacheDir`], which a job's worker can hold.
//!
//! The shapes these types take in a project file are pinned by this module's tests: a change to one must raise the
//! project format version and add a migration (`florastack_project::schema`).

use std::path::Path;

use florastack_align::{
    AlignError, AlignSettings, Canvas, DefectMask, End, Flag, Framing, MotionRegion, ProcessingOrder, StackAlignment, Status, aligned_thumbnail,
    thumbnail,
};
use florastack_codecs::{
    BitDepth, ChromaSubsampling, CodecError, ExifSummary, OrientationPolicy, OutputFormat, OutputMetadata, OutputProfile, OutputSpec, ReadOptions,
    TiffCompression,
};
use florastack_core::ops::{OpError, resize};
use florastack_core::{Affine, AnyImage, Image, SampleFormat};
use florastack_project::{
    AlignedFrame, Alignment, AlignmentRun, Area, CacheDir, CacheRef, ContentHash, Develop, Edit, Frame, FrameFile, FrameId, Group, MapName, Project,
    ProjectError, Value, from_data, to_data,
};
use florastack_raw::{DevelopSettings, RawDevelop, RawError, Resolved};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::frames::{FrameError, FrameSource};

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
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("cannot read the frame {}", path.display())]
    ReadFrame {
        path: std::path::PathBuf,
        #[source]
        source: FrameError,
    },
    #[error(transparent)]
    Align(#[from] AlignError),
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
    project.develop = Some(develop_record(reference, develop)?);
    Ok(())
}

/// The stack's development, resolved on the frame `reference`, as the project records it (for [`Edit::SetDevelop`]).
pub fn develop_record(reference: FrameId, develop: &RawDevelop) -> Result<Develop, ProjectDataError> {
    let settings = to_data("develop settings", develop.settings())?;
    let resolved = to_data("resolved development", &develop.resolved())?;
    Ok(Develop { reference, settings, resolved })
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
    let (file, exif) = frame_file(path, &bytes)?;
    drop(bytes);
    Ok(project.add_frame(file, exif))
}

/// A file read to become a frame ([`read_frame_file`]).
#[derive(Clone, Debug, PartialEq)]
pub struct FrameRead {
    pub file: FrameFile,
    /// Its EXIF summary, as the project stores it.
    pub exif: Option<Value>,
    /// Which crate reads it.
    pub source: FrameSource,
}

/// Reads the file at `path` once, to add it as a frame: checks that it is a camera RAW or image file FloraStack reads
/// (from its bytes, not its name), hashes it and takes its metadata summary.
pub fn read_frame_file(path: &Path) -> Result<FrameRead, ProjectDataError> {
    let bytes = std::fs::read(path).map_err(|source| ProjectError::Read { path: path.to_path_buf(), source })?;
    let source = crate::frames::source(&bytes)?;
    let (file, exif) = frame_file(path, &bytes)?;
    Ok(FrameRead { file, exif, source })
}

fn frame_file(path: &Path, bytes: &[u8]) -> Result<(FrameFile, Option<Value>), ProjectDataError> {
    let file = FrameFile::from_bytes(path, bytes)?;
    let summary = florastack_codecs::read_metadata(bytes).summary().clone();
    let exif = if summary == ExifSummary::default() { None } else { Some(to_data("EXIF summary", &summary)?) };
    Ok((file, exif))
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
pub fn write_image(folder: &impl AsRef<CacheDir>, stem: &str, image: &AnyImage, key: Option<String>) -> Result<CacheRef, ProjectDataError> {
    let encoded = florastack_codecs::write(image, &lossless(image.format()), &OutputMetadata::default())?;
    Ok(folder.as_ref().store(Area::Cache, stem, "tiff", &encoded.bytes, key)?)
}

/// Reads an image from the project's cache, after checking the file against its reference.
pub fn read_image(folder: &impl AsRef<CacheDir>, reference: &CacheRef) -> Result<AnyImage, ProjectDataError> {
    let bytes = folder.as_ref().read(reference)?;
    // Cache files are written upright; their pixels are used as stored.
    let options = ReadOptions { orientation: OrientationPolicy::Ignore, ..ReadOptions::default() };
    Ok(florastack_codecs::read(&bytes, &options)?.image)
}

/// The long side of a preview, in pixels.
pub const PREVIEW_SIDE: usize = 512;

/// Writes a preview of a linear image into `previews/`: at most [`PREVIEW_SIDE`] pixels on its long side, an sRGB JPEG,
/// with `key` (what it was made from).
pub fn write_preview(folder: &impl AsRef<CacheDir>, stem: &str, image: &Image<f32>, key: Option<String>) -> Result<CacheRef, ProjectDataError> {
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
    Ok(folder.as_ref().store(Area::Previews, stem, "jpg", &encoded.bytes, key)?)
}

/// The alignment settings the preferences hold, every key they don't set at its default, checked.
pub fn align_settings(project: &Project) -> Result<AlignSettings, ProjectDataError> {
    let settings: AlignSettings = project.preferences.get(Group::Align)?;
    settings.validate()?;
    Ok(settings)
}

/// What moved in a frame against its neighbor, once aligned, as a frame's report stores it (florastack-align's
/// `Motion`, without its map).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameMotion {
    /// The share of the frame's measured cells that moved, from 0 to 1.
    pub score: f64,
    /// The largest displacement measured, in frame pixels.
    pub peak: f64,
    /// The regions that moved, in frame pixels, largest first.
    pub regions: Vec<MotionRegion>,
}

/// What alignment found about a frame beyond its transform, as a project stores it (`Frame::report`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameReport {
    /// Whether the frame was aligned, or rejected (its transform is then its reference's), or is the master.
    pub status: Status,
    /// What needs a look: a term at its limit, a low score, no match, a repeating pattern, out of focus.
    pub flags: Vec<Flag>,
    /// The frame it was aligned to, its neighbor in the stack; none for the frame alignment started from.
    pub reference: Option<FrameId>,
    /// The share of the stack's in-focus detail on which the frame is among the sharpest, from 0 to 1, when it could
    /// be told.
    pub sharp_fraction: Option<f64>,
    /// What moved in it against its reference; none for the master and rejected frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<FrameMotion>,
}

/// What an alignment run found about the stack, as a project stores it (`AlignmentRun::report`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunReport {
    /// The frames' size, in pixels, which the transforms are in.
    pub width: usize,
    pub height: usize,
    /// The end of the stack with the narrower field of view, when the frames' scales tell.
    pub narrow_end: Option<End>,
    /// The end the stack is processed from.
    pub start: End,
    /// The run's frames, in the order the stack is processed.
    pub order: Vec<FrameId>,
    /// The frame at the narrow end, whose field of view narrow framing keeps.
    pub narrow_frame: Option<FrameId>,
}

/// The edit that records an alignment run: `alignment`, of the frames `ids` (in the order it aligned them), ordered as
/// `order` says, with the settings it ran with. Every other frame of `project` loses its alignment and report, so that
/// the frames with an alignment are the run's.
pub fn alignment_edit(
    project: &Project,
    ids: &[FrameId],
    alignment: &StackAlignment,
    order: &ProcessingOrder,
    settings: &AlignSettings,
) -> Result<Edit, ProjectDataError> {
    let id = |i: usize| ids.get(i).copied().ok_or(AlignError::Master { master: i, frames: ids.len() });
    let report = RunReport {
        width: alignment.width,
        height: alignment.height,
        narrow_end: order.narrow,
        start: order.start,
        order: order.frames.iter().map(|&i| id(i)).collect::<Result<_, _>>()?,
        narrow_frame: order.narrow_frame.map(id).transpose()?,
    };
    let run = AlignmentRun {
        master: id(alignment.master)?,
        settings: to_data("alignment settings", settings)?,
        report: Some(to_data("alignment run", &report)?),
    };
    let mut frames = Vec::with_capacity(project.frames.len());
    for frame in &project.frames {
        let found = ids.iter().position(|&i| i == frame.id()).and_then(|i| alignment.frames.get(i));
        let (alignment, report) = match found {
            Some(f) => {
                let report = FrameReport {
                    status: f.status,
                    flags: f.flags.clone(),
                    reference: f.reference.map(id).transpose()?,
                    sharp_fraction: f.sharp_fraction,
                    motion: f.motion.as_ref().map(|m| FrameMotion { score: m.score, peak: m.peak, regions: m.regions.clone() }),
                };
                (Some(Alignment { transform: f.transform, brightness_gain: f.gain, quality: f.score }), Some(to_data("frame alignment", &report)?))
            }
            None => (None, None),
        };
        frames.push(AlignedFrame { frame: frame.id(), alignment, report });
    }
    Ok(Edit::SetAlignment { run: Some(run), frames })
}

/// A frame of an alignment run, as the project holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredFrame {
    pub frame: FrameId,
    pub alignment: Alignment,
    /// Its report; `None` when the project has none for it, or one this version can't read.
    pub report: Option<FrameReport>,
}

/// An alignment run, as the project holds it ([`stored_alignment`]).
#[derive(Clone, Debug, PartialEq)]
pub struct StoredAlignment {
    pub master: FrameId,
    pub settings: AlignSettings,
    pub report: RunReport,
    /// The run's frames, in the project's frame order.
    pub frames: Vec<StoredFrame>,
}

impl StoredAlignment {
    /// The output canvas `framing` gives the run's frames that weren't rejected (`florastack_align::Canvas`).
    pub fn canvas(&self, framing: Framing) -> Result<Canvas, AlignError> {
        let used: Vec<&StoredFrame> = self.frames.iter().filter(|f| f.report.as_ref().is_none_or(|r| r.status != Status::Rejected)).collect();
        let transforms: Vec<florastack_core::Affine> = used.iter().map(|f| f.alignment.transform).collect();
        let narrow = self.report.narrow_frame.and_then(|n| used.iter().position(|f| f.frame == n));
        Canvas::new(framing, &transforms, self.report.width, self.report.height, narrow)
    }
}

/// The project's alignment run, if it has one: its settings, its report and its frames.
pub fn stored_alignment(project: &Project) -> Result<Option<StoredAlignment>, ProjectDataError> {
    let Some(run) = &project.alignment else { return Ok(None) };
    let Some(report) = &run.report else { return Ok(None) };
    let report: RunReport = from_data("alignment run", report)?;
    let settings: AlignSettings = from_data("alignment settings", &run.settings)?;
    let frames = project
        .frames
        .iter()
        .filter_map(|f| {
            let alignment = f.alignment?;
            // A report this version can't read is left out, rather than failing the whole run.
            let report = f.report.as_ref().and_then(|r| from_data::<FrameReport>("frame alignment", r).ok());
            Some(StoredFrame { frame: f.id(), alignment, report })
        })
        .collect();
    Ok(Some(StoredAlignment { master: run.master, settings, report, frames }))
}

/// A frame's alignment report, if it has one this version can read.
pub fn frame_report(frame: &Frame) -> Option<FrameReport> {
    frame.report.as_ref().and_then(|r| from_data("frame alignment", r).ok())
}

/// The name of the stack's defect mask among the project's maps.
pub const DEFECTS: &str = "defects";

/// The defect mask's name, as a [`MapName`] (always a valid one).
pub fn defects_map() -> Option<MapName> {
    MapName::new(DEFECTS).ok()
}

/// Where a defect mask came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MaskSource {
    /// Detected from the frames (`florastack_align::DefectDetector`).
    Detected,
    /// Imported from an image file.
    Imported,
}

/// What a stored defect mask is: the summary its cache key holds, so that it can be told without reading the mask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MaskSummary {
    pub source: MaskSource,
    /// Its size, which must be the frames'.
    pub width: usize,
    pub height: usize,
    /// How many pixels are masked, in how many spots (8-connected groups).
    pub pixels: usize,
    pub spots: usize,
}

impl MaskSummary {
    pub fn of(mask: &DefectMask, source: MaskSource) -> MaskSummary {
        MaskSummary { source, width: mask.width(), height: mask.height(), pixels: mask.count(), spots: mask.spots().len() }
    }
}

/// Writes a defect mask into the project's cache, losslessly, as an 8-bit image of black spots on white, with its
/// summary as the key.
pub fn store_defect_mask(folder: &impl AsRef<CacheDir>, mask: &DefectMask, source: MaskSource) -> Result<CacheRef, ProjectDataError> {
    let key = serde_json::to_string(&MaskSummary::of(mask, source)).map_err(|source| ProjectError::Data { what: "defect mask summary", source })?;
    write_image(folder, DEFECTS, &AnyImage::U8(mask.to_image()?), Some(key))
}

/// The project's defect mask, if it has one.
pub fn defect_mask_ref(project: &Project) -> Option<&CacheRef> {
    project.maps.iter().find(|(name, _)| name.as_str() == DEFECTS).map(|(_, r)| r)
}

/// A stored mask's summary, from its key; `None` when the key isn't one.
pub fn mask_summary(reference: &CacheRef) -> Option<MaskSummary> {
    reference.key.as_deref().and_then(|k| serde_json::from_str(k).ok())
}

/// Reads a defect mask from the project's cache.
pub fn read_defect_mask(folder: &impl AsRef<CacheDir>, reference: &CacheRef) -> Result<DefectMask, ProjectDataError> {
    mask_from_image(read_image(folder, reference)?)
}

/// The defect mask an image draws: black spots on white (`florastack_align::DefectMask::from_image`), whatever its
/// sample type, on the values as stored.
pub fn mask_from_image(image: AnyImage) -> Result<DefectMask, ProjectDataError> {
    let image = match image {
        AnyImage::U8(i) => i.convert::<f32>().map_err(AlignError::from)?,
        AnyImage::U16(i) => i.convert::<f32>().map_err(AlignError::from)?,
        AnyImage::F32(i) => i,
    };
    Ok(DefectMask::from_image(&image)?)
}

/// Reads a defect mask from an image file the user made or edited: any format florastack-codecs reads, turned upright
/// as the frames are.
pub fn read_mask_file(path: &Path) -> Result<DefectMask, ProjectDataError> {
    let bytes = std::fs::read(path).map_err(|source| ProjectError::Read { path: path.to_path_buf(), source })?;
    mask_from_image(florastack_codecs::read(&bytes, &ReadOptions::default())?.image)
}

/// The key of a frame's preview: the frame's contents, the develop settings it was developed with and the defect mask
/// it was inpainted with. A preview whose key differs is out of date.
pub fn preview_key(frame: &Frame, develop: &DevelopSettings, mask: Option<&CacheRef>) -> Result<String, ProjectDataError> {
    let develop = serde_json::to_vec(develop).map_err(|source| ProjectError::Data { what: "develop settings", source })?;
    let mask = mask.map_or_else(|| "none".to_owned(), |m| m.hash.to_string());
    Ok(format!("frame {} develop {} defects {mask}", frame.hash, ContentHash::of(&develop)))
}

/// The key of a frame's aligned preview: its preview, its transform and the canvas, hashed.
pub fn aligned_preview_key(preview: &CacheRef, transform: &Affine, canvas: &Canvas) -> String {
    let text = format!("{} {:?} {} {} {:?}", preview.hash, transform.m, canvas.width, canvas.height, canvas.to_master.m);
    format!("aligned {}", ContentHash::of(text.as_bytes()))
}

/// Writes the preview of frame `id`, read and developed as `image` (and inpainted), keyed `key`.
pub fn write_frame_preview(folder: &impl AsRef<CacheDir>, id: FrameId, image: &Image<f32>, key: String) -> Result<CacheRef, ProjectDataError> {
    let small = thumbnail(image, PREVIEW_SIDE)?;
    write_preview(folder, &format!("frame-{}", id.get()), &small, Some(key))
}

/// Writes the aligned preview of frame `id`, from its preview: a `width` x `height` frame with alignment `transform`,
/// warped onto `canvas` at the preview's scale; what the frame doesn't cover is black.
pub fn write_aligned_preview(
    folder: &impl AsRef<CacheDir>,
    id: FrameId,
    preview: &CacheRef,
    (width, height): (usize, usize),
    transform: &Affine,
    canvas: &Canvas,
) -> Result<CacheRef, ProjectDataError> {
    let small = match read_image(folder, preview)? {
        AnyImage::U8(i) => i.to_linear_f32().map_err(AlignError::from)?,
        AnyImage::U16(i) => i.to_linear_f32().map_err(AlignError::from)?,
        AnyImage::F32(i) => i,
    };
    let aligned = aligned_thumbnail(&small, width, height, transform, canvas)?;
    write_preview(folder, &format!("frame-{}-aligned", id.get()), &aligned.image, Some(aligned_preview_key(preview, transform, canvas)))
}
