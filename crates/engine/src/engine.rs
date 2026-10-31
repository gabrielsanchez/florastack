//! [`Engine`]: the registry and the session, behind one API that the desktop app, the CLI, the control channel and
//! the MCP server all use.
//!
//! An engine belongs to one thread, the one that calls it (the UI thread, the CLI's main thread, the MCP server's
//! thread): it holds the open project, and nothing else changes it, so there is no lock around the project. Other
//! threads send that thread their requests. Jobs run on workers; [`Engine::poll`] (each UI frame, or when the waker
//! fires) and [`Engine::wait`] bring their progress and results back, commit them, and start the jobs waiting in line.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use florastack_project::{Project, Trash};
use serde_json::Value;

use crate::compute::Compute;
use crate::error::{CommandError, Invalid};
use crate::events::Event;
use crate::jobs::{Clock, Executor, JobId, JobInfo, JobState, Jobs, Waker};
use crate::registry::Registry;
use crate::session::{Host, Session};

/// The largest JSON text [`Engine::dispatch_json`] reads as parameters.
pub const MAX_PARAMS_BYTES: usize = 16 << 20;

/// How an engine is set up.
pub struct EngineConfig {
    /// The folder for temporary (unnamed) projects, and where orphan cleanup looks; the app's data folder.
    pub temporary_projects: Option<PathBuf>,
    /// Where orphaned temporary projects go: the system's trash, or a folder in tests.
    pub trash: Arc<dyn Trash + Send + Sync>,
    pub executor: Arc<dyn Executor>,
    pub clock: Arc<dyn Clock>,
    /// Called from workers when a job has news (see [`Waker`]).
    pub waker: Option<Waker>,
    /// How many heavy jobs run at once (1: one at a time, the others queued).
    pub max_heavy_jobs: usize,
    /// How many preview jobs run at once, alongside the heavy ones.
    pub max_preview_jobs: usize,
    /// Which device stacks, and the GPU's memory budget (`compute.set` changes it; [`crate::compute`]).
    pub compute: Compute,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for EngineConfig {
    /// No temporary-projects folder, the system trash, threads for jobs, the system clock, the GPU when one passes
    /// the self-test.
    fn default() -> EngineConfig {
        EngineConfig {
            temporary_projects: None,
            trash: Arc::new(florastack_project::SystemTrash),
            executor: Arc::new(crate::jobs::ThreadExecutor::new()),
            clock: Arc::new(crate::jobs::SystemClock::new()),
            waker: None,
            max_heavy_jobs: 1,
            max_preview_jobs: 2,
            compute: Compute::default(),
        }
    }
}

/// What dispatching a command did.
#[derive(Clone, Debug, PartialEq)]
pub enum Dispatched {
    /// It ran; its result.
    Done(Value),
    /// It started a job (queued or running); its result comes with the job.
    Started(JobId),
}

impl Dispatched {
    /// As a JSON client sees it: the result, or `{"job": <id>}`.
    pub fn to_json(&self) -> Value {
        match self {
            Dispatched::Done(v) => v.clone(),
            Dispatched::Started(id) => serde_json::json!({ "job": id.0 }),
        }
    }
}

/// How to dispatch a command, beyond its parameters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DispatchOptions {
    /// Calls with the same key, one right after the other, are one undo step: the calls of one slider drag.
    pub coalesce: Option<String>,
}

/// The engine. See the module documentation.
pub struct Engine {
    registry: Arc<Registry>,
    session: Session,
    subscribers: Vec<Sender<Event>>,
}

impl Engine {
    /// An engine with FloraStack's commands.
    pub fn new(config: EngineConfig) -> Engine {
        Engine::with_registry(config, Registry::standard())
    }

    /// An engine with the commands of `registry` (the standard ones and a test's, say).
    pub fn with_registry(config: EngineConfig, registry: Arc<Registry>) -> Engine {
        let jobs = Jobs::new(config.executor, config.clock, config.waker, config.max_heavy_jobs, config.max_preview_jobs);
        let host = Host { temporary_projects: config.temporary_projects, trash: config.trash, compute: config.compute };
        Engine { registry, session: Session::new(host, jobs), subscribers: Vec::new() }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Runs the command `name` with JSON parameters (an object, or `null` for none). A job command returns
    /// [`Dispatched::Started`]; its result comes through [`Engine::wait`], [`Engine::job`] or the job events.
    pub fn dispatch(&mut self, name: &str, params: Value) -> Result<Dispatched, CommandError> {
        self.dispatch_with(name, params, DispatchOptions::default())
    }

    /// [`Engine::dispatch`] with parameters as JSON text, at most [`MAX_PARAMS_BYTES`] long.
    pub fn dispatch_json(&mut self, name: &str, params: &str) -> Result<Dispatched, CommandError> {
        let command = self.registry.spec(name)?.name();
        if params.len() > MAX_PARAMS_BYTES {
            return Err(CommandError::Invalid { command: command.to_owned(), source: Invalid::new("params", "are longer than 16 MiB") });
        }
        let params = if params.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(params).map_err(|source| CommandError::Params { command: command.to_owned(), source })?
        };
        self.dispatch(name, params)
    }

    /// [`Engine::dispatch`] with options.
    pub fn dispatch_with(&mut self, name: &str, params: Value, options: DispatchOptions) -> Result<Dispatched, CommandError> {
        let spec = self.registry.spec(name)?.clone();
        tracing::debug!("dispatch `{}`", spec.name());
        let result = self.session.dispatch(&spec, params, options.coalesce);
        self.flush();
        result
    }

    /// Runs a command to its end: dispatches it and, for a job, waits for its result.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run(&mut self, name: &str, params: Value) -> Result<Value, CommandError> {
        match self.dispatch(name, params)? {
            Dispatched::Done(value) => Ok(value),
            Dispatched::Started(job) => self.wait(job),
        }
    }

    /// Waits for job `job` to end, handling the other jobs' news meanwhile, and returns its result, or why it failed.
    /// Not in the browser build, whose main thread may not block: it polls instead.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn wait(&mut self, job: JobId) -> Result<Value, CommandError> {
        loop {
            if !self.session.job_active(job) {
                self.flush();
                let info = self.session.jobs.info(job).ok_or_else(|| CommandError::NotFound { what: job.to_string() })?;
                return match info.state {
                    JobState::Done => Ok(info.result.unwrap_or(Value::Null)),
                    JobState::Cancelled => Err(CommandError::Cancelled),
                    _ => Err(self.session.jobs.take_failure(job).unwrap_or_else(|| CommandError::Unavailable {
                        command: info.command.clone(),
                        reason: info.error.map_or_else(|| "the job failed".to_owned(), |e| e.message),
                    })),
                };
            }
            self.session.pump(Some(Duration::from_millis(50)));
            self.flush();
        }
    }

    /// Brings the jobs' news in: progress, finished jobs (committed now), queued jobs started. Cheap when nothing
    /// happened; a UI calls it every frame. Returns how many messages there were.
    pub fn poll(&mut self) -> usize {
        let n = self.session.pump(None);
        self.flush();
        n
    }

    /// Asks a job to stop. False when no such job is queued or running.
    pub fn cancel(&mut self, job: JobId) -> bool {
        let cancelled = self.session.jobs.cancel(job);
        self.flush();
        cancelled
    }

    /// The jobs queued and running, then the last ones that finished.
    pub fn jobs(&self) -> Vec<JobInfo> {
        self.session.jobs.list()
    }

    pub fn job(&self, job: JobId) -> Option<JobInfo> {
        self.session.jobs.info(job)
    }

    /// The open project, to show.
    pub fn project(&self) -> Option<&Project> {
        self.session.project().ok()
    }

    /// The open project's folder.
    pub fn project_path(&self) -> Option<&Path> {
        self.session.folder().ok().map(florastack_project::ProjectFolder::path)
    }

    /// Whether the open project has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.session.is_dirty()
    }

    /// What an undo would undo now.
    pub fn undo_label(&self) -> Option<&str> {
        self.session.undo_label()
    }

    /// What a redo would redo now.
    pub fn redo_label(&self) -> Option<&str> {
        self.session.redo_label()
    }

    /// A channel of the engine's [`Event`]s from now on. A receiver that is dropped is forgotten.
    pub fn subscribe(&mut self) -> Receiver<Event> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.subscribers.push(tx);
        rx
    }

    fn flush(&mut self) {
        let events = self.session.take_events();
        if events.is_empty() || self.subscribers.is_empty() {
            return;
        }
        for event in events {
            self.subscribers.retain(|s| s.send(event.clone()).is_ok());
        }
    }
}
