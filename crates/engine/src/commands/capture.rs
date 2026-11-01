//! Device commands and managed shooting. The device lane serializes hardware access; the reusable watcher
//! coordinates processing on the job worker while a scoped I/O worker shoots. A barrier prevents shooting before
//! ingest setup succeeds. Explicit completion prevents quiet timeouts during exposures. On any failure the I/O
//! worker is stopped and joined; completed originals remain in input, independently of staged stack outputs.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use florastack_capture::{Devices, Error, Focus, optics, serial};
use florastack_core::Monitor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::batch::{self, BatchResult, unavailable};
use super::watch::{self, WatchParams};
use crate::jobs::{JobContext, JobPlan, Lane, commit};
use crate::registry::{CommandSpec, Effects, Meta, NoParams, Params};
use crate::{CommandError, Invalid, Session};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(Meta::new("capture.info", "Capture devices", "Reports connected devices, capabilities and relative lens marks.")
            .help("Does no device I/O; reports busy while a worker owns the devices. Camera control requires a gphoto build on Linux or macOS. Manual focus or autofocus invalidates relative marks: use invalidateMarks before marking again."), info),
        CommandSpec::query(Meta::new("capture.calculate", "Calculate rail step", "Recommends diffraction-based physical rail spacing and explains sensor sampling.")
            .help("Uses 550 nm light, air, pupil ratio 1, N_eff=N(1+m), NA=m/(2 N_eff), wave depth=λ/NA² and 50% overlap. Pixel pitch assesses Airy-disk sampling, not a circle of confusion. Approximate paraxial model; see docs/capture.md. Motor pulses cannot be converted to this spacing.")
            .examples(&[r#"{"magnification":1,"aperture":4,"pixelPitch":4}"#]), calculate),
        CommandSpec::job(Meta::new("capture.camera", "Control camera", "Discovers, connects or operates a supported camera on a device worker.")
            .help("Actions: discover, connect (exactly one camera), disconnect, liveView (returns a temporary JPEG path), focus (signed unit pulses), markNear, markFar, invalidateMarks. Marks exist only within this connection and count relative commands, not absolute optical positions. Disable continuous autofocus and do not touch the focus ring; invalidate marks whenever uncertain. A focus error invalidates both marks. gphoto on Linux/macOS only; other builds use the watch-folder workflow.")
            .effects(Effects::Files).examples(&[r#"{"action":"discover"}"#,r#"{"action":"focus","pulses":1}"#]), Lane::Device, camera),
        CommandSpec::job(Meta::new("capture.rail", "Control rail", "Lists serial ports, connects a rail, moves it or stops it.")
            .help("Actions: ports, connect (settings required), disconnect, move (micrometers), stop. Establish a safe physical zero before connecting and set the travel limit accordingly. Marlin uses relative X millimeters and M400; Arduino uses calibrated integer steps. Stop is best-effort, invalidates position and requires reconnect; use job.cancel for an active device job, which stops at its next I/O checkpoint. Zaber ASCII is explicitly unavailable. No proprietary protocols are inferred.")
            .effects(Effects::Files).examples(&[r#"{"action":"ports"}"#,r#"{"action":"move","micrometers":50}"#]), Lane::Device, rail),
        CommandSpec::job(Meta::new("capture.run", "Capture and stack", "Shoots a lens or rail sequence into the watched folder while stacking previews.")
            .help("Requires an empty input folder, output outside it and a preset. mode=rail takes frames photos separated by stepMicrometers after settling; mode=lens takes frames photos including both relative near/far marks (frames-1 intervals). Trigger=camera downloads each original atomically with a unique filename; controller sends DIY SHUTTER then waits for external ingest; external waits for a new readable frame before advancing. External/controller workflows require one photo per position and frameTimeoutMs greater than exposure plus transfer/processing time. Managed completion overrides watch.idleSeconds while shooting. On cancellation or failure, stops further movement/shots, joins hardware work and keeps completed originals. A blocking libgphoto2 call finishes before cancellation can stop the next operation; final stack publication is atomic as for watch.run.")
            .effects(Effects::Files).examples(&[r#"{"watch":{"input":"C:/Capture","output":"C:/Results","preset":"builtin:insect-macro"},"frames":10,"mode":"rail","trigger":"external","stepMicrometers":50}"#]), Lane::Device, run),
    ]
}

fn error(error: Error) -> CommandError {
    match error {
        Error::Cancelled(error) => error.into(),
        error => unavailable("capture", error.to_string()),
    }
}
fn lock(devices: &Mutex<Devices>) -> Result<std::sync::MutexGuard<'_, Devices>, CommandError> {
    devices.lock().map_err(|_| unavailable("capture", "device worker failed; restart the application before reconnecting"))
}

impl Params for optics::Optics {
    fn check(&self) -> Result<(), Invalid> {
        self.validate().map_err(|e| Invalid::new("optics", e.to_string()))
    }
}
fn calculate(_: &Session, p: optics::Optics) -> Result<optics::Recommendation, CommandError> {
    p.recommend().map_err(error)
}

#[derive(Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct DeviceInfo {
    busy: bool,
    camera_control: bool,
    camera: Option<florastack_capture::Capabilities>,
    rail: bool,
    focus: Focus,
}
fn info(session: &Session, _: NoParams) -> Result<DeviceInfo, CommandError> {
    let camera_control = cfg!(all(feature = "gphoto", any(target_os = "linux", target_os = "macos")));
    match session.capture.try_lock() {
        Ok(devices) => Ok(DeviceInfo {
            camera_control,
            camera: devices.camera.as_ref().map(|c| c.capabilities()),
            rail: devices.rail.is_some(),
            focus: devices.focus.clone(),
            busy: false,
        }),
        Err(std::sync::TryLockError::WouldBlock) => Ok(DeviceInfo { camera_control, busy: true, ..DeviceInfo::default() }),
        Err(_) => Err(unavailable("capture.info", "device worker failed; restart before reconnecting")),
    }
}

#[derive(Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum CameraAction {
    Discover,
    Connect,
    Disconnect,
    LiveView,
    Focus,
    MarkNear,
    MarkFar,
    InvalidateMarks,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CameraParams {
    /// discover, connect, disconnect, liveView, focus, markNear, markFar or invalidateMarks.
    action: CameraAction,
    /// Signed unit motor pulses for focus only; positive moves toward far.
    #[serde(default)]
    #[schemars(range(min = -10000, max = 10000))]
    pulses: i32,
}
impl Params for CameraParams {
    fn check(&self) -> Result<(), Invalid> {
        if self.pulses.unsigned_abs() > 10000 || (self.action == CameraAction::Focus) == (self.pulses == 0) {
            return Err(Invalid::new("pulses", "focus needs 1..10000 signed pulses; other actions require zero"));
        }
        Ok(())
    }
}
#[derive(Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct DeviceResult {
    devices: Vec<String>,
    path: Option<String>,
    focus: Option<Focus>,
}

fn camera(session: &Session, p: CameraParams) -> Result<JobPlan<DeviceResult>, CommandError> {
    let devices = session.capture.clone();
    let preview = session.capture_preview.clone();
    Ok(JobPlan::new(move |ctx| {
        ctx.checkpoint()?;
        let mut devices = lock(&devices)?;
        let mut result = DeviceResult::default();
        match p.action {
            CameraAction::Discover => result.devices = discover_camera()?,
            CameraAction::Connect => {
                if devices.camera.is_some() {
                    return Err(unavailable("capture.camera", "disconnect the current camera first"));
                }
                devices.camera = Some(connect_camera()?);
                devices.focus.invalidate();
            }
            CameraAction::Disconnect => {
                devices.camera = None;
                devices.focus.invalidate();
            }
            CameraAction::InvalidateMarks => devices.focus.invalidate(),
            CameraAction::LiveView => {
                let image =
                    devices.camera.as_mut().ok_or_else(|| unavailable("capture.camera", "connect a camera first"))?.live_view(ctx).map_err(error)?;
                let mut file = tempfile::Builder::new()
                    .prefix("florastack-live-")
                    .suffix(".jpg")
                    .tempfile()
                    .map_err(|e| unavailable("capture.camera", e.to_string()))?;
                file.write_all(&image.bytes).map_err(|e| unavailable("capture.camera", e.to_string()))?;
                let path = file.into_temp_path();
                result.path = Some(super::display(&path));
                *preview.lock().map_err(|_| unavailable("capture.camera", "preview lock failed"))? = Some(path);
            }
            CameraAction::Focus => move_focus(&mut devices, p.pulses, ctx)?,
            CameraAction::MarkNear | CameraAction::MarkFar => {
                if !devices.camera.as_ref().is_some_and(|c| c.capabilities().focus && c.capabilities().live_view) {
                    return Err(unavailable("capture.camera", "lens marks require camera live view and relative focus support"));
                }
                if p.action == CameraAction::MarkNear {
                    devices.focus.near = Some(devices.focus.position);
                } else {
                    devices.focus.far = Some(devices.focus.position);
                }
            }
        }
        result.focus = Some(devices.focus.clone());
        Ok(commit(move |_| Ok(result)))
    }))
}

#[cfg(all(feature = "gphoto", any(target_os = "linux", target_os = "macos")))]
fn discover_camera() -> Result<Vec<String>, CommandError> {
    florastack_capture::gphoto::discover().map_err(error)
}
#[cfg(all(feature = "gphoto", any(target_os = "linux", target_os = "macos")))]
fn connect_camera() -> Result<Box<dyn florastack_capture::Camera>, CommandError> {
    Ok(Box::new(florastack_capture::gphoto::Connection::connect().map_err(error)?))
}
#[cfg(not(all(feature = "gphoto", any(target_os = "linux", target_os = "macos"))))]
fn discover_camera() -> Result<Vec<String>, CommandError> {
    Err(unavailable(
        "capture.camera",
        "built-in camera control requires gphoto on Linux/macOS; use a tether application saving into the watched folder",
    ))
}
#[cfg(not(all(feature = "gphoto", any(target_os = "linux", target_os = "macos"))))]
fn connect_camera() -> Result<Box<dyn florastack_capture::Camera>, CommandError> {
    discover_camera()?;
    Err(unavailable("capture.camera", "camera control unavailable"))
}

fn move_focus(devices: &mut Devices, pulses: i32, monitor: &dyn Monitor) -> Result<(), CommandError> {
    let camera = devices.camera.as_mut().ok_or_else(|| unavailable("capture.camera", "connect a camera first"))?;
    if !camera.capabilities().focus {
        return Err(unavailable("capture.camera", "relative lens focus unsupported"));
    }
    if let Err(e) = camera.focus(pulses, monitor) {
        devices.focus.invalidate();
        return Err(error(e));
    }
    devices.focus.moved(pulses).map_err(error)
}

#[derive(Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum RailAction {
    Ports,
    Connect,
    Disconnect,
    Move,
    Stop,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RailParams {
    /// ports, connect, disconnect, move or stop.
    action: RailAction,
    /// Required only for connect; origin must already be physically established.
    settings: Option<serial::Settings>,
    /// Signed displacement for move only, in micrometers from the current position.
    #[serde(default)]
    #[schemars(range(min = -1_000_000, max = 1_000_000))]
    micrometers: f64,
}
impl Params for RailParams {
    fn check(&self) -> Result<(), Invalid> {
        if (self.action == RailAction::Connect) != self.settings.is_some() {
            return Err(Invalid::new("settings", "required for connect only"));
        }
        if let Some(settings) = &self.settings {
            settings.validate().map_err(|e| Invalid::new("settings", e.to_string()))?;
        }
        if !self.micrometers.is_finite() || self.micrometers.abs() > 1_000_000.0 || (self.action == RailAction::Move) == (self.micrometers == 0.0) {
            return Err(Invalid::new("micrometers", "move requires a finite nonzero displacement up to 1000000 µm; other actions require zero"));
        }
        Ok(())
    }
}
fn rail(session: &Session, p: RailParams) -> Result<JobPlan<DeviceResult>, CommandError> {
    let devices = session.capture.clone();
    Ok(JobPlan::new(move |ctx| {
        ctx.checkpoint()?;
        let mut devices = lock(&devices)?;
        let mut result = DeviceResult::default();
        match p.action {
            RailAction::Ports => result.devices = serial::ports().map_err(error)?,
            RailAction::Connect => {
                if devices.rail.is_some() {
                    return Err(unavailable("capture.rail", "disconnect current rail first"));
                }
                devices.rail = Some(serial::connect(p.settings.ok_or_else(|| unavailable("capture.rail", "settings missing"))?, ctx).map_err(error)?);
            }
            RailAction::Disconnect => {
                if let Some(mut rail) = devices.rail.take() {
                    rail.stop().map_err(error)?;
                }
            }
            RailAction::Move => devices
                .rail
                .as_mut()
                .ok_or_else(|| unavailable("capture.rail", "connect a rail first"))?
                .move_by(p.micrometers, ctx)
                .map_err(error)?,
            RailAction::Stop => devices.rail.as_mut().ok_or_else(|| unavailable("capture.rail", "connect a rail first"))?.stop().map_err(error)?,
        }
        Ok(commit(move |_| Ok(result)))
    }))
}

#[derive(Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum Mode {
    Rail,
    Lens,
}
#[derive(Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum Trigger {
    Camera,
    Controller,
    External,
}
fn settle() -> u64 {
    300
}
fn timeout() -> u64 {
    120_000
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Sequence {
    /// Watch-folder ingest, preview, preset and final output settings. idleSeconds applies only after shooting ends.
    watch: WatchParams,
    /// Number of photos; lens mode includes both marked endpoints (frames-1 intervals).
    #[schemars(range(min = 2, max = 10001))]
    frames: u32,
    /// rail for physical spacing, lens for relative near/far motor marks.
    mode: Mode,
    /// camera downloads, controller triggers the DIY shutter, external waits for a tethered image.
    trigger: Trigger,
    /// Signed physical spacing in micrometers for rail mode; zero for lens mode.
    #[serde(default)]
    #[schemars(range(min = -1_000_000, max = 1_000_000))]
    step_micrometers: f64,
    /// Vibration settling after completed movement, milliseconds.
    #[serde(default = "settle")]
    #[schemars(range(min = 0, max = 60000))]
    settle_ms: u64,
    /// External/controller timeout per new ingested frame, including exposure, transfer and preview processing.
    #[serde(default = "timeout")]
    #[schemars(range(min = 100, max = 3_600_000))]
    frame_timeout_ms: u64,
}
impl Params for Sequence {
    fn check(&self) -> Result<(), Invalid> {
        self.watch.check()?;
        if !(2..=10001).contains(&self.frames) {
            return Err(Invalid::new("frames", "must be 2..10001"));
        }
        if self.settle_ms > 60000 || !(100..=3_600_000).contains(&self.frame_timeout_ms) {
            return Err(Invalid::new("timing", "settling or frame timeout out of range"));
        }
        if !self.step_micrometers.is_finite()
            || self.step_micrometers.abs() > 1_000_000.0
            || (self.mode == Mode::Rail) == (self.step_micrometers == 0.0)
        {
            return Err(Invalid::new("stepMicrometers", "rail requires finite nonzero spacing up to 1000000 µm; lens requires zero"));
        }
        if self.mode == Mode::Lens && self.trigger != Trigger::Camera {
            return Err(Invalid::new("trigger", "lens bracketing requires camera control"));
        }
        Ok(())
    }
}

struct Shooting<'a> {
    ctx: &'a JobContext,
    abort: &'a AtomicBool,
}
impl Monitor for Shooting<'_> {
    fn is_cancelled(&self) -> bool {
        self.ctx.is_cancelled() || self.abort.load(Ordering::Acquire)
    }
    fn progress(&self, _done: u64, _total: u64) {}
}

fn run(session: &Session, p: Sequence) -> Result<JobPlan<BatchResult>, CommandError> {
    let (compute, tiling) = (session.compute(), session.tiling());
    let devices = session.capture.clone();
    Ok(JobPlan::new(move |ctx| {
        let input = super::absolute(&p.watch.input)?;
        if batch::children(&input)?.iter().any(|path| crate::frames::is_frame_name(path)) {
            return Err(unavailable("capture.run", "managed capture requires a fresh input folder without existing images"));
        }
        let abort = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let (ready_tx, ready_rx) = channel();
            let (done_tx, done_rx) = channel();
            let (accepted_tx, accepted_rx) = channel();
            let (drivers, sequence, folder, abort_ref) = (&devices, &p, &input, &abort);
            let worker = std::thread::Builder::new()
                .name("florastack-shoot".into())
                .spawn_scoped(scope, move || {
                    let monitor = Shooting { ctx, abort: abort_ref };
                    let result = shoot(drivers, sequence, folder, &ready_rx, &accepted_rx, &monitor);
                    if result.is_err()
                        && let Ok(mut devices) = drivers.lock()
                    {
                        devices.focus.invalidate();
                        if let Some(rail) = devices.rail.as_mut() {
                            let _ = rail.stop();
                        }
                    }
                    let _ = done_tx.send(result);
                })
                .map_err(|e| unavailable("capture.run", format!("cannot start device worker: {e}")))?;
            let result = watch::ingest(
                ctx,
                p.watch.clone(),
                compute,
                tiling,
                Some(watch::Managed { ready: ready_tx, completion: done_rx, accepted: accepted_tx }),
            );
            abort.store(true, Ordering::Release);
            worker.join().map_err(|_| unavailable("capture.run", "device worker failed; originals remain in the input folder"))?;
            let result = result?;
            Ok(commit(move |_| Ok(result)))
        })
    })
    .stages(&[("Shooting and stacking previews", 1.0)]))
}

fn await_count(rx: &Receiver<usize>, wanted: usize, timeout: Duration, monitor: &dyn Monitor) -> Result<(), CommandError> {
    let start = Instant::now();
    loop {
        monitor.checkpoint()?;
        if start.elapsed() >= timeout {
            return Err(unavailable("capture.run", "timed out waiting for the next readable tethered frame"));
        }
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(count) if count == wanted => return Ok(()),
            Ok(count) if count > wanted => {
                return Err(unavailable("capture.run", "more than one photo arrived at this rail position; stop and use one photo per position"));
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(unavailable("capture.run", "ingest stopped before the next photo")),
        }
    }
}

fn shoot(
    devices: &Arc<Mutex<Devices>>,
    p: &Sequence,
    input: &Path,
    ready: &Receiver<()>,
    accepted: &Receiver<usize>,
    monitor: &dyn Monitor,
) -> Result<usize, CommandError> {
    loop {
        monitor.checkpoint()?;
        match ready.recv_timeout(Duration::from_millis(20)) {
            Ok(()) => break,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(unavailable("capture.run", "watch setup failed before shooting")),
        }
    }
    let mut devices = lock(devices)?;
    if p.trigger == Trigger::Camera && !devices.camera.as_ref().is_some_and(|c| c.capabilities().capture) {
        return Err(unavailable("capture.run", "camera capture unavailable"));
    }
    if p.mode == Mode::Rail && devices.rail.is_none() {
        return Err(unavailable("capture.run", "connect a rail first"));
    }
    let positions = if p.mode == Mode::Lens { devices.focus.bracket(p.frames - 1).map_err(error)? } else { Vec::new() };
    for frame in 0..p.frames {
        monitor.checkpoint()?;
        if p.mode == Mode::Lens {
            let target = positions.get(frame as usize).ok_or_else(|| unavailable("capture.run", "missing lens position"))?;
            let delta = target.checked_sub(devices.focus.position).ok_or_else(|| unavailable("capture.run", "lens movement overflow"))?;
            if delta != 0 {
                move_focus(&mut devices, delta, monitor)?;
            }
        } else if frame > 0 {
            devices
                .rail
                .as_mut()
                .ok_or_else(|| unavailable("capture.run", "rail disconnected"))?
                .move_by(p.step_micrometers, monitor)
                .map_err(error)?;
        }
        florastack_capture::wait(Duration::from_millis(p.settle_ms), monitor).map_err(error)?;
        match p.trigger {
            Trigger::Camera => {
                let photo =
                    devices.camera.as_mut().ok_or_else(|| unavailable("capture.run", "camera disconnected"))?.capture(monitor).map_err(error)?;
                publish(input, frame, photo)?;
            }
            Trigger::Controller | Trigger::External => {
                if p.trigger == Trigger::Controller {
                    devices.rail.as_mut().ok_or_else(|| unavailable("capture.run", "rail disconnected"))?.shutter(monitor).map_err(error)?;
                }
                await_count(accepted, frame as usize + 1, Duration::from_millis(p.frame_timeout_ms), monitor)?;
            }
        }
    }
    Ok(p.frames as usize)
}

fn publish(input: &Path, frame: u32, photo: florastack_capture::Capture) -> Result<PathBuf, CommandError> {
    let extension = Path::new(&photo.name)
        .extension()
        .and_then(|s| s.to_str())
        .filter(|s| s.len() <= 8 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        .ok_or_else(|| unavailable("capture.run", "camera returned an invalid file extension"))?;
    let mut file =
        tempfile::Builder::new().prefix(&format!("capture-{frame:05}-")).suffix(".part").tempfile_in(input).map_err(|e| batch::io_error(input, e))?;
    let destination = file.path().with_extension(extension);
    if !crate::frames::is_frame_name(&destination) {
        return Err(unavailable("capture.run", "camera returned an unsupported image format"));
    }
    file.write_all(&photo.bytes).map_err(|e| batch::io_error(&destination, e))?;
    file.as_file().sync_all().map_err(|e| batch::io_error(&destination, e))?;
    file.persist_noclobber(&destination).map_err(|e| batch::io_error(&destination, e.error))?;
    Ok(destination)
}
