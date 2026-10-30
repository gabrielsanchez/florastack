//! The jobs of one engine: the queue of each lane, the running jobs and the last finished ones. It lives on the
//! engine's thread; workers reach it only through its inbox.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use florastack_project::Aspects;
use serde_json::Value;

use super::{Clock, Commit, Executor, JobContext, JobId, JobInfo, JobPlan, JobState, Lane, Message, Phase, Tracker, Waker};
use crate::error::CommandError;
use crate::registry::CommandSpec;

/// The most finished jobs kept for `job.list` and [`crate::Engine::wait`].
const RECENT: usize = 64;

enum State {
    Queued(Value),
    Running,
}

struct Active {
    id: JobId,
    spec: Arc<CommandSpec>,
    lane: Lane,
    state: State,
    phase: Arc<Phase>,
    locks: Aspects,
    tracker: Tracker,
    preview: Option<super::LivePreview>,
}

impl Active {
    fn info(&self, now: Duration) -> JobInfo {
        JobInfo {
            id: self.id,
            command: self.spec.name().to_owned(),
            title: self.spec.describe().title.to_owned(),
            lane: self.lane,
            state: match self.state {
                State::Queued(_) => JobState::Queued,
                State::Running => JobState::Running,
            },
            cancel_requested: self.phase.is_cancelled(),
            progress: self.tracker.progress(now),
            preview: self.preview.clone(),
            result: None,
            error: None,
        }
    }
}

/// A job whose work has ended, for the engine to commit (or not).
pub(crate) struct Finished {
    pub(crate) id: JobId,
    pub(crate) spec: Arc<CommandSpec>,
    pub(crate) cancelled: bool,
    pub(crate) outcome: Result<Commit<Value>, CommandError>,
    info: JobInfo,
}

/// The jobs. See the module documentation.
pub(crate) struct Jobs {
    executor: Arc<dyn Executor>,
    clock: Arc<dyn Clock>,
    waker: Option<Waker>,
    max_heavy: usize,
    max_preview: usize,
    next: u64,
    tx: Sender<Message>,
    rx: Receiver<Message>,
    active: Vec<Active>,
    recent: VecDeque<JobInfo>,
    /// Jobs whose state or progress changed since the engine last looked.
    changed: Vec<JobInfo>,
    /// Why the recently failed jobs failed, until [`Jobs::take_failure`] takes it.
    failures: HashMap<JobId, CommandError>,
}

impl Jobs {
    pub(crate) fn new(executor: Arc<dyn Executor>, clock: Arc<dyn Clock>, waker: Option<Waker>, max_heavy: usize, max_preview: usize) -> Jobs {
        let (tx, rx) = std::sync::mpsc::channel();
        Jobs {
            executor,
            clock,
            waker,
            max_heavy: max_heavy.max(1),
            max_preview: max_preview.max(1),
            next: 1,
            tx,
            rx,
            active: Vec::new(),
            recent: VecDeque::new(),
            changed: Vec::new(),
            failures: HashMap::new(),
        }
    }

    fn now(&self) -> Duration {
        self.clock.now()
    }

    /// Queues a job of `spec` with checked `params`, to be planned when its lane has room.
    pub(crate) fn submit(&mut self, spec: Arc<CommandSpec>, lane: Lane, params: Value) -> JobId {
        self.add(spec, lane, State::Queued(params))
    }

    /// Adds a job of `spec` that is planned already and starts at once ([`Jobs::launch`]).
    pub(crate) fn submit_running(&mut self, spec: Arc<CommandSpec>, lane: Lane) -> JobId {
        self.add(spec, lane, State::Running)
    }

    fn add(&mut self, spec: Arc<CommandSpec>, lane: Lane, state: State) -> JobId {
        if spec.supersede {
            let older: Vec<JobId> = self.active.iter().filter(|a| a.spec.name() == spec.name()).map(|a| a.id).collect();
            for id in older {
                self.cancel(id);
            }
        }
        let id = JobId(self.next);
        self.next = self.next.saturating_add(1);
        let now = self.now();
        let job =
            Active { id, spec, lane, state, phase: Arc::new(Phase::default()), locks: Aspects::NONE, tracker: Tracker::new(&[], now), preview: None };
        self.changed.push(job.info(now));
        self.active.push(job);
        id
    }

    fn running(&self, lane: Lane) -> usize {
        self.active.iter().filter(|a| a.lane == lane && matches!(a.state, State::Running)).count()
    }

    /// Whether a job submitted to `lane` now would start at once: the lane has room and nobody waits in it.
    pub(crate) fn has_room(&self, lane: Lane) -> bool {
        let max = if lane == Lane::Heavy { self.max_heavy } else { self.max_preview };
        self.running(lane) < max && !self.active.iter().any(|a| a.lane == lane && matches!(a.state, State::Queued(_)))
    }

    /// The next queued job whose lane has room, marked running, with its spec and parameters, for the engine to plan.
    pub(crate) fn next_to_start(&mut self) -> Option<(JobId, Arc<CommandSpec>, Value)> {
        for lane in [Lane::Heavy, Lane::Preview] {
            let max = if lane == Lane::Heavy { self.max_heavy } else { self.max_preview };
            if self.running(lane) >= max {
                continue;
            }
            let now = self.now();
            if let Some(job) = self.active.iter_mut().find(|a| a.lane == lane && matches!(a.state, State::Queued(_))) {
                let State::Queued(params) = std::mem::replace(&mut job.state, State::Running) else { continue };
                job.tracker.restart(now);
                return Some((job.id, job.spec.clone(), params));
            }
        }
        None
    }

    /// Starts a planned job's work on a worker.
    pub(crate) fn launch(&mut self, id: JobId, plan: JobPlan<Value>) -> Result<(), CommandError> {
        let now = self.now();
        let (tx, waker, clock, executor) = (self.tx.clone(), self.waker.clone(), self.clock.clone(), self.executor.clone());
        let Some(job) = self.active.iter_mut().find(|a| a.id == id) else { return Ok(()) };
        job.locks = plan.locks;
        job.tracker = Tracker::new(&plan.stages, now);
        let info = job.info(now);
        let (lane, name, phase) = (job.lane, job.spec.name(), job.phase.clone());
        self.changed.push(info);
        let ctx = JobContext::new(id, phase, tx, waker, clock);
        let work = plan.work;
        let task = Box::new(move || {
            // Last resort (AGENTS.md, Never crash): a panic in a job is that job's failure.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&ctx))).unwrap_or_else(|_| {
                tracing::error!("the job `{name}` panicked");
                Err(CommandError::Internal { command: name.to_owned() })
            });
            ctx.finish(outcome);
        });
        executor.spawn(lane, task).map_err(|source| CommandError::Spawn { command: name.to_owned(), source })
    }

    /// Ends a job that couldn't be planned or started.
    pub(crate) fn fail(&mut self, id: JobId, error: CommandError) {
        let now = self.now();
        if let Some(i) = self.active.iter().position(|a| a.id == id) {
            let job = self.active.remove(i);
            let mut info = job.info(now);
            if matches!(error, CommandError::Cancelled) {
                info.state = JobState::Cancelled;
            } else {
                info.state = JobState::Failed;
                info.error = Some(error.info());
                self.failures.insert(id, error);
            }
            self.finished(info);
        }
    }

    fn finished(&mut self, info: JobInfo) {
        self.changed.push(info.clone());
        self.recent.push_back(info);
        while self.recent.len() > RECENT {
            if let Some(old) = self.recent.pop_front() {
                self.failures.remove(&old.id);
            }
        }
    }

    /// Why job `id` failed, taken (the job's info keeps the message).
    pub(crate) fn take_failure(&mut self, id: JobId) -> Option<CommandError> {
        self.failures.remove(&id)
    }

    /// Asks a job to stop: a queued one ends at once, a running one at its next checkpoint. False when no such job is
    /// queued or running, or when it is past its point of no return ([`JobContext::finalize`]).
    pub(crate) fn cancel(&mut self, id: JobId) -> bool {
        let now = self.now();
        let Some(i) = self.active.iter().position(|a| a.id == id) else { return false };
        let queued = self.active.get(i).is_some_and(|a| matches!(a.state, State::Queued(_)));
        if queued {
            let job = self.active.remove(i);
            let mut info = job.info(now);
            info.state = JobState::Cancelled;
            info.cancel_requested = true;
            self.finished(info);
        } else if let Some(job) = self.active.get(i) {
            if !job.phase.cancel() {
                return false;
            }
            self.changed.push(job.info(now));
        }
        true
    }

    /// Asks every job to stop.
    pub(crate) fn cancel_all(&mut self) {
        let ids: Vec<JobId> = self.active.iter().map(|a| a.id).collect();
        for id in ids {
            self.cancel(id);
        }
    }

    pub(crate) fn has_running(&self) -> bool {
        self.active.iter().any(|a| matches!(a.state, State::Running))
    }

    pub(crate) fn is_active(&self, id: JobId) -> bool {
        self.active.iter().any(|a| a.id == id)
    }

    /// The running job that locks any of `aspects`, with what it locks of them.
    pub(crate) fn conflict(&self, aspects: Aspects) -> Option<(&'static str, Aspects)> {
        self.active
            .iter()
            .filter(|a| matches!(a.state, State::Running) && a.locks.intersects(aspects))
            .map(|a| (a.spec.describe().title, a.locks.intersection(aspects)))
            .next()
    }

    /// Handles one message from a worker; a finished job is returned for the engine to commit.
    pub(crate) fn handle(&mut self, message: Message) -> Option<Finished> {
        let now = self.now();
        match message {
            Message::Preview { job, preview } => {
                if let Some(a) = self.active.iter_mut().find(|a| a.id == job) {
                    a.preview = Some(preview);
                    self.changed.push(a.info(now));
                }
                None
            }
            Message::Progress { job, stage, done, total, at } => {
                if let Some(a) = self.active.iter_mut().find(|a| a.id == job) {
                    a.tracker.update(stage, done, total, at);
                    let info = a.info(now);
                    self.changed.push(info);
                }
                None
            }
            Message::Finished { job, outcome } => {
                let i = self.active.iter().position(|a| a.id == job)?;
                let mut a = self.active.remove(i);
                a.tracker.complete(now);
                let cancelled = a.phase.is_cancelled();
                Some(Finished { id: job, spec: a.spec.clone(), cancelled, outcome, info: a.info(now) })
            }
        }
    }

    /// Records how a finished job ended: its result, or why it failed or was cancelled.
    pub(crate) fn complete(&mut self, finished: Finished, result: Result<Value, CommandError>) {
        let Finished { info, cancelled, .. } = finished;
        let id = info.id;
        let info = match result {
            Ok(value) => JobInfo { state: JobState::Done, result: Some(value), ..info },
            Err(CommandError::Cancelled) => JobInfo { state: JobState::Cancelled, cancel_requested: cancelled, ..info },
            Err(e) => {
                let error = Some(e.info());
                self.failures.insert(id, e);
                JobInfo { state: JobState::Failed, error, ..info }
            }
        };
        self.finished(info);
    }

    /// The next message, waiting up to `timeout` for one (not at all with `None`).
    pub(crate) fn receive(&self, timeout: Option<Duration>) -> Option<Message> {
        match timeout {
            None => self.rx.try_recv().ok(),
            Some(t) => self.rx.recv_timeout(t).ok(),
        }
    }

    /// A job queued, running or recently finished.
    pub(crate) fn info(&self, id: JobId) -> Option<JobInfo> {
        let now = self.now();
        self.active.iter().find(|a| a.id == id).map(|a| a.info(now)).or_else(|| self.recent.iter().rev().find(|j| j.id == id).cloned())
    }

    /// The jobs queued and running, then the last ones that finished, oldest first.
    pub(crate) fn list(&self) -> Vec<JobInfo> {
        let now = self.now();
        self.active.iter().map(|a| a.info(now)).chain(self.recent.iter().cloned()).collect()
    }

    pub(crate) fn take_changed(&mut self) -> Vec<JobInfo> {
        std::mem::take(&mut self.changed)
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        // Workers own snapshots only; stop them so that they don't keep working for nobody.
        for job in &self.active {
            job.phase.cancel();
        }
    }
}
