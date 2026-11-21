//! What `batch.run` and `watch.run` share: the output folder they work for ([`Workspace`]), the staging folder in it
//! where their work stays until it is published, and the records that let a run that was interrupted resume.
//!
//! - **Staging**: everything a run makes goes into `<output>/.queue/` first, and moves into the output only at the
//!   end ([`Workspace::publish`]), so that the output never holds half a run. The output's `.queue.lock` keeps a
//!   second run out while one works there.
//! - **The journal** (`.queue/journal.json`, [`Journal`]) records, after each stack a batch finishes or each frame a
//!   capture takes, what is done: a run that ends without cleaning up (a crash, a killed process, a power cut) leaves
//!   it behind, and the next run on the same output with the same settings carries on from it instead of starting
//!   again. A run with other settings is refused, unless it is asked to `restart`, which discards it. An error that
//!   stops a run keeps its work for the next one too; a cancelled run is discarded, as the user asked.
//! - **The capture ledger** (`<output>/captures.json`, [`Ledger`]), published with each capture, lists every capture
//!   of one input folder made into that output with its frames, so that another capture into the same output
//!   (`watch.run`'s `append`) leaves them out and adds its row to `summary.csv`.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use florastack_core::time::SystemTime;
use florastack_core::{LockFile, Monitor};
use florastack_project::ProjectError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::batch::{Entry, children, io_error, unavailable};
use super::{absolute, display};
use crate::compute::Compute;
use crate::error::CommandError;
use crate::jobs::JobContext;
use crate::{Engine, EngineConfig};

/// The staging folder's name in the output folder.
const STAGE: &str = ".queue";
/// The journal's name in the staging folder.
const JOURNAL: &str = "journal.json";
/// The folder of the staging engine's temporary projects, in the staging folder.
const TEMPORARY: &str = "temporary";
/// The summary's name in the output folder.
pub(super) const SUMMARY: &str = "summary.csv";
/// The capture ledger's name in the output folder.
pub(super) const LEDGER: &str = "captures.json";
/// The journal's and the ledger's format; a file of another format is not read.
const FORMAT: u32 = 1;

/// Which command a journal is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Kind {
    Batch,
    Capture,
}

impl Kind {
    fn noun(self) -> &'static str {
        match self {
            Self::Batch => "batch",
            Self::Capture => "capture",
        }
    }
}

/// A file's size and modification time, which say whether it changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Stamp {
    pub size: u64,
    /// Seconds and nanoseconds since the Unix epoch, when the file system tells.
    pub modified: Option<(u64, u32)>,
}

impl Stamp {
    pub(super) fn of(path: &Path) -> Result<Stamp, CommandError> {
        let metadata = florastack_core::fs::metadata(path).map_err(|e| io_error(path, e))?;
        let modified = metadata.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| (d.as_secs(), d.subsec_nanos()));
        Ok(Stamp { size: metadata.len(), modified })
    }
}

/// A batch entry that is done, with the files it staged.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Done {
    pub entry: Entry,
    /// Their names in the staging folder.
    pub files: Vec<String>,
}

/// A frame a capture took.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Taken {
    /// Its name in the input folder.
    pub name: String,
    pub stamp: Stamp,
}

/// The capture in progress.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct CaptureState {
    /// Its stack's name, which its project and files are named after.
    pub name: String,
    /// Which capture of the output it is, from 1.
    pub number: usize,
    pub frames: Vec<Taken>,
}

/// What a run has done so far, for resuming it (see the module documentation).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Journal {
    format: u32,
    kind: Kind,
    /// The parameters that decide the work, the input folder included: a run resumes only with the same.
    params: Value,
    /// A batch's stacks, in order: each one's name and frames.
    #[serde(default)]
    pub queue: Vec<(String, Vec<PathBuf>)>,
    /// The batch's entries done, in order.
    #[serde(default)]
    pub done: Vec<Done>,
    /// The capture in progress.
    #[serde(default)]
    pub capture: Option<CaptureState>,
}

impl Journal {
    fn new(kind: Kind, params: Value) -> Journal {
        Journal { format: FORMAT, kind, params, queue: Vec::new(), done: Vec::new(), capture: None }
    }

    /// Whether anything done would be lost without it.
    fn has_progress(&self) -> bool {
        !self.done.is_empty() || self.capture.as_ref().is_some_and(|c| !c.frames.is_empty())
    }
}

/// One published capture in the ledger.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Captured {
    pub entry: Entry,
    /// The names of its frames in the input folder.
    pub frames: Vec<String>,
}

/// The captures of one input folder published in an output folder (see the module documentation).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Ledger {
    format: u32,
    /// The input folder, canonical.
    input: String,
    pub captures: Vec<Captured>,
}

/// What a run asks of its output folder.
pub(super) struct Request<'a> {
    /// The command, for errors.
    pub command: &'a str,
    pub kind: Kind,
    pub input: &'a str,
    pub output: &'a str,
    pub preset: &'a str,
    /// The parameters that decide the work, but the input folder, which is added canonical.
    pub params: Value,
    /// Discard an interrupted run's work instead of resuming it.
    pub restart: bool,
    /// The output may hold a summary of earlier captures, which this run's row joins.
    pub append: bool,
}

/// The output folder a batch or a capture works for, held for the run (see the module documentation).
pub(super) struct Workspace {
    pub output: PathBuf,
    /// The input folder, canonical.
    pub input: PathBuf,
    /// The staging folder.
    pub stage: PathBuf,
    command: String,
    /// Keep the staging folder when the workspace is dropped: an interrupted run's, or this one's for a later run to
    /// resume.
    keep: Cell<bool>,
    _lock: LockFile,
}

impl Workspace {
    /// Takes the output folder, with the journal of the interrupted run to resume there (and `true`), or a new one.
    pub(super) fn open(request: Request<'_>) -> Result<(Workspace, Journal, bool), CommandError> {
        let command = request.command;
        let input = florastack_core::fs::canonicalize(absolute(request.input)?).map_err(|e| io_error(Path::new(request.input), e))?;
        if !florastack_core::fs::path_is_dir(&input) {
            return Err(unavailable(command, "input must be a folder"));
        }
        super::preset::load(request.preset)?;
        let output = absolute(request.output)?;
        florastack_core::fs::create_dir_all(&output).map_err(|e| io_error(&output, e))?;
        let output = florastack_core::fs::canonicalize(&output).map_err(|e| io_error(&output, e))?;
        if output.starts_with(&input) {
            return Err(unavailable(command, "output must be outside the input folder so exports cannot be ingested as frames"));
        }
        let lock_path = output.join(".queue.lock");
        let lock = LockFile::try_acquire(&lock_path)
            .map_err(|e| io_error(&lock_path, e))?
            .ok_or_else(|| unavailable(command, "another batch or capture is using this output folder"))?;
        let mut params = request.params;
        params["input"] = Value::String(display(&input));
        let stage = output.join(STAGE);
        // Until this run takes the staging folder on, it is an interrupted run's, which a refusal leaves alone.
        let workspace = Workspace { output, input, stage, command: command.into(), keep: Cell::new(true), _lock: lock };
        let journal = workspace.interrupted(request.kind, &params, request.restart)?;
        let continues = request.append || journal.as_ref().and_then(|j| j.capture.as_ref()).is_some_and(|c| c.number > 1);
        let summary = workspace.output.join(SUMMARY);
        if !continues && florastack_core::fs::path_exists(&summary) {
            return Err(ProjectError::Exists { path: summary }.into());
        }
        florastack_core::fs::create_dir_all(&workspace.stage).map_err(|e| io_error(&workspace.stage, e))?;
        workspace.keep.set(false);
        Ok(match journal {
            Some(journal) => (workspace, journal, true),
            None => (workspace, Journal::new(request.kind, params), false),
        })
    }

    /// The journal an interrupted run left with these parameters, after discarding what can't be resumed.
    fn interrupted(&self, kind: Kind, params: &Value, restart: bool) -> Result<Option<Journal>, CommandError> {
        if !florastack_core::fs::path_exists(&self.stage) {
            return Ok(None);
        }
        let path = self.stage.join(JOURNAL);
        let journal = match florastack_core::fs::read(&path) {
            Ok(bytes) => Some(serde_json::from_slice::<Journal>(&bytes).ok().filter(|j| j.format == FORMAT)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_error(&path, e)),
        };
        match journal {
            Some(Some(journal)) if !restart && journal.kind == kind && journal.params == *params => {
                tracing::info!("resuming the interrupted {} in {}", kind.noun(), self.stage.display());
                return Ok(Some(journal));
            }
            Some(other) if !restart => {
                let what = other.map_or("run", |j| j.kind.noun());
                return Err(unavailable(
                    &self.command,
                    format!(
                        "an interrupted {what} with other settings is in {}: run it again with its settings to resume it, or \
                         restart (`restart`, the CLI's --restart) to discard it",
                        self.stage.display()
                    ),
                ));
            }
            // Nothing to resume: a stage a run left before writing its journal, or one to discard.
            _ => {}
        }
        florastack_core::fs::remove_dir_all(&self.stage).map_err(|e| io_error(&self.stage, e))?;
        Ok(None)
    }

    /// An engine for the workspace's projects, stacking as the parent engine does (`compute`, `tiling`).
    pub(super) fn engine(&self, capture: bool, compute: Compute, tiling: florastack_stack::Tiling) -> Result<Engine, CommandError> {
        let config = EngineConfig { temporary_projects: Some(self.stage.join(TEMPORARY)), compute, tiling, ..EngineConfig::default() };
        if !capture {
            return Ok(Engine::new(config));
        }
        let registry = crate::Registry::builder()
            .standard()
            .command(super::align::capture_spec())
            .build()
            .map_err(|e| unavailable(&self.command, e.to_string()))?;
        Ok(Engine::with_registry(config, std::sync::Arc::new(registry)))
    }

    /// Writes the journal, replacing the one before.
    pub(super) fn record(&self, journal: &Journal) -> Result<(), CommandError> {
        let bytes = serde_json::to_vec_pretty(journal).map_err(|e| unavailable(&self.command, format!("the journal can't be written: {e}")))?;
        super::output::write_atomically(&self.stage.join(JOURNAL), &bytes)
    }

    /// What ended the run: on an error but cancellation, with something done, the staging folder stays for the next
    /// run to resume.
    pub(super) fn ended<T>(&self, ctx: &JobContext, journal: &Journal, result: &Result<T, CommandError>) {
        if let Err(error) = result
            && !ctx.is_cancelled()
            && !matches!(error, CommandError::Cancelled)
            && journal.has_progress()
        {
            tracing::warn!("{} stopped; its work stays in {} for the next run to resume", self.command, self.stage.display());
            self.keep.set(true);
        }
    }

    /// The names of the files and folders staged, without the workspace's own.
    pub(super) fn staged(&self) -> Result<BTreeSet<String>, CommandError> {
        Ok(children(&self.stage)?.iter().filter_map(|p| p.file_name()?.to_str().map(str::to_owned)).filter(|n| !own(n)).collect())
    }

    /// Removes what an interrupted run staged beyond `keep`: the work it hadn't finished.
    pub(super) fn clear_except(&self, keep: &BTreeSet<String>) -> Result<(), CommandError> {
        for name in self.staged()?.difference(keep) {
            let path = self.stage.join(name);
            let removed = if florastack_core::fs::path_is_dir(&path) {
                florastack_core::fs::remove_dir_all(&path)
            } else {
                florastack_core::fs::remove_file(&path)
            };
            removed.map_err(|e| io_error(&path, e))?;
        }
        Ok(())
    }

    /// The captures of this input already published in the output, read from its ledger. An output with a summary
    /// but no ledger holds captures from a FloraStack that kept none.
    pub(super) fn ledger(&self) -> Result<Ledger, CommandError> {
        let path = self.output.join(LEDGER);
        let input = display(&self.input);
        match florastack_core::fs::read(&path) {
            Ok(bytes) => {
                let ledger: Ledger = serde_json::from_slice(&bytes)
                    .ok()
                    .filter(|l: &Ledger| l.format == FORMAT)
                    .ok_or_else(|| unavailable(&self.command, format!("{} isn't a capture ledger this FloraStack can read", path.display())))?;
                if ledger.input != input {
                    return Err(unavailable(&self.command, format!("the output's captures are of another input folder, {}", ledger.input)));
                }
                Ok(ledger)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if florastack_core::fs::path_exists(&self.output.join(SUMMARY)) {
                    return Err(unavailable(
                        &self.command,
                        format!("the output's summary has no {LEDGER} listing its captures' frames: choose another output"),
                    ));
                }
                Ok(Ledger { format: FORMAT, input, captures: Vec::new() })
            }
            Err(e) => Err(io_error(&path, e)),
        }
    }

    /// Moves the staged artifacts into the output, then the records: `summary.csv`, listing `rows`, and `records`
    /// (a name and its contents). The point of no return comes after checking that no artifact's name is taken in
    /// the output; with `replace`, the records may replace earlier ones. Returns the summary's path.
    pub(super) fn publish(&self, ctx: &JobContext, rows: &[Entry], records: &[(&str, Vec<u8>)], replace: bool) -> Result<String, CommandError> {
        let mut all = vec![(SUMMARY, summary(rows).into_bytes())];
        all.extend(records.iter().map(|(name, bytes)| (*name, bytes.clone())));
        let artifacts: Vec<PathBuf> = self.staged()?.into_iter().filter(|n| !all.iter().any(|(r, _)| r == n)).map(|n| self.stage.join(n)).collect();
        let taken = |name: &str| florastack_core::fs::path_exists(&self.output.join(name));
        for name in artifacts.iter().filter_map(|p| p.file_name()?.to_str()).chain(all.iter().filter(|_| !replace).map(|(n, _)| *n)) {
            if taken(name) {
                return Err(ProjectError::Exists { path: self.output.join(name) }.into());
            }
        }
        ctx.finalize()?;
        // Without its journal, a publication cut short is never resumed over what it published.
        let journal = self.stage.join(JOURNAL);
        if florastack_core::fs::path_exists(&journal) {
            florastack_core::fs::remove_file(&journal).map_err(|e| io_error(&journal, e))?;
        }
        for file in artifacts {
            let target = self.output.join(file.file_name().unwrap_or_default());
            if florastack_core::fs::path_is_dir(&file) {
                florastack_core::fs::rename(&file, &target).map_err(|e| io_error(&target, e))?;
            } else {
                publish_file(&file, &target)?;
            }
        }
        // The summary is the completion record, so it comes last.
        all.rotate_left(1);
        for (name, bytes) in all {
            let target = self.output.join(name);
            if replace {
                super::output::write_atomically(&target, &bytes)?;
            } else {
                let staged = self.stage.join(name);
                florastack_core::fs::write(&staged, &bytes).map_err(|e| io_error(&staged, e))?;
                publish_file(&staged, &target)?;
            }
        }
        Ok(display(&self.output.join(SUMMARY)))
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if !self.keep.get()
            && let Err(error) = florastack_core::fs::remove_dir_all(&self.stage)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "the staging folder {} could not be removed", self.stage.display());
        }
    }
}

impl Ledger {
    /// The names of every frame its captures took.
    pub(super) fn frames(&self) -> BTreeSet<&str> {
        self.captures.iter().flat_map(|c| &c.frames).map(String::as_str).collect()
    }

    pub(super) fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).unwrap_or_default()
    }
}

/// Whether a staging folder's entry is the workspace's own rather than an artifact: the journal, the temporary
/// projects, and a file being written atomically.
fn own(name: &str) -> bool {
    name == JOURNAL || name == TEMPORARY || (name.starts_with('.') && Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("part")))
}

/// `summary.csv`'s text.
fn summary(rows: &[Entry]) -> String {
    let mut csv = String::from("stack,frames,method,time,warnings,project,output,error\r\n");
    for entry in rows {
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
    csv
}

fn publish_file(source: &Path, target: &Path) -> Result<(), CommandError> {
    // Unlike rename on Unix, hard_link fails atomically if the destination appeared meanwhile.
    match florastack_core::fs::hard_link(source, target) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Err(io_error(target, error)),
        Err(_) => {} // FAT and some network filesystems do not support links; reserve a fresh file instead.
    }
    let mut destination = florastack_core::fs::OpenOptions::new().write(true).create_new(true).open(target).map_err(|e| io_error(target, e))?;
    let copied = (|| {
        let mut file = florastack_core::fs::File::open(source)?;
        std::io::copy(&mut file, &mut destination)?;
        destination.sync_all()
    })();
    drop(destination);
    copied.map_err(|error| {
        let _ = florastack_core::fs::remove_file(target);
        io_error(target, error)
    })
}
