//! GPU compute versions of the hot paths of alignment and stacking, written as WGSL shaders on wgpu so they run on Metal,
//! Vulkan, DX12 and WebGPU. Every kernel has a test that compares it with its CPU reference in `florastack-core` (the
//! conversions and basic operations), `florastack-stack` or `florastack-align`, and the engine falls back to the CPU
//! when no adapter passes a self-test. It may depend only on `florastack-core`.
