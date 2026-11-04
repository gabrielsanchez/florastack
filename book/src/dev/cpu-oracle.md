# GPU kernels and the CPU oracle

`florastack-gpu` runs the hot paths on the graphics card through [wgpu](https://wgpu.rs): Vulkan, Metal or DirectX 12,
one WGSL kernel a file in `crates/gpu/src/shaders/`. A GPU computes fast, but drivers differ and floating point
rounds differently from one device to the next, so every kernel has a CPU *oracle*: a reference implementation that
defines what is correct, and a test that runs both and checks that they agree.

## The rule

- **The CPU version comes first.** It lives in `florastack-core` (conversions and basic operations), `florastack-stack`
  or `florastack-align`. It defines the result, and it is the fallback wherever no GPU is usable.
- **A kernel lands with its CPU reference and its agreement test,** never before them.
- **The agreement tests live in `florastack-gpu`** (`crates/gpu/tests/kernels.rs` for the operations,
  `stackers.rs` for whole stacking methods), which may use `florastack-align` and `florastack-stack` as
  dev-dependencies for this alone.
- **Results agree within a documented tolerance,** not bit for bit: next to each assertion, a comment says what the
  tolerance is and why it is enough.

An assertion from the blur's test shows the style:

```rust,ignore
let mut want = image.clone();
gaussian_blur_in_place(&mut want, sigma, method).unwrap();
let got = ops::gaussian_blur(gpu, &image, sigma, method).unwrap();
// The exact kernel sums the same taps in the same order in f32: up to 2 x 25 roundings of samples below 2
// (1.2e-7 each) apart, under 1e-5. The box passes run in f64 on the CPU and in f32 here: each of 8 passes sums at
// most 2 x 13 + 3 samples below 2, about 3.5e-6 of rounding a pass; 5e-5 bounds them with room.
let tolerance = if sigma <= 4.0 && matches!(method, BlurMethod::Auto) { 1e-5 } else { 5e-5 };
assert!(max_difference(&got, &want) <= tolerance);
```

The inputs are seeded random images of awkward sizes (odd, one pixel wide or tall), every channel count, values beyond
`[0, 1]` and partial coverage, plus the synthetic stacks for the stacking methods.

## Without a GPU

An agreement test must never pass silently on a machine without an adapter. The tests get their device from
`common::gpu()`, which calls `florastack_testkit::skip!(Gpu, "...")` and returns `None` when there is none, so that
`cargo xtask ci` lists the test as skipped. With `FLORASTACK_REQUIRE_GPU` set, a missing adapter fails them instead.

The tests take any adapter, software ones included (`GpuOptions::any`), and `WGPU_ADAPTER_NAME` picks one by name.
CI runs them on Mesa's lavapipe on Linux and WARP on Windows with `FLORASTACK_REQUIRE_GPU` set, and on Metal on macOS
where the runner has an adapter. To run them on a machine without a GPU, install a software adapter (on Debian or
Ubuntu, `sudo apt-get install mesa-vulkan-drivers`; Windows has WARP) and select it:

```sh
WGPU_ADAPTER_NAME=llvmpipe cargo test -p florastack-gpu
```

A device that computes wrong results always fails them.

## At run time

The engine doesn't trust a GPU because it has one. On first use, `compute` probes the best adapter, compiles every
kernel and runs `Gpu::self_test`, which compares the kernels' results on small inputs with the CPU's; only an adapter
that passes stacks, and with the `auto` preference a software adapter is never taken. A GPU that fails during a stack
makes the stack start again on the CPU. The engine's and the quality runner's tests stack on the CPU
(`Compute::CPU`) unless a test is about the GPU, so that their results and memory are the same on every machine.

## Writing a kernel

1. Write or find the CPU version, with its tests, in the crate that owns it.
2. Add the WGSL file in `crates/gpu/src/shaders/` and its dispatch in `ops.rs`, `levels.rs` or `stack/`. Memory on the
   GPU is budgeted like the CPU's: a stack's accumulators are tiled within a VRAM budget and spill to the host beyond
   it (`GpuTiling`).
3. Add the agreement test, with its tolerance argued in a comment, and run it on a real GPU and a software adapter.
4. Time it against the CPU: `cargo bench -p florastack-gpu --bench compositors` times the stacking methods on both
   devices (100 frames of 24 MP; `--quick` takes 8).
