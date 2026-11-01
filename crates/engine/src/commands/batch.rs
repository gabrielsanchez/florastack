//! A serial queue of independent stack projects. Work stays in a private staging folder until publication.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use florastack_align::SplitSettings;
use florastack_core::{LockFile, Monitor, branding};
use florastack_project::{ProjectError, Timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{MAX_ITEMS, absolute, check_path, display};
use crate::compute::Compute;
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobContext, JobPlan, Lane, commit};
use crate::registry::{CommandSpec, Effects, Meta, Params};
use crate::{Dispatched, Engine, EngineConfig, Session};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::job(
        Meta::new("batch.run", "Process batch", "Queues stacks as separate projects, exports their outputs and writes a summary CSV.")
            .help(
                "Immediate subfolders are stacks; a folder with images directly inside is auto-split with project.findStacks. \
                   Mixed layouts are refused. Each stack receives the preset, a TOML file or a built-in one as \
                   `builtin:<id>` (`preset.list`), including depth-map contrast thresholds; each output is exported as the \
                   preset's output settings say (`export.run`: a 16-bit TIFF unless they say otherwise, with the maps and \
                   aligned frames they ask for), the template's extension giving way to their format's. \
                   Failures are recorded and the queue continues. Outputs and projects are staged until the cancellable \
                   work ends, then published without replacing existing files. summary.csv lists stack, frames, method, \
                   time in seconds, warnings, project, output and error. Templates accept {stack}, {method}, {range}, \
                   {first}, {last}, {date} (UTC YYYY-MM-DD), and {params} (the method's effective settings).",
            )
            .effects(Effects::Files)
            .examples(&[
                r#"{"input":"C:/Stacks","output":"C:/Results","preset":"C:/Presets/macro.toml"}"#,
                r#"{"input":"C:/Stacks","output":"C:/Results","preset":"builtin:insect-macro"}"#,
            ]),
        Lane::Heavy,
        run,
    )]
}

/// A stacking method for unattended processing.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Method {
    /// Laplacian pyramid fusion.
    #[default]
    Pyramid,
    /// Depth-map compositing with the preset's contrast threshold.
    DepthMap,
    /// Sharpness-weighted average.
    Average,
}

impl Method {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Pyramid => "pyramid",
            Self::DepthMap => "depth-map",
            Self::Average => "average",
        }
    }
    fn command(self) -> &'static str {
        match self {
            Self::Pyramid => "stack.pyramid",
            Self::DepthMap => "stack.depthMap",
            Self::Average => "stack.average",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::DepthMap => "depth_map",
            _ => self.name(),
        }
    }
}

pub(super) fn default_template() -> String {
    "{stack}-{method}-{range}.tif".into()
}

/// Queues a folder of stack folders, or auto-splits a folder of frames.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BatchParams {
    /// The folder of stack folders, or one folder of frames to auto-split.
    input: String,
    /// Destination for individual projects, TIFFs and summary.csv; existing entries are refused.
    output: String,
    /// TOML preset applied to each project, including its depth-map contrast threshold; or a built-in one as
    /// `builtin:<id>` (`preset.list`).
    preset: String,
    /// Method to run for each stack; pyramid by default.
    #[serde(default)]
    method: Method,
    /// A TIFF filename with {stack}, {method}, {range}, {first}, {last}, {date} and {params} tokens.
    #[serde(default = "default_template")]
    template: String,
    /// Boundary detection settings for a flat folder; omitted settings use their defaults.
    #[serde(default)]
    split: SplitSettings,
}

impl Params for BatchParams {
    fn check(&self) -> Result<(), Invalid> {
        check_paths(&self.input, &self.output, &self.preset, &self.template)?;
        self.split.validate().map_err(|e| Invalid::new("split", e.to_string()))
    }
}

pub(super) fn check_paths(input: &str, output: &str, preset: &str, template: &str) -> Result<(), Invalid> {
    check_path("input", input)?;
    check_path("output", output)?;
    match preset.strip_prefix(super::preset::BUILT_IN_PREFIX) {
        Some(id) if super::preset::BUILT_IN.iter().any(|(name, _)| *name == id) => {}
        Some(_) => {
            let ids = super::preset::BUILT_IN.map(|(id, _)| format!("{}{id}", super::preset::BUILT_IN_PREFIX));
            return Err(Invalid::new("preset", format!("names no built-in preset: there are {}", ids.join(", "))));
        }
        None => check_path("preset", preset)?,
    }
    filename(template, "stack", Method::Pyramid, &[PathBuf::from("first.tif"), PathBuf::from("last.tif")], &json!({}))?;
    Ok(())
}

/// One completed or failed queue entry.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct Entry {
    /// Stack name, unique within the queue.
    pub stack: String,
    /// Number of input frames.
    pub frames: usize,
    /// Stacking method.
    pub method: String,
    /// Elapsed processing time in seconds.
    pub time: f64,
    /// Alignment flags, skipped inputs and export warnings.
    pub warnings: Vec<String>,
    /// Saved project folder, on success.
    pub project: Option<String>,
    /// Exported TIFF, on success.
    pub output: Option<String>,
    /// Why the entry failed, when it did.
    pub error: Option<String>,
}

/// The queue's summary and per-stack results.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchResult {
    /// The CSV summary file.
    pub summary: String,
    /// Each queue entry, in processing order.
    pub stacks: Vec<Entry>,
    /// Number of failed entries; a CLI reports failure when nonzero.
    pub failed: usize,
}

pub(super) fn unavailable(command: &str, reason: impl Into<String>) -> CommandError {
    CommandError::Unavailable { command: command.into(), reason: reason.into() }
}

pub(super) fn io_error(path: &Path, source: std::io::Error) -> CommandError {
    ProjectError::Write { path: path.to_path_buf(), source }.into()
}

/// Waits for a child command, forwarding cancellation, including to its running worker before staging is removed.
pub(super) fn call(engine: &mut Engine, ctx: &JobContext, name: &str, params: Value) -> Result<Value, CommandError> {
    ctx.checkpoint()?;
    let id = match engine.dispatch(name, params)? {
        Dispatched::Done(value) => return Ok(value),
        Dispatched::Started(id) => id,
    };
    loop {
        engine.poll();
        if ctx.is_cancelled() {
            engine.cancel(id);
        }
        if engine.job(id).is_none_or(|job| job.state.is_finished()) {
            let value = engine.wait(id)?;
            ctx.checkpoint()?;
            return Ok(value);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// All artifacts stay beneath this folder until the parent's point of no return.
pub(super) struct Workspace {
    pub output: PathBuf,
    pub stage: tempfile::TempDir,
    _lock: LockFile,
}

impl Workspace {
    pub(super) fn new(input: &str, output: &str, preset: &str) -> Result<Self, CommandError> {
        let input = std::fs::canonicalize(absolute(input)?).map_err(|e| io_error(Path::new(input), e))?;
        if !input.is_dir() {
            return Err(unavailable("batch.run", "input must be a folder"));
        }
        super::preset::load(preset)?;
        let output = absolute(output)?;
        std::fs::create_dir_all(&output).map_err(|e| io_error(&output, e))?;
        let output = std::fs::canonicalize(&output).map_err(|e| io_error(&output, e))?;
        if output.starts_with(&input) {
            return Err(unavailable("batch.run", "output must be outside the input folder so exports cannot be ingested as frames"));
        }
        let lock_path = output.join(".queue.lock");
        let lock = LockFile::try_acquire(&lock_path)
            .map_err(|e| io_error(&lock_path, e))?
            .ok_or_else(|| unavailable("batch.run", "another batch or capture is using this output folder"))?;
        if output.join("summary.csv").exists() {
            return Err(ProjectError::Exists { path: output.join("summary.csv") }.into());
        }
        let stage = tempfile::Builder::new().prefix(".queue-").tempdir_in(&output).map_err(|e| io_error(&output, e))?;
        Ok(Self { output, stage, _lock: lock })
    }

    /// An engine for the workspace's projects, stacking as the parent engine does (`compute`, `tiling`).
    pub(super) fn engine(&self, capture: bool, compute: Compute, tiling: florastack_stack::Tiling) -> Result<Engine, CommandError> {
        let config = EngineConfig { temporary_projects: Some(self.stage.path().join("temporary")), compute, tiling, ..EngineConfig::default() };
        if !capture {
            return Ok(Engine::new(config));
        }
        let registry = crate::Registry::builder()
            .standard()
            .command(super::align::capture_spec())
            .build()
            .map_err(|e| unavailable("watch.run", e.to_string()))?;
        Ok(Engine::with_registry(config, std::sync::Arc::new(registry)))
    }

    pub(super) fn publish(&self, ctx: &JobContext, stacks: Vec<Entry>) -> Result<BatchResult, CommandError> {
        let mut csv = String::from("stack,frames,method,time,warnings,project,output,error\r\n");
        for entry in &stacks {
            let fields = [
                entry.stack.clone(),
                entry.frames.to_string(),
                entry.method.clone(),
                format!("{:.3}", entry.time),
                entry.warnings.join("; "),
                entry.project.clone().unwrap_or_default(),
                entry.output.clone().unwrap_or_default(),
                entry.error.clone().unwrap_or_default(),
            ];
            csv.push_str(&fields.iter().map(|s| format!("\"{}\"", s.replace('"', "\"\""))).collect::<Vec<_>>().join(","));
            csv.push_str("\r\n");
        }
        let summary = self.stage.path().join("summary.csv");
        std::fs::write(&summary, csv).map_err(|e| io_error(&summary, e))?;
        let mut files = children(self.stage.path())?;
        // The CSV is the completion record; publish it after its referenced artifacts.
        files.sort_by_key(|path| path.file_name().is_some_and(|name| name == "summary.csv"));
        for file in &files {
            let target = self.output.join(file.file_name().unwrap_or_default());
            if target.exists() {
                return Err(ProjectError::Exists { path: target }.into());
            }
        }
        ctx.finalize()?;
        for file in files {
            let target = self.output.join(file.file_name().unwrap_or_default());
            if file.is_dir() {
                std::fs::rename(&file, &target).map_err(|e| io_error(&target, e))?;
            } else {
                publish_file(&file, &target)?;
            }
        }
        let failed = stacks.iter().filter(|s| s.error.is_some()).count();
        Ok(BatchResult { summary: display(&self.output.join("summary.csv")), stacks, failed })
    }
}

fn publish_file(source: &Path, target: &Path) -> Result<(), CommandError> {
    // Unlike rename on Unix, hard_link fails atomically if the destination appeared meanwhile.
    match std::fs::hard_link(source, target) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Err(io_error(target, error)),
        Err(_) => {} // FAT and some network filesystems do not support links; reserve a fresh file instead.
    }
    let mut destination = std::fs::OpenOptions::new().write(true).create_new(true).open(target).map_err(|e| io_error(target, e))?;
    let copied = (|| {
        let mut file = std::fs::File::open(source)?;
        std::io::copy(&mut file, &mut destination)?;
        destination.sync_all()
    })();
    drop(destination);
    copied.map_err(|error| {
        let _ = std::fs::remove_file(target);
        io_error(target, error)
    })
}

pub(super) fn children(path: &Path) -> Result<Vec<PathBuf>, CommandError> {
    let entries = std::fs::read_dir(path).map_err(|e| io_error(path, e))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| io_error(path, e))?;
        // Do not follow directory or file links outside the capture folder.
        if entry.file_type().map_err(|e| io_error(&entry.path(), e))?.is_symlink() {
            continue;
        }
        out.push(entry.path());
        if out.len() > MAX_ITEMS {
            return Err(unavailable("batch.run", "folder has more than 100000 entries"));
        }
    }
    out.sort();
    Ok(out)
}

pub(super) fn safe(text: &str) -> String {
    let value: String = text.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '_' }).take(160).collect();
    let value = value.trim_matches(['.', ' ']);
    if value.is_empty() { "stack".into() } else { value.into() }
}

fn portable(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
    !name.is_empty()
        && name.len() <= 240
        && !name.ends_with(['.', ' '])
        && !name.chars().any(|c| c.is_control() || "<>:\"/\\|?*{}".contains(c))
        && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.ends_with(['1', '2', '3', '4', '5', '6', '7', '8', '9']))
}

/// Renders one portable filename, never a path. Token values cannot inject new tokens or separators.
pub(super) fn filename(template: &str, stack: &str, method: Method, files: &[PathBuf], settings: &Value) -> Result<String, Invalid> {
    if template.len() > 2048 {
        return Err(Invalid::new("template", "is longer than 2048 bytes"));
    }
    let stem = |file: Option<&PathBuf>| safe(file.and_then(|p| p.file_stem()).and_then(|s| s.to_str()).unwrap_or("frame"));
    let (first, last) = (stem(files.first()), stem(files.last()));
    let date = Timestamp::now().to_string();
    let params = settings
        .as_object()
        .map_or_else(|| "default".into(), |map| map.iter().map(|(k, v)| format!("{k}-{}", safe(&v.to_string()))).collect::<Vec<_>>().join("_"));
    let mut rest = template;
    let mut out = String::new();
    while let Some((before, tail)) = rest.split_once('{') {
        out.push_str(before);
        let (token, after) = tail.split_once('}').ok_or_else(|| Invalid::new("template", "has an unclosed token"))?;
        let value = match token {
            "stack" => safe(stack),
            "method" => method.name().into(),
            "range" => format!("{first}-{last}"),
            "first" => first.clone(),
            "last" => last.clone(),
            "date" => date.get(..10).unwrap_or("1970-01-01").into(),
            "params" => params.clone(),
            _ => return Err(Invalid::new("template", format!("has unknown token {{{token}}}"))),
        };
        out.push_str(&value);
        rest = after;
    }
    out.push_str(rest);
    let lower = out.to_ascii_lowercase();
    if !portable(&out) || !IMAGE_EXTENSIONS.iter().any(|extension| lower.ends_with(&format!(".{extension}"))) {
        return Err(Invalid::new(
            "template",
            format!("must render one portable image filename of at most 240 bytes, ending in .{}", IMAGE_EXTENSIONS.join(", .")),
        ));
    }
    Ok(out)
}

/// The extensions a template may end in; the file gets its format's (`with_format`).
const IMAGE_EXTENSIONS: [&str; 7] = ["tif", "tiff", "png", "jpg", "jpeg", "dng", "exr"];

/// `name` with the extension of `format` in place of its own, kept when it is one of the format's.
fn with_format(name: &str, format: super::export::FileFormat) -> String {
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    let same = match format {
        super::export::FileFormat::Tiff => matches!(extension.to_ascii_lowercase().as_str(), "tif" | "tiff"),
        super::export::FileFormat::Jpeg => matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg"),
        other => extension.eq_ignore_ascii_case(other.extension()),
    };
    if same { name.to_owned() } else { format!("{stem}.{}", format.extension()) }
}

pub(super) fn new_project(engine: &mut Engine, ctx: &JobContext, workspace: &Workspace, name: &str, preset: &str) -> Result<(), CommandError> {
    let target = workspace.output.join(format!("{name}{}", branding::PROJECT_EXTENSION));
    if target.exists() {
        return Err(ProjectError::Exists { path: target }.into());
    }
    call(engine, ctx, "project.new", json!({"name": name, "folder": display(workspace.stage.path())}))?;
    let params = match preset.strip_prefix(super::preset::BUILT_IN_PREFIX) {
        Some(id) => json!({ "builtin": id }),
        None => json!({ "path": preset }),
    };
    call(engine, ctx, "preset.apply", params)?;
    Ok(())
}

pub(super) fn warnings(result: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for (key, label) in [("moved", "movement"), ("outOfFocus", "out-of-focus"), ("rejected", "rejected alignment")] {
        if let Some(frames) = result.get(key).and_then(Value::as_array).filter(|v| !v.is_empty()) {
            out.push(format!("{label}: {frames:?}"));
        }
    }
    out
}

pub(super) fn finish_stack(
    engine: &mut Engine,
    ctx: &JobContext,
    workspace: &Workspace,
    entry: &mut Entry,
    files: &[PathBuf],
    template: &str,
    method: Method,
) -> Result<(), CommandError> {
    let aligned = call(engine, ctx, "align.run", json!({}))?;
    entry.warnings.extend(warnings(&aligned));
    let settings = call(engine, ctx, "stack.get", json!({}))?;
    let settings = settings.get("settings").and_then(|s| s.get(method.key())).unwrap_or(&Value::Null);
    let name =
        filename(template, &entry.stack, method, files, settings).map_err(|source| CommandError::Invalid { command: "batch.run".into(), source })?;
    // The file is written as the preset's output settings say, named by the template.
    let export = call(engine, ctx, "export.get", json!({}))?;
    let format: super::export::FileFormat =
        serde_json::from_value(export.get("settings").and_then(|s| s.get("format")).cloned().unwrap_or(Value::Null)).unwrap_or_default();
    let name = with_format(&name, format);
    let target = workspace.output.join(&name);
    if target.exists() {
        return Err(ProjectError::Exists { path: target }.into());
    }
    let stacked = call(engine, ctx, method.command(), json!({}))?;
    let output = stacked.get("output").and_then(Value::as_u64).ok_or_else(|| unavailable("batch.run", "stack returned no output"))?;
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
    let exported =
        call(engine, ctx, "export.run", json!({"output": output, "folder": display(workspace.stage.path()), "settings": { "template": stem }}))?;
    entry.warnings.extend(exported.get("warnings").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned));
    let written = exported.get("files").and_then(|f| f.get(0)).and_then(|f| f.get("path")).and_then(Value::as_str).map(Path::new);
    let target = written.and_then(Path::file_name).map_or(target, |file| workspace.output.join(file));
    call(engine, ctx, "project.save", json!({}))?;
    call(engine, ctx, "project.close", json!({}))?;
    entry.project = Some(display(&workspace.output.join(format!("{}{}", entry.stack, branding::PROJECT_EXTENSION))));
    entry.output = Some(display(&target));
    Ok(())
}

pub(super) fn entry(name: String, frames: usize, method: Method) -> Entry {
    Entry { stack: name, frames, method: method.name().into(), time: 0.0, warnings: Vec::new(), project: None, output: None, error: None }
}

fn queue(engine: &mut Engine, ctx: &JobContext, input: &str, split: SplitSettings) -> Result<Vec<(String, Vec<PathBuf>)>, CommandError> {
    let root = absolute(input)?;
    let all = children(&root)?;
    let dirs: Vec<_> = all.iter().filter(|p| p.is_dir() && !p.to_string_lossy().ends_with(branding::PROJECT_EXTENSION)).collect();
    let frames: Vec<_> = all.iter().filter(|p| p.is_file() && crate::frames::is_frame_name(p)).collect();
    if !dirs.is_empty() && !frames.is_empty() {
        return Err(unavailable("batch.run", "input mixes stack folders and loose frames: choose one layout"));
    }
    if !dirs.is_empty() {
        let mut queue = Vec::new();
        let mut count = 0usize;
        for dir in dirs {
            let files: Vec<_> = children(dir)?.into_iter().filter(|p| p.is_file() && crate::frames::is_frame_name(p)).collect();
            count = count.saturating_add(files.len());
            if count > MAX_ITEMS {
                return Err(unavailable("batch.run", "queue has more than 100000 frames"));
            }
            queue.push((safe(dir.file_name().and_then(|s| s.to_str()).unwrap_or("stack")), files));
        }
        return Ok(queue);
    }
    if frames.is_empty() {
        return Err(unavailable("batch.run", "input has no frame files or stack folders"));
    }
    let found = call(engine, ctx, "project.findStacks", json!({"paths": [input], "split": split}))?;
    if found.get("skipped").and_then(Value::as_array).is_some_and(|v| !v.is_empty()) {
        return Err(unavailable("batch.run", format!("auto-split could not read all inputs: {}", found["skipped"])));
    }
    let name = safe(root.file_name().and_then(|s| s.to_str()).unwrap_or("stack"));
    Ok(found
        .get("stacks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, s)| {
            let files = s.get("files").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(PathBuf::from).collect();
            (format!("{name}-{:03}", i + 1), files)
        })
        .collect())
}

fn run(session: &Session, p: BatchParams) -> Result<JobPlan<BatchResult>, CommandError> {
    let (compute, tiling) = (session.compute(), session.tiling());
    Ok(JobPlan::new(move |ctx| {
        let workspace = Workspace::new(&p.input, &p.output, &p.preset)?;
        let mut engine = workspace.engine(false, compute, tiling)?;
        let queue = queue(&mut engine, ctx, &p.input, p.split)?;
        let mut names = BTreeSet::new();
        if queue.iter().any(|(name, _)| !names.insert(name.to_lowercase())) {
            return Err(unavailable("batch.run", "stack folder names collide after filename sanitization"));
        }
        let total = queue.len() as u64;
        let mut entries = Vec::new();
        for (i, (name, files)) in queue.into_iter().enumerate() {
            ctx.checkpoint()?;
            let start = Instant::now();
            let mut entry = entry(name, files.len(), p.method);
            let result = (|| {
                if files.is_empty() {
                    return Err(unavailable("batch.run", "stack folder has no frames"));
                }
                new_project(&mut engine, ctx, &workspace, &entry.stack, &p.preset)?;
                let added = call(&mut engine, ctx, "project.addFrames", json!({"paths": files}))?;
                if added.get("skipped").and_then(Value::as_array).is_some_and(|v| !v.is_empty()) {
                    return Err(unavailable("batch.run", format!("not all frames were ingested: {}", added["skipped"])));
                }
                finish_stack(&mut engine, ctx, &workspace, &mut entry, &files, &p.template, p.method)
            })();
            if let Err(error) = result {
                if ctx.is_cancelled() || matches!(error, CommandError::Cancelled) {
                    return Err(CommandError::Cancelled);
                }
                entry.error = Some(error.info().message);
                // Preserve a failed stack's project for diagnosis without leaking its editing lock.
                if engine.project().is_some() {
                    call(&mut engine, ctx, "project.save", json!({}))?;
                    entry.project = engine.project_path().and_then(Path::file_name).map(|n| display(&workspace.output.join(n)));
                    call(&mut engine, ctx, "project.close", json!({}))?;
                }
            }
            entry.time = start.elapsed().as_secs_f64();
            entries.push(entry);
            ctx.step(i as u64 + 1, total)?;
        }
        let result = workspace.publish(ctx, entries)?;
        Ok(commit(move |_| Ok(result)))
    })
    .stages(&[("Processing stack queue", 1.0)]))
}
