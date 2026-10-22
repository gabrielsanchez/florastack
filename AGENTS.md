# AGENTS.md

This guide is for everyone who changes FloraStack: human contributors and AI coding agents alike. FloraStack is a free, open-source focus-stacking application written in pure Rust (see the [README](README.md)). Read this file before your first change. [`docs/ROADMAP.md`](docs/ROADMAP.md) says what is done and what comes next. Claude Code reads this file through [`CLAUDE.md`](CLAUDE.md).

## Build and check

The toolchain is pinned in `rust-toolchain.toml`, and the README lists the build commands. Before you push, run:

```sh
cargo xtask ci
```

It runs everything CI runs (`.github/workflows/ci.yml`), in the same order: `cargo fmt --check`, Clippy with warnings as errors, `cargo xtask check-layers`, `cargo xtask corpus --fixtures` (which downloads the test files, about 110 MB, nearly all of it camera RAW samples, unless they are already there), the tests, `cargo deny check` and `cargo audit`. It stops at the first failure and ends with a summary that lists the tests skipped for want of a GPU or corpus files. It needs cargo-deny and cargo-audit: `cargo install --locked cargo-deny cargo-audit`.

The dev profile, which the tests use, optimizes `florastack-core` and `florastack-align` (`[profile.dev.package]` in the root `Cargo.toml`): unoptimized, their pixel loops would make the tests on synthetic stacks 10 to 50 times slower. Debug assertions and overflow checks stay on.

## Crate map

| Crate | Path | Responsibility | May depend on |
|---|---|---|---|
| `florastack-core` | `crates/core` | Image buffers (`Image<T>`, `AnyImage`), pixel formats and their conversions (sample types, sRGB and linear), geometry (`Affine`, `Rect`), the tiled view (`TileGrid`), the tile cache (`TileCache` over a `TileStore`, temporary files by default, with `TempFileStore::remove_abandoned` for the tile folders of crashed runs), lock files (`LockFile`), progress and cancellation for long operations (`Monitor`, `Cancelled`), the basic operations (blur, resize, Sobel, Laplacian, warps), EXIF orientation (`Orientation`, `Image::oriented`), error types and the product identity (`branding`, including the XMP namespace); behind its test-only `testutil` feature, the synthetic stack generator | no FloraStack crate |
| `florastack-codecs` | `crates/codecs` | Image files: reading JPEG, PNG, TIFF, OpenEXR, WebP and BMP, and writing JPEG, PNG, TIFF, OpenEXR and linear DNG, with their EXIF, IPTC and XMP metadata, ICC profiles and orientation; built on LightCraft's tiff, codecs, meta and color crates, and raw for the DNG writer's lossless JPEG (`docs/lightcraft.md`), none of whose types appear in its API | core |
| `florastack-raw` | `crates/raw` | RAW decoding behind its `RawDecoder` trait (LightCraft's decoders: DNG, CR2, CR3, NEF, ARW, RAF, RW2, PEF, uncompressed ORF and SRW, and a few more) into plain `RawFrame` data, and the develop step to linear Rec. 2020 RGB with one `DevelopSettings` per stack, resolved once on a reference frame (`RawDevelop`); built on LightCraft's raw, pipeline, develop, raster, codecs and color crates (`docs/lightcraft.md`), none of whose types appear in its API | core |
| `florastack-align` | `crates/align` | Frame alignment: each frame's shift, scale, rotation and brightness gain relative to a master frame (`StackAligner`, `align_frames`; `AlignSettings`: the terms to correct, their limits, the minimum score and rejection), by ECC coarse to fine on band-passed luminance after a phase-correlation first guess, each frame chained to its neighbor, with flags for limits, low scores and repeating patterns; later stack ordering and framing, dust and movement handling; CPU reference for GPU kernels | core |
| `florastack-stack` | `crates/stack` | Stacking methods (weighted average, pyramid, depth map); CPU reference for GPU kernels | core |
| `florastack-gpu` | `crates/gpu` | wgpu compute (WGSL) versions of the hot paths | core, plus align and stack as dev-dependencies for the agreement tests |
| `florastack-project` | `crates/project` | The project model (`Project`: frames, alignment results, outputs with their retouch layers, preferences), `<name>.florastack/` folders (`ProjectFolder`: the versioned `project.json` with its migrations, the `cache/` and `previews/` files by `CacheRef` with their SHA-256 hashes, the folder's lock), changes as values with their inverses for undo (`Edit`, `Project::apply`), the cache's files from any thread (`CacheDir`), layered TOML presets (`Preset`, `Preferences`) and orphan cleanup (`Orphans`, moving old temporary projects to a `Trash`, the system's through the `trash` crate on native targets). What other crates define (develop settings, EXIF summaries, stacking parameters, retouch strokes) it stores as JSON data that the engine converts | core |
| `florastack-retouch` | `crates/retouch` | Retouching brushes and stroke layers | core, project |
| `florastack-engine` | `crates/engine` | The command registry (`Registry`, `CommandSpec`: named commands with schemars JSON schemas, help texts and an undoable flag; `commands`, FloraStack's own), the engine and its session (`Engine`, `Session`: the open project, changed only through commands, and its undo history), the job engine (`jobs`: lanes, progress with ETAs, cancellation at checkpoints) and orchestration, such as reading a stack's frames (`frames`: camera RAW and DNG files to florastack-raw, 16-bit TIFF and other image files to florastack-codecs) and the project with the other crates' types (`project`: develop settings and their resolution, EXIF summaries, and the cache's lossless TIFF images and JPEG previews through florastack-codecs); re-exports what the apps need | every library crate above |
| `florastack-metrics` | `crates/metrics` | Image-quality metrics for tests and benchmarks: PSNR, SSIM, halos, flat-region noise and color shift (CIEDE2000) | core; other crates use it as a dev-dependency only |
| `florastack-testkit` | `crates/testkit` | Test tooling: `skip!`, which records a test that can't run here | nothing; other crates use it as a dev-dependency only |
| `florastack` | `apps/florastack` | The egui desktop app | engine only |
| `florastack-cli` | `apps/florastack-cli` | The command-line tool | engine only |
| `xtask` | `xtask` | Developer tooling: `cargo xtask check-layers`, `ci`, `corpus` and `bench` | no FloraStack crate |

### Layering rules

`cargo xtask check-layers` enforces these rules, in CI and in `cargo xtask ci`. The allowlist is `RULES` in `xtask/src/layers.rs`.

- A crate depends only on the workspace crates in its row, in every dependency table, plus the two test-only crates below as dev-dependencies. `florastack-core` depends on none, so that every crate can depend on it.
- The apps depend on `florastack-engine` alone. When an app needs a type from another crate, the engine re-exports it.
- `florastack-metrics` and `florastack-testkit` appear only in `[dev-dependencies]`, and so does `florastack-core`'s `testutil` feature (which no `default` feature may include), so none of their code ships.
- Only `apps/florastack` may use a UI toolkit (`egui*`, `eframe`, `winit`, `rfd`). The engine and the libraries stay headless.
- Every crate inherits the workspace lints with `[lints] workspace = true`, and the binaries named in `branding.toml` must exist.
- A new crate gets its `RULES` entry and a row in this table in the change that adds it. Don't widen an allowlist to make a build pass: move the code to the right crate instead.
- External dependency versions live in `[workspace.dependencies]` in the root `Cargo.toml`, and crates use them with `name.workspace = true`.

## Test data and benchmarks

Alignment and stacking are scored, not eyeballed, against three kinds of test data:

- **Synthetic stacks** with exact ground truth: `florastack_core::testutil` (enable the `testutil` feature in `[dev-dependencies]`). `generate` turns a sharp image and a depth map, such as `scene::textured`'s, into frames with depth-dependent blur, focus breathing, sub-pixel shift, rotation, brightness jitter, noise and dust; `StackParams::identity`, `clean` and `noisy_dusty` are the standard sets. Score alignment with `GroundTruth::relative` and `transform_error`, and stacking against `GroundTruth::sharp` with `florastack-metrics`. Defocus moves the synthetic content as well as blurring it (a receding surface's blur changes unevenly with focus, and the generator blurs each side of a depth edge with its own blur), by 0.06 to 0.12 px between frames that differ only in focus on the test scenes, and frames gain or lose up to 0.8% of their light near bright depth edges: `crates/align/tests/accuracy.rs` explains how its targets allow for it.
- **Real stacks** in the gitignored `corpus/` folder: `cargo xtask corpus` downloads the ones `corpus/manifest.toml` pins (sha256-verified) and checks the local ones; `corpus/README.md` says how to add either kind. A test that needs one calls `skip!(Corpus, ...)` when it is missing. Never commit corpus files.
- **Format fixtures**: files written by other software (PngSuite, and TIFF, WebP and OpenEXR test files) for the codec tests, and one camera RAW file per format florastack-raw decodes (CC0, from raw.pixls.us) for the RAW tests. The same manifest pins them, `cargo xtask corpus --fixtures` downloads them into `corpus/fixtures/<set>/` (CI does so before the tests), and tests find them with `florastack_testkit::fixture` and `fixtures`, calling `skip!(Corpus, ...)` when a set is missing.
- **Benchmarks**: criterion benches in each crate's `benches/` (each file starts with the `#![allow(...)]` line of the never-crash rules). `cargo xtask bench` runs them and writes `target/bench-summary.json`, which later changes compare against. CI compiles the benches but doesn't run them.

## Everything is a command

Every user-facing action is a named command in `florastack-engine`'s registry (such as `align.run` or `stack.pyramid`) with a serde params struct, a JSON schema, help text and an undoable flag. That covers buttons, menu items, shortcuts, CLI subcommands, and calls from the control channel and the MCP server. The registry is the only way project state changes.

- The desktop app and the CLI parse input, dispatch commands, and show state and progress. No logic lives only in the UI or the CLI: when a screen needs a computation, add it to the engine as a command (or a read-only query) with tests, then call it.
- Only pure view state, such as zoom, pan, panel layout and the open tab, lives in the UI alone.
- Long commands (aligning, stacking) run as cancellable jobs that report progress.
- A new command comes with a test that dispatches it from JSON.

### Adding a command

Commands live in `crates/engine/src/commands/<area>.rs`; `crates/engine/src/registry.rs` documents the details.

- **Name** it `area.verbObject` (`project.addFrames`, `develop.set`): lower-case area, lower camel-case action, at most 64 characters. Names are an interface for scripts and MCP clients: add it to `crates/engine/tests/commands.txt` in the same change, and keep a renamed command's old name as an alias (`commands::ALIASES`).
- **Make it** with `CommandSpec::query` (reads), `CommandSpec::instant` (runs on the engine's thread) or `CommandSpec::job` (long work), and a `Meta`: a title, a one-sentence summary, help text, examples, and flags (`undoable()`, what it requires and what it changes).
- **Parameters** are a struct with `#[serde(deny_unknown_fields, rename_all = "camelCase")]`, `JsonSchema`, a doc comment on every field (it becomes the schema's description) and `Params::check` for what types can't say: ranges, lengths, paths without NUL. Results are a struct too. Types of other crates that commands take derive `JsonSchema` in their own crate, next to their serde derives, with `#[schemars(range(..))]` mirroring their `validate`.
- **Change the project** only through `Session::apply` with an `Edit` (florastack-project): a command's edits are one undo step, undone by their inverse. A new kind of change is a new `Edit` variant whose inverse a test checks.
- **A job** plans on the engine's thread from a snapshot of what it reads, and locks those aspects of the project (`JobPlan::locks`); its work runs on a worker, calls `Monitor::step` after each unit (a frame, a tile) and stops with `Cancelled` there; it never touches the session, and returns a commit that the engine runs. Something irreversible outside the project, such as writing the user's file, comes after `JobContext::finalize`.
- **Test it**: dispatch it from JSON in `crates/engine/tests/dispatch.rs` (a test fails when a registered command is never dispatched there), add an undoable one to the cases of `tests/undo.rs`, and its hostile parameters to `tests/hostile.rs`. `tests/registry.rs` checks every command's help, schemas and examples.

## CPU oracle

Every GPU kernel in `florastack-gpu` has a CPU reference implementation in `florastack-core` (conversions and the basic operations), `florastack-stack` or `florastack-align`, and a test that runs both on random and synthetic inputs and asserts that they agree within a documented tolerance. Floating-point results need not be bitwise equal: write the tolerance, and why it is enough, next to the assertion.

- Write the CPU version first. It defines what is correct, and it is the fallback when no GPU adapter is usable.
- A kernel lands with its CPU reference and its agreement test, never before them.
- The agreement tests live in `florastack-gpu`, which may use `florastack-align` and `florastack-stack` as dev-dependencies for this purpose only.
- Without a usable adapter, an agreement test must not pass silently: it calls `florastack_testkit::skip!(Gpu, "...")` and returns, so that `cargo xtask ci` lists it as skipped.

## Clean-room

FloraStack's code is either original or taken from sources whose licenses allow it, and the record of what came from where is kept in `ATTRIBUTION.md`.

- **Proprietary stackers** (Helicon Focus, Zerene Stacker): never use their code, shaders or assets, and don't decompile, disassemble or otherwise reverse-engineer them. Learning from their public documentation and comparing their output with ours is fine; don't commit their output to the repository.
- **GPL and LGPL image projects**: do not read or copy code from enfuse/enblend, CombineZP, darktable, RawTherapee, LibRaw, dcraw or rawler/rawloader, not even to see how something is done. Work from the papers they build on instead.
- **Allowed sources**: published papers, cited in the doc comment of the code that implements them, and permissively licensed code such as focus-stack (MIT), PhotoCraft and LightCraft (MIT OR Apache-2.0). Code adapted from such a project gets a row in `ATTRIBUTION.md`, with its copyright line in `NOTICE`, in the same commit, and a comment at the adapted code that names its source.
- **Dependencies** must be permissively licensed. `cargo deny check` enforces the policy in `deny.toml`: MIT, Apache-2.0, the BSD licenses, ISC, Zlib, 0BSD, BSL-1.0, CC0-1.0, Unicode-3.0 and MPL-2.0 are allowed, and GPL, LGPL and AGPL are denied. The licenses of egui's bundled fonts (OFL-1.1 and the Ubuntu Font Licence) are allowed for that font crate alone, and the Independent JPEG Group's license for `jpeg-encoder` alone (its credit line is in `NOTICE`). Crates come from crates.io, except LightCraft's, which come from its git repository pinned to one commit (`docs/lightcraft.md`).
- The camera control planned for Phase 6 talks to libgphoto2 (LGPL-2.1) as a separately installed shared library loaded at run time, never as a crate, so that users can replace it.

## Never crash

A malformed file, a hostile parameter or a full disk must produce an error the user can act on, never a panic: a crash loses work.

- **No `unwrap()`, `expect()` or `panic!` outside tests**, and no `unreachable!`, `todo!` or `unimplemented!` either. Return the crate's error type and propagate it with `?`. Use `ok_or`, `let ... else`, `if let`, or an `unwrap_or*` fallback where the fallback is truly correct. Clippy enforces this: the workspace lints reject these calls, and `clippy.toml` allows `unwrap`, `expect` and `panic!` in `#[test]` functions and `#[cfg(test)]` code. That leaves out the helper functions of integration tests (`tests/`), and examples and benches altogether, so such a file that needs them starts with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.
- **Checked indexing on anything derived from file input**: sizes, offsets, counts, tag values, frame indices and everything computed from them. Use `get()` instead of `[i]` and `[a..b]`, `checked_*` or `saturating_*` arithmetic, guard divisions and float-to-integer casts, and validate image dimensions before allocating, so that a hostile file can trigger neither a panic nor a huge allocation. Parameters from the CLI, the control channel and MCP get the same care.
- **Every crash fix comes with a regression test** that crashed before the fix, on a small synthetic input.
- **`unsafe` only in the modules listed here.** The workspace denies `unsafe_code`. A listed module opts in with `#![allow(unsafe_code)]`, and each `unsafe` block in it has a `// SAFETY:` comment that explains why it is sound (Clippy's `undocumented_unsafe_blocks` checks that the comment is there). Adding a module to the list is a reviewed change of its own.

  Modules where `unsafe` is allowed: none yet.

## Conventions

- **Errors**: library crates define their error types with `thiserror`, as enums whose messages say what failed and keep the underlying error as their source. No `String` or `Box<dyn Error>` errors in library APIs; the binaries (the apps and xtask) may turn errors into messages at the top.
- **Logging**: `tracing` (`debug!`, `info!`, `warn!`, `error!`), never `println!` or `eprintln!`, in library crates. The apps decide where log output goes. Their terminal output, and xtask's, is a user interface, not logging.
- **CPU parallelism**: `rayon`, with parallel iterators over rows, tiles and frames, rather than hand-made threads. Results must not depend on thread scheduling: stacking is deterministic for a given input order and settings, so reductions combine in a fixed order.
- Add `thiserror`, `tracing` and `rayon` to `[workspace.dependencies]` when the first crate needs them, like every external crate.
- **Style**: `rustfmt.toml` (lines up to 150 columns) and Clippy pedantic, with the few exceptions documented in the root `Cargo.toml`. Allow a lint locally, with a comment that says why, rather than contorting the code.
- **Product identity**: names come from `branding.toml` through `florastack_core::branding`, which the engine re-exports. Never hard-code the display name, binary names, project-folder extension or app ID.
- **Tests that can't run here**, for want of a GPU adapter or a corpus file, call `florastack_testkit::skip!` and return instead of passing silently, so that `cargo xtask ci` lists them.
- **Pure Rust**: prefer pure-Rust crates. A dependency that needs a C or C++ library (a `-sys` crate, a `cc` or `cmake` build step, or a system library) is a design decision to discuss before adding it. `unsafe` code inside third-party crates is accepted (codecs and SIMD routines use it); FloraStack's own `unsafe` follows the rules in Never crash above.
- **LightCraft**: FloraStack uses LightCraft's low-level crates (`lightcraft-tiff`, `-codecs`, `-meta`, `-color` and `-raw`, and for the RAW develop step `-pipeline`, `-develop` and `-raster`, never its engine or UI) from its git repository, all pinned to one commit in `[workspace.dependencies]`. Their types stay inside `florastack-codecs` and `florastack-raw`, behind FloraStack's own types and traits. `docs/lightcraft.md` says how to move the pin.

## Per-change checklist

Before you commit:

1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. `cargo xtask check-layers`
5. Update `docs/ROADMAP.md`: tick the items the change finishes, once their acceptance checks pass, and add the follow-ups it leaves.

`cargo xtask ci` runs steps 1 to 4 the way CI does (formatting in `--check` mode, Clippy and the tests with `--locked`) plus `cargo deny check` and `cargo audit`. Run it before you push. In the same commit, when they apply, also update:

- `ATTRIBUTION.md` and `NOTICE`, for code, data or assets taken from elsewhere;
- this file's crate map and `RULES` in `xtask/src/layers.rs`, for a new crate or a change to which crate may depend on which;
- `deny.toml`, for a new git source or a justified exception, and `.cargo/audit.toml` with it for an ignored advisory;
- the README, when building or running FloraStack changes.

## CI

`.github/workflows/ci.yml` runs the `check` job (formatting, Clippy, check-layers, fetching the format fixtures and the tests) on `ubuntu-latest`, `macos-latest` (arm64) and `windows-latest`, and the `dependencies` job (`cargo deny check` and `cargo audit`) on Linux. `cargo xtask ci` runs the same commands, and a test in `xtask/src/ci.rs` fails when the two lists differ, so change them together. Third-party actions are pinned to a full commit SHA, and the toolchain always comes from `rust-toolchain.toml`.
