//! A serial queue of independent stack projects. Work stays in the output's staging folder until publication, and a
//! queue that was interrupted resumes from its journal (`queue`).

use florastack_core::time::{Duration, Instant};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use florastack_align::SplitSettings;
use florastack_core::{Monitor, branding};
use florastack_project::{ProjectError, Timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::queue::{Done, Journal, Kind, Request, Workspace};
use super::{MAX_ITEMS, absolute, check_path, display};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobContext, JobPlan, Lane, commit};
use crate::registry::{CommandSpec, Effects, Meta, Params};
use crate::{Dispatched, Engine, Session};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::job(
        Meta::new("batch.run", "Process batch", "Queues stacks as separate projects, exports their outputs and writes a summary CSV.")
            .help(
                "Immediate subfolders are stacks; a folder with images directly inside is auto-split with project.findStacks. \
                   Mixed layouts are refused. Each stack receives the preset, a TOML file or a built-in one as \
                   `builtin:<id>` (`preset.list`), including depth-map contrast thresholds; each output is exported as the \
                   preset's output settings say (`export.run`: a 16-bit TIFF unless they say otherwise, with the maps and \
                   aligned frames they ask for), the template's extension giving way to their format's. \
                   Failures are recorded and the queue continues. Outputs and projects are staged in the output's \
                   `.queue` folder until the cancellable work ends, then published without replacing existing files. \
                   An interrupted queue (a crash, a killed process, or an error that stopped it) resumes when it is run \
                   again with the same input, output, preset, method, template and split, from the first stack it \
                   hadn't finished; `restart` discards it instead. Cancelling discards the staged work. summary.csv \
                   lists stack, frames, method, time in seconds, warnings, project, output and error. Templates accept {stack}, {method}, {range}, \
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
    /// Shift-invariant wavelet fusion, on the CPU only.
    Wavelet,
}

impl Method {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Pyramid => "pyramid",
            Self::DepthMap => "depth-map",
            Self::Average => "average",
            Self::Wavelet => "wavelet",
        }
    }
    fn command(self) -> &'static str {
        match self {
            Self::Pyramid => "stack.pyramid",
            Self::DepthMap => "stack.depthMap",
            Self::Average => "stack.average",
            Self::Wavelet => "stack.wavelet",
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
    /// Discard the staged work of an interrupted queue in output instead of resuming it.
    #[serde(default)]
    restart: bool,
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
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
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
    /// What was carried over from an interrupted run: the entries a batch had finished, or the frames a capture had
    /// taken.
    pub resumed: usize,
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

pub(super) fn children(path: &Path) -> Result<Vec<PathBuf>, CommandError> {
    let entries = florastack_core::fs::read_dir(path).map_err(|e| io_error(path, e))?;
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
    if florastack_core::fs::path_exists(&target) {
        return Err(ProjectError::Exists { path: target }.into());
    }
    call(engine, ctx, "project.new", json!({"name": name, "folder": display(&workspace.stage)}))?;
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
    if florastack_core::fs::path_exists(&target) {
        return Err(ProjectError::Exists { path: target }.into());
    }
    let stacked = call(engine, ctx, method.command(), json!({}))?;
    let output = stacked.get("output").and_then(Value::as_u64).ok_or_else(|| unavailable("batch.run", "stack returned no output"))?;
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
    let exported = call(engine, ctx, "export.run", json!({"output": output, "folder": display(&workspace.stage), "settings": { "template": stem }}))?;
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
    let dirs: Vec<_> =
        all.iter().filter(|p| florastack_core::fs::path_is_dir(p) && !p.to_string_lossy().ends_with(branding::PROJECT_EXTENSION)).collect();
    let frames: Vec<_> = all.iter().filter(|p| florastack_core::fs::path_is_file(p) && crate::frames::is_frame_name(p)).collect();
    if !dirs.is_empty() && !frames.is_empty() {
        return Err(unavailable("batch.run", "input mixes stack folders and loose frames: choose one layout"));
    }
    if !dirs.is_empty() {
        let mut queue = Vec::new();
        let mut count = 0usize;
        for dir in dirs {
            let files: Vec<_> =
                children(dir)?.into_iter().filter(|p| florastack_core::fs::path_is_file(p) && crate::frames::is_frame_name(p)).collect();
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
        let request = Request {
            command: "batch.run",
            kind: Kind::Batch,
            input: &p.input,
            output: &p.output,
            preset: &p.preset,
            params: json!({"preset": p.preset, "method": p.method, "template": p.template, "split": p.split}),
            restart: p.restart,
            append: false,
        };
        let (workspace, mut journal, resumed) = Workspace::open(request)?;
        let result = process(ctx, &p, &workspace, &mut journal, resumed, compute, tiling);
        workspace.ended(ctx, &journal, &result);
        let result = result?;
        Ok(commit(move |_| Ok(result)))
    })
    .stages(&[("Processing stack queue", 1.0)]))
}

/// Works through the queue from the first stack the journal doesn't have done, then publishes it.
fn process(
    ctx: &JobContext,
    p: &BatchParams,
    workspace: &Workspace,
    journal: &mut Journal,
    resumed: bool,
    compute: crate::compute::Compute,
    tiling: florastack_stack::Tiling,
) -> Result<BatchResult, CommandError> {
    let mut engine = workspace.engine(false, compute, tiling)?;
    if resumed {
        // What the stack it was working on had staged goes: it starts again.
        workspace.clear_except(&journal.done.iter().flat_map(|d| d.files.iter().cloned()).collect())?;
    } else {
        let queue = queue(&mut engine, ctx, &p.input, p.split.clone())?;
        let mut names = BTreeSet::new();
        if queue.iter().any(|(name, _)| !names.insert(name.to_lowercase())) {
            return Err(unavailable("batch.run", "stack folder names collide after filename sanitization"));
        }
        journal.queue = queue;
        workspace.record(journal)?;
    }
    let carried = journal.done.len();
    let total = journal.queue.len() as u64;
    let mut entries: Vec<Entry> = journal.done.iter().map(|d| d.entry.clone()).collect();
    for (i, (name, files)) in journal.queue.clone().into_iter().enumerate().skip(carried) {
        ctx.checkpoint()?;
        let before = workspace.staged()?;
        let start = Instant::now();
        let mut entry = entry(name, files.len(), p.method);
        let result = (|| {
            if files.is_empty() {
                return Err(unavailable("batch.run", "stack folder has no frames"));
            }
            new_project(&mut engine, ctx, workspace, &entry.stack, &p.preset)?;
            let added = call(&mut engine, ctx, "project.addFrames", json!({"paths": files}))?;
            if added.get("skipped").and_then(Value::as_array).is_some_and(|v| !v.is_empty()) {
                return Err(unavailable("batch.run", format!("not all frames were ingested: {}", added["skipped"])));
            }
            finish_stack(&mut engine, ctx, workspace, &mut entry, &files, &p.template, p.method)
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
        let files = workspace.staged()?.difference(&before).cloned().collect();
        journal.done.push(Done { entry: entry.clone(), files });
        workspace.record(journal)?;
        entries.push(entry);
        ctx.step(i as u64 + 1, total)?;
    }
    let summary = workspace.publish(ctx, &entries, &[], false)?;
    let failed = entries.iter().filter(|s| s.error.is_some()).count();
    Ok(BatchResult { summary, stacks: entries, failed, resumed: carried })
}
