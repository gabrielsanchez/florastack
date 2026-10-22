//! A job's progress as its clients see it ([`Progress`]), worked out from what the job reports ([`Tracker`]).
//!
//! A job declares its stages up front, each with a weight (its share of the whole), and reports, stage by stage, how
//! many of how many units it has done. The fraction done is the weights of the finished stages plus the done part of
//! the current one, and it never goes back. The ETA is the time left at the average rate since the first report: it
//! is given once the job is at least 1 % done and has reported for at least a second, so that its setup isn't taken
//! for its pace, and is `None` before.

use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;

/// A job's progress.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// From 0 to 1.
    pub fraction: f32,
    /// The current stage's name, such as `Reading frames`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// The current stage, from 0.
    pub stage_index: usize,
    pub stages: usize,
    /// Units of the current stage done, of `total` (0: not known).
    pub done: u64,
    pub total: u64,
    /// Since the job started (or was queued, while it waits).
    pub elapsed_ms: u64,
    /// The time left, once it can be told.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_ms: Option<u64>,
}

/// The least fraction done, and the least time since the first report, before an ETA is given.
const ETA_MIN_FRACTION: f32 = 0.01;
const ETA_MIN_TIME: Duration = Duration::from_secs(1);

/// Turns a job's reports into its [`Progress`]. See the module documentation.
#[derive(Clone, Debug)]
pub(crate) struct Tracker {
    stages: Vec<(&'static str, f32)>,
    stage: usize,
    done: u64,
    total: u64,
    fraction: f32,
    start: Duration,
    first: Option<(Duration, f32)>,
    last: Option<(Duration, f32)>,
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Seconds as whole milliseconds, rounded; `None` unless finite, at least 0 and less than a thousand years.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // In range: checked first.
fn round_millis(secs: f64) -> Option<u64> {
    (secs.is_finite() && (0.0..3.2e10).contains(&secs)).then(|| (secs * 1000.0).round() as u64)
}

impl Tracker {
    /// A tracker for a job with these stages (weights that aren't positive and finite count as 1), started at `start`.
    pub(crate) fn new(stages: &[(&'static str, f32)], start: Duration) -> Tracker {
        let stages = stages.iter().map(|&(name, w)| (name, if w.is_finite() && w > 0.0 { w } else { 1.0 })).collect();
        Tracker { stages, stage: 0, done: 0, total: 0, fraction: 0.0, start, first: None, last: None }
    }

    /// Restarts the clock, when a queued job starts.
    pub(crate) fn restart(&mut self, start: Duration) {
        self.start = start;
    }

    /// A report at `at`: `done` of `total` units of stage `stage`.
    pub(crate) fn update(&mut self, stage: usize, done: u64, total: u64, at: Duration) {
        let count = self.stages.len().max(1);
        self.stage = stage.min(count - 1);
        self.done = if total > 0 { done.min(total) } else { done };
        self.total = total;
        let weight = |i: usize| self.stages.get(i).map_or(1.0, |&(_, w)| w);
        let sum: f32 = (0..count).map(weight).sum();
        let before: f32 = (0..self.stage).map(weight).sum();
        let part = if total > 0 { self.done as f32 / total as f32 } else { 0.0 };
        let fraction = ((before + weight(self.stage) * part) / sum).clamp(0.0, 1.0);
        self.fraction = self.fraction.max(fraction);
        if self.first.is_none() {
            self.first = Some((at, self.fraction));
        }
        self.last = Some((at, self.fraction));
    }

    /// Marks the job as complete.
    pub(crate) fn complete(&mut self, at: Duration) {
        self.fraction = 1.0;
        self.last = Some((at, 1.0));
    }

    /// The progress at `now`.
    pub(crate) fn progress(&self, now: Duration) -> Progress {
        let eta = match (self.first, self.last) {
            (Some((t0, f0)), Some((t1, f1))) if (ETA_MIN_FRACTION..1.0).contains(&f1) && f1 > f0 && t1.saturating_sub(t0) >= ETA_MIN_TIME => {
                let rate = f64::from(f1 - f0) / t1.saturating_sub(t0).as_secs_f64();
                // The time since the last report has passed already.
                round_millis(f64::from(1.0 - f1) / rate).map(|left| left.saturating_sub(millis(now.saturating_sub(t1))))
            }
            _ => None,
        };
        Progress {
            fraction: self.fraction,
            stage: self.stages.get(self.stage).map(|&(name, _)| name.to_owned()),
            stage_index: self.stage,
            stages: self.stages.len(),
            done: self.done,
            total: self.total,
            elapsed_ms: millis(now.saturating_sub(self.start)),
            eta_ms: eta,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: f64) -> Duration {
        Duration::from_secs_f64(secs)
    }

    #[test]
    fn stages_weigh_the_fraction_and_it_never_goes_back() {
        let mut t = Tracker::new(&[("Reading", 1.0), ("Aligning", 3.0)], Duration::ZERO);
        t.update(0, 5, 10, s(1.0));
        assert!((t.progress(s(1.0)).fraction - 0.125).abs() < 1e-6);
        t.update(1, 0, 10, s(2.0));
        assert!((t.progress(s(2.0)).fraction - 0.25).abs() < 1e-6);
        assert_eq!(t.progress(s(2.0)).stage.as_deref(), Some("Aligning"));
        t.update(1, 5, 10, s(3.0));
        assert!((t.progress(s(3.0)).fraction - 0.625).abs() < 1e-6);
        t.update(0, 0, 10, s(4.0));
        assert!((t.progress(s(4.0)).fraction - 0.625).abs() < 1e-6, "a report from an earlier stage doesn't move it back");
        t.update(7, 20, 10, s(4.0));
        assert_eq!(t.progress(s(4.0)).stage_index, 1, "the stage index is clamped");
        assert_eq!(t.progress(s(4.0)).done, 10, "so is the count");
        let mut nan = Tracker::new(&[("A", f32::NAN), ("B", -1.0)], Duration::ZERO);
        nan.update(0, 1, 2, s(1.0));
        assert!((nan.progress(s(1.0)).fraction - 0.25).abs() < 1e-6, "bad weights count as 1");
    }

    #[test]
    fn the_eta_follows_a_steady_rate() {
        let mut t = Tracker::new(&[("Work", 1.0)], s(0.0));
        // Setup takes 5 s, then 1 % a second.
        t.update(0, 0, 100, s(5.0));
        assert_eq!(t.progress(s(5.0)).eta_ms, None, "no rate yet");
        t.update(0, 1, 100, s(5.5));
        assert_eq!(t.progress(s(5.5)).eta_ms, None, "less than a second of reports");
        t.update(0, 10, 100, s(15.0));
        let p = t.progress(s(15.0));
        assert_eq!(p.eta_ms, Some(90_000), "90 % left at 1 % a second, setup not counted");
        assert_eq!(p.elapsed_ms, 15_000);
        assert_eq!(t.progress(s(25.0)).eta_ms, Some(80_000), "time passes since the last report");
        t.complete(s(30.0));
        assert_eq!(t.progress(s(30.0)).eta_ms, None);
        assert!((t.progress(s(30.0)).fraction - 1.0).abs() < f32::EPSILON);
    }
}
