# FloraStack roadmap

One checkbox per deliverable, grouped by phase and by the build step that delivers it. Tick an item in the change that finishes it, once that step's acceptance checks pass, and add follow-ups as new items where they belong (see the per-change checklist in [AGENTS.md](../AGENTS.md)). Steps marked *(plan first)* start with a reviewed plan.

## Phase 0 — Foundations

### 0.1 Workspace scaffold *(plan first)*

- [x] Rust workspace (edition 2024) with the toolchain pinned in `rust-toolchain.toml` (1.99.1; at least 1.95 for the LightCraft crates)
- [x] Library crates core, codecs, raw, align, stack, gpu, project, retouch, engine and metrics, each documenting its responsibility and allowed dependencies
- [x] `apps/florastack` (desktop) and `apps/florastack-cli` as stubs over `florastack-engine`, plus `xtask`
- [x] `cargo xtask check-layers`: the dependency allowlist, florastack-metrics as a dev-dependency only, UI toolkits only in the desktop app, workspace lints in every crate, and the binaries named in `branding.toml`
- [x] Product identity in `branding.toml`, compiled into `florastack_core::branding`
- [x] `rustfmt.toml`, Clippy pedantic workspace-wide with documented exceptions, and the `cargo xtask` alias
- [x] LICENSE-MIT, LICENSE-APACHE, NOTICE, ATTRIBUTION.md and a README with build instructions
- [x] App ID owner filled in: `io.github.gabrielsanchez.FloraStack`

### 0.2 AGENTS.md, CLAUDE.md, roadmap and CI

- [x] AGENTS.md (crate map and layering, commands, CPU oracle, clean-room, never crash, conventions, per-change checklist) and a CLAUDE.md that imports it
- [x] This roadmap
- [x] Never-crash lints: no `unwrap`, `expect` or `panic!` outside tests, and a `// SAFETY:` comment on every `unsafe` block
- [x] GitHub Actions CI: fmt, Clippy, check-layers and the tests on Ubuntu, macOS (arm64) and Windows, with cargo caching
- [x] Dependency policy job: `cargo deny check` with the license policy in `deny.toml`, and `cargo audit`
- [x] `cargo xtask ci`: runs what CI runs and prints a summary that lists the tests skipped for want of hardware or corpus files (recorded with `florastack-testkit`'s `skip!`)
- [x] First CI run on GitHub green on all three operating systems (PR #1, 2026-10-20)

### 0.3 Test corpus, golden images and benchmarks

- [x] `cargo xtask corpus`: pinned, sha256-verified sample stacks from `corpus/manifest.toml` into the gitignored `corpus/`, with a README on adding stacks
- [x] Local stacks in `corpus/local/<name>/`, each with a `stack.toml` (subject type, frame order, optional ground-truth notes)
- [x] Synthetic stack generator in florastack-core's test utilities: depth-dependent blur, focus breathing, sub-pixel shift, rotation, brightness jitter, sensor noise and dust, with the sharp image and true transforms as ground truth
- [x] florastack-metrics: PSNR, SSIM, halo detector, flat-region noise estimate and color shift (mean deltaE2000)
- [x] `cargo xtask bench`: criterion benchmarks with a JSON summary in `target/bench-summary.json`
- [x] Tests: the generator with zero blur and no transform returns its input
- [x] `Image<T>` and the `Affine` transform in florastack-core ahead of 1.1, because the generator and the metrics need them (see 1.1)
- [x] check-layers: florastack-core's `testutil` feature only from `[dev-dependencies]`, never by default

## Phase 1 — Core I/O and model

### 1.1 Image buffers and tiling

- [x] `Image<T>` over u8, u16 and f32 with 1 to 4 channels, a row stride and a color-space tag (linear or sRGB-encoded, optional ICC profile), done in 0.3; the rest of 1.1 builds on it
- [x] Conversions between sample types and between encoded and linear, vectorized and parallel with rayon; stacking math in linear f32 (`florastack_core::convert`: exact round trips for every `u8` and `u16` value, sRGB through tables or a branch-free power function)
- [x] Tiled view: overlapping tiles stitched back with feathered seams (`TileGrid`, with `stitch_with` holding two rows of tiles at a time)
- [x] Disk-backed tile cache: `TileCache` with LRU eviction and a RAM budget over a `TileStore` (`TempFileStore`: temp files, plain file I/O), so 500+ frames at 60 MP never need to be resident
- [x] Basic ops: Lanczos3 resize, separable Gaussian blur with a running-sum approximation for large radii, Sobel, Laplacian, and bilinear and bicubic affine warps (`florastack_core::ops`)
- [x] Property tests (proptest) for conversions and tile round trips; criterion benches for blur and warp on a 24 MP image
- [x] The synthetic generator's private exact blur and bicubic warp (`crates/core/src/testutil/ops.rs`) replaced in the generator by the public ops, which match them bit for bit, and kept as the tests' oracle

### 1.2 LightCraft dependency, codecs, metadata and color *(plan first)*

- [x] LightCraft's low-level crates (tiff, codecs, meta, color, raw) as a pinned dependency, kept out of public APIs behind FloraStack's own traits; ATTRIBUTION.md and `docs/lightcraft.md` on bumping the pin
- [x] Read JPEG, PNG 8/16, TIFF 8/16/32f (uncompressed, LZW and Deflate, including compressed 16-bit), OpenEXR, WebP and BMP
- [x] Write JPEG with a quality setting, PNG 8/16, TIFF 8/16/32f with compression options, OpenEXR, and linear 16-bit DNG
- [x] EXIF, IPTC and XMP read and carried over from a chosen source frame, with the software tag and the frame count in XMP
- [x] Embedded ICC profiles kept (sRGB assumed without one), with output in the input profile or a wide working space (ProPhoto RGB by default, or Rec. 2020)
- [x] EXIF orientation honored or ignored by option (or forced), with `Orientation` and `Image::oriented` in florastack-core
- [x] Tests: round trip per format and bit depth, metadata, orientation; PngSuite, TIFF, WebP and OpenEXR fixtures fetched by `cargo xtask corpus`, and by CI before the tests
- [x] Images whose ICC profile has another curve than sRGB's are linearized through the profile (`florastack_core::convert` knows linear and sRGB only, and carries the profile over unchanged)
- [x] `cargo xtask corpus`: archive entries (PngSuite ships as one `.tgz`) and a place for format fixtures beside the stacks
- [ ] Open a linear DNG written by FloraStack in Lightroom Classic or Adobe Camera Raw, and check it with the DNG SDK's `dng_validate`: the tests read it back with LightCraft's RAW decoder only, and the DNG specification couldn't be downloaded while the writer was built
- [ ] Move the LightCraft pin to a commit with quick-xml 0.41 or later (storytold/lightcraft#1464) and drop the two advisory ignores in `deny.toml` and `.cargo/audit.toml`
- [ ] 12-bit TIFF: the tiff crate (0.11.3) rejects 12-bit samples with an "unsupported" error; read them once it supports them (the scaling to 16 bits is in place and tested)
- [ ] Streaming TIFF and DNG writers that take the image a band of rows at a time, so that large outputs are never whole in memory
- [ ] Float DNG and a preview IFD in the DNG writer
- [ ] Revisit the `exr = "=1.74.0"` pin once exr no longer pulls in `paste` (RUSTSEC-2024-0436) through `pulp`: 1.74.0 can't read DWA-compressed files, which fail with an "unsupported" error that suggests ZIP or PIZ

### 1.3 RAW import through LightCraft

- [x] `RawDecoder` trait in florastack-raw, implemented with LightCraft's raw crate (`LightCraftRaw`), decoding into plain `RawFrame` data (normalized samples, the color filter array, the DNG color model) that another decoder could produce
- [x] LightCraft's coverage (DNG, CR2, CR3, ARW, NEF, RAF Bayer and X-Trans, RW2/RWL, PEF, uncompressed ORF and SRW), with a clear error suggesting DNG or 16-bit TIFF for anything else (`RawError::Unsupported`)
- [x] `RawDevelop` to linear, scene-referred f32 RGB (Rec. 2020): white balance (as shot, auto, temperature and tint), fast and high-quality demosaic (X-Trans included), highlight reconstruction, exposure, lateral chromatic aberration and light noise reduction, with no tone curve or look; LightCraft's develop code from its raw and pipeline crates
- [x] Lens distortion correction off by default (`DevelopSettings::lens_distortion`; the file's vignetting correction still applies)
- [x] One `DevelopSettings` per stack, resolved once on a reference frame (`RawDevelop::new`, `Resolved`) so that every frame gets the same white and corrections, with serde for the project file and partial presets; storing it in the project is 1.4's (below)
- [x] 16-bit TIFF and DNG imported unchanged: demosaiced (`LinearRaw`) DNG files by florastack-raw, TIFF and the other image files by florastack-codecs, the choice made in `florastack_engine::frames` (no layering change)
- [x] Tests: one file per format (twelve CC0 files from raw.pixls.us, the `raw` fixture set, cached in CI), identical settings give identical output, preview and final demosaic agree on smooth regions, output stays linear (a gray step wedge)
- [x] `lightcraft-raw`'s `jxl` feature turned on: JPEG XL compressed DNGs (DNG 1.7) through jxl-oxide, pure Rust
- [ ] Camera color: eight of the ten pinned non-DNG cameras (all but the Canon EOS R6 and the Fujifilm X-S10) have no color matrix in LightCraft's low-level crates (`ColorSource::Fallback`, camera RGB taken for sRGB, flagged `DevelopWarning::ApproximateColor`); LightCraft's per-file camera-look fits and bundled profiles live in its engine crate. Find a permissible source of matrices for the popular bodies, or ask LightCraft to move its fits into a low-level crate
- [ ] More samples: a lossless CRX CR3 (the pinned one is C-RAW), and RWL, NRW, ERF, MEF and 3FR or FFF files, which decode through paths the pinned files don't reach
- [ ] Reduced-size development straight from the mosaic, for thumbnails, the filmstrip (2.3) and quarter-scale previews (3.1): `Quality::Preview` demosaics the whole frame
- [ ] `RawDecoder::decode` checks the size from the headers before decoding, and LightCraft's header probe unpacks the samples of uncompressed ORF and PEF files to read them: those files are decoded twice
- [ ] A truncated RAW file, such as one a camera is still writing, is a `Malformed` error like a damaged one: the watch folder (4.2) must wait until a file's size settles
- [ ] `cargo check -p florastack-raw --target wasm32-unknown-unknown`, with LightCraft's pipeline, develop and raster crates (7.3)

### 1.4 Project model

- [x] `Project`: ordered frames (path, content hash, EXIF summary, enabled flag), develop settings, alignment results, outputs with their parameters, retouch layers and preferences (`florastack_project::model`; frames, outputs and layers have ids that are never reused)
- [x] `<name>.florastack/` folders: versioned `project.json` with migrations, `cache/` of lossless intermediates and `previews/` (`ProjectFolder`, `schema`; atomic saves, a lock against a second editor, cache files named after their SHA-256 and checked against it when read)
- [x] Reopened projects can be retouched at once, with no recomputation: outputs, maps and aligned frames are lossless TIFF files in `cache/` (written and read by `florastack_engine::project` through florastack-codecs), strokes are in `project.json`
- [x] Partial TOML presets that layer (`Preset`, `Preferences::layer`: tables merge key by key, other values replace)
- [x] Orphan cleanup: old unnamed temporary projects moved to the OS trash, never deleted (`Orphans`, behind the `Trash` trait; `SystemTrash` on native targets through the `trash` crate, tests use `FolderTrash`)
- [x] Tile folders that a crashed run left behind (`TempFileStore`, named after the GUI binary with `-tiles-`) removed at startup: each folder now holds a lock, and `TempFileStore::remove_abandoned` deletes those whose lock is free (spilled tiles only, so deleted rather than trashed)
- [x] Tests: save and load round trip, migration from a v0 fixture (`crates/project/tests/fixtures/v0/`), partial preset layering; plus hostile project files (with a proptest), orphans, locks and cache integrity
- [x] The stack's `DevelopSettings` and its `Resolved` in the project: kept as serialized data that the engine reads (`florastack_engine::project::develop`), like the EXIF summary, the stacking parameters and the retouch strokes, so `RULES` is unchanged; the engine's tests pin the JSON shape of each
- [ ] Call the startup cleanup: `TempFileStore::remove_abandoned` on every folder that holds tile stores, and `Orphans::find` on the temporary-projects folder, with the orphans offered to the user (5.1) and in the CLI's `project clean-orphans` (4.1)
- [ ] The temporary-projects folder and the orphan age (30 days by default, `Orphans::DEFAULT_DAYS`) as preferences (5.4), with the folder on a local drive: on Windows, the shell may delete instead of recycle on a drive without a Recycle Bin, such as a network share
- [ ] Check `SystemTrash` by hand on Windows, macOS and Linux (a temporary project lands in the Recycle Bin or Trash and can be restored): the tests never call it, so as not to touch the real trash of the machine they run on
- [ ] Save As: naming a temporary project and moving it out of the temporary-projects folder (5.1)
- [ ] Frames found again when a project moves to another machine (the stored paths are absolute; a relative path is resolved from the folder that holds the project folder)
- [ ] Retouch strokes: `project.json` is capped at 64 MiB, so long stroke lists may have to move into cache files (5.3)
- [ ] `cargo check -p florastack-project --target wasm32-unknown-unknown` in CI (it builds and passes Clippy locally), and a project-folder backend over the browser's origin-private storage (7.3)

### 1.5 Command registry and job engine *(plan first)*

- [ ] Command registry: named commands with serde params, a JSON schema (schemars), help text and an undoable flag; `list()` and `dispatch(name, json)`
- [ ] Engine re-exports of the core, project and retouch types the apps need
- [ ] Job engine: cancellable jobs on a worker pool with progress events (fraction, stage, ETA); one heavy job at a time, previews at lower priority
- [ ] Undo and redo through inverse operations, one retouch stroke per step
- [ ] Tests: every command has a schema and help text, dispatch from JSON, cancellation leaves the project consistent

### Before Phase 2

- [ ] 5–10 real stacks (a bristly insect, a flower, a coin, a handheld landscape) in the corpus: fill the placeholders of `corpus/manifest.toml` or add local stacks (`corpus/README.md`)

## Phase 2 — Alignment

### 2.1 Frame alignment *(plan first)*

- [ ] Per-frame shift, scale, rotation and brightness gain relative to a master frame, each term switchable
- [ ] Coarse-to-fine ECC on gradient-magnitude pyramids with a phase-correlation first guess; frames chained to their neighbors, optionally refined against the master
- [ ] Limits per term; alignments that hit a limit or score low are flagged, and so are false matches on repeating patterns
- [ ] Brightness gain from robust statistics of low-pass content
- [ ] Synthetic-stack accuracy tests: mean error at most 0.1 px on clean stacks and 0.3 px on noisy, dusty ones
- [ ] Bench timing for a 100-frame 24 MP stack

### 2.2 Ordering, framing and edge handling

- [ ] Narrow-end detection that sets the processing order, with automatic order and reverse order as overrides
- [ ] Framing: crop to the narrow end (default), keep the wide field of view with a coverage mask, or crop to what all frames cover
- [ ] Sort by scale to repair out-of-order stacks, as a command
- [ ] Out-of-focus frame detection
- [ ] Tests: a shuffled order is restored; keep-wide framing leaves no streaks

### 2.3 Dust, hot pixels and movement

- [ ] Defect stage before warping: a user mask or an auto-detected dust and hot-pixel mask, inpainted per frame and saved for reuse
- [ ] Movement detection: residual-motion maps, flagged frames and regions, and a per-frame motion score in the project
- [ ] Filmstrip data API: cached low-res previews of every frame, aligned and unaligned
- [ ] Tests: no dust trails after masking; injected movement is flagged

## Phase 3 — Stacking engines

### 3.1 Streaming framework and weighted average

- [ ] Streaming method trait (`begin`, `add_frame`, `finish`) with progress, tiled for large images and deterministic
- [ ] Weighted-average method: local-contrast weights raised to a power, with radius and smoothing
- [ ] Quarter-scale preview mode for every method
- [ ] Tests: beats the best single frame on SSIM; peak memory flat from 20 to 200 frames

### 3.2 Pyramid method *(plan first)*

- [ ] Laplacian pyramid and complex wavelets evaluated on the metrics and one chosen (Burt and Adelson; Forster et al.; focus-stack may be adapted with attribution)
- [ ] Streaming selection or soft blend of the highest-energy coefficients per level
- [ ] Grit suppression: fine-level choices checked against the coarser level's
- [ ] Unclipped float result with a final local tone compression, and a "retain extended range" output
- [ ] Decisions on luminance or on all channels
- [ ] Limits on inversion halos around strong edges
- [ ] Tests: beats weighted average on SSIM and halos on thin overlapping structures; noise and color shift recorded

### 3.3 Depth-map method

- [ ] Focus measure (modified Laplacian or Tenengrad) with a per-pixel best frame and a confidence
- [ ] Edge-aware smoothing, with low-contrast regions filled from confident neighbors
- [ ] Compositing from interpolated sub-frame depth that never alters color or contrast
- [ ] Two-pass API: cached focus statistics, so a new threshold re-composites in under a second
- [ ] Preset thresholds (absolute or percentile) for batch runs, and an optional low-confidence mask
- [ ] Outputs: the composite, a 16-bit depth map and the confidence map
- [ ] Tests: depth correlates with ground truth, deltaE near zero, fast threshold changes

### 3.4 Slabbing and very deep stacks

- [ ] Slabbing that wraps any pair of methods, with slab size and overlap defaulting from the frame count, and slab outputs kept as project outputs
- [ ] Automatic splitting of a folder of several stacks by jumps in framing or scale, or gaps in capture time
- [ ] Tests: slabbed pyramid then depth map on 300 frames matches unslabbed quality and runs faster; three concatenated stacks are split apart

### 3.5 Quality report and regression gate

- [ ] `cargo xtask quality`: every method and slab combination on every corpus stack, with metrics, runtime and peak memory, in an HTML report with 100% crops
- [ ] A baseline JSON in the repository; CI fails on regressions beyond tolerance unless the same commit updates the baseline

## Phase 4 — CLI and GPU

### 4.1 Command-line tool

- [ ] florastack-cli on clap (derive), dispatching registry commands only
- [ ] `stack` with method, output, slab, alignment, crop, preset, RAW white balance, output depth and dust-mask options
- [ ] `run`, `commands` (with their JSON schemas) and `project new|info|clean-orphans`
- [ ] `--help` with examples, documented exit codes, and progress on stderr: a bar on a TTY or `--json-progress`
- [ ] Windows drag-and-drop: file arguments alone stack with default settings next to the first file
- [ ] Tests: assert_cmd per method, `--json-progress` parses; a CLI quick start in the README

### 4.2 Batch queue and watch folder

- [ ] `batch`: queue many stacks, or auto-split one folder, with a preset, and write a summary CSV with warnings
- [ ] `watch`: ingest frames as they are written, align incrementally, update a running preview and finalize after a quiet period
- [ ] Output filename templates (stack name, method, frame range, date, parameters)
- [ ] Tests: batch on three synthetic stacks; watch with frames written slowly

### 4.3 GPU acceleration with a CPU oracle *(plan first)*

- [ ] florastack-gpu kernels (WGSL on wgpu): format conversion, Gaussian and Laplacian pyramids, focus measures, the alignment warp, pyramid selection and the depth-map statistics
- [ ] Streaming on the GPU: one frame uploaded at a time, accumulators kept on the GPU, tiling beyond a VRAM budget
- [ ] An agreement test per kernel against its CPU reference in florastack-core (conversions, blur, resize, gradients, warps), florastack-stack or florastack-align, with documented tolerances
- [ ] GPU chosen at startup when an adapter passes a self-test, otherwise the CPU with a logged reason; a preference forces the CPU
- [ ] Decoding frame N+1 while frame N is processed
- [ ] Agreement tests in CI on software adapters (lavapipe on Linux, WARP on Windows); on macOS only with a Metal adapter, otherwise reported as skipped
- [ ] CPU and GPU timings per method in `cargo xtask bench` (100 frames, 24 MP), with GPU numbers from real hardware

## Phase 5 — Desktop UI and retouching

### 5.1 App shell *(plan first)*

- [ ] egui and eframe on wgpu, with no processing logic: the UI dispatches commands and follows project state and job progress
- [ ] Layout: top bar with job progress and Cancel, frame list, central viewer, settings panel, and Stack, Retouch, Export and Batch tabs
- [ ] GPU viewer: smooth zoom and pan, fit, 100% and 200%, anti-aliased downscaling, sRGB display transform
- [ ] Fluent i18n (English first), AccessKit, dark and light themes, shortcuts listed in Help > Shortcuts, a command palette (Ctrl/Cmd+K) and a remembered layout
- [ ] Files: drag-and-drop of files and folders, File > Open, paste, recent projects and autosave; XWayland by default, with an environment variable for native Wayland
- [ ] Builds on all three operating systems in CI; a headless smoke test drives commands through the registry
- [ ] Follow-up: monitor-profile color management in the viewer

### 5.2 Stack tab

- [ ] Settings always on screen: method, radius and smoothing, alignment toggles, framing and an Advanced section, each with a tooltip and a "Learn more" link
- [ ] Add frames, align (with per-frame quality) and stack, with quarter-scale previews on every change while the full render runs
- [ ] The depth map's contrast-threshold step inline, on a live preview
- [ ] Filmstrip playback of the stack, aligned or unaligned
- [ ] Outputs list with method and parameters, compared side by side or with a split slider
- [ ] UI test (egui_kittest or similar): add, align and stack produce an output

### 5.3 Retouching *(plan first)*

- [ ] Source and target viewers side by side, with zoom, pan and the brush cursor in sync
- [ ] Source from any frame, slab or output, plus a jump to the frame where the clicked point is sharpest
- [ ] Brushes: Details (adaptive hardness, brightness and color matching), Pixels, Darken, Lighten and Eraser, with size, hardness, opacity and pen pressure, and a brightness boost for dark regions
- [ ] Non-destructive, deterministic stroke layers saved in the project, with undo and redo per stroke
- [ ] Tests: replay is deterministic, undo and redo round-trip, a Details stroke from an identical source changes nothing; manual checklist in `docs/retouching.md`

### 5.4 Export, presets and preferences

- [ ] Export tab: format, bit depth, compression, color profile, metadata, extended range, filename template, extra maps and aligned frames, and an estimated file size
- [ ] Full and partial presets, built-in ones (Insect macro, Product shot, Landscape, Microscope), applied from the Batch tab too
- [ ] Preferences: memory budget, GPU on or off with a self-test, cache location and size, autosave interval, theme and language; no telemetry
- [ ] Tests: exported files reopen with the right bit depth, profile and metadata; presets round-trip

## Phase 6 — Capture and integrations

### 6.1 Camera and rail control *(plan first)*

- [ ] florastack-capture crate (core only; the engine may depend on it; RULES updated) and a Capture tab
- [ ] Watch-folder capture documented with in-camera focus bracketing and, on Windows, the camera makers' tethering apps
- [ ] libgphoto2 camera control on Linux and macOS behind the `gphoto` feature, loaded at run time through a thin FFI module, the one module AGENTS.md lists for `unsafe`; ATTRIBUTION.md entry
- [ ] Serial rail control: G-code and the simple serial protocols of DIY rails, stubs for commercial rails with public protocols
- [ ] Focus bracketing by lens (gphoto builds), a wave-optics step-size calculator, and captured frames streamed into the watch-folder ingest
- [ ] Tests: builds with and without `--features gphoto`; a mock camera and rail drive an end-to-end capture

### 6.2 Scientific exports and annotations

- [ ] Exports: depth map (16-bit, and calibrated in micrometers when the step is known), confidence map, aligned and corrected frames, contribution maps and an alignment CSV
- [ ] Calibration profiles: pixels per micrometer for each objective or magnification
- [ ] Non-destructive annotations: scale bar and text labels
- [ ] A reproducibility sidecar (settings, version, input hashes) with every export
- [ ] Tests: a calibrated depth export matches the synthetic depths; the sidecar reproduces the output through the CLI

### 6.3 3D model and stereo

- [ ] Textured mesh from the depth map, exported as glTF 2.0 (.glb) and OBJ, with an in-app 3D viewer
- [ ] Synthetic stereo: parallel and cross-eyed pairs, and red-cyan anaglyphs
- [ ] Rocking parallax animation as video or animated WebP or GIF
- [ ] Tests: the .glb passes the glTF validator; the stereo parallax goes the right way

### 6.4 MCP server, control channel and Python

- [ ] `florastack-cli mcp`: an MCP server (stdio) with one tool per registered command, and resources for project state and thumbnails
- [ ] `florastack --control`: an authenticated, loopback-only JSON control channel with offscreen screenshots
- [ ] Python bindings (PyO3 and maturin) in `python/florastack`, with numpy arrays out
- [ ] Tests: MCP lists the tools and runs a stack; pytest checks the shape of a stacked output

### 6.5 Photo-editor integrations

- [ ] Lightroom Classic export plugin in `packaging/lightroom/`
- [ ] darktable Lua script
- [ ] "Open with FloraStack" desktop entry and a CLI recipe for LightCraft, digiKam and other editors
- [ ] Scripts lint; manual checklist in `docs/integrations.md`

## Phase 7 — Release

### 7.1 Installers and signing

- [ ] `cargo xtask dist` and a release workflow on version tags, reading names and the app ID from `branding.toml` and refusing the OWNER placeholder
- [ ] Windows x64 and arm64 (.msi and .zip), a macOS universal .dmg (signed and notarized when secrets are set), Linux x86_64 and aarch64 (AppImage, .deb, .rpm, Flatpak, tarball), CLI archives, Python wheels and SHA256SUMS.txt
- [ ] gphoto in the Linux and macOS builds only: libgphoto2 bundled in the macOS app and the Flatpak, a distro dependency in .deb and .rpm, with instructions for swapping it
- [ ] Artifacts from `cargo xtask dist --target <host>` install locally; the workflow runs on a test tag in a fork
- [ ] Third-party notices in every package: the license text of each crate compiled into the apps, generated from `Cargo.lock`, next to `NOTICE`

### 7.2 Documentation and onboarding

- [ ] mdBook user guide in `book/`: install, first stack, choosing a method, retouching, slabbing, shooting advice, batch and watch folders, capture, scientific exports and troubleshooting
- [ ] Developer guide: architecture with a crate diagram, the command registry, the CPU oracle, adding a stacking method, the quality gate, bumping the LightCraft pin, releasing
- [ ] CONTRIBUTING.md, SECURITY.md (fuzzing codecs and the RAW adapter with cargo-fuzz), issue templates, and README screenshots taken through the control channel
- [ ] `mdbook build` passes, lychee finds no broken links, and a test checks the command reference against the registry

### 7.3 Browser build *(optional)*

- [ ] wasm32 build with eframe's web backend and WebGPU, with a CPU fallback on a Web Worker pool
- [ ] OPFS-backed `TileStore`, capture disabled, and a clear message when a stack needs more memory than the browser has
- [ ] Cross-origin isolation headers (COOP and COEP) in the Dockerfile's server config and documented for other hosts; `cargo xtask web`
- [ ] A headless browser test stacks a 20-frame synthetic stack
- [ ] `cargo check -p florastack-codecs --target wasm32-unknown-unknown` in CI (exr is single-threaded there, and a decoder panic aborts instead of becoming an error)

### After the last step

- [ ] `cargo xtask quality` on the real corpus, compared side by side with Helicon Focus and Zerene Stacker trial outputs; the gaps become the next round of work
