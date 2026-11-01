//! Host-injected capture hardware used by JSON dispatch and end-to-end tests.
#![allow(dead_code)]
use florastack_engine::capture::{Camera, Capabilities, Capture, Error, Rail};
use florastack_engine::{Engine, Monitor};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct MockCamera {
    pub files: Vec<PathBuf>,
    pub shots: Arc<AtomicUsize>,
    pub gate: Option<Receiver<()>>,
    pub focus_moves: Arc<Mutex<Vec<i32>>>,
    pub fail_focus: bool,
}
impl Camera for MockCamera {
    fn capabilities(&self) -> Capabilities {
        Capabilities { capture: true, live_view: true, focus: true }
    }
    fn capture(&mut self, monitor: &dyn Monitor) -> Result<Capture, Error> {
        if let Some(gate) = &self.gate {
            loop {
                monitor.checkpoint()?;
                match gate.recv_timeout(Duration::from_millis(10)) {
                    Ok(()) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return Err(Error::Camera("mock camera disconnected".into())),
                }
            }
        }
        monitor.checkpoint()?;
        let index = self.shots.load(Ordering::Acquire);
        let path = self.files.get(index).ok_or_else(|| Error::Camera("mock camera disconnected".into()))?;
        let bytes = std::fs::read(path)?;
        self.shots.fetch_add(1, Ordering::Release);
        Ok(Capture { name: "camera.tif".into(), bytes })
    }
    fn live_view(&mut self, _: &dyn Monitor) -> Result<Capture, Error> {
        let path = self.files.first().ok_or_else(|| Error::Camera("no mock preview".into()))?;
        Ok(Capture { name: "preview.tif".into(), bytes: std::fs::read(path)? })
    }
    fn focus(&mut self, pulses: i32, monitor: &dyn Monitor) -> Result<(), Error> {
        monitor.checkpoint()?;
        if self.fail_focus {
            return Err(Error::Camera("mock focus motor disconnected".into()));
        }
        self.focus_moves.lock().map_err(|_| Error::Camera("mock lock".into()))?.push(pulses);
        Ok(())
    }
}
#[derive(Clone, Default)]
pub struct MockRail {
    pub moves: Arc<Mutex<Vec<f64>>>,
    pub stops: Arc<AtomicUsize>,
    pub shutters: Arc<AtomicUsize>,
    pub fail_move: bool,
}
impl Rail for MockRail {
    fn move_by(&mut self, value: f64, monitor: &dyn Monitor) -> Result<(), Error> {
        monitor.checkpoint()?;
        if self.fail_move {
            return Err(Error::Protocol("mock rail disconnected".into()));
        }
        self.moves.lock().map_err(|_| Error::Protocol("mock lock".into()))?.push(value);
        Ok(())
    }
    fn shutter(&mut self, monitor: &dyn Monitor) -> Result<(), Error> {
        monitor.checkpoint()?;
        self.shutters.fetch_add(1, Ordering::Release);
        Ok(())
    }
    fn stop(&mut self) -> Result<(), Error> {
        self.stops.fetch_add(1, Ordering::Release);
        Ok(())
    }
}
pub fn install(engine: &Engine, files: Vec<PathBuf>, gate: Option<Receiver<()>>) -> (Arc<AtomicUsize>, MockRail, Arc<Mutex<Vec<i32>>>) {
    let shots = Arc::new(AtomicUsize::new(0));
    let rail = MockRail::default();
    let focus_moves = Arc::new(Mutex::new(Vec::new()));
    let handle = engine.capture_devices();
    let mut devices = handle.lock().unwrap();
    devices.camera = Some(Box::new(MockCamera { files, shots: shots.clone(), gate, focus_moves: focus_moves.clone(), fail_focus: false }));
    devices.rail = Some(Box::new(rail.clone()));
    (shots, rail, focus_moves)
}
