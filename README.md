# FloraStack

FloraStack is a free, open-source focus-stacking application written in pure Rust. It aligns a series of photos focused at different distances and merges the sharp parts of each into one image with more depth of field than any single exposure allows, for macro, microscopy, product and landscape photography. All the work happens in an engine of library crates, and the egui desktop app and the command-line tool are thin layers over it. FloraStack is in early development: its engine and CLI align and stack frames (by weighted average, pyramid or depth map, with quarter-scale previews, and in slabs for very deep stacks), on the GPU when one passes a self-test, and the engine finds the stacks in a folder that holds several. The desktop app aligns and stacks with live previews, shows each frame's alignment, sets the depth map's threshold on a live picture, plays the stack as a filmstrip, compares outputs, retouches them from their frames with non-destructive brushes, and exports them (TIFF, PNG, JPEG, DNG or OpenEXR, with a frame's metadata, the depth and confidence maps and the aligned frames), on a GPU viewer. Presets, built-in or saved, carry any of its settings; FloraStack sends nothing anywhere, no telemetry.

## Building

Install [rustup](https://rustup.rs). The toolchain is pinned in `rust-toolchain.toml`, and rustup installs it on first use (or run `rustup toolchain install` in this folder). On Windows you also need the Visual Studio C++ build tools ("Desktop development with C++"). The first build downloads LightCraft's image crates from GitHub, where they are pinned to one commit ([`docs/lightcraft.md`](docs/lightcraft.md)), so it needs network access; later builds use Cargo's copy.

```sh
cargo build --workspace                                 # build everything
cargo run -p florastack                                 # the desktop app
cargo run -p florastack-cli                             # the command-line tool
cargo test --workspace                                  # run the tests
cargo clippy --workspace --all-targets -- -D warnings   # lint (Clippy pedantic, warnings are errors)
cargo fmt --all --check                                 # check formatting
cargo xtask check-layers                                # check which crate may depend on which
cargo xtask ci                                          # everything CI runs, with a summary of skipped tests
cargo xtask corpus                                      # download the sample stacks and test fixtures pinned in corpus/manifest.toml
cargo xtask bench                                       # criterion benchmarks and the CPU-against-GPU run, summarized in target/bench-summary.json
cargo xtask quality                                     # every method on every stack: a report with crops in target/quality/, and the regression gate
```

`cargo xtask ci` also runs `cargo deny check` (the license and dependency policy in `deny.toml`) and `cargo audit`; install both tools once with `cargo install --locked cargo-deny cargo-audit`. `cargo xtask corpus` downloads with `curl` and unpacks archives with `tar`, which come with Windows 10 and later, macOS and most Linux systems; `cargo xtask ci` runs `cargo xtask corpus --fixtures` before the tests, so that the codec and RAW tests have their files (about 110 MB the first time, nearly all of it camera RAW samples). [`corpus/README.md`](corpus/README.md) explains the test corpus and how to add your own stacks. `cargo xtask quality` stacks the synthetic stacks and every corpus stack by every method, scores and times each output, and fails when one is worse than `quality/baseline.json` beyond its tolerance; `cargo xtask ci` runs its gate on the synthetic stacks, as CI does.

## Capture

The [Capture tab and workflow](docs/capture.md) connect in-camera bracketing or tethering apps to the watch folder on all platforms, with serial rails and a diffraction step calculator. Linux/macOS can add built-in camera control with `cargo build -p florastack --features gphoto` and a separately installed libgphoto2 shared library. Windows builds omit that feature and use the camera maker's tethering app.

## GPU

Stacking runs on the GPU through [wgpu](https://wgpu.rs) (Vulkan, Metal or DX12; WebGPU in a browser later) when a hardware adapter passes a self-test, and on the CPU otherwise; the log says which and why, and `compute.info` reports it. Nothing needs installing beyond the graphics driver. `--device cpu` (CLI) or `compute.set` with `{"device": "cpu"}` forces the CPU; `gpu` also takes software adapters such as Mesa's lavapipe and Windows' WARP, which compute correctly but more slowly than the CPU. The GPU's results agree with the CPU's within the tolerances that `crates/gpu/tests/` documents, and a GPU that fails part way through a stack makes it start again on the CPU.

The GPU tests compare every kernel and every method with its CPU reference. Without an adapter they are listed as skipped in `cargo xtask ci`'s summary; to run them on a machine without a GPU, install a software adapter (on Debian or Ubuntu, `sudo apt-get install mesa-vulkan-drivers`; Windows has WARP) and select it, for example with `WGPU_ADAPTER_NAME=llvmpipe` or `WGPU_ADAPTER_NAME="Microsoft Basic Render Driver"`. `FLORASTACK_REQUIRE_GPU=1` makes a missing adapter fail them instead, as CI does on Linux and Windows. `cargo bench -p florastack-gpu --bench compositors` times the three methods on 100 frames of 24 MP on both devices (a few minutes on the CPU; `--quick` takes 8 frames).

## Desktop app

Build with `cargo build -p florastack --release` and run `target/release/florastack` (`.exe` on Windows), or `cargo run -p florastack --release`. Drop photos or a folder of them on the window (or pass them on the command line) to start a temporary project, align and stack them from the Stack tab ([`docs/stack-tab.md`](docs/stack-tab.md) explains its settings), retouch the result from its frames in the Retouch tab ([`docs/retouching.md`](docs/retouching.md)), and export it from the Export tab, whose settings (format, bit depth, profile, metadata, file name and what goes with the output) are the project's; File > Save as names the project and moves it out of the temporary projects' folder. The Presets menu applies the built-in presets (Insect macro, Product shot, Landscape, Microscope) or a preset file, all of it or some of its settings, and saves the project's settings as one; File > Preferences sets the memory stacking may use, the GPU, the cache folder, autosave, the theme and the language. Ctrl+K (Cmd+K on macOS) opens the command palette, which runs any engine command, and Help > Keyboard shortcuts lists the shortcuts.

On Linux the app runs through XWayland when an X server is available, since file drag-and-drop doesn't work in native Wayland windows yet; set `FLORASTACK_WAYLAND=1` to use Wayland itself. `FLORASTACK_LOG=info` (or `debug`) shows the log on standard error. Temporary projects live in the app's local data folder (on Windows under `%LOCALAPPDATA%`, in the folder named after the app ID in `branding.toml`); after 30 days unused, the app offers to move them to the trash.

## CLI quick start

Build with `cargo build -p florastack-cli --release`, then run `target/release/florastack-cli` (`.exe` on Windows), or use `cargo run -p florastack-cli --` before the arguments:

```sh
florastack-cli stack photos --method pyramid --out result.tif
florastack-cli stack frame1.tif frame2.tif --method depthmap --out depthmap.tif --depth 32f
florastack-cli stack photos --method both --out result.tif --crop common
florastack-cli stack photos --slab 15:5 --align shift,scale,rotation,brightness --preset macro.toml --raw-wb asshot --dust-mask dust.png --out result.tif
florastack-cli stack photos --out result.tif --json-progress 2> progress.jsonl
florastack-cli batch --in stacks --out results --preset macro.toml --method depthmap
florastack-cli watch capture --out results --preset macro.toml --idle 5 --json-progress
florastack-cli project new Bee --folder .
florastack-cli project info Bee.florastack
florastack-cli run Bee.florastack --cmd project.addFrames --params '{"paths":["photos"]}' --cmd align.run --params '{}' --cmd stack.pyramid --params '{}'
florastack-cli commands > commands.json
florastack-cli project clean-orphans --dry-run
florastack-cli project clean-orphans --days 30
```

Inputs are files in focus order, or folders whose image and camera RAW files are read directly, sorted by name. An unreadable or duplicate input stops `stack` with a diagnostic. `--method` accepts `pyramid` (default), `depthmap`, `average`, or `both`; `both` writes `result-pyramid.tif` and `result-depthmap.tif`. Exports are TIFF, with `--depth 16` (default) or `32f`, and existing files are refused. `--slab SIZE:OVERLAP` uses the chosen method for both the slabs and their final stack. A preset is applied first; explicit alignment, crop and RAW white-balance options override it. Omitted options keep the preset or engine defaults. `--crop` accepts `narrow`, `wide`, or `common`; the dust mask is a frame-sized image with black defects on white.

`batch --in FOLDER --out FOLDER --preset FILE` queues the input's immediate subfolders as stacks. When the input contains images directly, it auto-splits them with `project.findStacks`; mixed layouts are refused. Each stack gets a named project and its output in the output folder, as the preset's `[output]` settings say (a 16-bit TIFF unless they say otherwise, with the maps and aligned frames they ask for; the template's extension follows the format). Every `--preset` also takes a built-in preset as `builtin:<id>`: `insect-macro`, `product-shot`, `landscape` or `microscope`. `summary.csv` records stack, frame count, method, processing seconds, alignment warnings (including movement and out-of-focus frames), project, output and any error. A failed stack is recorded and the queue continues; the CLI exits with code 1 when any entry fails. Batch and watch accept `--method pyramid`, `depthmap` or `average` (pyramid by default). Both require a preset; for example, unattended depth-map settings can be:

```toml
[stack.depth_map]
threshold = "percentile"
percentile = 40
```

`watch FOLDER --preset FILE` is the tethered capture path, including on Windows: native filesystem events wake the ingest loop, and directory polling catches missed events or network shares without notifications. It includes existing files and watches only the folder's immediate image and RAW files. A file must keep its size and modification time unchanged for `--settle-ms` (1000 by default), decode successfully, and remain unchanged during the read. Partial or locked files are retried; increase the settling interval if the tethering application pauses while writing valid intermediate images. Temporary filenames are ignored until renamed with an image extension. Files rewritten after ingestion stop capture with an error. Each ingest extends the alignment chain and updates a quarter-size preview; JSON job events include `job.preview.path` and `job.preview.frames`. Capture waits for its first frame, then finalizes once no new or changed frame arrives for `--idle` seconds (5 by default, and at least the settling interval). Unreadable files left at that point appear as warnings. Finalization runs full alignment and saves the project, TIFF, preview and CSV. Watch handles one capture and exits. The default output is a sibling folder named `<capture-folder>-stacked`; `--out` overrides it.

Batch and watch expose `batch.run` and `watch.run` in the engine registry. Their output folder must be outside the input. Work is staged until publication so engine job cancellation leaves no published artifacts; existing files and `summary.csv` are refused. Use a fresh output folder for another run. `--template` changes the TIFF filename (default `{stack}-{method}-{range}.tif`): `{stack}` is the stack name, `{method}` the method, `{first}` and `{last}` the input file stems, `{range}` their joined stems, `{date}` the UTC processing date as `YYYY-MM-DD`, and `{params}` the effective method settings. Rendered names must be portable TIFF filenames of at most 240 bytes; paths and unknown tokens are errors.

Results go to stdout as JSON. Progress goes to stderr as a bar on a terminal, stage messages when redirected, or engine events as JSON lines with `--json-progress`. Runtime warnings and errors also become JSON lines in that mode. `run` pairs the first `--cmd` with the first `--params`, and so on, waits for each job, prints one result per line, and saves changes after all commands succeed. Give one JSON object per command, or omit all `--params` to use `{}` for every command. It stops on the first error; earlier external file writes remain, and project changes since the last save are not saved. `commands` prints the complete registry, including help, examples, flags and parameter/result JSON schemas. Every subcommand has `--help` with examples. Examples above use shell quoting accepted by Bash and PowerShell; in `cmd.exe`, escape JSON quotes as required by that shell.

Running the binary with file arguments alone, including dragging photos onto its executable in Windows Explorer, stacks them with default settings into `<first-file-stem>-stacked.tif` beside the first file. `stack` saves an unnamed project with its alignment and outputs in the system temporary folder under the app ID's `projects/` directory and includes its path in the JSON result. `--temporary-projects FOLDER` changes that root for any subcommand, and `--device auto|cpu|gpu` which device stacks (see [GPU](#gpu)). `project clean-orphans` moves closed, unnamed projects unused for 30 days to the system trash; `--dry-run` lists candidates, and `--days` changes the age. Named projects are excluded.

| Exit code | Meaning |
|---|---|
| 0 | Success, including help and version |
| 1 | Processing, project, filesystem or output failure |
| 2 | Invalid CLI arguments, unknown command, invalid JSON or command parameters |
| 130 | An engine job was cancelled |

## Contributing

Read [AGENTS.md](AGENTS.md) first: it has the crate map and the rules every change follows, for human contributors and AI coding agents alike. [docs/ROADMAP.md](docs/ROADMAP.md) tracks what is done and what comes next.

## License

Licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in FloraStack by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
