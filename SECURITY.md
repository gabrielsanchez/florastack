# Security policy

FloraStack opens files from anywhere: photos and camera RAW files, project folders, presets, export sidecars and defect
masks, any of which may come from someone else. It can also listen for local control (`--control`) and serve MCP
clients. This page says how to report a security problem, and how FloraStack's file readers are fuzzed.

## Supported versions

Security fixes go into the next release and the `main` branch. Use the latest release.

## Reporting a vulnerability

Report it privately, through GitHub: the repository's **Security** tab, **Report a vulnerability**
(<https://github.com/gabrielsanchez/florastack/security/advisories/new>). Please don't open a public issue for it.

Say what an attacker controls (a file someone opens, a project folder, a preset, the control channel, an MCP client),
what happens (a crash, a hang, memory or disk exhausted, a file written outside where it should be, code run), the
version and platform, and the smallest input that shows it. The maintainers will confirm what they can reproduce, work
on a fix, publish an advisory with the release that fixes it, and credit you unless you'd rather not be.

### In scope

- Memory unsafety, or code execution, from any input.
- A file, project, preset or sidecar that makes FloraStack write, move or delete files outside the folders the user
  chose.
- Getting past the control channel's token, or reaching it from another machine.
- A crafted input that crashes FloraStack, hangs it, or makes it allocate far beyond the input's size. FloraStack's
  rule is that a malformed file is an error, never a crash, so these are bugs worth reporting even when they are not
  dangerous; a plain crash may go in a public issue.

### Out of scope

- Problems in libgphoto2 (report them [upstream](https://github.com/gphoto/libgphoto2)) or in LightCraft's crates
  ([upstream](https://github.com/storytold/lightcraft)); tell us too if FloraStack should guard against them.
- What a local user who can already run programs as you can do, such as reading the control token from your files.

## How FloraStack defends itself

- **Rust, with almost no `unsafe`.** FloraStack's own unsafe code is limited to the module that loads libgphoto2 at run
  time (`crates/capture/src/gphoto/ffi.rs`), each block with its reasoning; the workspace denies it everywhere else.
- **Every reader treats its input as hostile.** Sizes are checked before anything is allocated, against a pixel limit;
  indexing into anything a file decides is checked; the decoding libraries run behind a panic guard that turns a panic
  into an error. `unwrap`, `expect` and `panic!` are refused outside tests by Clippy.
- **The control channel** listens on the loopback address only, and every request must carry the token from a file the
  user created.
- **Dependencies** must be permissively licensed and free of known advisories: `cargo deny check` and `cargo audit` run
  in CI.

## Fuzzing

The readers that see untrusted bytes first, florastack-codecs (image files and their metadata) and florastack-raw (the
adapter over LightCraft's RAW decoders and the develop step), are fuzzed with
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) and libFuzzer. The targets are in `fuzz/`, a crate outside the
workspace that builds with the nightly toolchain pinned in `fuzz/rust-toolchain.toml`:

| Target | What it reads |
|---|---|
| `codecs_probe` | A file's format, size and layout, and its EXIF, IPTC and XMP metadata: what is read from every file added |
| `codecs_read` | A whole image: JPEG, PNG, TIFF, OpenEXR, WebP and BMP, with color profiles and orientation |
| `raw_decode` | A camera RAW file's headers and sensor data, and the develop step on what decodes |

To run them, on Linux or macOS (on Windows, see the note below):

```sh
rustup toolchain install nightly-2026-11-01 --profile minimal   # the toolchain fuzz/rust-toolchain.toml pins
cargo install --locked cargo-fuzz
cargo xtask corpus --fixtures                                     # sample files to start from

cd fuzz
cargo fuzz run codecs_probe corpus/codecs_probe ../corpus/fixtures/pngsuite ../corpus/fixtures/tiff -- -max_total_time=600
cargo fuzz run codecs_read corpus/codecs_read ../corpus/fixtures/pngsuite ../corpus/fixtures/tiff ../corpus/fixtures/webp ../corpus/fixtures/openexr -- -max_total_time=3600
cargo fuzz run raw_decode corpus/raw_decode ../corpus/fixtures/raw -- -max_len=4194304 -rss_limit_mb=4096 -max_total_time=3600
```

The first folder after the target is the corpus the fuzzer grows; the others are read-only seeds, the format fixtures.
`-max_total_time` is in seconds; without it, a run goes on until it finds something. RAW files are large, so
`raw_decode` needs a larger input size (`-max_len`) and memory limit than libFuzzer's defaults.

The fuzzer treats every panic as a crash, the ones the panic guard would turn into errors in the app included: those
are bugs too, in FloraStack or upstream. A crash is saved under `fuzz/artifacts/<target>/`:

```sh
cargo fuzz run codecs_read artifacts/codecs_read/crash-<hash>     # reproduce it
cargo fuzz tmin codecs_read artifacts/codecs_read/crash-<hash>    # shrink it to the smallest input that crashes
```

Fix it with a regression test, in the crate that crashed, on a small synthetic input that crashed before the fix
(AGENTS.md, Never crash). A crash inside LightCraft gets reported upstream, and guarded against here meanwhile. Report
a crash that could be exploited privately, as above.

Fuzz on Linux, macOS or WSL. On Windows, the targets build only with AddressSanitizer (without it, MSVC can't link
libFuzzer's coverage instrumentation), and run only with a Visual Studio AddressSanitizer runtime
(`clang_rt.asan_dynamic-x86_64.dll`) on `PATH` that matches the nightly's instrumentation; an older one fails to
start. The [developer guide](book/src/dev/fuzzing.md) says more, and the weekly `Fuzz` workflow
(`.github/workflows/fuzz.yml`) runs every target on Linux.
