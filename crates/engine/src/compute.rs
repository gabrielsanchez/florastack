//! Which device stacks: the GPU (florastack-gpu) or the CPU (florastack-stack), chosen once per process.
//!
//! The engine's preference ([`Compute`], in [`crate::EngineConfig`] and changed by `compute.set`) is a [`Device`]
//! and the GPU's memory budget. With `auto`, the default, the engine takes the GPU when a hardware adapter passes the
//! self-test (every kernel compiled, and a few compared with their CPU references: `Gpu::self_test`), and otherwise
//! the CPU, logging why; `cpu` forces the CPU; `gpu` takes software adapters (lavapipe, WARP) too, which compute
//! correctly but more slowly than the CPU. The probe runs when a stack first needs it (or `compute.info` asks), once
//! per process, shared by every engine in it (a batch makes one per stack), and again after a device is lost.
//!
//! Stacking on the GPU falls back to the CPU when the GPU fails part way (a lost device, a driver error): the stack
//! starts again on the CPU, frames read again, and the failure is logged. Cancellation and errors reading frames are
//! never a reason to fall back.

use std::sync::{Mutex, PoisonError};

use florastack_gpu::{AdapterSummary, Gpu, GpuError, GpuTiling};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which device stacks. See the module documentation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Device {
    /// The GPU when a hardware adapter passes the self-test, the CPU otherwise.
    #[default]
    Auto,
    /// Always the CPU.
    Cpu,
    /// The GPU when any adapter, software ones included, passes the self-test; the CPU otherwise.
    Gpu,
}

impl Device {
    pub fn as_str(self) -> &'static str {
        match self {
            Device::Auto => "auto",
            Device::Cpu => "cpu",
            Device::Gpu => "gpu",
        }
    }
}

/// The engine's compute preference: the device, and how much of the GPU's memory a stack's accumulators may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Compute {
    pub device: Device,
    /// Bytes; beyond them, a stack's tiles are kept on the host between frames.
    pub budget: usize,
}

impl Compute {
    /// The CPU, whatever the machine has: for tests and measurements that must not depend on it.
    pub const CPU: Compute = Compute { device: Device::Cpu, budget: GpuTiling::DEFAULT_BUDGET };
}

impl Default for Compute {
    fn default() -> Self {
        Compute { device: Device::Auto, budget: GpuTiling::DEFAULT_BUDGET }
    }
}

/// What stacks, resolved from a preference.
#[derive(Clone, Debug)]
pub(crate) enum Backend {
    /// The CPU, and why not the GPU.
    Cpu {
        reason: String,
    },
    Gpu {
        gpu: Gpu,
        tiling: GpuTiling,
    },
}

/// The outcome of probing for a GPU.
#[derive(Clone, Debug)]
enum Probe {
    Ready(Gpu),
    /// Why no GPU is used.
    Unusable(String),
}

/// The process's probes: hardware adapters only, and software ones too.
static PROBES: Mutex<[Option<Probe>; 2]> = Mutex::new([None, None]);

/// Makes a device on the best adapter `software` allows, compiles every kernel and runs the self-test.
#[cfg(not(target_arch = "wasm32"))]
fn probe(software: bool) -> Probe {
    let options = if software { florastack_gpu::GpuOptions::any() } else { florastack_gpu::GpuOptions::hardware() };
    let tried = std::panic::catch_unwind(|| {
        let gpu = Gpu::blocking(&options)?;
        gpu.prepare_kernels()?;
        gpu.self_test()?;
        Ok::<_, GpuError>(gpu)
    });
    match tried {
        Ok(Ok(gpu)) => {
            tracing::info!("stacking on the GPU: {} (it passed the self-test)", gpu.adapter());
            Probe::Ready(gpu)
        }
        Ok(Err(e @ GpuError::NoAdapter { .. })) => {
            tracing::info!("stacking on the CPU: {e}");
            Probe::Unusable(e.to_string())
        }
        Ok(Err(e)) => {
            tracing::warn!("stacking on the CPU: the GPU failed its self-test: {e}");
            Probe::Unusable(format!("the GPU failed its self-test: {e}"))
        }
        Err(_) => {
            tracing::warn!("stacking on the CPU: the GPU driver panicked while it was probed");
            Probe::Unusable("the GPU driver panicked while it was probed".to_owned())
        }
    }
}

/// In a browser the jobs can't wait for the GPU yet: they stack on the CPU.
#[cfg(target_arch = "wasm32")]
fn probe(_software: bool) -> Probe {
    Probe::Unusable("the web build stacks on the CPU until its jobs can wait for the GPU".to_owned())
}

impl Compute {
    /// The device this preference stacks on, probing for a GPU the first time one is needed (see the module
    /// documentation).
    pub(crate) fn backend(&self) -> Backend {
        let software = match self.device {
            Device::Cpu => return Backend::Cpu { reason: "the compute preference is `cpu`".to_owned() },
            Device::Auto => false,
            Device::Gpu => true,
        };
        let mut probes = PROBES.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = &mut probes[usize::from(software)];
        if matches!(slot, Some(Probe::Ready(gpu)) if gpu.is_lost()) {
            tracing::warn!("the GPU device was lost: probing again");
            *slot = None;
        }
        match slot.get_or_insert_with(|| probe(software)) {
            Probe::Ready(gpu) => Backend::Gpu { gpu: gpu.clone(), tiling: GpuTiling { budget: self.budget, ..GpuTiling::default() } },
            Probe::Unusable(reason) => Backend::Cpu { reason: reason.clone() },
        }
    }
}

/// Probes for a GPU again, now, for `device`: the self-test that `compute.test` runs on demand.
pub(crate) fn reprobe(device: Device) {
    let software = match device {
        Device::Cpu => return,
        Device::Auto => false,
        Device::Gpu => true,
    };
    let mut probes = PROBES.lock().unwrap_or_else(PoisonError::into_inner);
    probes[usize::from(software)] = None;
}

/// The memory installed, in bytes, when the system says.
#[cfg(not(target_arch = "wasm32"))]
pub fn installed_memory() -> Option<u64> {
    let mut system = sysinfo::System::new().ok()?;
    system.refresh_memory();
    Some(system.total_memory()).filter(|&m| m > 0)
}

#[cfg(target_arch = "wasm32")]
pub fn installed_memory() -> Option<u64> {
    None
}

/// The memory budget suggested for stacking with `installed` bytes of memory: a quarter of it, from 512 MiB to 16 GiB;
/// 1 GiB when the memory isn't known.
pub fn suggested_budget(installed: Option<u64>) -> usize {
    const MIB: u64 = 1 << 20;
    let bytes = installed.map_or(1 << 30, |m| (m / 4).clamp(512 * MIB, 16 * 1024 * MIB));
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

/// How much memory stacking may use, as `compute.info` reports it.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryInfo {
    /// The memory a stack's working data may take before tiles move to the tile folder, in MiB.
    pub budget_mb: usize,
    /// The memory installed, in MiB, when the system says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_mb: Option<u64>,
    /// The budget suggested for this machine, in MiB: a quarter of the memory installed, from 512 MiB to 16 GiB.
    pub suggested_mb: usize,
    /// Where tiles beyond the budget go: the folder set, or the system's temporary folder.
    pub tile_folder: String,
}

impl MemoryInfo {
    pub(crate) fn of(tiling: &florastack_stack::Tiling) -> MemoryInfo {
        let installed = installed_memory();
        let folder = tiling.spill_dir.clone().unwrap_or_else(std::env::temp_dir);
        MemoryInfo {
            budget_mb: tiling.budget >> 20,
            installed_mb: installed.map(|m| m >> 20),
            suggested_mb: suggested_budget(installed) >> 20,
            tile_folder: folder.display().to_string(),
        }
    }
}

/// The compute device as `compute.info` and `compute.set` report it.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputeInfo {
    /// The preference: `auto`, `cpu` or `gpu`.
    pub preference: Device,
    /// What stacks: `gpu` or `cpu`.
    pub device: &'static str,
    /// The GPU's adapter, when stacking on the GPU.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter: Option<Adapter>,
    /// Why the CPU stacks, when it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The GPU memory a stack's accumulators may take, in MiB.
    pub budget_mb: usize,
    /// The memory stacking may use on the host, and where it spills beyond it.
    pub memory: MemoryInfo,
}

/// A GPU adapter.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct Adapter {
    pub name: String,
    /// `vulkan`, `metal`, `dx12` or `webgpu`.
    pub backend: String,
    /// `discrete`, `integrated`, `virtual`, `software` or `other`.
    pub kind: &'static str,
    pub driver: String,
}

impl From<&AdapterSummary> for Adapter {
    fn from(a: &AdapterSummary) -> Adapter {
        Adapter { name: a.name.clone(), backend: a.backend.clone(), kind: a.kind.as_str(), driver: a.driver.clone() }
    }
}

impl ComputeInfo {
    pub(crate) fn of(compute: &Compute, tiling: &florastack_stack::Tiling) -> ComputeInfo {
        let (device, adapter, reason) = match compute.backend() {
            Backend::Gpu { gpu, .. } => ("gpu", Some(Adapter::from(gpu.adapter())), None),
            Backend::Cpu { reason } => ("cpu", None, Some(reason)),
        };
        ComputeInfo { preference: compute.device, device, adapter, reason, budget_mb: compute.budget >> 20, memory: MemoryInfo::of(tiling) }
    }
}
