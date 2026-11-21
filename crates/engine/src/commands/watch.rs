//! Tethered capture through native filesystem notifications with a periodic directory scan as a fallback. A capture
//! that was interrupted resumes from its journal, and another capture into the same output leaves out the frames of
//! the earlier ones (`queue`).

use florastack_core::time::{Duration, Instant};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};

use florastack_core::Monitor;
use notify::{EventKind, RecursiveMode, Watcher};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::batch::{self, BatchResult, Method, call, children, io_error, unavailable};
use super::queue::{CaptureState, Captured, Journal, Kind, LEDGER, Request, Stamp, Taken, Workspace};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobContext, JobPlan, Lane, commit};
use crate::registry::{CommandSpec, Effects, Meta, Params};
use crate::session::Session;

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::job(
        Meta::new("watch.run", "Watch capture folder", "Ingests settled frames, aligns arrivals and updates a preview until capture goes quiet.")
            .help(
                "Watches files directly inside input, including existing images, using native events plus polling. \
                 A frame must have unchanged size and modification time for settleMs and decode successfully, with \
                 unchanged metadata before and after the read. Truncated or locked files are retried until the quiet \
                 timeout, then reported as warnings. Files rewritten after ingest are refused. New frames extend \
                 the neighbor alignment chain; job events expose the latest preview path and frame count. After \
                 idleSeconds without a new or changed frame, counted from the latest frame's preview, runs final \
                 alignment and exports one stack and its project, with summary.csv and captures.json (each capture's \
                 frames). Waits for the first frame indefinitely. Output must be outside input. Template tokens are \
                 the same as batch.run; preset depth-map thresholds need no interaction. With `append`, the capture \
                 joins earlier ones of the same input in output: their frames are left out, its stack is numbered \
                 after theirs (`<input>-capture-2`) and summary.csv gains its row, so that watching again after each \
                 capture keeps a tethered session going. A capture that was interrupted (a crash, a killed process, \
                 or an error that stopped it) resumes when it is run again with the same input, output, preset, \
                 method and template, its frames taken again unless they changed; `restart` discards it instead. \
                 Cancelling discards the capture in progress.",
            )
            .effects(Effects::Files)
            .examples(&[
                r#"{"input":"C:/Capture","output":"C:/Results","preset":"C:/Presets/macro.toml","idleSeconds":5}"#,
                r#"{"input":"C:/Capture","output":"C:/Results","preset":"builtin:insect-macro","append":true}"#,
            ]),
        Lane::Heavy,
        run,
    )]
}

fn idle() -> f64 {
    5.0
}
fn settle() -> u64 {
    1000
}
fn poll() -> u64 {
    250
}

/// Watches one capture and finalizes after a quiet period.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct WatchParams {
    /// Folder populated by a tethered camera or its capture application.
    pub(super) input: String,
    /// Folder for the saved project, TIFF, summary.csv and captures.json, outside input.
    pub(super) output: String,
    /// TOML preset, including contrast thresholds for unattended depth-map runs; or a built-in one as `builtin:<id>`.
    pub(super) preset: String,
    /// Method used for the running preview and the final output.
    #[serde(default)]
    pub(super) method: Method,
    /// A portable image filename; accepts batch.run's template tokens, its extension following the preset's format.
    #[serde(default = "batch::default_template")]
    pub(super) template: String,
    /// Seconds without a new or changed frame before finalization, counted from the latest frame's preview; waits at
    /// least settleMs too.
    #[serde(default = "idle")]
    #[schemars(range(min = 0.1, max = 86400))]
    idle_seconds: f64,
    /// Milliseconds a file's size and modification time must remain unchanged before decoding.
    #[serde(default = "settle")]
    #[schemars(range(min = 50, max = 60000))]
    settle_ms: u64,
    /// Maximum milliseconds between directory scans, including when native events are unavailable.
    #[serde(default = "poll")]
    #[schemars(range(min = 10, max = 5000))]
    poll_ms: u64,
    /// Join the earlier captures of this input in output, leaving out their frames, instead of requiring an output
    /// without a summary.
    #[serde(default)]
    pub(super) append: bool,
    /// Discard an interrupted capture's staged work in output instead of resuming it.
    #[serde(default)]
    pub(super) restart: bool,
}

impl Params for WatchParams {
    fn check(&self) -> Result<(), Invalid> {
        batch::check_paths(&self.input, &self.output, &self.preset, &self.template)?;
        if !self.idle_seconds.is_finite() || !(0.1..=86400.0).contains(&self.idle_seconds) {
            return Err(Invalid::new("idleSeconds", "must be between 0.1 and 86400"));
        }
        if !(50..=60000).contains(&self.settle_ms) {
            return Err(Invalid::new("settleMs", "must be between 50 and 60000"));
        }
        if !(10..=5000).contains(&self.poll_ms) {
            return Err(Invalid::new("pollMs", "must be between 10 and 5000"));
        }
        Ok(())
    }
}

struct Pending {
    stamp: Stamp,
    changed: Instant,
    accepted: bool,
    error: Option<String>,
}

struct Capture {
    files: BTreeMap<PathBuf, Pending>,
    activity: Instant,
    frames: Vec<PathBuf>,
    /// The names of the frames earlier captures took, which this one leaves alone.
    earlier: BTreeSet<String>,
}

impl Capture {
    fn new(earlier: BTreeSet<String>) -> Self {
        Self { files: BTreeMap::new(), activity: Instant::now(), frames: Vec::new(), earlier }
    }

    /// Takes a frame an interrupted run of this capture had taken, which must not have changed since.
    fn resume(&mut self, path: PathBuf, before: Option<Stamp>) -> Result<(), CommandError> {
        let now = Stamp::of(&path)
            .map_err(|_| unavailable("watch.run", format!("{} is gone since the capture was interrupted: restart it", path.display())))?;
        if before.is_some_and(|before| before != now) {
            return Err(unavailable("watch.run", format!("{} changed since the capture was interrupted: restart it", path.display())));
        }
        self.files.insert(path.clone(), Pending { stamp: now, changed: Instant::now(), accepted: true, error: None });
        self.frames.push(path);
        Ok(())
    }

    /// The frames taken, as the journal records them.
    fn taken(&self) -> Vec<Taken> {
        self.frames
            .iter()
            .filter_map(|path| Some(Taken { name: path.file_name()?.to_str()?.to_owned(), stamp: self.files.get(path)?.stamp }))
            .collect()
    }

    /// A bounded rescan also catches rename-to-final-name, missed events and unsupported network shares.
    fn scan(&mut self, input: &Path) -> Result<(), CommandError> {
        let paths = children(input)?;
        for path in paths.into_iter().filter(|p| florastack_core::fs::path_is_file(p) && crate::frames::is_frame_name(p)) {
            if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| self.earlier.contains(n)) {
                continue;
            }
            let Ok(current) = Stamp::of(&path) else { continue };
            if let Some(file) = self.files.get_mut(&path) {
                if file.stamp != current {
                    if file.accepted {
                        return Err(unavailable(
                            "watch.run",
                            format!("{} changed after ingestion: use a fresh filename for each capture", path.display()),
                        ));
                    }
                    file.stamp = current;
                    file.changed = Instant::now();
                    file.error = None;
                    self.activity = Instant::now();
                }
            } else {
                self.files.insert(path, Pending { stamp: current, changed: Instant::now(), accepted: false, error: None });
                self.activity = Instant::now();
            }
        }
        // Missing pending files are usually temporary filenames renamed by the tethering application.
        self.files.retain(|path, file| file.accepted || florastack_core::fs::path_exists(path));
        if self.files.len() > super::MAX_ITEMS {
            return Err(unavailable("watch.run", "capture has more than 100000 frames"));
        }
        Ok(())
    }

    fn ready(
        &mut self,
        settle: Duration,
        settings: &florastack_raw::DevelopSettings,
        ctx: &crate::jobs::JobContext,
    ) -> Result<Vec<PathBuf>, CommandError> {
        let mut ready = Vec::new();
        for (path, file) in &mut self.files {
            ctx.checkpoint()?;
            if file.accepted || file.changed.elapsed() < settle || file.stamp.size == 0 {
                continue;
            }
            // A settled length alone is insufficient: writers can pause partway through a RAW or TIFF file.
            let readable = (|| {
                let bytes = florastack_core::fs::read(path).map_err(|e| io_error(path, e))?;
                let mut reader = crate::frames::FrameReader::new(settings.clone(), florastack_codecs::ReadOptions::default());
                reader.read(&bytes, florastack_raw::Quality::Preview)?;
                Ok::<_, CommandError>(())
            })();
            match readable {
                Ok(()) if Stamp::of(path).is_ok_and(|now| now == file.stamp) => ready.push(path.clone()),
                Ok(()) => {
                    file.changed = Instant::now();
                }
                Err(error) => {
                    file.error = Some(error.info().message);
                }
            }
        }
        Ok(ready)
    }
}

fn run(session: &Session, p: WatchParams) -> Result<JobPlan<BatchResult>, CommandError> {
    let (compute, tiling) = (session.compute(), session.tiling());
    Ok(JobPlan::new(move |ctx| {
        let result = ingest(ctx, p, compute, tiling, None)?;
        Ok(commit(move |_| Ok(result)))
    })
    .stages(&[("Capturing frames and updating preview", 1.0)]))
}

/// Managed capture supplies explicit completion and waits for this initialization barrier before shooting.
pub(super) struct Managed {
    pub ready: std::sync::mpsc::Sender<()>,
    pub completion: std::sync::mpsc::Receiver<Result<usize, CommandError>>,
    pub accepted: std::sync::mpsc::Sender<usize>,
}

fn notifications(input: &Path) -> (Option<notify::RecommendedWatcher>, std::sync::mpsc::Receiver<()>) {
    let (tx, rx) = sync_channel(1);
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok_and(|event| !matches!(event.kind, EventKind::Access(_))) {
            let _ = tx.try_send(());
        }
    });
    // Events are an optimization. Polling remains active even when setup fails or the event queue overflows.
    let mut watcher = watcher.ok();
    if let Some(watcher) = &mut watcher {
        if let Err(error) = watcher.watch(input, RecursiveMode::NonRecursive) {
            tracing::warn!(%error, "filesystem events unavailable; polling capture folder");
        }
    } else {
        tracing::warn!("filesystem events unavailable; polling capture folder");
    }
    (watcher, rx)
}

/// Takes the output folder for a capture, runs it, and keeps its work for a resume when an error stops it.
pub(super) fn ingest(
    ctx: &JobContext,
    p: WatchParams,
    compute: crate::compute::Compute,
    tiling: florastack_stack::Tiling,
    managed: Option<Managed>,
) -> Result<BatchResult, CommandError> {
    let request = Request {
        command: if managed.is_some() { "capture.run" } else { "watch.run" },
        kind: Kind::Capture,
        input: &p.input,
        output: &p.output,
        preset: &p.preset,
        params: json!({"preset": p.preset, "method": p.method, "template": p.template}),
        restart: p.restart,
        append: p.append,
    };
    let (workspace, mut journal, _) = Workspace::open(request)?;
    let result = capture(ctx, &p, &workspace, &mut journal, compute, tiling, managed);
    workspace.ended(ctx, &journal, &result);
    result
}

#[allow(clippy::too_many_lines)] // The watch's loop, in the order it runs: scan, ingest, preview, and the quiet end.
fn capture(
    ctx: &JobContext,
    p: &WatchParams,
    workspace: &Workspace,
    journal: &mut Journal,
    compute: crate::compute::Compute,
    tiling: florastack_stack::Tiling,
    managed: Option<Managed>,
) -> Result<BatchResult, CommandError> {
    let input = super::absolute(&p.input)?;
    let mut ledger = workspace.ledger()?;
    let mut engine = workspace.engine(true, compute, tiling)?;
    let mut capture = Capture::new(ledger.frames().into_iter().map(str::to_owned).collect());
    let state = if let Some(state) = journal.capture.clone() {
        let project = workspace.stage.join(format!("{}{}", state.name, florastack_core::branding::PROJECT_EXTENSION));
        if florastack_core::fs::path_is_dir(&project) {
            call(&mut engine, ctx, "project.open", json!({"path": super::display(&project)}))?;
            // The project's frames are the capture's: the journal may lag behind its last save.
            let frames: Vec<PathBuf> = engine.project().into_iter().flat_map(|project| &project.frames).map(|f| PathBuf::from(&f.path)).collect();
            for path in frames {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                let before = state.frames.iter().find(|t| t.name == name).map(|t| t.stamp);
                capture.resume(path, before)?;
            }
        } else {
            batch::new_project(&mut engine, ctx, workspace, &state.name, &p.preset)?;
        }
        state
    } else {
        let base = format!("{}-capture", batch::safe(input.file_name().and_then(|s| s.to_str()).unwrap_or("stack")));
        let number = ledger.captures.len() + 1;
        let name = if number == 1 { base } else { format!("{base}-{number}") };
        batch::new_project(&mut engine, ctx, workspace, &name, &p.preset)?;
        CaptureState { name, number, frames: Vec::new() }
    };
    let resumed = capture.frames.len();
    journal.capture = Some(CaptureState { frames: capture.taken(), ..state.clone() });
    workspace.record(journal)?;
    let settings = crate::project::develop_settings(engine.project().ok_or_else(|| unavailable("watch.run", "capture project is missing"))?)?;
    let (_watcher, rx) = notifications(&input);
    let start = Instant::now();
    let settle = Duration::from_millis(p.settle_ms);
    let quiet = Duration::from_secs_f64(p.idle_seconds).max(settle);
    let interval = Duration::from_millis(p.poll_ms).min(Duration::from_millis(100));
    let mut entry = batch::entry(state.name.clone(), 0, p.method);
    let preview_name = format!("{}-preview.tif", entry.stack);
    if resumed > 0 {
        let preview = workspace.stage.join(&preview_name);
        if florastack_core::fs::path_is_file(&preview) {
            ctx.preview(super::display(&preview), resumed);
        }
        tracing::info!("resumed the capture {} with {resumed} frames", entry.stack);
    }
    if let Some(managed) = &managed {
        managed.ready.send(()).map_err(|_| unavailable("capture.run", "device worker disconnected before shooting"))?;
    }
    let mut expected = None;
    loop {
        ctx.checkpoint()?;
        if expected.is_none()
            && let Some(managed) = &managed
        {
            match managed.completion.try_recv() {
                Ok(result) => expected = Some(result?),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Err(unavailable("capture.run", "device worker disconnected")),
            }
        }
        capture.scan(&input)?;
        let ready = capture.ready(settle, &settings, ctx)?;
        if !ready.is_empty() {
            let added = call(&mut engine, ctx, "project.addFrames", json!({"paths": ready}))?;
            for skip in added.get("skipped").and_then(Value::as_array).into_iter().flatten() {
                if let Some(file) = skip.get("path").and_then(Value::as_str).and_then(|path| capture.files.get_mut(Path::new(path))) {
                    file.error = Some(skip.get("reason").and_then(Value::as_str).unwrap_or("not ingested").into());
                }
            }
            let accepted: Vec<_> = engine.project().into_iter().flat_map(|project| &project.frames).map(|frame| PathBuf::from(&frame.path)).collect();
            for path in ready {
                if accepted.contains(&path) {
                    if let Some(file) = capture.files.get_mut(&path) {
                        if Stamp::of(&path)? != file.stamp {
                            return Err(unavailable("watch.run", "a frame changed while being ingested: increase settleMs"));
                        }
                        file.accepted = true;
                        file.error = None;
                    }
                    capture.frames.push(path);
                }
            }
            if added.get("added").and_then(Value::as_array).is_some_and(|v| !v.is_empty()) {
                call(&mut engine, ctx, "watch.align", json!({}))?;
                let preview = call(&mut engine, ctx, "stack.preview", json!({"method": p.method.name(), "keepFile": true}))?;
                let mut shown = None;
                if let Some(path) = preview.get("path").and_then(Value::as_str) {
                    let destination = workspace.stage.join(&preview_name);
                    let bytes = florastack_core::fs::read(path).map_err(|e| io_error(Path::new(path), e))?;
                    super::output::write_atomically(&destination, &bytes)?;
                    // stack.preview creates an unreferenced cache file. Keep only the published capture copy.
                    florastack_core::fs::remove_file(path).map_err(|e| io_error(Path::new(path), e))?;
                    shown = Some(destination);
                }
                call(&mut engine, ctx, "project.save", json!({}))?;
                journal.capture = Some(CaptureState { frames: capture.taken(), ..state.clone() });
                workspace.record(journal)?;
                // Shown once it is on record: a frame in a preview is never taken again after an interruption.
                if let Some(destination) = shown {
                    ctx.preview(super::display(&destination), capture.frames.len());
                }
                if let Some(managed) = &managed {
                    let _ = managed.accepted.send(capture.frames.len());
                }
                ctx.step(capture.frames.len() as u64, 0)?;
                // The quiet time counts from here, when the watch is ready for the next frame: counted from the last
                // file change, a preview slower than idleSeconds (a busy machine, a deep stack) ended the watch before
                // the camera could send another frame.
                capture.activity = Instant::now();
                // Always rescan after processing before deciding that capture is quiet.
                continue;
            }
        }
        let completed = expected.is_some_and(|count| capture.frames.len() >= count);
        let quiet_end = capture.activity.elapsed() >= quiet && (expected.is_some() || managed.is_none());
        // A resumed capture ends after a quiet period too, though nothing new arrives.
        if completed || ((!capture.files.is_empty() || resumed > 0) && quiet_end) {
            for (path, file) in &capture.files {
                if !file.accepted {
                    entry.warnings.push(format!("{}: {}", path.display(), file.error.as_deref().unwrap_or("file did not finish writing")));
                }
            }
            if capture.frames.is_empty() {
                return Err(unavailable("watch.run", format!("no readable frames arrived: {}", entry.warnings.join("; "))));
            }
            if let Some(expected) = expected
                && capture.frames.len() != expected
            {
                return Err(unavailable(
                    "capture.run",
                    format!("expected {expected} frames, ingested {}: {}", capture.frames.len(), entry.warnings.join("; ")),
                ));
            }
            break;
        }
        // A disconnected event source must not turn the polling fallback into a busy loop.
        if matches!(rx.recv_timeout(interval), Err(RecvTimeoutError::Disconnected)) {
            std::thread::sleep(interval);
        }
    }
    entry.frames = capture.frames.len();
    batch::finish_stack(&mut engine, ctx, workspace, &mut entry, &capture.frames, &p.template, p.method)?;
    entry.time = start.elapsed().as_secs_f64();
    let earlier = !ledger.captures.is_empty();
    ledger.captures.push(Captured { entry: entry.clone(), frames: capture.taken().into_iter().map(|t| t.name).collect() });
    let rows: Vec<_> = ledger.captures.iter().map(|c| c.entry.clone()).collect();
    let summary = workspace.publish(ctx, &rows, &[(LEDGER, ledger.to_bytes())], earlier)?;
    ctx.preview(super::display(&workspace.output.join(preview_name)), capture.frames.len());
    let failed = usize::from(entry.error.is_some());
    Ok(BatchResult { summary, stacks: vec![entry], failed, resumed })
}
