//! The time that job progress and ETAs are measured with: a [`Clock`], so that tests can drive it by hand
//! ([`ManualClock`]) and the browser build can supply its own (`std::time::Instant` isn't available on
//! `wasm32-unknown-unknown`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// A monotonic clock: the time since some fixed point.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

/// The system's monotonic clock.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct SystemClock {
    start: std::time::Instant,
}

#[cfg(not(target_arch = "wasm32"))]
impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock { start: std::time::Instant::now() }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for SystemClock {
    fn default() -> SystemClock {
        SystemClock::new()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// A clock that moves only when told to, for tests.
#[derive(Debug, Default)]
pub struct ManualClock {
    millis: AtomicU64,
}

impl ManualClock {
    pub fn new() -> ManualClock {
        ManualClock::default()
    }

    pub fn advance(&self, by: Duration) {
        let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        self.millis.fetch_add(millis, Ordering::Relaxed);
    }

    pub fn set(&self, to: Duration) {
        self.millis.store(u64::try_from(to.as_millis()).unwrap_or(u64::MAX), Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.millis.load(Ordering::Relaxed))
    }
}
