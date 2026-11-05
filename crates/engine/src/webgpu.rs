//! Stacking on the GPU in a browser: WebGPU on a worker of its own, which jobs stack through.
//!
//! wgpu's objects can't move between a browser's threads, and its readbacks must be awaited, which a job can't do: a
//! job runs to its end on a worker that blocks. So one worker, `florastack-gpu`, owns the device, from its creation to
//! the page's end, and runs an event loop that serves requests: it makes the device, compiles the kernels and runs the
//! self-test (florastack-gpu's `self_test_async`), then makes stackers, folds frames into them and finishes them
//! (`finish_async`), each at a job's request. A job sends each request with a copy of what it needs (a frame's
//! samples) and waits for the answer, through [`WebGpu`] and [`RemoteStacker`], which have the methods of
//! florastack-gpu's `Gpu` and `GpuStacker` that the engine's stacking uses (`commands::stack`), so that the same code
//! stacks on either.
//!
//! The device is made once, when [`probe`] is first asked: while its self-test runs, the page's main thread, which
//! may not wait, hears that the CPU stacks; a job waits for the outcome. A stack whose working data outgrows the GPU's
//! budget would have to read tiles back during its frames, which a browser can't do without awaiting them: its frame
//! fails, and the engine stacks it again on the CPU, as after any GPU failure.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;

use florastack_core::sync::Mutex;
use florastack_core::{Image, Monitor, Unmonitored};
use florastack_gpu::{AdapterSummary, Frame, Gpu, GpuError, GpuOptions, GpuStacked, GpuStacker, GpuTiling, Method};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use crate::compute::Probe;

type Reply<T> = mpsc::Sender<Result<T, String>>;

/// A frame, owned, as a job sends it.
enum OwnedFrame {
    Warp { image: Image<f32>, geometry: florastack_gpu::ops::WarpGeometry },
    Canvas { image: Image<f32>, coverage: Option<Image<f32>> },
}

impl OwnedFrame {
    fn of(frame: &Frame<'_>) -> OwnedFrame {
        match frame {
            Frame::Warp { image, geometry } => OwnedFrame::Warp { image: (*image).clone(), geometry: *geometry },
            Frame::Canvas { image, coverage } => OwnedFrame::Canvas { image: (*image).clone(), coverage: coverage.map(Clone::clone) },
        }
    }

    fn frame(&self) -> Frame<'_> {
        match self {
            OwnedFrame::Warp { image, geometry } => Frame::Warp { image, geometry: *geometry },
            OwnedFrame::Canvas { image, coverage } => Frame::Canvas { image, coverage: coverage.as_ref() },
        }
    }
}

enum Request {
    New { method: Method, tiling: GpuTiling, reply: Reply<u32> },
    Begin { stacker: u32, width: usize, height: usize, channels: usize, reply: Reply<()> },
    Add { stacker: u32, frame: OwnedFrame, reply: Reply<()> },
    Finish { stacker: u32, reply: Reply<GpuStacked> },
    Drop { stacker: u32 },
}

/// The GPU worker's device, as the engine sees it from any thread. See the module documentation.
#[derive(Clone, Debug)]
pub(crate) struct WebGpu {
    requests: UnboundedSender<Request>,
    adapter: AdapterSummary,
    /// Set once the device is lost.
    lost: Arc<AtomicBool>,
}

impl WebGpu {
    pub(crate) fn adapter(&self) -> &AdapterSummary {
        &self.adapter
    }

    pub(crate) fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Sends a request and waits for its answer: on a thread that may block (a job's).
    fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Request) -> Result<T, GpuError> {
        let gone = || GpuError::Lost { message: "the GPU worker has stopped".to_owned() };
        let (reply, answer) = mpsc::channel();
        self.requests.unbounded_send(make(reply)).map_err(|_| gone())?;
        answer.recv().map_err(|_| gone())?.map_err(|message| GpuError::Device { message })
    }
}

/// A stacker on the GPU worker, with florastack-gpu's `GpuStacker`'s methods. See the module documentation.
pub(crate) struct RemoteStacker {
    gpu: WebGpu,
    id: u32,
}

impl RemoteStacker {
    pub(crate) fn new(gpu: &WebGpu, method: Method, tiling: GpuTiling) -> Result<RemoteStacker, GpuError> {
        let id = gpu.call(|reply| Request::New { method, tiling, reply })?;
        Ok(RemoteStacker { gpu: gpu.clone(), id })
    }

    pub(crate) fn begin(&mut self, width: usize, height: usize, channels: usize) -> Result<(), GpuError> {
        let stacker = self.id;
        self.gpu.call(|reply| Request::Begin { stacker, width, height, channels, reply })
    }

    /// Folds `frame` in; `monitor` hears of it once it is folded, and cancels before.
    pub(crate) fn add_frame(&mut self, frame: &Frame<'_>, monitor: &dyn Monitor) -> Result<(), GpuError> {
        monitor.checkpoint()?;
        let (stacker, frame) = (self.id, OwnedFrame::of(frame));
        self.gpu.call(|reply| Request::Add { stacker, frame, reply })?;
        monitor.step(1, 1)?;
        Ok(())
    }

    pub(crate) fn finish(&mut self, monitor: &dyn Monitor) -> Result<GpuStacked, GpuError> {
        monitor.checkpoint()?;
        let stacker = self.id;
        let result = self.gpu.call(|reply| Request::Finish { stacker, reply })?;
        monitor.progress(1, 1);
        Ok(result)
    }
}

impl Drop for RemoteStacker {
    fn drop(&mut self) {
        let _ = self.gpu.requests.unbounded_send(Request::Drop { stacker: self.id });
    }
}

/// The probe's state: not asked yet, the worker testing its device, or its outcome.
#[derive(Clone, Debug)]
enum State {
    Testing,
    Done(Probe),
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn state() -> std::sync::MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The GPU's probe in a browser (see the module documentation): the device, or why the CPU stacks. Starts the GPU
/// worker the first time; on the page's main thread, `None` while its self-test runs.
pub(crate) fn probe() -> Option<Probe> {
    let started = {
        let mut state = state();
        match &*state {
            Some(State::Done(Probe::Ready(gpu))) if gpu.is_lost() => {
                return Some(Probe::Unusable("the browser's GPU device was lost: reload the page to use it again".to_owned()));
            }
            Some(State::Done(probe)) => return Some(probe.clone()),
            Some(State::Testing) => false,
            None => {
                *state = Some(State::Testing);
                true
            }
        }
    };
    if started && let Err(e) = start() {
        let probe = Probe::Unusable(format!("no GPU worker: {e}"));
        *state() = Some(State::Done(probe.clone()));
        return Some(probe);
    }
    if !florastack_core::sync::can_block() {
        return None;
    }
    // A job waits for the self-test, which takes a moment once the browser has the device.
    loop {
        if let Some(State::Done(probe)) = &*state() {
            return Some(probe.clone());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Starts the GPU worker, which tests its device and records the outcome, then serves the jobs.
fn start() -> std::io::Result<()> {
    let (requests, received) = unbounded();
    florastack_core::thread::spawn_async("florastack-gpu", move || serve(requests, received))
}

async fn serve(requests: UnboundedSender<Request>, mut received: UnboundedReceiver<Request>) {
    let made = async {
        let gpu = Gpu::new(&GpuOptions::hardware()).await?;
        gpu.prepare_kernels()?;
        gpu.self_test_async().await?;
        Ok::<_, GpuError>(gpu)
    }
    .await;
    let lost = Arc::new(AtomicBool::new(false));
    let gpu = match made {
        Ok(gpu) => {
            tracing::info!("stacking on the GPU: {} (it passed the self-test)", gpu.adapter());
            // Browsers keep the adapter's name to themselves.
            let mut adapter = gpu.adapter().clone();
            if adapter.name.is_empty() {
                "WebGPU".clone_into(&mut adapter.name);
            }
            let web = WebGpu { requests, adapter, lost: Arc::clone(&lost) };
            *state() = Some(State::Done(Probe::Ready(web)));
            gpu
        }
        Err(e) => {
            tracing::info!("stacking on the CPU: {e}");
            let reason = match e {
                GpuError::NoAdapter { .. } => format!("this browser has no WebGPU adapter ({e})"),
                e => format!("the GPU failed its self-test: {e}"),
            };
            *state() = Some(State::Done(Probe::Unusable(reason)));
            return;
        }
    };
    let next = AtomicU32::new(1);
    let mut stackers: HashMap<u32, GpuStacker> = HashMap::new();
    while let Some(request) = std::future::poll_fn(|cx| futures_core::Stream::poll_next(Pin::new(&mut received), cx)).await {
        match request {
            Request::New { method, tiling, reply } => {
                let made = GpuStacker::new(&gpu, method, tiling).map(|stacker| {
                    let id = next.fetch_add(1, Ordering::Relaxed);
                    stackers.insert(id, stacker);
                    id
                });
                let _ = reply.send(made.map_err(|e| e.to_string()));
            }
            Request::Begin { stacker, width, height, channels, reply } => {
                let done = stackers.get_mut(&stacker).ok_or_else(missing).and_then(|s| s.begin(width, height, channels).map_err(|e| e.to_string()));
                let _ = reply.send(done);
            }
            Request::Add { stacker, frame, reply } => {
                let done =
                    stackers.get_mut(&stacker).ok_or_else(missing).and_then(|s| s.add_frame(&frame.frame(), &Unmonitored).map_err(|e| e.to_string()));
                let _ = reply.send(done);
            }
            Request::Finish { stacker, reply } => {
                let done = match stackers.get_mut(&stacker) {
                    Some(s) => s.finish_async(&Unmonitored).await.map_err(|e| e.to_string()),
                    None => Err(missing()),
                };
                let _ = reply.send(done);
            }
            Request::Drop { stacker } => {
                stackers.remove(&stacker);
            }
        }
        if gpu.is_lost() {
            lost.store(true, Ordering::Release);
        }
    }
}

fn missing() -> String {
    "no such stacker on the GPU".to_owned()
}
