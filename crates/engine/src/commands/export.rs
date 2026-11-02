//! Exporting outputs with the project's output settings (the preferences' `output` group, [`OutputSettings`]): the
//! settings themselves (`export.get`, `export.set`, `export.reset`), the size the files will take (`export.estimate`),
//! and the export (`export.run`, a job): the output under a file name made from a template, with its metadata copied
//! from one of its frames, and, alongside, its depth and confidence maps and its frames aligned onto its canvas.

use florastack_codecs::{
    BitDepth, ChromaSubsampling, DngCompression, DngLook, ExrCompression, OutputFormat, OutputProfile, OutputSpec, TiffCompression,
};
use florastack_project::{Edit, FrameId, Group, OutputId, Preferences, Project, Table};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::check_path;
use crate::error::{CommandError, Invalid};
use crate::jobs::Lane;
use crate::registry::{CommandSpec, Effects, Meta, NoParams, Params, Requires};
use crate::retouch::output_frames;
use crate::session::Session;

/// The tokens a file name template may hold.
pub const TOKENS: [&str; 8] = ["project", "name", "method", "frames", "first", "last", "date", "id"];
/// The longest file name a template may make, in characters.
const MAX_NAME: usize = 200;

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("export.get", "Export settings", "The project's output settings: format, depth, profile, metadata, file names and extras.")
                .requires(Requires::Project),
            get,
        ),
        CommandSpec::instant(
            Meta::new("export.set", "Set export settings", "Changes some of the project's output settings.")
                .help(
                    "Only the keys given change, as when a preset is applied. `format`: `tiff`, `png`, `jpeg`, `dng` or \
                     `open-exr`; `depth`: `u8`, `u16` or `f32`, as the format holds; then each format's options \
                     (`tiff_compression`, `exr_compression` and `exr_half`, `jpeg_quality` and `jpeg_subsampling`, \
                     `dng_compression` and `dng_look`); `profile`: `input` (the frames' own), `srgb`, `{\"wide\": \
                     \"pro-photo\"}` or `{\"wide\": \"rec2020\"}`, or `untagged`; `metadata` and `metadata_frame` (the \
                     output's frame, from 1 in the filmstrip's order, whose EXIF, IPTC and XMP the file gets); \
                     `extended_range` (32-bit float files keep values beyond [0, 1]); `template`, the file name without \
                     its extension, with the tokens {project}, {name}, {method}, {frames}, {first}, {last}, {date} and \
                     {id}; and `depth_map`, `confidence_map` and `aligned_frames`, written alongside. Scientific \
                     options: `calibrated_depth` with signed `z_step_micrometers` and `z_origin_micrometers`, \
                     `contribution_maps`, `alignment_csv`, `annotations`, and `pixels_per_micrometer` with \
                     `calibration_name`. Null clears an optional step or lateral calibration. The keys are the preset files' `[output]` keys.",
                )
                .undoable()
                .examples(&[
                    r#"{ "format": "jpeg", "depth": "u8", "jpeg_quality": 92, "profile": "srgb" }"#,
                    r#"{ "template": "{project}-{method}-{date}", "depth_map": true }"#,
                ]),
            set,
        ),
        CommandSpec::instant(Meta::new("export.reset", "Reset export settings", "Puts every output setting back at its default.").undoable(), reset),
        CommandSpec::query(
            Meta::new("export.estimate", "Estimate export size", "About how many bytes exporting an output will write, and in how many files.")
                .help(
                    "From the output's size and the settings (the project's, or the ones given): exact for uncompressed \
                     files, an estimate from typical compression otherwise, with the extras the settings ask for.",
                )
                .requires(Requires::Project)
                .examples(&[r#"{ "output": 3 }"#, r#"{ "output": 3, "settings": { "format": "jpeg", "depth": "u8" } }"#]),
            estimate,
        ),
        CommandSpec::job(
            Meta::new("export.run", "Export output", "Writes an output into a folder with the output settings, and its extras alongside.")
                .help(
                    "The file is named by the settings' template, with the format's extension; the depth and \
                     confidence maps (a depth-map output's) are written beside it as 16-bit TIFF files ending in \
                     `-depth` and `-confidence`, and the frames aligned onto the output's canvas as 16-bit TIFF files in \
                     a folder ending in ` aligned`. Settings given apply to this export only, on top of the project's. \
                     Existing files are refused unless `overwrite`. Each file is written whole and then renamed, so a \
                     failure leaves no partial file. Scientific extras include a float TIFF in micrometers, \
                     per-frame contribution maps and alignment CSV; visible annotation layers are rendered only \
                     onto the output copy. Every export includes a settings/version/input-hash JSON sidecar, \
                     published last; `export.reproduce` replays it. Runs as a job.",
                )
                .requires(Requires::Project)
                .effects(Effects::Files)
                .examples(&[
                    r#"{ "output": 3, "folder": "C:/Stacks/Exports" }"#,
                    r#"{ "output": 3, "folder": "C:/Stacks", "settings": { "aligned_frames": true }, "overwrite": true }"#,
                ]),
            Lane::Heavy,
            run,
        ),
    ]
}

/// A file format, as the output settings name it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FileFormat {
    #[default]
    Tiff,
    Png,
    Jpeg,
    Dng,
    OpenExr,
}

impl FileFormat {
    pub fn extension(self) -> &'static str {
        match self {
            FileFormat::Tiff => "tif",
            FileFormat::Png => "png",
            FileFormat::Jpeg => "jpg",
            FileFormat::Dng => "dng",
            FileFormat::OpenExr => "exr",
        }
    }
}

/// What outputs are exported as: the preferences' `output` group, which presets carry. See `export.set`'s help.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Independent switches, as presets and the Export tab set them.
pub struct OutputSettings {
    /// `tiff`, `png`, `jpeg`, `dng` or `open-exr`.
    pub format: FileFormat,
    /// `u8`, `u16` or `f32`, as the format holds: JPEG 8-bit, PNG 8 or 16, TIFF 8, 16 or 32-bit float, DNG 16,
    /// OpenEXR 32-bit float.
    pub depth: BitDepth,
    /// TIFF's compression: `none`, `lzw` or `deflate`.
    pub tiff_compression: TiffCompression,
    /// OpenEXR's compression: `none`, `rle`, `zips`, `zip` or `piz`.
    pub exr_compression: ExrCompression,
    /// OpenEXR with half floats rather than full ones.
    pub exr_half: bool,
    /// JPEG's quality, from 1 to 100.
    #[schemars(range(min = 1, max = 100))]
    pub jpeg_quality: u8,
    /// JPEG's chroma subsampling: `s444`, `s422` or `s420`.
    pub jpeg_subsampling: ChromaSubsampling,
    /// DNG's compression: `uncompressed` or `lossless-jpeg`.
    pub dng_compression: DngCompression,
    /// DNG's look: `rendered` or `raw`.
    pub dng_look: DngLook,
    /// The color profile: `input` (the frames' own), `srgb`, `{"wide": "pro-photo"}`, `{"wide": "rec2020"}` or
    /// `untagged`.
    pub profile: OutputProfile,
    /// Copy a frame's metadata (EXIF, IPTC, XMP) into the file.
    pub metadata: bool,
    /// The frame whose metadata is copied: its position among the output's frames in the project's order (the
    /// filmstrip's), from 1 (the first); beyond the last, the last.
    #[schemars(range(min = 1))]
    pub metadata_frame: usize,
    /// In 32-bit float files, keep values beyond [0, 1] (the result from before tone compression).
    pub extended_range: bool,
    /// The file's name without its extension: {project}, {name}, {method}, {frames}, {first}, {last}, {date} and
    /// {id} are filled in.
    pub template: String,
    /// Write the depth map (a depth-map output's) alongside, as a 16-bit TIFF.
    pub depth_map: bool,
    /// Write the confidence map (a depth-map output's) alongside, as a 16-bit TIFF.
    pub confidence_map: bool,
    /// Write every frame aligned onto the output's canvas alongside, as 16-bit TIFF files in a folder.
    pub aligned_frames: bool,
    /// Write full-precision calibrated depth as float TIFF values in micrometers.
    pub calibrated_depth: bool,
    /// Signed capture step in micrometers, in the recorded capture order; unset when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub z_step_micrometers: Option<f64>,
    /// Z of the first captured frame, in micrometers.
    pub z_origin_micrometers: f64,
    /// Write a normalized 16-bit contribution TIFF per input of a depth-map output.
    pub contribution_maps: bool,
    /// Write master-to-input shift, scale, rotation, brightness gain and score as CSV.
    pub alignment_csv: bool,
    /// Render visible annotation layers on the output copy.
    pub annotations: bool,
    /// Lateral calibration in master-image pixels per micrometer; copied from a profile or measured directly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixels_per_micrometer: Option<f64>,
    /// Name of the selected objective or magnification, retained with its measured value.
    pub calibration_name: String,
}

impl Default for OutputSettings {
    fn default() -> Self {
        OutputSettings {
            format: FileFormat::Tiff,
            depth: BitDepth::U16,
            tiff_compression: TiffCompression::Deflate,
            exr_compression: ExrCompression::Zip,
            exr_half: false,
            jpeg_quality: 95,
            jpeg_subsampling: ChromaSubsampling::S444,
            dng_compression: DngCompression::LosslessJpeg,
            dng_look: DngLook::Rendered,
            profile: OutputProfile::Input,
            metadata: true,
            metadata_frame: 1,
            extended_range: false,
            template: "{project} {name}".to_owned(),
            depth_map: false,
            confidence_map: false,
            aligned_frames: false,
            calibrated_depth: false,
            z_step_micrometers: None,
            z_origin_micrometers: 0.0,
            contribution_maps: false,
            alignment_csv: false,
            annotations: true,
            pixels_per_micrometer: None,
            calibration_name: String::new(),
        }
    }
}

impl OutputSettings {
    /// The codecs' spec for the main file.
    pub fn spec(&self) -> OutputSpec {
        let format = match self.format {
            FileFormat::Tiff => OutputFormat::Tiff { compression: self.tiff_compression },
            FileFormat::Png => OutputFormat::Png,
            FileFormat::Jpeg => OutputFormat::Jpeg { quality: self.jpeg_quality, subsampling: self.jpeg_subsampling },
            FileFormat::Dng => OutputFormat::Dng { compression: self.dng_compression, look: self.dng_look },
            FileFormat::OpenExr => OutputFormat::OpenExr { compression: self.exr_compression, half: self.exr_half },
        };
        OutputSpec { format, depth: self.depth, profile: self.profile, extended_range: self.extended_range }
    }

    /// Checks the settings: a depth the format holds, options in range, a template with known tokens.
    pub fn validate(&self) -> Result<(), String> {
        self.spec().validate().map_err(|e| e.to_string())?;
        // The other formats' options too, as a preset carries them all.
        if !(1..=100).contains(&self.jpeg_quality) {
            return Err("jpeg_quality must be from 1 to 100".to_owned());
        }
        if self.metadata_frame == 0 {
            return Err("metadata_frame counts from 1".to_owned());
        }
        if self.z_step_micrometers.is_some_and(|v| !v.is_finite() || v == 0.0 || v.abs() > 1e12) {
            return Err("z_step_micrometers must be finite, nonzero and at most 1e12 in magnitude".into());
        }
        if !self.z_origin_micrometers.is_finite() || self.z_origin_micrometers.abs() > 1e12 {
            return Err("z_origin_micrometers must be finite and at most 1e12 in magnitude".into());
        }
        if self.pixels_per_micrometer.is_some_and(|v| !v.is_finite() || !(1e-9..=1e9).contains(&v)) {
            return Err("pixels_per_micrometer must be finite and from 1e-9 to 1e9".into());
        }
        if self.calibration_name.chars().count() > 200 || self.calibration_name.chars().any(char::is_control) {
            return Err("calibration_name must have at most 200 characters and no control characters".into());
        }
        check_template(&self.template)
    }
}

/// Checks a file name template: not empty, at most 200 characters, its tokens known.
pub fn check_template(template: &str) -> Result<(), String> {
    if template.trim().is_empty() || template.chars().count() > MAX_NAME {
        return Err("the template must hold from 1 to 200 characters".to_owned());
    }
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { return Err("the template has a `{` without its `}`".to_owned()) };
        let token = &after[..close];
        if !TOKENS.contains(&token) {
            return Err(format!("the template's token {{{token}}} is unknown: use {}", TOKENS.map(|t| format!("{{{t}}}")).join(", ")));
        }
        rest = &after[close + 1..];
    }
    Ok(())
}

/// The template filled in with `values` (token, value), made safe as a file name on every system.
pub fn file_name(template: &str, values: &[(&str, String)]) -> String {
    let mut name = template.to_owned();
    for (token, value) in values {
        name = name.replace(&format!("{{{token}}}"), value);
    }
    let cleaned: String = name.chars().map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c }).take(MAX_NAME).collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']).trim_start_matches('.');
    if trimmed.is_empty() { "output".to_owned() } else { trimmed.to_owned() }
}

/// The project's output settings.
pub fn output_settings(project: &Project) -> Result<OutputSettings, CommandError> {
    Ok(project.preferences.get(Group::Output)?)
}

/// The settings for one export: the project's, with `overrides` on top, checked.
fn merged(session: &Session, overrides: Option<&Table>) -> Result<OutputSettings, CommandError> {
    let mut preferences = session.project()?.preferences.only(&[Group::Output]);
    if let Some(table) = overrides {
        clear_optional(&mut preferences, table)?;
        let mut top = Preferences::new();
        top.set(Group::Output, table)?;
        preferences.layer(&top);
    }
    let settings: OutputSettings = preferences.get(Group::Output)?;
    settings.validate().map_err(|e| session.invalid("settings", e))?;
    Ok(settings)
}

/// The settings.
#[derive(Debug, Serialize, JsonSchema)]
struct Settings {
    settings: OutputSettings,
}

fn get(session: &Session, _: NoParams) -> Result<Settings, CommandError> {
    Ok(Settings { settings: output_settings(session.project()?)? })
}

/// Some output settings: any of [`OutputSettings`]' keys, checked as such.
#[derive(Debug)]
struct SettingsPatch(Table);

impl JsonSchema for SettingsPatch {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        OutputSettings::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        OutputSettings::json_schema(generator)
    }
}

impl<'de> Deserialize<'de> for SettingsPatch {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let table = Table::deserialize(deserializer)?;
        // Every key must be one of the settings' (with its type): checked whole once merged.
        serde_json::from_value::<OutputSettings>(serde_json::Value::Object(table.clone())).map_err(serde::de::Error::custom)?;
        Ok(SettingsPatch(table))
    }
}

impl Params for SettingsPatch {}

fn set_table(session: &mut Session, table: Table) -> Result<Settings, CommandError> {
    let mut preferences = Preferences::new();
    preferences.set(Group::Output, &table)?;
    let settings: OutputSettings = preferences.get(Group::Output)?;
    settings.validate().map_err(|e| session.invalid("settings", e))?;
    if session.project()?.preferences.group(Group::Output) != preferences.group(Group::Output) {
        session.apply(Edit::SetPreferences { group: Group::Output, table: preferences.group(Group::Output).clone() })?;
    }
    Ok(Settings { settings })
}

fn set(session: &mut Session, patch: SettingsPatch) -> Result<Settings, CommandError> {
    let mut top = Preferences::new();
    top.set(Group::Output, &patch.0)?;
    let mut merged = session.project()?.preferences.only(&[Group::Output]);
    clear_optional(&mut merged, &patch.0)?;
    merged.layer(&top);
    set_table(session, merged.group(Group::Output).clone())
}

/// Null clears optional calibrations instead of inheriting a previously measured value.
fn clear_optional(preferences: &mut Preferences, patch: &Table) -> Result<(), CommandError> {
    let mut table = preferences.group(Group::Output).clone();
    for key in ["pixels_per_micrometer", "z_step_micrometers"] {
        if patch.get(key).is_some_and(serde_json::Value::is_null) {
            table.remove(key);
        }
    }
    preferences.set(Group::Output, &table)?;
    Ok(())
}

fn reset(session: &mut Session, _: NoParams) -> Result<Settings, CommandError> {
    set_table(session, Table::new())
}

/// An output, with settings for this export alone.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EstimateParams {
    /// The output, as `output.list` lists it.
    output: OutputId,
    /// Settings on top of the project's, for this estimate only.
    #[serde(default)]
    settings: Option<SettingsPatch>,
}

impl Params for EstimateParams {}

/// What an export will write.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Estimate {
    /// About how many bytes, every file included.
    bytes: u64,
    /// How many files.
    files: usize,
    /// Whether `bytes` is a guess (compressed files), rather than known but for the headers, the profile and the
    /// metadata (a few KiB).
    approximate: bool,
    /// The main file's name.
    name: String,
}

/// The output's size in pixels and channels: from its canvas as the project's alignment frames it, else from its
/// file's header.
fn output_shape(session: &Session, id: OutputId) -> Result<(u64, u64, u64), CommandError> {
    let project = session.project()?;
    let output = project.output(id).ok_or(CommandError::NotFound { what: id.to_string() })?;
    let framing = crate::retouch::output_framing(output);
    let channels = if framing == florastack_align::Framing::Wide { 4 } else { 3 };
    let source = super::scientific::source(project, output)?;
    if let Ok(Some(stored)) = crate::project::stored_alignment(&source)
        && let Ok(canvas) = stored.canvas(framing)
    {
        return Ok((canvas.width as u64, canvas.height as u64, channels));
    }
    let bytes = session.cache_dir()?.read(&output.image)?;
    let info = florastack_codecs::probe(&bytes)?;
    Ok((info.width as u64, info.height as u64, info.channels as u64))
}

/// About how many bytes a file of `pixels` pixels and `channels` channels takes with `spec`, and whether that's exact.
fn file_bytes(pixels: u64, channels: u64, spec: OutputSpec) -> (u64, bool) {
    let sample = match spec.depth {
        BitDepth::U8 => 1.0,
        BitDepth::U16 => 2.0,
        BitDepth::F32 => 4.0,
    };
    #[allow(clippy::cast_precision_loss)] // A size, estimated.
    let raw = pixels as f64 * channels as f64 * sample;
    let (ratio, exact) = match spec.format {
        OutputFormat::Tiff { compression: TiffCompression::None } | OutputFormat::Dng { compression: DngCompression::Uncompressed, .. } => {
            (1.0, true)
        }
        OutputFormat::Tiff { compression: TiffCompression::Lzw } => (0.75, false),
        OutputFormat::Tiff { compression: TiffCompression::Deflate } | OutputFormat::Png => {
            (if spec.depth == BitDepth::F32 { 0.85 } else { 0.6 }, false)
        }
        OutputFormat::OpenExr { compression, half } => {
            let half = if half { 0.5 } else { 1.0 };
            let ratio = match compression {
                ExrCompression::None => 1.0,
                ExrCompression::Rle => 0.9,
                ExrCompression::Zips | ExrCompression::Zip => 0.7,
                ExrCompression::Piz => 0.65,
            };
            (ratio * half, compression == ExrCompression::None)
        }
        OutputFormat::Dng { .. } => (0.55, false),
        OutputFormat::Jpeg { quality, .. } => {
            // Bits per pixel grow steeply with the quality: about 1 at 75, 3.5 at 95.
            let q = f64::from(quality) / 100.0;
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A size, estimated, positive.
            return (((0.6 + 3.4 * q * q * q) * pixels as f64 / 8.0) as u64, false);
        }
    };
    // The headers, the profile and the metadata: a few KiB, unless a camera's EXIF is large.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A size, estimated, positive.
    ((raw * ratio) as u64 + 4096, exact)
}

/// The spec of the files written alongside: 16-bit TIFF, Deflate, with `profile`.
fn extra_spec(profile: OutputProfile) -> OutputSpec {
    OutputSpec { format: OutputFormat::Tiff { compression: TiffCompression::Deflate }, depth: BitDepth::U16, profile, extended_range: false }
}

/// The values of a template's tokens for output `id`.
fn template_values(session: &Session, id: OutputId) -> Result<Vec<(&'static str, String)>, CommandError> {
    let project = session.project()?;
    let output = project.output(id).ok_or(CommandError::NotFound { what: id.to_string() })?;
    let folder = session.folder()?;
    let frames = frames_in_order(project, output);
    let stem = |f: Option<&FrameId>| {
        f.and_then(|f| project.frame(*f)).and_then(|f| folder.frame_path(f).file_stem().map(|s| s.to_string_lossy().into_owned())).unwrap_or_default()
    };
    let created = output.created().to_string();
    Ok(vec![
        ("project", project.name().unwrap_or("Untitled").to_owned()),
        ("name", output.name.clone()),
        ("method", output.method.clone()),
        ("frames", frames.len().to_string()),
        ("first", stem(frames.first())),
        ("last", stem(frames.last())),
        ("date", created.get(..10).unwrap_or(&created).to_owned()),
        ("id", id.get().to_string()),
    ])
}

/// The frames an output was made from, in the project's order (the filmstrip's), which `metadata_frame`, {first},
/// {last} and the aligned frames' numbers count in, rather than the order they were stacked in.
fn frames_in_order(project: &Project, output: &florastack_project::Output) -> Vec<FrameId> {
    let mut frames = output_frames(project, output);
    frames.sort_by_key(|id| project.frames.iter().position(|f| f.id() == *id).unwrap_or(usize::MAX));
    frames
}

fn estimate(session: &Session, p: EstimateParams) -> Result<Estimate, CommandError> {
    let settings = merged(session, p.settings.as_ref().map(|s| &s.0))?;
    let (w, h, channels) = output_shape(session, p.output)?;
    let pixels = w.saturating_mul(h);
    let (mut bytes, mut exact) = file_bytes(pixels, channels, settings.spec());
    let mut files = 1;
    let project = session.project()?;
    let output = project.output(p.output).ok_or(CommandError::NotFound { what: p.output.to_string() })?;
    let map = |name: &str| florastack_project::MapName::new(name).is_ok_and(|n| output.maps.contains_key(&n));
    for (wanted, name) in [(settings.depth_map, "depth"), (settings.confidence_map, "confidence")] {
        if wanted && map(name) {
            let (b, _) = file_bytes(pixels, 1, extra_spec(OutputProfile::Untagged));
            bytes = bytes.saturating_add(b);
            files += 1;
            exact = false;
        }
    }
    if settings.aligned_frames {
        let n = output_frames(project, output).len() as u64;
        let (b, _) = file_bytes(pixels, 3, extra_spec(OutputProfile::Input));
        bytes = bytes.saturating_add(b.saturating_mul(n));
        files += usize::try_from(n).unwrap_or(usize::MAX);
        exact = false;
    }
    if settings.calibrated_depth && map("depth-frames") {
        let spec = OutputSpec { depth: BitDepth::F32, extended_range: true, ..extra_spec(OutputProfile::Untagged) };
        bytes = bytes.saturating_add(file_bytes(pixels, 1, spec).0);
        files += 1;
        exact = false;
    }
    if settings.contribution_maps && map("depth-frames") {
        let n = output.inputs.len();
        bytes = bytes.saturating_add(file_bytes(pixels, 1, extra_spec(OutputProfile::Untagged)).0.saturating_mul(n as u64));
        files += n;
        exact = false;
    }
    if settings.alignment_csv {
        bytes = bytes.saturating_add((output_frames(project, output).len() as u64).saturating_mul(256).saturating_add(128));
        files += 1;
        exact = false;
    }
    let sidecar = super::reproduce::Sidecar::new(session, p.output, super::reproduce::Export::Run { settings: settings.clone() })?;
    bytes = bytes.saturating_add(sidecar.estimated_bytes()?).saturating_add((files as u64).saturating_mul(300));
    files += 1;
    let name = format!("{}.{}", file_name(&settings.template, &template_values(session, p.output)?), settings.format.extension());
    Ok(Estimate { bytes, files, approximate: !exact, name })
}

/// An export.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RunParams {
    /// The output, as `output.list` lists it.
    output: OutputId,
    /// The folder the files go into, made if it doesn't exist.
    folder: String,
    /// Settings on top of the project's, for this export only.
    #[serde(default)]
    settings: Option<SettingsPatch>,
    /// Replace files that exist.
    #[serde(default)]
    overwrite: bool,
}

impl Params for RunParams {
    fn check(&self) -> Result<(), Invalid> {
        check_path("folder", &self.folder)
    }
}

/// A file written.
#[derive(Debug, Serialize, JsonSchema)]
struct FileWritten {
    path: String,
    bytes: u64,
    /// `output`, `depth`, `confidence` or `aligned`.
    what: &'static str,
}

/// What an export wrote.
#[derive(Debug, Serialize, JsonSchema)]
struct Exported {
    files: Vec<FileWritten>,
    /// What the files leave out or couldn't hold.
    warnings: Vec<String>,
}

mod files;
use files::run;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_checked_and_filled_in_safely() {
        assert!(check_template("{project}-{name}").is_ok());
        assert!(check_template("{nme}").unwrap_err().contains("{nme}"));
        assert!(check_template("open {name").is_err());
        assert!(check_template("  ").is_err());
        let values = [("project", "Bee: head".to_owned()), ("name", "Pyramid/1".to_owned())];
        assert_eq!(file_name("{project} - {name}", &values), "Bee_ head - Pyramid_1");
        assert_eq!(file_name("...", &values), "output", "nothing left is no name");
        assert_eq!(file_name("{name}. ", &values), "Pyramid_1");
    }

    #[test]
    fn sizes_are_estimated_from_the_format() {
        let mut settings = OutputSettings { tiff_compression: TiffCompression::None, ..OutputSettings::default() };
        let (bytes, exact) = file_bytes(1000, 3, settings.spec());
        assert_eq!((bytes, exact), (6000 + 4096, true), "uncompressed 16-bit TIFF: exactly the samples, and the headers");
        settings.format = FileFormat::Jpeg;
        settings.depth = BitDepth::U8;
        let (jpeg, exact) = file_bytes(24_000_000, 3, settings.spec());
        assert!(!exact && (8_000_000..14_000_000).contains(&jpeg), "{jpeg}");
        assert!(OutputSettings { format: FileFormat::Jpeg, ..OutputSettings::default() }.validate().is_err(), "16-bit JPEG");
        assert!(OutputSettings::default().validate().is_ok());
    }
}
