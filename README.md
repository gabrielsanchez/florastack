# FloraStack

FloraStack is a free, open-source focus-stacking application written in pure Rust. It aligns a series of photos focused at different distances and merges the sharp parts of each into one image with more depth of field than any single exposure allows, for macro, microscopy, product and landscape photography. All the work happens in an engine of library crates, and the egui desktop app and the command-line tool are thin layers over it. FloraStack is in early development: its engine and CLI align and stack frames (by weighted average, pyramid or depth map, with quarter-scale previews, and in slabs for very deep stacks) and the engine finds the stacks in a folder that holds several. The desktop app is still a stub.

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
cargo xtask bench                                       # criterion benchmarks, summarized in target/bench-summary.json
cargo xtask quality                                     # every method on every stack: a report with crops in target/quality/, and the regression gate
```

`cargo xtask ci` also runs `cargo deny check` (the license and dependency policy in `deny.toml`) and `cargo audit`; install both tools once with `cargo install --locked cargo-deny cargo-audit`. `cargo xtask corpus` downloads with `curl` and unpacks archives with `tar`, which come with Windows 10 and later, macOS and most Linux systems; `cargo xtask ci` runs `cargo xtask corpus --fixtures` before the tests, so that the codec and RAW tests have their files (about 110 MB the first time, nearly all of it camera RAW samples). [`corpus/README.md`](corpus/README.md) explains the test corpus and how to add your own stacks. `cargo xtask quality` stacks the synthetic stacks and every corpus stack by every method, scores and times each output, and fails when one is worse than `quality/baseline.json` beyond its tolerance; `cargo xtask ci` runs its gate on the synthetic stacks, as CI does.

## CLI quick start

Build with `cargo build -p florastack-cli --release`, then run `target/release/florastack-cli` (`.exe` on Windows), or use `cargo run -p florastack-cli --` before the arguments:

```sh
florastack-cli stack photos --method pyramid --out result.tif
florastack-cli stack frame1.tif frame2.tif --method depthmap --out depthmap.tif --depth 32f
florastack-cli stack photos --method both --out result.tif --crop common
florastack-cli stack photos --slab 15:5 --align shift,scale,rotation,brightness --preset macro.toml --raw-wb asshot --dust-mask dust.png --out result.tif
florastack-cli stack photos --out result.tif --json-progress 2> progress.jsonl
florastack-cli project new Bee --folder .
florastack-cli project info Bee.florastack
florastack-cli run Bee.florastack --cmd project.addFrames --params '{"paths":["photos"]}' --cmd align.run --params '{}' --cmd stack.pyramid --params '{}'
florastack-cli commands > commands.json
florastack-cli project clean-orphans --dry-run
florastack-cli project clean-orphans --days 30
```

Inputs are files in focus order, or folders whose image and camera RAW files are read directly, sorted by name. An unreadable or duplicate input stops `stack` with a diagnostic. `--method` accepts `pyramid` (default), `depthmap`, `average`, or `both`; `both` writes `result-pyramid.tif` and `result-depthmap.tif`. Exports are TIFF, with `--depth 16` (default) or `32f`, and existing files are refused. `--slab SIZE:OVERLAP` uses the chosen method for both the slabs and their final stack. A preset is applied first; explicit alignment, crop and RAW white-balance options override it. Omitted options keep the preset or engine defaults. `--crop` accepts `narrow`, `wide`, or `common`; the dust mask is a frame-sized image with black defects on white.

Results go to stdout as JSON. Progress goes to stderr as a bar on a terminal, stage messages when redirected, or engine events as JSON lines with `--json-progress`. Runtime warnings and errors also become JSON lines in that mode. `run` pairs the first `--cmd` with the first `--params`, and so on, waits for each job, prints one result per line, and saves changes after all commands succeed. Give one JSON object per command, or omit all `--params` to use `{}` for every command. It stops on the first error; earlier external file writes remain, and project changes since the last save are not saved. `commands` prints the complete registry, including help, examples, flags and parameter/result JSON schemas. Every subcommand has `--help` with examples. Examples above use shell quoting accepted by Bash and PowerShell; in `cmd.exe`, escape JSON quotes as required by that shell.

Running the binary with file arguments alone, including dragging photos onto its executable in Windows Explorer, stacks them with default settings into `<first-file-stem>-stacked.tif` beside the first file. `stack` saves an unnamed project with its alignment and outputs in the system temporary folder under the app ID's `projects/` directory and includes its path in the JSON result. `--temporary-projects FOLDER` changes that root for any subcommand. `project clean-orphans` moves closed, unnamed projects unused for 30 days to the system trash; `--dry-run` lists candidates, and `--days` changes the age. Named projects are excluded.

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
