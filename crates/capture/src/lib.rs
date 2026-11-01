//! Camera and rail interfaces. Device operations are blocking I/O: hosts run them on device workers.
//! Frames are returned as original file bytes, for atomic publication into a watched folder by the host.

#[cfg(all(feature = "gphoto", target_os = "windows"))]
compile_error!("gphoto is supported only on Linux/macOS; Windows builds use watch-folder capture and serial rails");

#[cfg(all(feature = "gphoto", any(target_os = "linux", target_os = "macos")))]
pub mod gphoto;
pub mod optics;
#[cfg(not(target_arch = "wasm32"))]
pub mod serial;

use florastack_core::{Cancelled, Monitor};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("capture cancelled")]
    Cancelled(#[from] Cancelled),
    #[error("invalid capture setting: {0}")]
    Invalid(String),
    #[error("device capability unavailable: {0}")]
    Unavailable(String),
    #[error("device I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[cfg(not(target_arch = "wasm32"))]
    #[error("serial operation failed ({operation}): {source}")]
    Serial {
        operation: String,
        #[source]
        source: serialport::Error,
    },
    #[cfg(all(feature = "gphoto", any(target_os = "linux", target_os = "macos")))]
    #[error("camera library failed ({operation}): {source}")]
    Library {
        operation: String,
        #[source]
        source: libloading::Error,
    },
    #[error("serial device failed: {0}")]
    Protocol(String),
    #[error("camera library failed: {0}")]
    Camera(String),
}

/// A downloaded camera file, retaining its native format.
pub struct Capture {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub capture: bool,
    pub live_view: bool,
    /// Relative, uncalibrated unit pulses; no claim of an absolute optical position.
    pub focus: bool,
}

/// Camera instances belong to one worker at a time; drivers must report unsupported operations.
pub trait Camera: Send {
    fn capabilities(&self) -> Capabilities;
    fn capture(&mut self, monitor: &dyn Monitor) -> Result<Capture, Error>;
    fn live_view(&mut self, monitor: &dyn Monitor) -> Result<Capture, Error>;
    /// Execute signed unit pulses. Failed movement makes the tracked position uncertain.
    fn focus(&mut self, pulses: i32, monitor: &dyn Monitor) -> Result<(), Error>;
}

/// A rail reports completion only after the controller has finished moving, then the host allows settling.
pub trait Rail: Send {
    fn move_by(&mut self, micrometers: f64, monitor: &dyn Monitor) -> Result<(), Error>;
    fn shutter(&mut self, monitor: &dyn Monitor) -> Result<(), Error>;
    /// Best-effort immediate stop; a stopped rail must be reconnected before another move.
    fn stop(&mut self) -> Result<(), Error>;
}

/// Session-relative focus marks. Manual focus, autofocus, reconnects and any movement error invalidate both marks.
#[derive(Default, Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Focus {
    pub position: i32,
    pub near: Option<i32>,
    pub far: Option<i32>,
}

impl Focus {
    pub fn invalidate(&mut self) {
        *self = Self::default();
    }

    pub fn moved(&mut self, pulses: i32) -> Result<(), Error> {
        let Some(position) = self.position.checked_add(pulses) else {
            self.invalidate();
            return Err(Error::Invalid("focus position overflow; reset near/far marks".into()));
        };
        self.position = position;
        Ok(())
    }

    /// N intervals, including both endpoints: N+1 photos. Rounding never adds an extra pulse beyond either mark.
    pub fn bracket(&self, intervals: u32) -> Result<Vec<i32>, Error> {
        if !(1..=10000).contains(&intervals) {
            return Err(Error::Invalid("lens intervals must be 1..10000".into()));
        }
        let (Some(near), Some(far)) = (self.near, self.far) else {
            return Err(Error::Unavailable("set near and far in live view first".into()));
        };
        let distance = i64::from(far) - i64::from(near);
        if distance == 0 || distance.unsigned_abs() < u64::from(intervals) {
            return Err(Error::Invalid("marks must span at least one motor pulse per interval".into()));
        }
        (0..=intervals)
            .map(|i| {
                i32::try_from(i64::from(near) + distance * i64::from(i) / i64::from(intervals)).map_err(|_| Error::Invalid("focus overflow".into()))
            })
            .collect()
    }
}

/// Host injection point for real or simulated drivers, independent of project state.
#[derive(Default)]
pub struct Devices {
    pub camera: Option<Box<dyn Camera>>,
    pub rail: Option<Box<dyn Rail>>,
    pub focus: Focus,
}

/// Cancellable wait for settling or controller polling; never hides cancellation for a full exposure interval.
pub fn wait(duration: std::time::Duration, monitor: &dyn Monitor) -> Result<(), Error> {
    let start = std::time::Instant::now();
    while start.elapsed() < duration {
        monitor.checkpoint()?;
        std::thread::sleep(duration.saturating_sub(start.elapsed()).min(std::time::Duration::from_millis(20)));
    }
    monitor.checkpoint()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn focus_marks_include_endpoints_and_reject_unresolvable_or_overflowed_ranges() {
        let mut focus = Focus { position: 7, near: Some(0), far: Some(7) };
        assert_eq!(focus.bracket(3).unwrap(), vec![0, 2, 4, 7]);
        focus.near = Some(7);
        focus.far = Some(0);
        assert_eq!(focus.bracket(3).unwrap(), vec![7, 5, 3, 0]);
        assert!(focus.bracket(8).is_err());
        assert!(focus.bracket(0).is_err());
        focus.position = i32::MAX;
        assert!(focus.moved(1).is_err());
        assert!(focus.near.is_none() && focus.far.is_none());
        assert!(focus.bracket(1).is_err());
    }
}
