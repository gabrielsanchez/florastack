//! The GPU context ([`Gpu`]): one wgpu device and queue, the adapter they came from, and the compute pipelines made on
//! it, shared by every kernel and stacker.
//!
//! # Choosing an adapter
//!
//! [`Gpu::new`] enumerates the adapters of the backends FloraStack builds with (Metal, Vulkan, DX12, and WebGPU in a
//! browser) and takes the best one: a discrete GPU before an integrated one, before a virtual one; Vulkan or Metal
//! before DX12 for the same GPU. Software adapters (lavapipe, WARP, `SwiftShader`) run compute shaders correctly but
//! more slowly than the CPU stackers, so they are taken only when [`GpuOptions::software`] allows them: for the
//! agreement tests on GPU-less CI runners. [`GpuOptions::adapter`] picks an adapter by name instead (wgpu's
//! `WGPU_ADAPTER_NAME` convention), software or not.
//!
//! The device is requested with the adapter's own limits, so that buffers can be as large as the adapter allows; the
//! stackers tile their work within them ([`Gpu::max_binding`]).
//!
//! # Errors
//!
//! wgpu reports most errors asynchronously, and its default handler panics. The context installs its own: errors the
//! device reports are kept and returned by the next [`Gpu::check`], which every readback calls, and a lost device
//! fails every operation from then on. A shader that doesn't compile fails when its pipeline is made, inside an error
//! scope. Never crash (AGENTS.md): a GPU failure is an error the caller can fall back from.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::error::GpuError;
use crate::shaders::Kernel;

/// How long a wait for the GPU may take before it counts as a lost device: far beyond any one submission.
pub(crate) const WAIT: Duration = Duration::from_secs(300);

/// Which adapters [`Gpu::new`] may take. See the module documentation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuOptions {
    /// Whether a software adapter may be taken when no hardware one is usable.
    pub software: bool,
    /// Only an adapter whose name contains this, ignoring case (software adapters included).
    pub adapter: Option<String>,
}

impl GpuOptions {
    /// Hardware adapters only, or the one `WGPU_ADAPTER_NAME` names: what the apps use.
    pub fn hardware() -> GpuOptions {
        GpuOptions { software: false, adapter: adapter_from_env() }
    }

    /// Any adapter, software ones included, or the one `WGPU_ADAPTER_NAME` names: what the agreement tests use.
    pub fn any() -> GpuOptions {
        GpuOptions { software: true, adapter: adapter_from_env() }
    }
}

/// The adapter name `WGPU_ADAPTER_NAME` gives, if it is set and not empty.
fn adapter_from_env() -> Option<String> {
    std::env::var("WGPU_ADAPTER_NAME").ok().filter(|s| !s.trim().is_empty())
}

/// What kind of adapter a device runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterKind {
    Discrete,
    Integrated,
    Virtual,
    /// A software rasterizer on the CPU: lavapipe, WARP, `SwiftShader`.
    Software,
    Other,
}

impl AdapterKind {
    fn of(t: wgpu::DeviceType) -> AdapterKind {
        match t {
            wgpu::DeviceType::DiscreteGpu => AdapterKind::Discrete,
            wgpu::DeviceType::IntegratedGpu => AdapterKind::Integrated,
            wgpu::DeviceType::VirtualGpu => AdapterKind::Virtual,
            wgpu::DeviceType::Cpu => AdapterKind::Software,
            wgpu::DeviceType::Other => AdapterKind::Other,
        }
    }

    /// The kind's name, as `compute.info` shows it.
    pub fn as_str(self) -> &'static str {
        match self {
            AdapterKind::Discrete => "discrete",
            AdapterKind::Integrated => "integrated",
            AdapterKind::Virtual => "virtual",
            AdapterKind::Software => "software",
            AdapterKind::Other => "other",
        }
    }
}

/// The adapter a [`Gpu`] runs on, as logs and `compute.info` show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterSummary {
    pub name: String,
    /// `vulkan`, `metal`, `dx12` or `webgpu`.
    pub backend: String,
    pub kind: AdapterKind,
    /// The driver and its version, where the backend says.
    pub driver: String,
}

impl AdapterSummary {
    fn of(info: &wgpu::AdapterInfo) -> AdapterSummary {
        let backend = match info.backend {
            wgpu::Backend::Vulkan => "vulkan",
            wgpu::Backend::Metal => "metal",
            wgpu::Backend::Dx12 => "dx12",
            wgpu::Backend::BrowserWebGpu => "webgpu",
            wgpu::Backend::Gl => "gl",
            wgpu::Backend::Noop => "other",
        };
        let driver = [info.driver.trim(), info.driver_info.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        AdapterSummary { name: info.name.clone(), backend: backend.to_owned(), kind: AdapterKind::of(info.device_type), driver }
    }
}

impl std::fmt::Display for AdapterSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}, {})", self.name, self.backend, self.kind.as_str())
    }
}

/// A GPU device and its queue, ready for FloraStack's kernels. Cheap to clone: clones share the device. See the module
/// documentation.
#[derive(Clone)]
pub struct Gpu {
    inner: Arc<Inner>,
}

struct Inner {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: AdapterSummary,
    limits: wgpu::Limits,
    /// Errors the device reported outside an error scope, oldest first.
    errors: Arc<Mutex<Vec<String>>>,
    /// Why the device was lost, once it is.
    lost: Arc<Mutex<Option<String>>>,
    pipelines: Mutex<HashMap<Kernel, wgpu::ComputePipeline>>,
    /// A small buffer bound where a kernel declares a plane it doesn't read this time.
    dummy: wgpu::Buffer,
}

impl std::fmt::Debug for Gpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gpu").field("adapter", &self.inner.adapter).finish_non_exhaustive()
    }
}

/// The best adapter for compute among `adapters` that `options` allow, and why none is when none is.
fn choose(adapters: Vec<wgpu::Adapter>, options: &GpuOptions) -> Result<wgpu::Adapter, GpuError> {
    let named = options.adapter.as_ref().map(|n| n.to_lowercase());
    let mut seen = Vec::new();
    let mut usable: Vec<(u8, u8, wgpu::Adapter)> = Vec::new();
    for adapter in adapters {
        let info = adapter.get_info();
        seen.push(format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type));
        if let Some(name) = &named {
            if !info.name.to_lowercase().contains(name) {
                continue;
            }
        } else if info.device_type == wgpu::DeviceType::Cpu && !options.software {
            continue;
        }
        let kind = match info.device_type {
            wgpu::DeviceType::DiscreteGpu => 0,
            wgpu::DeviceType::IntegratedGpu => 1,
            wgpu::DeviceType::VirtualGpu => 2,
            wgpu::DeviceType::Other => 3,
            wgpu::DeviceType::Cpu => 4,
        };
        let backend = match info.backend {
            wgpu::Backend::Vulkan | wgpu::Backend::Metal => 0,
            wgpu::Backend::Dx12 => 1,
            _ => 2,
        };
        usable.push((kind, backend, adapter));
    }
    // A stable sort keeps the enumeration's order among equals.
    usable.sort_by_key(|(kind, backend, _)| (*kind, *backend));
    if let Some((_, _, adapter)) = usable.into_iter().next() {
        return Ok(adapter);
    }
    let reason = match (&options.adapter, seen.is_empty()) {
        (_, true) => "the system has no Metal, Vulkan or DX12 adapter".to_owned(),
        (Some(name), false) => format!("no adapter is named like \"{name}\" (found: {})", seen.join(", ")),
        (None, false) => format!("only software adapters were found ({})", seen.join(", ")),
    };
    Err(GpuError::NoAdapter { reason })
}

impl Gpu {
    /// A device on the best adapter `options` allow. See the module documentation.
    pub async fn new(options: &GpuOptions) -> Result<Gpu, GpuError> {
        let backends = wgpu::Backends::VULKAN | wgpu::Backends::METAL | wgpu::Backends::DX12 | wgpu::Backends::BROWSER_WEBGPU;
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        let instance = wgpu::Instance::new(desc);
        let adapter = if cfg!(target_arch = "wasm32") {
            // A browser lists no adapters: it gives one when asked.
            let options = wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() };
            instance.request_adapter(&options).await.map_err(|e| GpuError::NoAdapter { reason: e.to_string() })?
        } else {
            choose(instance.enumerate_adapters(backends).await, options)?
        };
        let info = adapter.get_info();
        let summary = AdapterSummary::of(&info);
        let limits = adapter.limits();
        let desc = wgpu::DeviceDescriptor {
            label: Some("florastack"),
            required_features: wgpu::Features::empty(),
            required_limits: limits.clone(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        };
        let (device, queue) =
            adapter.request_device(&desc).await.map_err(|source| GpuError::RequestDevice { adapter: summary.to_string(), source })?;
        let errors = Arc::new(Mutex::new(Vec::new()));
        let lost = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&errors);
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            tracing::warn!("the GPU reported an error: {e}");
            sink.lock().unwrap_or_else(PoisonError::into_inner).push(e.to_string());
        }));
        let why = Arc::clone(&lost);
        device.set_device_lost_callback(move |reason, message| {
            *why.lock().unwrap_or_else(PoisonError::into_inner) = Some(format!("{reason:?}: {message}"));
        });
        let dummy = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unused plane"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        tracing::debug!("GPU device on {summary}");
        Ok(Gpu { inner: Arc::new(Inner { device, queue, adapter: summary, limits, errors, lost, pipelines: Mutex::new(HashMap::new()), dummy }) })
    }

    /// [`Gpu::new`], waiting for the device: native targets only, where wgpu's futures complete without an event loop.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn blocking(options: &GpuOptions) -> Result<Gpu, GpuError> {
        pollster::block_on(Gpu::new(options))
    }

    pub fn adapter(&self) -> &AdapterSummary {
        &self.inner.adapter
    }

    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.inner.device
    }

    pub(crate) fn queue(&self) -> &wgpu::Queue {
        &self.inner.queue
    }

    /// The largest storage buffer a kernel may bind, in bytes: a plane or a tile's state never exceeds it.
    pub fn max_binding(&self) -> u64 {
        self.inner.limits.max_storage_buffer_binding_size.min(self.inner.limits.max_buffer_size)
    }

    /// The most workgroups a dispatch may have along one dimension.
    pub(crate) fn max_groups(&self) -> u32 {
        self.inner.limits.max_compute_workgroups_per_dimension.max(1)
    }

    /// Whether the device has been lost: it can do nothing more.
    pub fn is_lost(&self) -> bool {
        self.inner.lost.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    /// The first error the device reported since the last check, or its loss.
    pub fn check(&self) -> Result<(), GpuError> {
        if let Some(message) = self.inner.lost.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            return Err(GpuError::Lost { message });
        }
        let mut errors = self.inner.errors.lock().unwrap_or_else(PoisonError::into_inner);
        if errors.is_empty() {
            return Ok(());
        }
        let message = errors.remove(0);
        errors.clear();
        Err(GpuError::Device { message })
    }

    /// Checks the device without waiting, running the callbacks of what it has finished.
    pub(crate) fn poll(&self) -> Result<(), GpuError> {
        self.inner.device.poll(wgpu::PollType::Poll).map_err(|e| GpuError::Lost { message: e.to_string() })?;
        self.check()
    }

    /// Waits until the GPU has done everything submitted so far.
    pub(crate) fn wait(&self) -> Result<(), GpuError> {
        self.inner
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(WAIT) })
            .map_err(|e| GpuError::Lost { message: e.to_string() })?;
        self.check()
    }

    /// Makes the pipeline of every kernel: an error names the first that doesn't compile on this device.
    pub fn prepare_kernels(&self) -> Result<(), GpuError> {
        for kernel in Kernel::ALL {
            self.pipeline(kernel)?;
        }
        self.check()
    }

    /// The buffer bound for a plane a kernel declares but doesn't read this time.
    pub(crate) fn dummy(&self) -> &wgpu::Buffer {
        &self.inner.dummy
    }

    /// The pipeline of `kernel`, made on first use: an error if its shader doesn't compile.
    pub(crate) fn pipeline(&self, kernel: Kernel) -> Result<wgpu::ComputePipeline, GpuError> {
        if let Some(p) = self.inner.pipelines.lock().unwrap_or_else(PoisonError::into_inner).get(&kernel) {
            return Ok(p.clone());
        }
        let device = &self.inner.device;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(kernel.name()),
            source: wgpu::ShaderSource::Wgsl(kernel.source().into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(kernel.name()),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        if let Some(e) = block_on(scope.pop()) {
            return Err(GpuError::Device { message: format!("the kernel {} doesn't compile: {e}", kernel.name()) });
        }
        self.inner.pipelines.lock().unwrap_or_else(PoisonError::into_inner).insert(kernel, pipeline.clone());
        Ok(pipeline)
    }
}

/// Waits for one of wgpu's futures, which complete at once on native targets once the device is polled.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn block_on<F: std::future::Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

/// In a browser nothing may block: error scopes aren't awaited there (the device's handler still records errors).
#[cfg(target_arch = "wasm32")]
pub(crate) fn block_on<F: std::future::Future<Output = Option<wgpu::Error>>>(_: F) -> Option<wgpu::Error> {
    None
}
