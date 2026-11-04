# Fuzzing

Every reader in FloraStack treats its input as hostile, and the tests check that on damaged files: the format fixtures
include fuzzer-found broken files, and every crash fix comes with a regression test. Fuzzing finds the cases nobody
thought of, by feeding the readers millions of mutated files and watching for panics, hangs and runaway allocations.
[SECURITY.md](https://github.com/gabrielsanchez/florastack/blob/main/SECURITY.md) has the commands to run it; this
chapter explains how it is set up.

## The fuzz crate

`fuzz/` is a [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) crate, outside the workspace (its `Cargo.toml` has
a `[workspace]` of its own):

- libFuzzer, which `libfuzzer-sys` compiles from C++, and the nightly toolchain that cargo-fuzz needs never touch the
  workspace's builds, its pure-Rust rule or its dependency policy, and none of it ships;
- `fuzz/rust-toolchain.toml` pins the nightly, so that a run is reproducible; move it from time to time, with
  `rustup toolchain install` in `fuzz/` to fetch the new one;
- `fuzz/Cargo.lock` is committed, like the workspace's;
- what fuzzing makes (`target/`, the grown `corpus/`, the `artifacts/` with crashes, `coverage/`) is ignored by git.

The targets call FloraStack's public APIs on the fuzzer's bytes, as the engine does with a file's:

| Target | Calls |
|---|---|
| `codecs_probe` | `florastack_codecs::sniff`, `probe` and `read_metadata`: the format, size and layout, and the EXIF, IPTC and XMP metadata |
| `codecs_read` | `florastack_codecs::read`, with a pixel limit of 4 megapixels so that each input decodes quickly |
| `raw_decode` | `florastack_raw::probe`, `info` and `decode` (up to 16 megapixels), then `RawDevelop::new` and `develop` for a preview on what decodes |

libfuzzer-sys turns every panic into a crash, the panics that the codecs' and RAW importer's panic guard would turn
into errors included: the guard keeps the app running, but a panic it catches is still a bug, in FloraStack or in a
library underneath.

## Platforms

Fuzz on Linux, macOS or WSL, where cargo-fuzz uses the toolchain's own AddressSanitizer runtime. On Windows the
targets build only with AddressSanitizer: without it, MSVC's linker can't resolve the bounds of libFuzzer's coverage
sections. The program then needs Visual Studio's AddressSanitizer runtime (`clang_rt.asan_dynamic-x86_64.dll`, from
its "C++ AddressSanitizer" component) on `PATH`, in a version that matches the nightly's instrumentation: with
Visual Studio 2022's MSVC 14.34 runtime, the targets build but fail to start (`STATUS_DLL_INIT_FAILED`).

## Seeds

A fuzzer gets further from real files than from nothing. The format fixtures (`cargo xtask corpus --fixtures`) are the
seeds: PngSuite, the TIFF, WebP and OpenEXR test files, and one camera RAW file per format. Pass their folders after
the target's own corpus folder, which is where the fuzzer keeps the new inputs it finds interesting:

```sh
cd fuzz
cargo fuzz run codecs_read corpus/codecs_read ../corpus/fixtures/pngsuite ../corpus/fixtures/tiff
```

## Coverage

To see which parts of a reader the corpus reaches, and so where seeds are missing:

```sh
rustup component add llvm-tools --toolchain nightly-2026-11-01
cargo fuzz coverage codecs_read corpus/codecs_read
```

then `llvm-cov show` on the result, as cargo-fuzz prints.

## From a crash to a fix

1. `cargo fuzz run <target> artifacts/<target>/crash-<hash>` reproduces it, with the panic's message and backtrace.
2. `cargo fuzz tmin <target> artifacts/<target>/crash-<hash>` shrinks the input.
3. Fix it in the crate that crashed, with a regression test on a small synthetic input that crashed before the fix
   (AGENTS.md, Never crash): a test builds the bytes itself, or reads a few bytes committed next to it, never a corpus
   file.
4. A crash inside a LightCraft crate goes upstream as well, and FloraStack guards against it meanwhile. A crash that
   could be exploited is reported privately (SECURITY.md).

## Adding a target

A new reader of untrusted bytes (a new image format, a project-file parser, a preset reader) gets a target: a file in
`fuzz/fuzz_targets/` and a `[[bin]]` entry in `fuzz/Cargo.toml`, calling the public API as the engine does, with a size
limit small enough for fast runs. Add it to the table above, to SECURITY.md's, and to the matrix of the weekly
workflow (`.github/workflows/fuzz.yml`), with seeds if the fixtures have any.

## In CI

The weekly **Fuzz** workflow runs every target for 15 minutes on Linux from the fixtures, and on demand
(**Actions > Fuzz > Run workflow**, with the time per target). A crash fails the run and is uploaded as an artifact. It
is not part of `cargo xtask ci`: fuzzing takes a nightly toolchain and minutes per target, and finds different things
on every run.
