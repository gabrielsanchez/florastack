# FloraStack

FloraStack is a free, open-source focus-stacking application written in pure Rust. It aligns a series of photos focused at different distances and merges the sharp parts of each into one image with more depth of field than any single exposure allows, for macro, microscopy, product and landscape photography. All the work happens in an engine of library crates, and the egui desktop app and the command-line tool are thin layers over it. FloraStack is in early development: its engine aligns and stacks frames (by weighted average so far, with quarter-scale previews), but the desktop app and the command-line tool are still stubs.

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
```

`cargo xtask ci` also runs `cargo deny check` (the license and dependency policy in `deny.toml`) and `cargo audit`; install both tools once with `cargo install --locked cargo-deny cargo-audit`. `cargo xtask corpus` downloads with `curl` and unpacks archives with `tar`, which come with Windows 10 and later, macOS and most Linux systems; `cargo xtask ci` runs `cargo xtask corpus --fixtures` before the tests, so that the codec and RAW tests have their files (about 110 MB the first time, nearly all of it camera RAW samples). [`corpus/README.md`](corpus/README.md) explains the test corpus and how to add your own stacks.

## Contributing

Read [AGENTS.md](AGENTS.md) first: it has the crate map and the rules every change follows, for human contributors and AI coding agents alike. [docs/ROADMAP.md](docs/ROADMAP.md) tracks what is done and what comes next.

## License

Licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in FloraStack by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
