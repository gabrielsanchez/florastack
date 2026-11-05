# Adding a stacking method

A stacking method touches every layer: the algorithm in `florastack-stack`, its settings in projects and presets, a
command in the engine, the quality gate, optionally a GPU kernel, and the front ends and documentation. This chapter
walks through them in order, for a hypothetical method called `sharpest` (each pixel from the single sharpest frame,
unsmoothed). Follow the weighted average (`average.rs`), the simplest existing method, as the model at each step.

## 0. Before the code

Work from a published paper, and cite it in the doc comment of the code that implements it. Read code only from
permissively licensed projects (focus-stack, PhotoCraft, LightCraft), never from the GPL and LGPL ones AGENTS.md
names, not even to see how something is done. Decide how the method will be scored: on which synthetic stacks it
should beat or match which existing method, and on what measure (SSIM, halos, noise, color shift).

## 1. The algorithm, in florastack-stack

Every method streams: it sees one aligned frame at a time, so that memory depends on the canvas, never the stack's
depth.

- **Settings.** `SharpestSettings` in `crates/stack/src/sharpest.rs`: `Serialize`, `Deserialize` and `JsonSchema`, a
  doc comment on every field, `#[schemars(range(..))]` mirroring `validate()`, and a `Default`. Add it to
  `StackSettings` (`settings.rs`), whose `validate` checks every method's. A project or preset without the new table
  must still load: the field takes its default when it is missing.
- **The kernel.** Implement `TileKernel` (`tiled.rs`): the per-tile state (`state`), how a frame's patch folds into it
  (`fold`), what it covered (`covered`) and the tile's result (`result`), with `context`, how far its filters reach,
  so that tiles overlap enough for the result to be the one the whole canvas would give. `Tiled` turns the kernel into
  a `Stacker` (`begin`, `add_frame`, `finish`) that keeps the state tile by tile in a `TileCache` within the memory
  budget, spilling beyond it.
- **The method.** Implement `Method` (`method.rs`) for the settings: its `name` (as outputs record it), `validate`,
  `stacker` and `reduced`, which scales every length in pixels for the quarter-size preview.
- **The rules.** A tile's fold reads only that tile; every per-pixel sum runs over the frames in their order, so the
  result is the same bit for bit whatever the number of threads; each frame's weights are multiplied by its coverage,
  so that it adds nothing where it has no pixels; `fold` and `result` report progress to the `Monitor` and stop when it
  is cancelled. No `unwrap` or indexing on anything a frame or setting decides without a check.

Export the new types from `lib.rs`, and describe the method in the crate documentation there, with its paper.

## 2. Its tests and benchmarks

In `crates/stack/tests/sharpest.rs`, following `average.rs` and `pyramid.rs`:

- **Quality** on the layered synthetic stacks (`florastack_core::testutil::generate_layered`, whose layers occlude one
  another as real subjects do), scored against `GroundTruth::sharp` with `florastack-metrics`, against the existing
  methods, with the targets written down and the measured values in a comment.
- **Equivalence**: the same result bit for bit with one tile or many, one thread or many, in memory or spilled.
- **Coverage**: in wide framing, no smear past a frame's edge and alpha 0 where no frame covers.
- **Progress and cancellation**, **hostile settings** (NaN, out of range) and tiny canvases (one pixel, one row).
- **The preview**: the quarter-size stack against the full stack reduced.
- **Memory**: peak heap flat from 20 to 200 frames (`tests/memory.rs`).

Add it to the criterion benches in `crates/stack/benches/` (`stack.rs` for time, `quality.rs` for the quality
records), which `cargo xtask bench` runs.

## 3. The engine command

In `crates/engine/src/commands/stack.rs`:

- the method's name for `stack.preview` and the slab stages (`MethodName`) and its settings (`Chosen`);
- a job command, `stack.sharpest`, made like `stack.average`: a `Meta` with its title, summary, help, examples and
  `undoable()`, on the heavy lane, stacking the stored alignment into a new output;
- the device: the engine's GPU path (`gpu_pass`) maps each method to its kernels, so until the new method has one,
  have the engine stack it on the CPU whatever the compute preference.

`stack.get`, `stack.set` and presets take the new settings through `StackSettings` (a `[stack.sharpest]` table). Then
the command's tests ([The command registry](commands.md#tests)): a dispatch in `dispatch.rs`, an undo case, hostile
parameters, its name in `commands.txt`, and the reference page written again
(`cargo xtask book --update-reference`).

## 4. The quality gate

Add the method to the runs of the quality runner (`crates/engine/examples/quality/main.rs`, which lists every method
and pair of slab and final methods), and run `cargo xtask quality`: its report puts 100% crops of every method side by
side, on the synthetic stacks and on the real ones you have locally. Then `cargo xtask quality --update-baseline`, and
commit `quality/baseline.json` with the change, so that its diff shows the new runs. [Tests, the corpus and the quality
gate](quality.md) explains both.

## 5. The GPU kernel (optional, later)

Once the CPU version is settled, it can get a GPU version: WGSL kernels in `crates/gpu/src/shaders/`, a `Method`
variant in `crates/gpu/src/stack/mod.rs`, the engine's GPU path for it, and an agreement test in
`crates/gpu/tests/stackers.rs` against the CPU stacker, with its tolerance argued.
[GPU kernels and the CPU oracle](cpu-oracle.md) sets the rules.

## 6. The front ends

- **The desktop app**: the method's choice and settings in the Stack tab (`apps/florastack/src/stack_tab.rs`), their
  labels and tooltips in `locales/en-US/florastack.ftl`, and how an output made with it describes itself in the outputs
  list. Each setting's **?** opens its section of [The Stack tab](../user/stack-tab.md): add the sections, whose
  anchors a test checks.
- **The command-line tool**: `--method sharpest` for `stack` (`apps/florastack-cli/src/args.rs`), and for `batch` and
  `watch` (the method names of `crates/engine/src/commands/batch.rs`).
- **Python**: the method's name in `Project.stack` (`python/florastack/python/florastack/__init__.py`).

## 7. The documentation

[Choosing a method](../user/methods.md) (what it is for, against the others; new crops with
`scripts/method_crops.py`), [The Stack tab](../user/stack-tab.md) (its settings), the
[command-line reference](../reference/cli.md), [Troubleshooting](../user/troubleshooting.md) if it has failure modes of
its own, and the roadmap. `cargo xtask book` checks that the links resolve.
