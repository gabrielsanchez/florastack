# FloraStack

FloraStack is a free, open-source focus-stacking application written in pure Rust. It aligns a series of photos focused at different distances and merges the sharp parts of each into one image with more depth of field than any single exposure allows, for macro, microscopy, product and landscape photography. All the work happens in an engine of library crates, and the egui desktop app and the command-line tool are thin layers over it. FloraStack is in early development and can't stack images yet.

## Building

Install [rustup](https://rustup.rs). The toolchain is pinned in `rust-toolchain.toml`, and rustup installs it on first use (or run `rustup toolchain install` in this folder). On Windows you also need the Visual Studio C++ build tools ("Desktop development with C++").

```sh
cargo build --workspace                                 # build everything
cargo run -p florastack                                 # the desktop app
cargo run -p florastack-cli                             # the command-line tool
cargo test --workspace                                  # run the tests
cargo clippy --workspace --all-targets -- -D warnings   # lint (Clippy pedantic, warnings are errors)
cargo fmt --all --check                                 # check formatting
cargo xtask check-layers                                # check which crate may depend on which
```

## License

Licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in FloraStack by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
