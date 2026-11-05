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
//!
//! In a browser the GPU is WebGPU on a worker of its own, probed once per page (`crate::webgpu`): [`GpuDevice`] is the
//! engine's handle on it there, florastack-gpu's `Gpu` on native targets.

#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Mutex, PoisonError};

use florastack_gpu::{AdapterSummary, GpuTiling};
#[cfg(not(target_arch = "wasm32"))]
use florastack_gpu::{Gpu, GpuError};

use crate::error::CommandError;

/// The device that stacks on the GPU: florastack-gpu's own natively, the browser's GPU worker's in a browser.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type GpuDevice = Gpu;
#[cfg(target_arch = "wasm32")]
pub(crate) type GpuDevice = crate::webgpu::WebGpu;
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
        gpu: GpuDevice,
        tiling: GpuTiling,
    },
}

/// The outcome of probing for a GPU.
#[derive(Clone, Debug)]
pub(crate) enum Probe {
    Ready(GpuDevice),
    /// Why no GPU is used.
    Unusable(String),
}

/// The process's probes: hardware adapters only, and software ones too.
#[cfg(not(target_arch = "wasm32"))]
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

impl Compute {
    /// The device this preference stacks on, probing for a GPU the first time one is needed (see the module
    /// documentation).
    pub(crate) fn backend(&self) -> Backend {
        let software = match self.device {
            Device::Cpu => return Backend::Cpu { reason: "the compute preference is `cpu`".to_owned() },
            Device::Auto => false,
            Device::Gpu => true,
        };
        match probed(software) {
            Some(Probe::Ready(gpu)) => Backend::Gpu { gpu, tiling: GpuTiling { budget: self.budget, ..GpuTiling::default() } },
            Some(Probe::Unusable(reason)) => Backend::Cpu { reason },
            None => Backend::Cpu { reason: "the browser's GPU is still being tested".to_owned() },
        }
    }
}

/// The probe for `software` adapters or not, probing the first time; `None` while a browser's GPU is being tested,
/// which the page's main thread doesn't wait for (a job does).
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::unnecessary_wraps, reason = "the browser's probe has no outcome while it runs")]
fn probed(software: bool) -> Option<Probe> {
    let mut probes = PROBES.lock().unwrap_or_else(PoisonError::into_inner);
    let slot = &mut probes[usize::from(software)];
    if matches!(slot, Some(Probe::Ready(gpu)) if gpu.is_lost()) {
        tracing::warn!("the GPU device was lost: probing again");
        *slot = None;
    }
    Some(slot.get_or_insert_with(|| probe(software)).clone())
}

/// A browser has one GPU, which it chooses: software or not, the same probe.
#[cfg(target_arch = "wasm32")]
fn probed(_software: bool) -> Option<Probe> {
    crate::webgpu::probe()
}

/// Probes for a GPU again, now, for `device`: the self-test that `compute.test` runs on demand.
pub(crate) fn reprobe(device: Device) {
    let software = match device {
        Device::Cpu => return,
        Device::Auto => false,
        Device::Gpu => true,
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut probes = PROBES.lock().unwrap_or_else(PoisonError::into_inner);
        probes[usize::from(software)] = None;
    }
    // A browser's GPU is tested once per page.
    #[cfg(target_arch = "wasm32")]
    let _ = software;
}

/// The most memory a page may have in the browser build: 4 GiB, WebAssembly's 32-bit limit, which `cargo xtask web`
/// links for.
pub const BROWSER_MEMORY: u64 = 4 << 30;

/// What a browser page keeps for itself beyond what a stack takes: the module, the app, its textures.
const BROWSER_RESERVE: u64 = 384 << 20;

/// The memory a stack of color frames of `frame` pixels (width and height) onto a canvas of `canvas` pixels may take
/// at its peak, with `budget` bytes for its working tiles: the frame being folded and the next, read beside it, as
/// linear `f32`; the working tiles; and the result with its alpha, its copy brought into range and its encoded file.
pub fn stack_memory(frame: (usize, usize), canvas: (usize, usize), budget: usize) -> u64 {
    const COLOR: u64 = 3 * 4;
    let pixels = |(w, h): (usize, usize)| (w as u64).saturating_mul(h as u64);
    let (frame, canvas) = (pixels(frame).saturating_mul(COLOR), pixels(canvas).saturating_mul(COLOR));
    frame.saturating_mul(2).saturating_add(budget as u64).saturating_add(canvas.saturating_mul(4))
}

/// Whether `needed` bytes fit in a page beside the `held` bytes of its files, within `limit`.
fn fits(needed: u64, held: u64, limit: u64) -> bool {
    needed.saturating_add(held).saturating_add(BROWSER_RESERVE) <= limit
}

/// In the browser build, an error that says what to do when `command`'s stack, which needs `needed` bytes
/// ([`stack_memory`]), wouldn't fit in the page's memory beside the files it holds. Native targets have the memory:
/// what doesn't fit spills to disk.
#[cfg_attr(not(target_arch = "wasm32"), allow(clippy::unnecessary_wraps, reason = "only a browser runs out"))]
pub(crate) fn check_memory(command: &str, needed: u64) -> Result<(), CommandError> {
    #[cfg(target_arch = "wasm32")]
    {
        let held = florastack_core::fs::bytes_in_memory().unwrap_or(0);
        if !fits(needed, held, BROWSER_MEMORY) {
            let gb = |b: u64| b as f64 / f64::from(1_u32 << 30);
            return Err(CommandError::Unavailable {
                command: command.to_owned(),
                reason: format!(
                    "the stack needs about {:.1} GB of memory beside the {:.1} GB of files the page holds, and a browser page can have \
                     {:.0} GB: stack fewer or smaller frames (the frames' size and the output's count, not their number), or use the \
                     desktop app",
                    gb(needed),
                    gb(held),
                    gb(BROWSER_MEMORY)
                ),
            });
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (command, needed, fits);
    Ok(())
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
        let folder = tiling.spill_dir.clone().unwrap_or_else(florastack_core::fs::temp_dir);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stacks_memory_is_two_frames_the_budget_and_four_canvases() {
        const MIB: u64 = 1 << 20;
        // 24 MP color frames onto their own canvas, with a 512 MiB budget: 2 x 288 MB + 512 MiB + 4 x 288 MB.
        let needed = stack_memory((6000, 4000), (6000, 4000), 512 << 20);
        assert_eq!(needed, 6 * 288_000_000 + 512 * MIB);
        assert!(fits(needed, 200 * MIB, BROWSER_MEMORY), "a 24 MP stack fits in a page beside 200 MB of files");
        let large = stack_memory((11_648, 8_736), (11_648, 8_736), 512 << 20);
        assert!(!fits(large, 0, BROWSER_MEMORY), "a 100 MP stack doesn't");
        assert_eq!(stack_memory((usize::MAX, usize::MAX), (1, 1), 0), u64::MAX, "no overflow");
        assert!(check_memory("stack.pyramid", u64::MAX).is_ok(), "native targets always have the memory");
    }
}
