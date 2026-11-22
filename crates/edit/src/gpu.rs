//! LightCraft's GPU renderer (`lightcraft-gpu`, wgpu compute), which a [`crate::Renderer`] uses for a request that
//! asks for it ([`crate::Request::gpu`]): previews, never files, which the CPU renders so that they are the same on
//! every machine. The CPU pipeline is its reference: `tests/gpu.rs` renders the same adjustments on both and bounds
//! the difference. It keeps a device of its own, on the best hardware adapter (never a software one, which is slower
//! than the CPU); a render it can't make, or one that fails, is made on the CPU.

use std::time::Duration;

/// Why the GPU renderer can't render here (no hardware adapter, the environment turning it off, an earlier failure),
/// or `None` when it can. The first call creates its device.
pub fn unavailable() -> Option<String> {
    if lightcraft_gpu::available() {
        None
    } else {
        Some(lightcraft_gpu::unavailable_reason().unwrap_or_else(|| "no hardware GPU adapter".to_owned()))
    }
}

/// Before the process ends: lets no new GPU work start, and waits up to `timeout` for what is in flight, which would
/// crash inside the driver if the process ended under it. Whether it all finished.
pub fn shutdown(timeout: Duration) -> bool {
    lightcraft_gpu::begin_shutdown();
    lightcraft_gpu::wait_idle(timeout)
}
