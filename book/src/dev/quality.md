# Tests, the corpus and the quality gate

Alignment and stacking are scored, not eyeballed. A change that makes stacks worse must fail a check, and a change
that makes them better must show by how much. Four kinds of test data serve that.

## Synthetic stacks

`florastack_core::testutil` (the core's `testutil` feature, enabled from `[dev-dependencies]` only) generates stacks
with exact ground truth. `generate` turns a sharp image and a depth map, such as `scene::textured`'s, into frames with
depth-dependent blur, focus breathing, sub-pixel shift, rotation, brightness jitter, noise and dust;
`StackParams::identity`, `clean` and `noisy_dusty` are the standard sets. `generate_layered` makes scenes of
flat-colored layers over a backdrop whose layers occlude one another, which have the veils and halos real subjects
leave: stacking methods are scored on those.

- Score alignment with `GroundTruth::relative` and `transform_error`.
- Score stacking against `GroundTruth::sharp` with `florastack-metrics`: PSNR, SSIM, halos, flat-region noise and
  color shift (CIEDE2000).

Defocus also moves the synthetic content a little, as it does on a real lens: `crates/align/tests/accuracy.rs`
explains how its targets allow for it.

## Real stacks: the corpus

Real stacks live in the gitignored `corpus/` folder; never commit them.

- `corpus/manifest.toml` pins stacks to download, each file by its SHA-256, with its source and license.
  `cargo xtask corpus` downloads them and checks them.
- `corpus/local/<name>/`, with a `stack.toml`, holds stacks you can't or don't want to publish: tests and the quality
  report use them on your machine only.
- `corpus/fixtures/<set>/` holds format fixtures, files written by other software (PngSuite, TIFF, WebP and OpenEXR
  test files) and one camera RAW file per format, for the codec and RAW tests. `cargo xtask corpus --fixtures`
  downloads them, as CI does before the tests.

A test that needs a corpus file calls `florastack_testkit::skip!(Corpus, "...")` when it is missing, and finds fixtures
with `florastack_testkit::fixture(set, name)`. [`corpus/README.md`](https://github.com/gabrielsanchez/florastack/blob/main/corpus/README.md)
says how to add a stack or a fixture.

## Skipped tests

libtest has no "skipped" result, so a test that can't run here, for want of a GPU adapter or a corpus file, records why
with `skip!` and returns. `cargo xtask ci` points the record at `target/ci/skips.jsonl` and lists every skip in its
summary, so that nothing passes silently.

## Benchmarks

Criterion benches live in each crate's `benches/`. `cargo xtask bench` runs them and writes
`target/bench-summary.json`, which later runs compare against; benches that score quality as well write metric
records that the summary lists as a table. CI compiles the benches but doesn't run them.

## The quality report and gate

`cargo xtask quality` stacks every stack (the synthetic ones of `crates/engine/examples/quality/synthetic.rs`, the
manifest's downloaded ones and your local ones) by every method and every pair of slab and final methods, through the
engine's commands. It scores each output against its truth (the synthetic ground truth, or a real stack's
`reference`), scores alignment against the synthetic transforms, measures each command's runtime and peak heap, and
writes `target/quality/index.html`, with 100% crops of the same regions of every output side by side.

```sh
cargo xtask quality                          # the full report, release build
cargo xtask quality --stack local/wasp-jpg   # one stack
cargo xtask quality --run pyramid            # only the runs whose name contains "pyramid"
cargo xtask quality --settings '{"pyramid": {"grit": 0}}'   # try a setting against the baseline
cargo xtask quality --gate                   # what CI runs: fails on a regression
cargo xtask quality --update-baseline        # write quality/baseline.json
```

`quality/baseline.json` holds every run of the synthetic stacks and the manifest's downloaded ones (never a local
stack's) with each measure's tolerance. `--gate` fails when a measure is worse than the baseline by more than its
tolerance; runtime counts only against a baseline made on the same machine with the same build profile. A change that
moves a measure on purpose, or changes a synthetic stack or the runner, runs `--update-baseline` and commits the
baseline with it, so that its diff shows what moved.

## Everything CI runs

`cargo xtask ci` runs, in CI's order, formatting, Clippy, the layering check, the fixtures, the tests, the Python
package's tests, the quality gate, the dependency policy (`cargo deny check`, `cargo audit`) and the book's build and
links (`cargo xtask book`). It stops at the first failure and ends with a summary of each step and the tests that were
skipped. A test in `xtask/src/ci.rs` fails when its list and `.github/workflows/ci.yml` differ.
