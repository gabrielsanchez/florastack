//! GPU compute versions of the hot paths of alignment and stacking, written as WGSL shaders on wgpu so they run on Metal,
//! Vulkan, DX12 and WebGPU. Every kernel has a test that compares it with its CPU reference in `florastack-core` (the
//! conversions and basic operations), `florastack-stack` or `florastack-align`, and the engine falls back to the CPU
//! when no adapter passes a self-test. It may depend only on `florastack-core`.
//!
//! - [`Gpu`] (`device.rs`): a device on the best adapter, with the errors wgpu reports kept for the next check
//!   instead of panicking, and [`Gpu::self_test`] (`selftest.rs`), which the engine runs before choosing the GPU.
//! - [`Plane`] (`plane.rs`): samples on the GPU, laid out as florastack-core's images are; kernels are recorded into
//!   a batch (`batch.rs`) and their WGSL sources live in `src/shaders/` (`shaders.rs`), one kernel a file.
//! - [`ops`]: conversions, the Gaussian blur, the Laplacian and the alignment warp with its coverage.
//! - [`levels`]: the pyramid's REDUCE (plain and normalized), EXPAND, local energy, Laplacian pyramid and collapse.
//! - [`GpuStacker`] (`stack/`): the weighted average, the pyramid method and the depth map's two passes, streamed one
//!   frame at a time with their accumulators kept on the GPU, tiled within a VRAM budget.

mod batch;
mod device;
mod error;
pub mod levels;
pub mod ops;
mod plane;
mod selftest;
mod shaders;
mod stack;

pub use device::{AdapterKind, AdapterSummary, Gpu, GpuOptions};
pub use error::GpuError;
pub use plane::Plane;
pub use stack::{Frame, GpuStacked, GpuStacker, GpuTiling, Method, levels_for};
