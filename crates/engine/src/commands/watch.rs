//! Tethered capture through native filesystem notifications with a periodic directory scan as a fallback.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant, SystemTime};

use florastack_core::Monitor;
use notify::{EventKind, RecursiveMode, Watcher};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::batch::{self, BatchResult, Method, Workspace, call, children, io_error, unavailable};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobPlan, Lane, commit};
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
                   idleSeconds without a new or changed frame, runs final alignment and exports one stack and its \
                   project plus summary.csv. Waits for the first frame indefinitely. Output must be outside input. \
                   Template tokens are the same as batch.run; preset depth-map thresholds need no interaction.",
            )
            .effects(Effects::Files)
            .examples(&[r#"{"input":"C:/Capture","output":"C:/Results","preset":"C:/Presets/macro.toml","idleSeconds":5}"#]),
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
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WatchParams {
    /// Folder populated by a tethered camera or its capture application.
    input: String,
    /// Folder for the saved project, TIFF and summary.csv, outside input.
    output: String,
    /// TOML preset, including contrast thresholds for unattended depth-map runs.
    preset: String,
    /// Method used for the running preview and the final output.
    #[serde(default)]
    method: Method,
    /// A portable TIFF filename; accepts batch.run's template tokens.
    #[serde(default = "batch::default_template")]
    template: String,
    /// Seconds without a new or changed frame before finalization; waits at least settleMs too.
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: Option<SystemTime>,
}

fn stamp(path: &Path) -> Result<Stamp, CommandError> {
    let metadata = std::fs::metadata(path).map_err(|e| io_error(path, e))?;
    Ok(Stamp { size: metadata.len(), modified: metadata.modified().ok() })
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
}

impl Capture {
    fn new() -> Self {
        Self { files: BTreeMap::new(), activity: Instant::now(), frames: Vec::new() }
    }

    /// A bounded rescan also catches rename-to-final-name, missed events and unsupported network shares.
    fn scan(&mut self, input: &Path) -> Result<(), CommandError> {
        let paths = children(input)?;
        for path in paths.into_iter().filter(|p| p.is_file() && crate::frames::is_frame_name(p)) {
            let Ok(current) = stamp(&path) else { continue };
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
        self.files.retain(|path, file| file.accepted || path.exists());
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
                let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
                let mut reader = crate::frames::FrameReader::new(settings.clone(), florastack_codecs::ReadOptions::default());
                reader.read(&bytes, florastack_raw::Quality::Preview)?;
                Ok::<_, CommandError>(())
            })();
            match readable {
                Ok(()) if stamp(path).is_ok_and(|now| now == file.stamp) => ready.push(path.clone()),
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

fn run(_: &Session, p: WatchParams) -> Result<JobPlan<BatchResult>, CommandError> {
    Ok(JobPlan::new(move |ctx| {
        let workspace = Workspace::new(&p.input, &p.output, &p.preset)?;
        let input = super::absolute(&p.input)?;
        let mut engine = workspace.engine(true)?;
        let name = format!("{}-capture", batch::safe(input.file_name().and_then(|s| s.to_str()).unwrap_or("stack")));
        batch::new_project(&mut engine, ctx, &workspace, &name, &p.preset)?;
        let settings = crate::project::develop_settings(engine.project().ok_or_else(|| unavailable("watch.run", "capture project is missing"))?)?;
        let (tx, rx) = sync_channel(1);
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if event.is_ok_and(|event| !matches!(event.kind, EventKind::Access(_))) {
                let _ = tx.try_send(());
            }
        });
        // Events are an optimization. Polling remains active even when setup fails or the event queue overflows.
        let mut watcher = watcher.ok();
        if let Some(watcher) = &mut watcher {
            if let Err(error) = watcher.watch(&input, RecursiveMode::NonRecursive) {
                tracing::warn!(%error, "filesystem events unavailable; polling capture folder");
            }
        } else {
            tracing::warn!("filesystem events unavailable; polling capture folder");
        }
        let mut capture = Capture::new();
        let start = Instant::now();
        let settle = Duration::from_millis(p.settle_ms);
        let quiet = Duration::from_secs_f64(p.idle_seconds).max(settle);
        let interval = Duration::from_millis(p.poll_ms).min(Duration::from_millis(100));
        let mut entry = batch::entry(name, 0, p.method);
        let preview_name = format!("{}-preview.tif", entry.stack);
        loop {
            ctx.checkpoint()?;
            capture.scan(&input)?;
            let ready = capture.ready(settle, &settings, ctx)?;
            if !ready.is_empty() {
                let added = call(&mut engine, ctx, "project.addFrames", json!({"paths": ready}))?;
                for skip in added.get("skipped").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(file) = skip.get("path").and_then(Value::as_str).and_then(|path| capture.files.get_mut(Path::new(path))) {
                        file.error = Some(skip.get("reason").and_then(Value::as_str).unwrap_or("not ingested").into());
                    }
                }
                let accepted: Vec<_> =
                    engine.project().into_iter().flat_map(|project| &project.frames).map(|frame| PathBuf::from(&frame.path)).collect();
                for path in ready {
                    if accepted.contains(&path) {
                        if let Some(file) = capture.files.get_mut(&path) {
                            if stamp(&path)? != file.stamp {
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
                    let preview = call(&mut engine, ctx, "stack.preview", json!({"method": p.method.name()}))?;
                    if let Some(path) = preview.get("path").and_then(Value::as_str) {
                        let destination = workspace.stage.path().join(&preview_name);
                        let bytes = std::fs::read(path).map_err(|e| io_error(Path::new(path), e))?;
                        super::output::write_atomically(&destination, &bytes)?;
                        // stack.preview creates an unreferenced cache file. Keep only the published capture copy.
                        std::fs::remove_file(path).map_err(|e| io_error(Path::new(path), e))?;
                        ctx.preview(super::display(&destination), capture.frames.len());
                    }
                    call(&mut engine, ctx, "project.save", json!({}))?;
                    ctx.step(capture.frames.len() as u64, 0)?;
                    // Always rescan after processing before deciding that capture is quiet.
                    continue;
                }
            }
            if !capture.files.is_empty() && capture.activity.elapsed() >= quiet {
                for (path, file) in &capture.files {
                    if !file.accepted {
                        entry.warnings.push(format!("{}: {}", path.display(), file.error.as_deref().unwrap_or("file did not finish writing")));
                    }
                }
                if capture.frames.is_empty() {
                    return Err(unavailable("watch.run", format!("no readable frames arrived: {}", entry.warnings.join("; "))));
                }
                break;
            }
            // A disconnected event source must not turn the polling fallback into a busy loop.
            if matches!(rx.recv_timeout(interval), Err(RecvTimeoutError::Disconnected)) {
                std::thread::sleep(interval);
            }
        }
        entry.frames = capture.frames.len();
        batch::finish_stack(&mut engine, ctx, &workspace, &mut entry, &capture.frames, &p.template, p.method)?;
        entry.time = start.elapsed().as_secs_f64();
        let result = workspace.publish(ctx, vec![entry])?;
        ctx.preview(super::display(&workspace.output.join(preview_name)), capture.frames.len());
        Ok(commit(move |_| Ok(result)))
    })
    .stages(&[("Capturing frames and updating preview", 1.0)]))
}
