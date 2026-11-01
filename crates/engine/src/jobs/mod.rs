//! The job engine: long commands (adding frames, writing outputs; later aligning and stacking) run as cancellable
//! jobs on workers, report their progress, and stop cleanly at their next checkpoint when cancelled.
//!
//! # A job's life
//!
//! 1. **Queued.** Dispatching a job command checks its parameters and queues it in its [`Lane`].
//! 2. **Running.** When its lane has room, the command's function plans the job on the engine's thread ([`JobPlan`]):
//!    it takes a snapshot of what the job reads (frame paths, settings, cache references, a
//!    [`CacheDir`](florastack_project::CacheDir) to write new cache files with) and says which [`Aspects`] of the
//!    project the job depends on. Those are locked until the job ends: an edit that would change them, from a command,
//!    an undo or a redo, is refused with [`CommandError::Conflict`]. The plan's work then runs on a worker
//!    ([`Executor`]), with a [`JobContext`] to report progress to and to check for cancellation at its checkpoints
//!    (it implements [`florastack_core::Monitor`], which the libraries take).
//! 3. **Committed.** The work returns a commit, a closure that the engine runs back on its own thread, where the
//!    project lives: it changes the project, as one undo step, and returns the job's result.
//!
//! A job that is cancelled, fails or panics never reaches its commit: the project is exactly as it was. Cache files it
//! wrote are new files that nothing refers to, removed when the project is next closed or opened. Managed capture
//! deliberately publishes completed original photos incrementally to the watched folder: cancellation preserves those
//! originals while discarding staged stack outputs. Cancelling wins over
//! a job that finishes anyway: its commit is dropped.
//!
//! # Lanes
//!
//! [`Lane::Heavy`] runs one job at a time by default (the others wait their turn, in order); [`Lane::Preview`] runs
//! up to two alongside it, on a smaller share of the machine ([`executor`]), and a newer preview of a command that
//! supersedes its older calls cancels them. The scheduler itself lives on the engine's thread: workers only send
//! messages (progress and their outcome) to the engine's inbox, a channel the engine drains when it is polled or waits,
//! so that the engine's thread never shares a project lock with a worker (the browser build's main thread may not block).
//! [`Lane::Device`] serializes camera and rail I/O independently; device queries use `try_lock` to stay responsive.

mod clock;
pub mod executor;
mod progress;
mod scheduler;

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use florastack_core::{Cancelled, Monitor};
use florastack_project::Aspects;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(not(target_arch = "wasm32"))]
pub use clock::SystemClock;
pub use clock::{Clock, ManualClock};
#[cfg(not(target_arch = "wasm32"))]
pub use executor::ThreadExecutor;
pub use executor::{Executor, InlineExecutor, Task};
pub use progress::Progress;
pub(crate) use progress::Tracker;
pub(crate) use scheduler::{Finished, Jobs};

use crate::error::{CommandError, ErrorInfo};
use crate::session::Session;

/// Called from workers when a job has news, so that a UI polls the engine soon (egui's `request_repaint`).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A job's id, unique in its engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct JobId(pub u64);

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "job {}", self.0)
    }
}

/// Where a job runs. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Lane {
    /// Long work, one job at a time by default.
    Heavy,
    /// Quick previews alongside, on a smaller share of the machine.
    Preview,
    /// Serialized camera/rail I/O, alongside image processing.
    Device,
}

/// Where a job is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    /// Whether the job has ended.
    pub fn is_finished(self) -> bool {
        matches!(self, JobState::Done | JobState::Failed | JobState::Cancelled)
    }
}

/// A job, as `job.list` and the job events show it.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: JobId,
    pub command: String,
    pub title: String,
    pub lane: Lane,
    pub state: JobState,
    /// Cancelling was asked for: the job stops at its next checkpoint.
    pub cancel_requested: bool,
    pub progress: Progress,
    /// The latest running capture preview; replaced after each successful ingest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<LivePreview>,
    /// The command's result, once done.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Why it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

/// A capture preview clients can display while a watch job is running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LivePreview {
    /// A lossless TIFF, available until the next preview or the job ends.
    pub path: String,
    /// The number of ingested frames.
    pub frames: usize,
}

/// What a job's work returns: the change to make back on the engine's thread, and the command's result.
pub type Commit<R> = Box<dyn FnOnce(&mut Session) -> Result<R, CommandError> + Send>;

/// A commit, boxed.
pub fn commit<R>(f: impl FnOnce(&mut Session) -> Result<R, CommandError> + Send + 'static) -> Commit<R> {
    Box::new(f)
}

type Work<R> = Box<dyn FnOnce(&JobContext) -> Result<Commit<R>, CommandError> + Send>;

/// How a job runs: its stages, the aspects it locks and its work. See the module documentation.
pub struct JobPlan<R> {
    pub(crate) stages: Vec<(&'static str, f32)>,
    pub(crate) locks: Aspects,
    pub(crate) work: Work<R>,
}

impl<R: 'static> JobPlan<R> {
    /// A job that does `work` (on a worker) in one stage and locks nothing.
    pub fn new(work: impl FnOnce(&JobContext) -> Result<Commit<R>, CommandError> + Send + 'static) -> JobPlan<R> {
        JobPlan { stages: Vec::new(), locks: Aspects::NONE, work: Box::new(work) }
    }

    /// The job's stages, by name and weight (its share of the whole); the work moves through them with
    /// [`JobContext::stage`].
    #[must_use]
    pub fn stages(mut self, stages: &[(&'static str, f32)]) -> JobPlan<R> {
        self.stages = stages.to_vec();
        self
    }

    /// The aspects of the project the job reads, which no edit may change until it ends.
    #[must_use]
    pub fn locks(mut self, aspects: Aspects) -> JobPlan<R> {
        self.locks = aspects;
        self
    }

    pub(crate) fn map_result(self, f: impl FnOnce(R) -> Result<Value, CommandError> + Send + 'static) -> JobPlan<Value> {
        let work = self.work;
        JobPlan {
            stages: self.stages,
            locks: self.locks,
            work: Box::new(move |ctx| {
                let commit = work(ctx)?;
                Ok(Box::new(move |session: &mut Session| f(commit(session)?)) as Commit<Value>)
            }),
        }
    }
}

/// Whether a job may still be cancelled: shared by the scheduler, which cancels, and the job's context, which checks
/// and may pass the point after which it can't be ([`JobContext::finalize`]).
#[derive(Debug, Default)]
pub(crate) struct Phase(AtomicU8);

const RUNNING: u8 = 0;
const CANCELLED: u8 = 1;
const FINAL: u8 = 2;

impl Phase {
    /// Asks the job to stop; false once it is past its point of no return.
    pub(crate) fn cancel(&self) -> bool {
        match self.0.compare_exchange(RUNNING, CANCELLED, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) | Err(CANCELLED) => true,
            Err(_) => false,
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == CANCELLED
    }

    fn finalize(&self) -> Result<(), Cancelled> {
        match self.0.compare_exchange(RUNNING, FINAL, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) | Err(FINAL) => Ok(()),
            Err(_) => Err(Cancelled),
        }
    }
}

/// What workers tell the engine.
pub(crate) enum Message {
    Preview { job: JobId, preview: LivePreview },
    Progress { job: JobId, stage: usize, done: u64, total: u64, at: Duration },
    Finished { job: JobId, outcome: Result<Commit<Value>, CommandError> },
}

/// How often a job's progress is sent while it stays in one stage: when this much time has passed since the last
/// report, or a hundredth of the stage, whichever comes first.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// What a job's work sees: where to report progress, and whether it should stop. It is a [`Monitor`], so it can be
/// handed to the libraries' long operations as is, and it is `Sync`, so parallel loops can share it.
pub struct JobContext {
    id: JobId,
    phase: Arc<Phase>,
    tx: Sender<Message>,
    waker: Option<Waker>,
    clock: Arc<dyn Clock>,
    stage: AtomicUsize,
    /// When progress was last sent, in milliseconds of the clock (`u64::MAX`: never), and the count it said.
    last_sent: AtomicU64,
    last_done: AtomicU64,
}

impl JobContext {
    /// Publishes a running capture preview through the ordinary job events.
    pub fn preview(&self, path: String, frames: usize) {
        let _ = self.tx.send(Message::Preview { job: self.id, preview: LivePreview { path, frames } });
        if let Some(wake) = &self.waker {
            wake();
        }
    }
    pub(crate) fn new(id: JobId, phase: Arc<Phase>, tx: Sender<Message>, waker: Option<Waker>, clock: Arc<dyn Clock>) -> JobContext {
        JobContext { id, phase, tx, waker, clock, stage: AtomicUsize::new(0), last_sent: AtomicU64::new(u64::MAX), last_done: AtomicU64::new(0) }
    }

    pub fn id(&self) -> JobId {
        self.id
    }

    /// The job's point of no return, just before it changes something outside the engine that can't be taken back
    /// (writing the file a user asked for): `Err(Cancelled)` if it was cancelled before, and from then on it can't be
    /// cancelled any more, so that a job reported as cancelled has never done its irreversible part.
    pub fn finalize(&self) -> Result<(), Cancelled> {
        self.phase.finalize()
    }

    /// Moves to stage `index` of the plan's stages, with nothing of it done yet.
    pub fn stage(&self, index: usize) {
        self.stage.store(index, Ordering::Relaxed);
        self.send(0, 0);
    }

    fn send(&self, done: u64, total: u64) {
        let at = self.clock.now();
        self.last_sent.store(u64::try_from(at.as_millis()).unwrap_or(u64::MAX - 1), Ordering::Relaxed);
        self.last_done.store(done, Ordering::Relaxed);
        let stage = self.stage.load(Ordering::Relaxed);
        if self.tx.send(Message::Progress { job: self.id, stage, done, total, at }).is_ok()
            && let Some(wake) = &self.waker
        {
            wake();
        }
    }

    pub(crate) fn finish(&self, outcome: Result<Commit<Value>, CommandError>) {
        // The engine may be gone (a closed session): nobody is waiting then.
        let _ = self.tx.send(Message::Finished { job: self.id, outcome });
        if let Some(wake) = &self.waker {
            wake();
        }
    }
}

impl Monitor for JobContext {
    fn is_cancelled(&self) -> bool {
        self.phase.is_cancelled()
    }

    /// Sends progress when 100 ms have passed since the last report or a hundredth of the stage is done since then, so
    /// that a long stage sends at most about 100 reports plus 10 a second, and always at the end of a stage.
    fn progress(&self, done: u64, total: u64) {
        let now = u64::try_from(self.clock.now().as_millis()).unwrap_or(u64::MAX - 1);
        let last = self.last_sent.load(Ordering::Relaxed);
        let interval = u64::try_from(PROGRESS_INTERVAL.as_millis()).unwrap_or(100);
        let moved = total > 0 && (done >= total || done.saturating_sub(self.last_done.load(Ordering::Relaxed)) >= (total / 100).max(1));
        if last == u64::MAX || now.saturating_sub(last) >= interval || moved {
            self.send(done, total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn past_its_point_of_no_return_a_job_cant_be_cancelled() {
        let phase = Phase::default();
        assert!(!phase.is_cancelled());
        assert_eq!(phase.finalize(), Ok(()));
        assert!(!phase.cancel(), "too late");
        assert!(!phase.is_cancelled());
        let phase = Phase::default();
        assert!(phase.cancel());
        assert!(phase.cancel(), "cancelling twice is fine");
        assert_eq!(phase.finalize(), Err(Cancelled), "cancelled before it got there");
    }

    #[test]
    fn progress_is_sent_often_enough_and_not_more() {
        let (tx, rx) = std::sync::mpsc::channel();
        let clock = Arc::new(ManualClock::new());
        let ctx = JobContext::new(JobId(1), Arc::default(), tx, None, clock.clone());
        for done in 0..=1000 {
            ctx.progress(done, 1000);
        }
        let sent = rx.try_iter().count();
        assert!((100..=102).contains(&sent), "a report every hundredth: {sent}");
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = JobContext::new(JobId(2), Arc::default(), tx, None, clock.clone());
        for done in 0..50 {
            ctx.progress(done, 0);
        }
        assert_eq!(rx.try_iter().count(), 1, "an unknown total: by time only");
        clock.advance(Duration::from_millis(100));
        ctx.progress(51, 0);
        assert_eq!(rx.try_iter().count(), 1);
    }
}
