//! Managed hardware capture through JSON, real watched files, running previews and final stacks.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
use florastack_engine::{Dispatched, Engine, JobId, JobState, Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

fn params(root: &Path, frames: usize) -> Value {
    std::fs::create_dir_all(root.join("input")).unwrap();
    json!({"watch":{"input":root.join("input"),"output":root.join("output"),"preset":"builtin:insect-macro","idleSeconds":0.1,"settleMs":50,"pollMs":10},
        "frames":frames,"mode":"rail","trigger":"camera","stepMicrometers":50,"settleMs":0})
}
fn start(engine: &mut Engine, params: &Value) -> JobId {
    let Dispatched::Started(id) = engine.dispatch_json("capture.run", &params.to_string()).unwrap() else { panic!("expected job") };
    id
}
fn until(engine: &mut Engine, description: &str, check: impl Fn(&Engine) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !check(engine) {
        engine.poll();
        assert!(Instant::now() < deadline, "waiting for {description}: {:?}", engine.jobs());
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn mock_camera_and_rail_stream_before_shooting_finishes_and_ignore_idle_during_slow_exposures() {
    let dir = common::TempDir::new("capture-stream");
    let files = common::synthetic_frames(dir.path(), "source", 80, 64, &common::breathing(3, 0.005, None, 2));
    let mut engine = Engine::new(common::config(dir.path()));
    let (tx, rx) = channel();
    let (shots, rail, _) = common::capture::install(&engine, files, Some(rx));
    let id = start(&mut engine, &params(dir.path(), 3));
    tx.send(()).unwrap();
    until(&mut engine, "first live preview", |e| e.job(id).unwrap().preview.as_ref().is_some_and(|p| p.frames == 1));
    let preview = engine.job(id).unwrap().preview.unwrap();
    assert!(Path::new(&preview.path).is_file());
    assert_eq!(shots.load(Ordering::Acquire), 1);
    // More than the watch quiet window while the second exposure remains open.
    std::thread::sleep(Duration::from_millis(220));
    engine.poll();
    assert_eq!(engine.job(id).unwrap().state, JobState::Running);
    tx.send(()).unwrap();
    until(&mut engine, "second live preview", |e| e.job(id).unwrap().preview.as_ref().is_some_and(|p| p.frames == 2));
    tx.send(()).unwrap();
    let result = engine.wait(id).unwrap();
    assert_eq!(result["failed"], 0, "{result}");
    assert_eq!(result["stacks"][0]["frames"], 3);
    assert!(Path::new(result["stacks"][0]["output"].as_str().unwrap()).is_file());
    assert_eq!(*rail.moves.lock().unwrap(), vec![50.0, 50.0]);
    let originals: Vec<_> = std::fs::read_dir(dir.path().join("input")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(originals.len(), 3);
    assert!(originals.iter().all(|p| p.extension().unwrap() == "tif"));
    assert_eq!(shots.load(Ordering::Acquire), 3);
}

#[test]
fn cancel_stops_subsequent_shots_and_preserves_completed_originals() {
    let dir = common::TempDir::new("capture-cancel");
    let files = common::synthetic_frames(dir.path(), "source", 64, 48, &common::breathing(3, 0.0, None, 2));
    let mut engine = Engine::new(common::config(dir.path()));
    let (tx, rx) = channel();
    let (shots, rail, _) = common::capture::install(&engine, files, Some(rx));
    let id = start(&mut engine, &params(dir.path(), 3));
    tx.send(()).unwrap();
    until(&mut engine, "first preview", |e| e.job(id).unwrap().preview.is_some());
    engine.run("job.cancel", json!({"job":id})).unwrap();
    assert!(engine.wait(id).is_err());
    assert_eq!(shots.load(Ordering::Acquire), 1);
    assert_eq!(rail.stops.load(Ordering::Acquire), 1);
    assert_eq!(std::fs::read_dir(dir.path().join("input")).unwrap().count(), 1);
    assert!(!dir.path().join("output/summary.csv").exists());
}

#[test]
fn disconnect_and_setup_failure_do_not_shoot_more_or_publish_a_partial_stack() {
    for setup_failure in [false, true] {
        let dir = common::TempDir::new("capture-disconnect");
        let files = common::synthetic_frames(dir.path(), "source", 64, 48, &common::breathing(2, 0.0, None, 2));
        let mut engine = Engine::new(common::config(dir.path()));
        let (shots, rail, _) = common::capture::install(&engine, files, None);
        let mut p = params(dir.path(), 3);
        if setup_failure {
            p["watch"]["output"] = json!(dir.path().join("input/feedback"));
        }
        let id = start(&mut engine, &p);
        assert!(engine.wait(id).is_err());
        assert_eq!(shots.load(Ordering::Acquire), if setup_failure { 0 } else { 2 });
        assert_eq!(rail.stops.load(Ordering::Acquire), 1);
        assert!(!dir.path().join("output/summary.csv").exists());
    }
}

#[test]
fn lens_marks_drive_inclusive_bracket_and_capability_errors_are_actionable() {
    let dir = common::TempDir::new("capture-lens");
    let files = common::synthetic_frames(dir.path(), "source", 64, 48, &common::breathing(3, 0.0, None, 2));
    let mut engine = Engine::new(common::config(dir.path()));
    let (_, _, focus) = common::capture::install(&engine, files, None);
    for p in [json!({"action":"markNear"}), json!({"action":"focus","pulses":4}), json!({"action":"markFar"})] {
        engine.run("capture.camera", p).unwrap();
    }
    let mut p = params(dir.path(), 3);
    p["mode"] = json!("lens");
    p["stepMicrometers"] = json!(0);
    let id = start(&mut engine, &p);
    engine.wait(id).unwrap();
    assert_eq!(*focus.lock().unwrap(), vec![4, -4, 2, 2]);
    engine.run("capture.camera", json!({"action":"invalidateMarks"})).unwrap();
    let info = engine.run("capture.info", json!({})).unwrap();
    assert!(info["focus"]["near"].is_null() && info["focus"]["far"].is_null());
    engine.run("capture.camera", json!({"action":"disconnect"})).unwrap();
    assert!(engine.run("capture.camera", json!({"action":"focus","pulses":1})).unwrap_err().to_string().contains("connect a camera"));
}

#[test]
fn external_and_controller_wait_for_ingested_frames_before_moving() {
    for trigger in ["external", "controller"] {
        let dir = common::TempDir::new("capture-external");
        let files = common::synthetic_frames(dir.path(), "source", 64, 48, &common::breathing(2, 0.0, None, 2));
        let mut engine = Engine::new(common::config(dir.path()));
        let (_, rail, _) = common::capture::install(&engine, files.clone(), None);
        let mut p = params(dir.path(), 2);
        p["trigger"] = json!(trigger);
        let id = start(&mut engine, &p);
        // External capture begins only after the device worker has passed the watch initialization barrier.
        until(&mut engine, "rail ready for the first photo", |e| e.capture_devices().try_lock().is_err());
        assert!(rail.moves.lock().unwrap().is_empty());
        // The external application's final rename, after writing its temporary file.
        std::fs::copy(&files[0], dir.path().join("input/first.part")).unwrap();
        std::fs::rename(dir.path().join("input/first.part"), dir.path().join("input/first.tif")).unwrap();
        until(&mut engine, "move after ingest", |_| !rail.moves.lock().unwrap().is_empty());
        // The hardware acknowledgement may be observed before the UI thread drains the earlier preview event.
        engine.poll();
        assert!(engine.job(id).unwrap().preview.is_some());
        std::fs::copy(&files[1], dir.path().join("input/second.tif")).unwrap();
        engine.wait(id).unwrap();
        assert_eq!(*rail.moves.lock().unwrap(), vec![50.0]);
        assert_eq!(rail.shutters.load(Ordering::Acquire), if trigger == "controller" { 2 } else { 0 });
    }
}

#[test]
fn external_timeout_stops_rail_and_does_not_advance() {
    let dir = common::TempDir::new("capture-timeout");
    let mut engine = Engine::new(common::config(dir.path()));
    let (_, rail, _) = common::capture::install(&engine, Vec::new(), None);
    let mut p = params(dir.path(), 2);
    p["trigger"] = json!("external");
    p["frameTimeoutMs"] = json!(100);
    let id = start(&mut engine, &p);
    assert!(engine.wait(id).unwrap_err().to_string().contains("timed out"));
    assert!(rail.moves.lock().unwrap().is_empty());
    assert_eq!(rail.stops.load(Ordering::Acquire), 1);
}

#[test]
fn failed_focus_invalidates_both_marks() {
    let dir = common::TempDir::new("capture-focus-failure");
    let mut engine = Engine::new(common::config(dir.path()));
    let handle = engine.capture_devices();
    {
        let mut devices = handle.lock().unwrap();
        devices.camera = Some(Box::new(common::capture::MockCamera {
            files: Vec::new(),
            shots: Arc::default(),
            gate: None,
            focus_moves: Arc::default(),
            fail_focus: true,
        }));
        devices.focus.near = Some(0);
        devices.focus.far = Some(10);
    }
    assert!(engine.run("capture.camera", json!({"action":"focus","pulses":1})).is_err());
    let info = engine.run("capture.info", json!({})).unwrap();
    assert!(info["focus"]["near"].is_null() && info["focus"]["far"].is_null());
}

#[test]
fn unsupported_camera_capabilities_are_refused_before_movement() {
    use florastack_engine::Monitor;
    use florastack_engine::capture::{Camera, Capabilities, Capture, Error};
    struct Unsupported;
    impl Camera for Unsupported {
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn capture(&mut self, _: &dyn Monitor) -> Result<Capture, Error> {
            Err(Error::Unavailable("capture".into()))
        }
        fn live_view(&mut self, _: &dyn Monitor) -> Result<Capture, Error> {
            Err(Error::Unavailable("live view".into()))
        }
        fn focus(&mut self, _: i32, _: &dyn Monitor) -> Result<(), Error> {
            Err(Error::Unavailable("focus".into()))
        }
    }
    let dir = common::TempDir::new("capture-capabilities");
    let mut engine = Engine::new(common::config(dir.path()));
    let (_, rail, _) = common::capture::install(&engine, Vec::new(), None);
    engine.capture_devices().lock().unwrap().camera = Some(Box::new(Unsupported));
    for action in ["markNear", "markFar", "liveView"] {
        assert!(engine.run("capture.camera", json!({"action":action})).is_err());
    }
    let id = start(&mut engine, &params(dir.path(), 2));
    assert!(engine.wait(id).unwrap_err().to_string().contains("camera capture unavailable"));
    assert!(rail.moves.lock().unwrap().is_empty());
}
