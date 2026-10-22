//! The engine's state: the open project with its folder and undo history, the jobs, and the settings of the host
//! (where temporary projects go, which trash to use). Commands receive it ([`Session`]); it is the only place the
//! project changes, through [`Session::apply`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use florastack_project::{Aspects, CacheDir, Edit, Project, ProjectFolder, Trash};

use crate::Dispatched;
use crate::error::{CommandError, Invalid};
use crate::events::{Event, ProjectEvent};
use crate::history::{History, StepList};
use crate::jobs::{Finished, JobId, Jobs, Lane};
use crate::registry::{CommandSpec, Requires, Runner, normalize};

/// The host's settings that commands use.
pub(crate) struct Host {
    pub(crate) temporary_projects: Option<PathBuf>,
    pub(crate) trash: Arc<dyn Trash + Send + Sync>,
}

/// An open project.
struct Open {
    folder: ProjectFolder,
    project: Project,
    history: History,
}

/// The command running now, for the labels of the undo steps it makes and the names in its errors.
#[derive(Clone)]
struct Current {
    command: &'static str,
    title: &'static str,
    coalesce: Option<String>,
    /// Whether it has recorded its undo step already: its further edits join that step.
    stepped: bool,
}

/// What a command sees of the engine. See the module documentation.
pub struct Session {
    host: Host,
    open: Option<Open>,
    pub(crate) jobs: Jobs,
    current: Option<Current>,
    revision: u64,
    changed: Aspects,
    history_changed: bool,
    events: Vec<Event>,
}

/// Runs `f`, turning a panic into an error (AGENTS.md, Never crash: a last resort; the project only changes through
/// [`Project::apply`], which is all or nothing, so a panic can't leave it half changed).
pub(crate) fn guarded<T>(command: &str, f: impl FnOnce() -> Result<T, CommandError>) -> Result<T, CommandError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        tracing::error!("`{command}` panicked");
        Err(CommandError::Internal { command: command.to_owned() })
    })
}

/// How long [`Session::close_project`] waits at a time for cancelled jobs to stop.
const STOP_POLL: Duration = Duration::from_millis(20);

impl Session {
    pub(crate) fn new(host: Host, jobs: Jobs) -> Session {
        Session { host, open: None, jobs, current: None, revision: 0, changed: Aspects::NONE, history_changed: false, events: Vec::new() }
    }

    fn command(&self) -> &'static str {
        self.current.as_ref().map_or("", |c| c.command)
    }

    /// An error saying that the running command can't run now, and why.
    pub fn unavailable(&self, reason: impl Into<String>) -> CommandError {
        CommandError::Unavailable { command: self.command().to_owned(), reason: reason.into() }
    }

    /// An error saying that a parameter of the running command can't be used.
    pub fn invalid(&self, field: impl Into<String>, reason: impl Into<String>) -> CommandError {
        CommandError::Invalid { command: self.command().to_owned(), source: Invalid::new(field, reason) }
    }

    fn no_project(&self) -> CommandError {
        self.unavailable("no project is open: create one with `project.new` or open one with `project.open`")
    }

    pub fn has_project(&self) -> bool {
        self.open.is_some()
    }

    /// The open project.
    pub fn project(&self) -> Result<&Project, CommandError> {
        self.open.as_ref().map(|o| &o.project).ok_or_else(|| self.no_project())
    }

    /// The open project's folder.
    pub fn folder(&self) -> Result<&ProjectFolder, CommandError> {
        self.open.as_ref().map(|o| &o.folder).ok_or_else(|| self.no_project())
    }

    /// The open project's cache, for a job's work to read and write cache files with.
    pub fn cache_dir(&self) -> Result<CacheDir, CommandError> {
        self.folder().map(|f| f.cache_dir().clone())
    }

    /// Where temporary projects go, if the host has said.
    pub fn temporary_projects(&self) -> Option<&Path> {
        self.host.temporary_projects.as_deref()
    }

    /// Where orphaned temporary projects are moved.
    pub fn trash(&self) -> &dyn Trash {
        self.host.trash.as_ref()
    }

    pub fn is_dirty(&self) -> bool {
        self.open.as_ref().is_some_and(|o| o.history.is_dirty())
    }

    pub(crate) fn undo_label(&self) -> Option<&str> {
        self.open.as_ref().and_then(|o| o.history.undo_label())
    }

    pub(crate) fn redo_label(&self) -> Option<&str> {
        self.open.as_ref().and_then(|o| o.history.redo_label())
    }

    /// The undo steps, newest first, and the redo steps, next first: their labels and the commands that made them.
    pub fn history_steps(&self) -> (StepList, StepList) {
        self.open.as_ref().map(|o| o.history.steps()).unwrap_or_default()
    }

    /// Refuses an edit of what a running job depends on.
    fn check_locks(&self, aspects: Aspects) -> Result<(), CommandError> {
        match self.jobs.conflict(aspects) {
            Some((job, locked)) => Err(CommandError::Conflict { command: self.command().to_owned(), job: job.to_owned(), aspects: locked }),
            None => Ok(()),
        }
    }

    fn writable(&mut self) -> Result<&mut Open, CommandError> {
        let refused = match &self.open {
            None => Some(self.no_project()),
            Some(open) if open.folder.is_read_only() => Some(self.unavailable("the project was opened read-only")),
            Some(_) => None,
        };
        match (refused, self.open.as_mut()) {
            (Some(e), _) => Err(e),
            (None, Some(open)) => Ok(open),
            (None, None) => Err(CommandError::Internal { command: String::new() }),
        }
    }

    fn changed(&mut self, aspects: Aspects) {
        self.revision = self.revision.saturating_add(1);
        self.changed = self.changed.union(aspects);
        self.history_changed = true;
    }

    /// Changes the project: makes `edit`, as the running command's undo step (the first edit of a command records
    /// the step, the others join it). Refused, with nothing changed, when it touches what a running job depends on.
    pub fn apply(&mut self, edit: Edit) -> Result<(), CommandError> {
        if matches!(&edit, Edit::Batch(edits) if edits.is_empty()) {
            return Ok(());
        }
        let aspects = edit.aspects();
        self.check_locks(aspects)?;
        let current = self.current.clone().unwrap_or(Current { command: "", title: "Edit", coalesce: None, stepped: false });
        let open = self.writable()?;
        let inverse = open.project.apply(edit)?;
        if current.stepped || open.history.coalesces(current.command, current.coalesce.as_deref()) {
            open.history.merge(inverse);
        } else {
            open.history.record(current.title, current.command, inverse, current.coalesce.clone());
        }
        if let Some(c) = &mut self.current {
            c.stepped = true;
        }
        self.changed(aspects);
        Ok(())
    }

    /// Changes the project without an undo step: bookkeeping that the user didn't ask for, such as a cached result.
    /// The project needs saving after it.
    pub fn apply_system(&mut self, edit: Edit) -> Result<(), CommandError> {
        let aspects = edit.aspects();
        self.check_locks(aspects)?;
        let open = self.writable()?;
        open.project.apply(edit)?;
        open.history.system_edit();
        self.changed(aspects);
        Ok(())
    }

    /// Undoes the last step and returns its label.
    pub(crate) fn undo(&mut self) -> Result<String, CommandError> {
        let aspects = match self.open.as_ref().and_then(|o| o.history.undo.last()) {
            Some(step) => step.edit.aspects(),
            None if self.open.is_some() => return Err(self.unavailable("there is nothing to undo")),
            None => return Err(self.no_project()),
        };
        self.check_locks(aspects)?;
        let nothing = self.unavailable("there is nothing to undo");
        let open = self.writable()?;
        let Some(step) = open.history.take_undo() else { return Err(nothing) };
        match open.project.apply(step.edit.clone()) {
            Ok(redo) => {
                let label = step.label.clone();
                open.history.undone(step, redo);
                self.changed(aspects);
                Ok(label)
            }
            Err(e) => {
                open.history.restore_undo(step);
                Err(e.into())
            }
        }
    }

    /// Redoes the last undone step and returns its label.
    pub(crate) fn redo(&mut self) -> Result<String, CommandError> {
        let aspects = match self.open.as_ref().and_then(|o| o.history.redo.last()) {
            Some(step) => step.edit.aspects(),
            None if self.open.is_some() => return Err(self.unavailable("there is nothing to redo")),
            None => return Err(self.no_project()),
        };
        self.check_locks(aspects)?;
        let nothing = self.unavailable("there is nothing to redo");
        let open = self.writable()?;
        let Some(step) = open.history.take_redo() else { return Err(nothing) };
        match open.project.apply(step.edit.clone()) {
            Ok(undo) => {
                let label = step.label.clone();
                open.history.redone(step, undo);
                self.changed(aspects);
                Ok(label)
            }
            Err(e) => {
                open.history.restore_redo(step);
                Err(e.into())
            }
        }
    }

    /// Makes `folder` and `project` the open project, after closing the one open (see [`Session::close_project`]).
    /// The cache files that nothing refers to are removed. Returns how many.
    pub(crate) fn open_project(&mut self, folder: ProjectFolder, project: Project) -> usize {
        let cleaned = if folder.is_read_only() {
            0
        } else {
            folder.remove_unreferenced(&project).map_or_else(
                |e| {
                    tracing::warn!("cannot remove the unused cache files of {}: {e}", folder.path().display());
                    0
                },
                |files| files.len(),
            )
        };
        let path = folder.path().display().to_string();
        self.open = Some(Open { folder, project, history: History::new() });
        self.events.push(Event::Project { state: ProjectEvent::Opened, path });
        self.changed(Aspects::ALL);
        cleaned
    }

    /// Closes the open project: refused when it has unsaved changes, unless `discard`. Its jobs are cancelled first,
    /// and waited for, since they may still be writing into its folder; then the cache files that nothing refers to
    /// any more (the undo history is gone with it) are removed and the folder's lock is released. Returns the folder
    /// and how many files were removed, or `None` when no project was open.
    pub(crate) fn close_project(&mut self, discard: bool) -> Result<Option<(PathBuf, usize)>, CommandError> {
        let Some(open) = &self.open else { return Ok(None) };
        if open.history.is_dirty() && !open.folder.is_read_only() && !discard {
            return Err(
                self.unavailable("the project has unsaved changes: save it first with `project.save`, or pass `discardChanges` to throw them away")
            );
        }
        self.jobs.cancel_all();
        while self.jobs.has_running() {
            self.pump(Some(STOP_POLL));
        }
        let Some(open) = self.open.take() else { return Ok(None) };
        let cleaned = if open.folder.is_read_only() {
            0
        } else {
            open.folder.remove_unreferenced(&open.project).map_or_else(
                |e| {
                    tracing::warn!("cannot remove the unused cache files of {}: {e}", open.folder.path().display());
                    0
                },
                |files| files.len(),
            )
        };
        let path = open.folder.path().to_path_buf();
        drop(open);
        self.events.push(Event::Project { state: ProjectEvent::Closed, path: path.display().to_string() });
        self.changed(Aspects::ALL);
        Ok(Some((path, cleaned)))
    }

    /// Saves the open project.
    pub(crate) fn save(&mut self) -> Result<PathBuf, CommandError> {
        let open = self.writable()?;
        open.folder.save(&mut open.project)?;
        open.history.mark_saved();
        let path = open.folder.path().to_path_buf();
        self.history_changed = true;
        self.events.push(Event::Project { state: ProjectEvent::Saved, path: path.display().to_string() });
        Ok(path)
    }

    fn check_requires(&self, spec: &CommandSpec) -> Result<(), CommandError> {
        match spec.info.requires {
            Requires::Nothing => Ok(()),
            Requires::Project => self.project().map(drop),
            Requires::WritableProject => match &self.open {
                None => Err(self.no_project()),
                Some(open) if open.folder.is_read_only() => Err(self.unavailable("the project was opened read-only")),
                Some(_) => Ok(()),
            },
        }
    }

    /// Runs a command with JSON parameters.
    pub(crate) fn dispatch(
        &mut self,
        spec: &Arc<CommandSpec>,
        params: serde_json::Value,
        coalesce: Option<String>,
    ) -> Result<Dispatched, CommandError> {
        let name = spec.name();
        let outer = self.current.replace(Current { command: name, title: spec.info.title, coalesce, stepped: false });
        let result = self.run(spec, params);
        self.current = outer;
        result
    }

    fn run(&mut self, spec: &Arc<CommandSpec>, params: serde_json::Value) -> Result<Dispatched, CommandError> {
        let name = spec.name();
        let params = normalize(name, params)?;
        self.check_requires(spec)?;
        match &spec.run {
            Runner::Query(f) => guarded(name, || f(self, params)).map(Dispatched::Done),
            Runner::Instant(f) => guarded(name, || f(self, params)).map(Dispatched::Done),
            Runner::Job(f) => {
                (spec.check)(params.clone())?;
                let lane = spec.lane().unwrap_or(Lane::Heavy);
                if !self.jobs.has_room(lane) {
                    let id = self.jobs.submit(spec.clone(), lane, params);
                    return Ok(Dispatched::Started(id));
                }
                // The lane is free: plan the job now, so that an error in the plan is the dispatch's.
                let plan = guarded(name, || f(self, params))?;
                let id = self.jobs.submit_running(spec.clone(), lane);
                if let Err(e) = self.jobs.launch(id, plan) {
                    self.jobs.fail(id, e);
                    return Err(self.jobs.take_failure(id).unwrap_or(CommandError::Internal { command: name.to_owned() }));
                }
                Ok(Dispatched::Started(id))
            }
        }
    }

    /// Starts queued jobs whose lane has room.
    pub(crate) fn start_queued(&mut self) {
        while let Some((id, spec, params)) = self.jobs.next_to_start() {
            let outer = self.current.replace(Current { command: spec.name(), title: spec.info.title, coalesce: None, stepped: false });
            let planned = self.check_requires(&spec).and_then(|()| match &spec.run {
                Runner::Job(f) => guarded(spec.name(), || f(self, params)),
                Runner::Query(_) | Runner::Instant(_) => Err(CommandError::Internal { command: spec.name().to_owned() }),
            });
            self.current = outer;
            if let Err(e) = planned.and_then(|plan| self.jobs.launch(id, plan)) {
                self.jobs.fail(id, e);
            }
        }
    }

    /// Commits a finished job: runs its commit unless it was cancelled or failed.
    fn commit(&mut self, mut finished: Finished) {
        let outcome = std::mem::replace(&mut finished.outcome, Err(CommandError::Cancelled));
        let result = if finished.cancelled {
            Err(CommandError::Cancelled)
        } else {
            match outcome {
                Ok(commit) => {
                    let spec = finished.spec.clone();
                    let outer = self.current.replace(Current { command: spec.name(), title: spec.info.title, coalesce: None, stepped: false });
                    let result = guarded(spec.name(), || commit(self));
                    self.current = outer;
                    result
                }
                Err(e) => Err(e),
            }
        };
        if let Err(e) = &result
            && !matches!(e, CommandError::Cancelled)
        {
            tracing::warn!("job {} (`{}`) failed: {e}", finished.id.0, finished.spec.name());
        }
        self.jobs.complete(finished, result);
    }

    /// Handles what workers sent: progress, and finished jobs, which are committed; then starts queued jobs. Waits up
    /// to `timeout` for the first message (not at all with `None`). Returns how many messages there were.
    pub(crate) fn pump(&mut self, timeout: Option<Duration>) -> usize {
        let mut count = 0;
        let mut wait = timeout;
        while let Some(message) = self.jobs.receive(wait.take()) {
            count += 1;
            if let Some(finished) = self.jobs.handle(message) {
                self.commit(finished);
            }
        }
        self.start_queued();
        count
    }

    /// Whether job `id` is still queued or running.
    pub(crate) fn job_active(&self, id: JobId) -> bool {
        self.jobs.is_active(id)
    }

    /// The events since the last call: project, change and history events, then job events.
    pub(crate) fn take_events(&mut self) -> Vec<Event> {
        let mut events = std::mem::take(&mut self.events);
        if !self.changed.is_empty() {
            events.push(Event::Changed { revision: self.revision, aspects: self.changed });
            self.changed = Aspects::NONE;
        }
        if std::mem::take(&mut self.history_changed) {
            events.push(Event::History {
                undo: self.undo_label().map(str::to_owned),
                redo: self.redo_label().map(str::to_owned),
                dirty: self.is_dirty(),
            });
        }
        events.extend(self.jobs.take_changed().into_iter().map(|job| Event::Job { job: Box::new(job) }));
        events
    }
}
