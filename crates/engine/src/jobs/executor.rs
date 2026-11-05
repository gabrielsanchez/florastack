//! Where jobs run: an [`Executor`] takes a job's task and runs it in its lane.
//!
//! - [`ThreadExecutor`] (native): a heavy job gets a thread of its own, which coordinates it; its data-parallel work
//!   runs on rayon's global pool, so it can use every core. Preview jobs run on a small rayon pool of their own
//!   (a quarter of the cores, at least one), so that a preview never takes more than its share from the heavy job,
//!   and the heavy job can't starve it either. That bounded share is what "lower priority" means: the standard
//!   library has no portable thread priorities.
//! - [`InlineExecutor`]: runs every task at once, on the calling thread: for a single-threaded host, and for tests that
//!   need jobs to finish in order.
//! - [`LaneExecutor`]: one thread per lane, started with its first task and kept, running its lane's tasks in order;
//!   their data-parallel work runs on rayon's global pool. The browser build's executor: its threads are Web Workers
//!   (`florastack_core::thread`), which take a moment to start and which the page's main thread may not wait for.

#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;
use std::sync::mpsc::Sender;

use crate::jobs::Lane;

/// A job's task: its work, which reports back through the channel it holds.
pub type Task = Box<dyn FnOnce() + Send + 'static>;

/// Runs jobs' tasks. See the module documentation.
pub trait Executor: Send + Sync {
    /// Starts `task` in `lane`. When it can't (no thread can be made), the task is dropped and the error returned.
    fn spawn(&self, lane: Lane, task: Task) -> Result<(), std::io::Error>;
}

/// Runs each task on the calling thread, before `spawn` returns.
#[derive(Clone, Copy, Debug, Default)]
pub struct InlineExecutor;

impl Executor for InlineExecutor {
    fn spawn(&self, _lane: Lane, task: Task) -> Result<(), std::io::Error> {
        task();
        Ok(())
    }
}

/// Threads for heavy jobs, a small rayon pool for previews. See the module documentation.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
pub struct ThreadExecutor {
    preview_threads: Option<usize>,
    previews: OnceLock<Option<rayon::ThreadPool>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ThreadExecutor {
    pub fn new() -> ThreadExecutor {
        ThreadExecutor::default()
    }

    /// An executor whose preview pool has `threads` threads (at least one).
    pub fn with_preview_threads(threads: usize) -> ThreadExecutor {
        ThreadExecutor { preview_threads: Some(threads.max(1)), previews: OnceLock::new() }
    }

    fn previews(&self) -> Option<&rayon::ThreadPool> {
        self.previews
            .get_or_init(|| {
                let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
                let threads = self.preview_threads.unwrap_or((cores / 4).max(1));
                match rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|i| format!("florastack-preview-{i}")).build() {
                    Ok(pool) => Some(pool),
                    Err(e) => {
                        tracing::warn!("no preview thread pool ({e}): previews run on threads of their own");
                        None
                    }
                }
            })
            .as_ref()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Executor for ThreadExecutor {
    fn spawn(&self, lane: Lane, task: Task) -> Result<(), std::io::Error> {
        if lane == Lane::Preview
            && let Some(pool) = self.previews()
        {
            pool.spawn(task);
            return Ok(());
        }
        let name = match lane {
            Lane::Heavy => "florastack-job",
            Lane::Preview => "florastack-preview",
            Lane::Device => "florastack-device",
        };
        std::thread::Builder::new().name(name.into()).spawn(task).map(drop)
    }
}

/// One thread per lane. See the module documentation.
#[derive(Debug, Default)]
pub struct LaneExecutor {
    heavy: florastack_core::sync::OnceLock<Option<Sender<Task>>>,
    preview: florastack_core::sync::OnceLock<Option<Sender<Task>>>,
    device: florastack_core::sync::OnceLock<Option<Sender<Task>>>,
}

impl LaneExecutor {
    pub fn new() -> LaneExecutor {
        LaneExecutor::default()
    }
}

impl Executor for LaneExecutor {
    fn spawn(&self, lane: Lane, task: Task) -> Result<(), std::io::Error> {
        let (cell, name) = match lane {
            Lane::Heavy => (&self.heavy, "florastack-job"),
            Lane::Preview => (&self.preview, "florastack-preview"),
            Lane::Device => (&self.device, "florastack-device"),
        };
        let sender = cell.get_or_init(|| {
            let (sender, tasks) = std::sync::mpsc::channel::<Task>();
            match florastack_core::thread::spawn(name, move || tasks.into_iter().for_each(|task| task())) {
                Ok(()) => Some(sender),
                Err(e) => {
                    tracing::warn!("no {name} thread: {e}");
                    None
                }
            }
        });
        let sender = sender.as_ref().ok_or_else(|| std::io::Error::other(format!("no {name} thread")))?;
        sender.send(task).map_err(|_| std::io::Error::other(format!("the {name} thread has stopped")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lane_executor_runs_each_lanes_tasks_in_order_on_one_thread() {
        let executor = LaneExecutor::new();
        let (sender, results) = std::sync::mpsc::channel();
        for i in 0..4 {
            let sender = sender.clone();
            executor.spawn(Lane::Heavy, Box::new(move || sender.send((i, std::thread::current().id())).unwrap())).unwrap();
        }
        let got: Vec<_> = results.iter().take(4).collect();
        assert_eq!(got.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert!(got.iter().all(|(_, t)| *t == got[0].1), "one thread for the lane");
        assert_ne!(got[0].1, std::thread::current().id());
        let (sender, results) = std::sync::mpsc::channel();
        executor.spawn(Lane::Preview, Box::new(move || sender.send(std::thread::current().id()).unwrap())).unwrap();
        assert_ne!(results.recv().unwrap(), got[0].1, "another for previews");
    }
}
