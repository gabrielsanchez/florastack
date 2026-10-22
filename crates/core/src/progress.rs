//! Progress and cancellation for long operations: a [`Monitor`] that an operation reports its progress to and asks,
//! at its checkpoints, whether it should stop.
//!
//! Alignment, stacking and the other long steps run as cancellable jobs (`florastack-engine`'s job engine), but the
//! crates that implement them can't depend on the engine. They take a `&dyn Monitor` instead, which the engine's job
//! context implements: the operation calls [`Monitor::step`] after each unit of work (a frame, a tile, a band of
//! rows), and stops with [`Cancelled`] (wrapped in its own error type) when the step says so. Between two checkpoints
//! the operation must not leave anything half done that its caller would have to clean up: its result is discarded
//! when it stops.
//!
//! A monitor is `Sync`, so that the iterations of a rayon parallel loop can check it and report to it from any
//! thread. [`Unmonitored`] is the monitor of a caller that neither cancels nor shows progress.

/// Why an operation stopped before its end: it was asked to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("cancelled")]
pub struct Cancelled;

/// Where a long operation reports its progress and learns that it should stop. See the module documentation.
pub trait Monitor: Sync {
    /// Whether the operation has been asked to stop.
    fn is_cancelled(&self) -> bool;

    /// Reports that `done` of `total` units of the current stage are done (`total` 0: unknown).
    fn progress(&self, done: u64, total: u64);

    /// A checkpoint: `Err(Cancelled)` once the operation has been asked to stop.
    fn checkpoint(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() { Err(Cancelled) } else { Ok(()) }
    }

    /// Reports progress, then checks for cancellation: what an operation calls after each unit of work.
    fn step(&self, done: u64, total: u64) -> Result<(), Cancelled> {
        self.progress(done, total);
        self.checkpoint()
    }
}

/// A monitor that is never cancelled and ignores progress.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unmonitored;

impl Monitor for Unmonitored {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn progress(&self, _done: u64, _total: u64) {}
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use super::*;

    struct Flag {
        cancel: AtomicBool,
        done: AtomicU64,
    }

    impl Monitor for Flag {
        fn is_cancelled(&self) -> bool {
            self.cancel.load(Ordering::Relaxed)
        }

        fn progress(&self, done: u64, _total: u64) {
            self.done.store(done, Ordering::Relaxed);
        }
    }

    /// A loop that checks its monitor after each item, as the stacking and alignment loops do.
    fn count(monitor: &dyn Monitor, n: u64) -> Result<u64, Cancelled> {
        let mut sum = 0;
        for i in 0..n {
            sum += i;
            monitor.step(i + 1, n)?;
        }
        Ok(sum)
    }

    #[test]
    fn a_cancelled_monitor_stops_at_the_next_checkpoint() {
        assert_eq!(count(&Unmonitored, 10), Ok(45));
        let flag = Flag { cancel: AtomicBool::new(false), done: AtomicU64::new(0) };
        assert_eq!(count(&flag, 4), Ok(6));
        assert_eq!(flag.done.load(Ordering::Relaxed), 4, "progress reached the end");
        flag.cancel.store(true, Ordering::Relaxed);
        assert_eq!(count(&flag, 4), Err(Cancelled));
        assert_eq!(flag.done.load(Ordering::Relaxed), 1, "stopped after the first item");
        assert_eq!(Unmonitored.checkpoint(), Ok(()));
    }
}
