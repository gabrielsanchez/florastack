//! The outputs: listing, renaming and removing them, and saving one to an image file (a job).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use florastack_codecs::{BitDepth, OutputMetadata, OutputProfile, OutputSpec};
use florastack_core::{AnyImage, Monitor};
use florastack_project::{Edit, FrameId, Input, LayerId, MapName, OutputId, ProjectError, Value};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{absolute, check_name, check_path, display};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobPlan, Lane, commit};
use crate::project::extended_map;
use crate::registry::{CommandSpec, Effects, Meta, NoParams, Params, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("output.list", "List outputs", "Lists the outputs, oldest first: how each was made, its maps and its retouch layers.")
                .requires(Requires::Project),
            list,
        ),
        CommandSpec::instant(
            Meta::new("output.rename", "Rename output", "Renames an output.").undoable().examples(&[r#"{ "output": 4, "name": "Pyramid, grit 0.5" }"#]),
            rename,
        ),
        CommandSpec::instant(
            Meta::new("output.remove", "Remove output", "Removes an output from the project.")
                .help("Its cache files stay until the project is closed, so that undo can bring it back whole.")
                .undoable()
                .examples(&[r#"{ "output": 4 }"#]),
            remove,
        ),
        CommandSpec::job(
            Meta::new("output.save", "Save output", "Writes an output to an image file, in the format, depth and color profile given.")
                .help(
                    "The metadata (EXIF, IPTC, XMP) comes from `metadataFrom`, or by default from the first of the \
                     output's frames, with the frame count in XMP. With `extended_range` in the spec and a 32-bit \
                     float depth (TIFF or OpenEXR), values outside [0, 1] are kept: for an output whose tone \
                     compression brought it into range (the pyramid method's), the result from before it is written \
                     (\"retain extended range\"). With `map`, one of the output's maps (`output.list` names them) is \
                     written instead, such as a depth-map output's `depth` and `confidence` maps: maps are data, so \
                     the spec's profile must be `untagged`, and a 16-bit depth keeps every level. The file is written \
                     whole to a temporary file and then renamed, so a failed write leaves nothing behind; an existing \
                     file is replaced only with `overwrite`. Runs as a job, which can be cancelled until writing \
                     starts.",
                )
                .requires(Requires::Project)
                .effects(Effects::Files)
                .examples(&[
                    r#"{ "output": 4, "path": "C:/Stacks/bee.tif", "spec": { "format": { "format": "tiff", "compression": "deflate" }, "depth": "u16" } }"#,
                    r#"{ "output": 4, "path": "C:/Stacks/bee.jpg", "spec": { "format": { "format": "jpeg", "quality": 92, "subsampling": "s444" }, "depth": "u8", "profile": "srgb" }, "overwrite": true }"#,
                    r#"{ "output": 5, "path": "C:/Stacks/bee.exr", "spec": { "format": { "format": "open-exr", "compression": "zip", "half": false }, "depth": "f32", "extended_range": true } }"#,
                    r#"{ "output": 6, "map": "depth", "path": "C:/Stacks/bee-depth.tif", "spec": { "format": { "format": "tiff", "compression": "deflate" }, "depth": "u16", "profile": "untagged" } }"#,
                ]),
            Lane::Heavy,
            save,
        ),
    ]
}

/// A retouch layer, listed.
#[derive(Debug, Serialize, JsonSchema)]
struct LayerInfo {
    id: LayerId,
    name: String,
    visible: bool,
    /// How many strokes it holds.
    strokes: usize,
}

/// An output, listed.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct OutputInfo {
    id: OutputId,
    name: String,
    /// The method's name, such as `pyramid`.
    method: String,
    /// The method's parameters.
    parameters: Value,
    /// When it was made, RFC 3339.
    created: String,
    /// The frames it was made from, in order.
    frames: Vec<FrameId>,
    /// The outputs it was made from (slabs).
    outputs: Vec<OutputId>,
    /// The names of its maps, such as `depth`.
    maps: Vec<String>,
    preview: bool,
    /// Its retouch layers, bottom first.
    layers: Vec<LayerInfo>,
}

/// The outputs.
#[derive(Debug, Serialize, JsonSchema)]
struct OutputList {
    outputs: Vec<OutputInfo>,
}

fn list(session: &Session, _: NoParams) -> Result<OutputList, CommandError> {
    let outputs = session
        .project()?
        .outputs
        .iter()
        .map(|o| OutputInfo {
            id: o.id(),
            name: o.name.clone(),
            method: o.method.clone(),
            parameters: o.parameters.clone(),
            created: o.created().to_string(),
            frames: o.inputs.iter().filter_map(|i| if let Input::Frame(f) = i { Some(*f) } else { None }).collect(),
            outputs: o.inputs.iter().filter_map(|i| if let Input::Output(f) = i { Some(*f) } else { None }).collect(),
            maps: o.maps.keys().map(|m| m.as_str().to_owned()).collect(),
            preview: o.preview.is_some(),
            layers: o.retouch.iter().map(|l| LayerInfo { id: l.id(), name: l.name.clone(), visible: l.visible, strokes: l.strokes.len() }).collect(),
        })
        .collect();
    Ok(OutputList { outputs })
}

/// Renames an output.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RenameParams {
    /// The output's id, as `output.list` lists it.
    output: OutputId,
    /// Its new name: at most 200 characters, no control characters.
    name: String,
}

impl Params for RenameParams {
    fn check(&self) -> Result<(), Invalid> {
        check_name("name", &self.name)
    }
}

/// An output's name now.
#[derive(Debug, Serialize, JsonSchema)]
struct Renamed {
    output: OutputId,
    name: String,
}

fn find(session: &Session, output: OutputId) -> Result<&florastack_project::Output, CommandError> {
    session.project()?.output(output).ok_or_else(|| CommandError::NotFound { what: output.to_string() })
}

fn rename(session: &mut Session, p: RenameParams) -> Result<Renamed, CommandError> {
    if find(session, p.output)?.name != p.name {
        session.apply(Edit::RenameOutput { output: p.output, name: p.name.clone() })?;
    }
    Ok(Renamed { output: p.output, name: p.name })
}

/// An output.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OutputParams {
    /// The output's id, as `output.list` lists it.
    output: OutputId,
}

impl Params for OutputParams {}

/// The output removed.
#[derive(Debug, Serialize, JsonSchema)]
struct Removed {
    output: OutputId,
}

fn remove(session: &mut Session, p: OutputParams) -> Result<Removed, CommandError> {
    find(session, p.output)?;
    session.apply(Edit::RemoveOutput(p.output))?;
    Ok(Removed { output: p.output })
}

/// Saves an output to an image file.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SaveParams {
    /// The output's id, as `output.list` lists it.
    output: OutputId,
    /// The file to write.
    path: String,
    /// The format, its options, the bit depth and the color profile.
    spec: OutputSpec,
    /// The frame whose metadata the file gets; by default the first of the output's frames.
    #[serde(default)]
    metadata_from: Option<FrameId>,
    /// Copy that frame's metadata (EXIF, IPTC, XMP) into the file; on by default. Off, the file has only what
    /// FloraStack writes itself (the frame count).
    #[serde(default = "yes")]
    metadata: bool,
    /// One of the output's maps to write instead of the output, such as `depth` or `confidence`; the spec's profile
    /// must then be `untagged`.
    #[serde(default)]
    map: Option<String>,
    /// Replace the file if it exists.
    #[serde(default)]
    overwrite: bool,
}

impl Params for SaveParams {
    fn check(&self) -> Result<(), Invalid> {
        check_path("path", &self.path)?;
        if let Some(map) = &self.map {
            MapName::new(map).map_err(|e| Invalid::new("map", e.to_string()))?;
            if self.spec.profile != OutputProfile::Untagged {
                return Err(Invalid::new("spec", "a map is data, not a picture: write it with the profile \"untagged\", which keeps its values"));
            }
        }
        self.spec.validate().map_err(|e| Invalid::new("spec", e.to_string()))
    }
}

/// The file written.
#[derive(Debug, Serialize, JsonSchema)]
struct Written {
    path: String,
    /// Reproducibility manifest written beside this image.
    sidecar: String,
    /// Its size.
    bytes: u64,
    /// What the file couldn't hold or carry, such as metadata the format has no place for.
    warnings: Vec<String>,
}

/// Writes `bytes` to `path` through a temporary file in the same folder, renamed over it once complete.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), CommandError> {
    let write = |source| CommandError::from(ProjectError::Write { path: path.to_path_buf(), source });
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.{}.part", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    result.map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        write(e)
    })
}

/// Files staged beside their destinations, then published only after the job's final cancellation checkpoint.
#[derive(Default)]
pub(super) struct Publication {
    #[cfg(not(target_arch = "wasm32"))]
    files: Vec<(PathBuf, tempfile::NamedTempFile)>,
    #[cfg(target_arch = "wasm32")]
    files: Vec<(PathBuf, Vec<u8>)>,
}

impl Publication {
    pub(super) fn stage(&mut self, path: &Path, bytes: &[u8]) -> Result<(), CommandError> {
        let parent = path.parent().ok_or_else(|| super::scientific::unavailable("an export needs a parent folder"))?;
        let error = |source| florastack_project::ProjectError::Write { path: path.to_path_buf(), source };
        std::fs::create_dir_all(parent).map_err(error)?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut file = tempfile::Builder::new().prefix(".export-").suffix(".part").tempfile_in(parent).map_err(error)?;
            file.write_all(bytes).map_err(error)?;
            file.as_file().sync_all().map_err(error)?;
            self.files.push((path.to_path_buf(), file));
        }
        #[cfg(target_arch = "wasm32")]
        self.files.push((path.to_path_buf(), bytes.to_vec()));
        Ok(())
    }

    pub(super) fn publish(self, ctx: &crate::jobs::JobContext, overwrite: bool) -> Result<(), CommandError> {
        if !overwrite && let Some((path, _)) = self.files.iter().find(|(p, _)| p.exists()) {
            return Err(florastack_project::ProjectError::Exists { path: path.clone() }.into());
        }
        ctx.finalize()?;
        for (path, file) in self.files {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let result = if overwrite { file.persist(&path) } else { file.persist_noclobber(&path) };
                result.map_err(|e| florastack_project::ProjectError::Write { path, source: e.error })?;
            }
            #[cfg(target_arch = "wasm32")]
            write_atomically(&path, &file)?;
        }
        Ok(())
    }
}

/// Where a retouched output's pixels come from.
enum Retouched {
    /// The result open for retouching, as it is.
    Painted(florastack_core::Image<f32>),
    /// The result cached when the project was saved.
    Cached(florastack_project::CacheRef),
    /// The strokes, to paint again.
    Paint { strokes: Vec<florastack_retouch::Stroke>, loader: Box<crate::retouch::SourceLoader> },
}

/// What a file of an output is written from: one of its maps, its result from before tone compression (extended
/// range), or its result, retouched when it has visible strokes. Chosen when the job is planned, read when it runs.
pub(super) struct Pixels {
    image: florastack_project::CacheRef,
    base: florastack_project::CacheRef,
    retouched: Option<Retouched>,
}

impl Pixels {
    /// The pixels of output `id` for a file with `spec`: `map`, or the result (see [`Pixels`]); what the choice
    /// leaves out goes into `notes`.
    pub(super) fn of(session: &Session, id: OutputId, map: Option<&str>, spec: OutputSpec, notes: &mut Vec<String>) -> Result<Pixels, CommandError> {
        let output = find(session, id)?;
        // "Retain extended range": the output's result from before its tone compression, when it has one.
        let extended = extended_map().and_then(|name| output.maps.get(&name)).cloned();
        let image = match (map, extended) {
            (Some(map), _) => {
                let name = MapName::new(map).map_err(|e| session.invalid("map", e.to_string()))?;
                output.maps.get(&name).cloned().ok_or_else(|| {
                    let names: Vec<&str> = output.maps.keys().map(MapName::as_str).collect();
                    session.invalid(
                        "map",
                        format!(
                            "output {id} has no map {map}: {}",
                            if names.is_empty() { "it has none".to_owned() } else { format!("it has {}", names.join(", ")) }
                        ),
                    )
                })?
            }
            (None, Some(extended)) if spec.extended_range && spec.depth == BitDepth::F32 => extended,
            (None, Some(_)) if spec.extended_range => {
                notes.push("extended range is kept in 32-bit float files only: this file holds the output brought into [0, 1]".to_owned());
                output.image.clone()
            }
            (None, _) => output.image.clone(),
        };
        // A retouched output is written with its visible strokes: the result open for retouching, the result cached,
        // or the strokes painted again.
        let strokes = crate::retouch::visible_strokes(output);
        let retouched = if map.is_none() && !strokes.is_empty() {
            if image != output.image {
                notes.push("a retouched output is written as retouched, brought into [0, 1]: its extended range isn't retouched".to_owned());
            }
            let key = crate::retouch::render_key(output, &strokes);
            let open = session.retouch.as_ref().filter(|r| r.output == id && r.live.is_none() && r.painted == strokes);
            let cached = output.maps.get(&crate::retouch::map_name(crate::retouch::RETOUCHED)?).filter(|r| r.key.as_deref() == Some(key.as_str()));
            Some(match (open, cached) {
                (Some(open), _) => Retouched::Painted(open.canvas.clone()),
                (None, Some(reference)) => Retouched::Cached(reference.clone()),
                (None, None) => Retouched::Paint { strokes, loader: Box::new(crate::retouch::SourceLoader::new(session, "output.save", id)?) },
            })
        } else {
            None
        };
        Ok(Pixels { image, base: output.image.clone(), retouched })
    }

    /// Reads the pixels (painting the strokes again when they must be); what goes wrong on the way goes into `notes`.
    pub(super) fn read(self, cache: &florastack_project::CacheDir, notes: &mut Vec<String>) -> Result<AnyImage, CommandError> {
        Ok(match self.retouched {
            None => crate::project::read_image(cache, &self.image)?,
            Some(Retouched::Painted(canvas)) => AnyImage::F32(canvas),
            Some(Retouched::Cached(reference)) => crate::project::read_image(cache, &reference)?,
            Some(Retouched::Paint { strokes, mut loader }) => {
                let base = crate::retouch::read_f32(cache, &self.base)?;
                let mut sources = Vec::new();
                // The frames' warped images aren't kept here: writing a file changes nothing in the project.
                let _ = crate::retouch::load_sources(&strokes, &mut sources, &mut loader, (base.width(), base.height()));
                let usable = crate::retouch::paintable(&strokes, &sources);
                if usable.len() < strokes.len() {
                    notes.push(format!("{} of the strokes can't be painted, their source being gone", strokes.len() - usable.len()));
                }
                AnyImage::F32(florastack_retouch::render(&base, &crate::retouch::painted(&usable, &base, &sources))?)
            }
        })
    }
}

/// The metadata of the frame file at `path`, for a file written from it; what goes wrong goes into `notes`.
pub(super) fn frame_metadata(path: Option<&Path>, notes: &mut Vec<String>) -> Option<florastack_codecs::Metadata> {
    let frame = path?;
    match std::fs::read(frame) {
        Ok(bytes) => Some(florastack_codecs::read_metadata(&bytes)),
        Err(e) => {
            notes.push(format!("the metadata of {} can't be read, so the file has none: {e}", frame.display()));
            None
        }
    }
}

fn yes() -> bool {
    true
}

fn save(session: &Session, p: SaveParams) -> Result<JobPlan<Written>, CommandError> {
    let project = session.project()?;
    let folder = session.folder()?;
    let output = find(session, p.output)?;
    let source_state = super::scientific::source(project, output)?;
    let settings = super::export::output_settings(project)?;
    settings.validate().map_err(|e| session.invalid("settings", e))?;
    let layers = if p.map.is_none() && settings.annotations { super::annotation::layers(output)? } else { Vec::new() };
    let calibration = super::scientific::canvas_calibration(&source_state, output, settings.pixels_per_micrometer)?;
    let mut sidecar = super::reproduce::Sidecar::new(
        session,
        p.output,
        super::reproduce::Export::Save { spec: p.spec, map: p.map.clone(), metadata: p.metadata, metadata_from: p.metadata_from, settings },
    )?;
    let mut early = Vec::new();
    let pixels = Pixels::of(session, p.output, p.map.as_deref(), p.spec, &mut early)?;
    let frames = crate::retouch::output_frames(project, output);
    let source = match p.metadata_from {
        Some(id) => Some(source_state.frame(id).or_else(|| project.frame(id)).ok_or_else(|| CommandError::NotFound { what: id.to_string() })?),
        None => frames.iter().find_map(|id| source_state.frame(*id)),
    };
    let source: Option<PathBuf> = source.map(|f| folder.frame_path(f));
    let frame_count = u32::try_from(frames.len()).unwrap_or(u32::MAX);
    let path = absolute(&p.path)?;
    let sidecar_path = super::reproduce::path(&path);
    if !p.overwrite && (path.exists() || sidecar_path.exists()) {
        let existing = if path.exists() { &path } else { &sidecar_path };
        return Err(session.unavailable(format!("{} exists: pass `overwrite` to replace it", existing.display())));
    }
    let cache = session.cache_dir()?;
    let spec = p.spec;
    let with_metadata = p.metadata;
    let plan = JobPlan::new(move |ctx| {
        ctx.stage(0);
        let mut warnings = early;
        sidecar.prepare(&cache)?;
        let pixels = super::annotation::render(pixels.read(&cache, &mut warnings)?, &layers, calibration)?;
        ctx.checkpoint()?;
        let metadata = if with_metadata { frame_metadata(source.as_deref(), &mut warnings) } else { None };
        let metadata = OutputMetadata { source: metadata, frame_count: Some(frame_count), modified: None };
        ctx.checkpoint()?;
        ctx.stage(1);
        let encoded = florastack_codecs::write(&pixels, &spec, &metadata)?;
        drop(pixels);
        ctx.stage(2);
        let mut publication = Publication::default();
        publication.stage(&path, &encoded.bytes)?;
        sidecar.artifact(&path, &path, if p.map.is_some() { "map" } else { "output" }, &encoded.bytes);
        publication.stage(&sidecar_path, &sidecar.bytes()?)?;
        publication.publish(ctx, p.overwrite)?;
        warnings.extend(encoded.warnings.iter().map(ToString::to_string));
        let written = Written { path: display(&path), sidecar: display(&sidecar_path), bytes: encoded.bytes.len() as u64, warnings };
        Ok(commit(move |_: &mut Session| Ok(written)))
    });
    Ok(plan.stages(&[("Reading", 0.2), ("Encoding", 0.7), ("Writing", 0.1)]))
}
