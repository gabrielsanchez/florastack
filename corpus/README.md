# Test corpus

Real focus stacks for testing and scoring FloraStack, and format fixtures for testing its image codecs and its RAW importer. Two files in this folder are tracked: [`manifest.toml`](manifest.toml), which pins the stacks and fixtures to download, and this README. Everything else here is gitignored: downloads, local stacks and anything you put next to them. Never commit images to the repository.

```text
corpus/
├── manifest.toml           tracked: stacks and fixtures to download, each file pinned by sha256
├── README.md               tracked: this file
├── stacks/<name>/          downloaded by `cargo xtask corpus`
├── fixtures/<name>/        downloaded by `cargo xtask corpus`: files from other writers, for the codec and RAW tests
└── local/<name>/           your own stacks, each with a stack.toml
    ├── stack.toml
    └── IMG_0001.CR3 ...
```

```sh
cargo xtask corpus               # download every stack and fixture in the manifest that is missing or fails its check, then list them
cargo xtask corpus bee-head      # only that stack (or fixture set)
cargo xtask corpus --fixtures    # only the format fixtures, which is what the codec and RAW tests need (CI runs this; local stacks that aren't ready are only warned about)
cargo xtask corpus --check       # download nothing; fail if a pinned file is missing or wrong
cargo xtask corpus --hash DIR    # print the [[stack.files]] entries for the files in DIR
cargo xtask corpus --hash --inline DIR   # the same as one-line entries, for a fixture's files or an archive's members
cargo xtask corpus --help
```

Downloads go through `curl` (it comes with Windows 10 and later, macOS and most Linux systems) with a generic User-Agent. Each file lands in a staging folder, is checked against its size and sha256, and only then moves into `stacks/<name>/` or `fixtures/<name>/`; a file that fails the check is deleted and the command fails. Files that already match are not downloaded again.

Tests that need corpus files call `florastack_testkit::skip!(Corpus, ...)` when they are missing, so `cargo xtask ci` lists them as skipped rather than letting them pass silently. Until real stacks are added, alignment and stacking are scored on the synthetic stacks of `florastack_core::testutil`, whose ground truth is exact.

## What to collect

The roadmap asks for 5 to 10 real stacks before Phase 2: a bristly insect, a flower, a coin and a handheld landscape at least, and a microscope stack if you have one. The manifest has a placeholder for each, describing what makes a good one. Prefer stacks you shot yourself or that come with a license that allows redistribution and testing (CC0, CC BY, public domain). RAW frames are best (the RAW decoders arrive in Phase 1), 16-bit TIFF next, JPEG last.

## Adding a stack to download

1. Put the frames where they have stable URLs that will not change, for example a release of a GitHub repository of your own, or a file host with permanent links.
2. Run `cargo xtask corpus --hash path/to/frames` and paste its `[[stack.files]]` entries into `manifest.toml` under a new `[[stack]]`, filling in each `url`. List the frames in focus order.
3. Fill in `name`, `subject`, `frame_order`, `source` (the page the stack comes from), `license` and, when the license asks for credit, `attribution`. `manifest.toml` documents every key and shows a complete example.
4. Run `cargo xtask corpus <name>`: it downloads the stack and checks every file. Commit the manifest change, never the files.

To replace a placeholder, delete its `placeholder = true` line and fill it in the same way.

## Format fixtures

The codec tests (`crates/codecs/tests/fixtures.rs`) read files that other software wrote, because a round trip through FloraStack's own writer can't catch a misreading that the writer shares, and the RAW tests (`crates/raw/tests/corpus.rs`) read what cameras wrote. Each `[[fixture]]` in the manifest is one set:

| Set | Source | License | What it covers |
|---|---|---|---|
| `pngsuite` | [PngSuite](http://www.schaik.com/pngsuite/) 2017-07-19, by Willem van Schaik | PngSuite license | Every PNG color type and bit depth, interlacing, filters, gamma and chunk variants, and damaged files |
| `tiff` | [image-tiff's test images](https://github.com/image-rs/image-tiff/tree/3302936b08e2260d4c66f40f51170efbb1fded4c/tests/images) | MIT | 8, 16 and 32-bit float TIFFs written by libtiff: LZW, Deflate and PackBits, predictors, strips and tiles, BigTIFF, both byte orders, alpha |
| `webp` | [image-webp's test files](https://github.com/image-rs/image-webp/tree/f4d80bd965df2c81e65b6f43c1f70e0750bd4b0f/tests) | MIT OR Apache-2.0 | Lossy, lossless and palette WebP, with the PNGs libwebp decodes them to |
| `openexr` | [openexr-images](https://github.com/AcademySoftwareFoundation/openexr-images/tree/e38ffb0790f62f05a6f083a6fa4cac150b3b7452) | BSD-3-Clause | Gray ramps, every half value, Rec. 709 and XYZ chromaticities, and fuzzer-found damaged files |
| `raw` | [raw.pixls.us](https://raw.pixls.us/), by its contributors | CC0-1.0 (each file's own mark in the site's listing) | One camera RAW file per format florastack-raw decodes, the smallest suitable CC0 sample of each: DNG (a camera's, and one demosaiced by Adobe DNG Converter), CR2, CR3, ARW, NEF, RAF (X-Trans and Bayer), RW2, PEF, ORF and SRW. About 107 MB, nearly all of the fixtures together; CI keeps the fixtures in a cache between runs |

A set lists its files one by one (`[[fixture.files]]`, like a stack's), or, when its source ships one archive, as PngSuite does, an `[fixture.archive]`: the archive's URL, sha256 and size, and the `members` to install from it. `cargo xtask corpus` checks the archive before extracting it with the system's `tar` in the staging folder, then checks every listed member again and installs only those. Pin files by commit (a `raw.githubusercontent.com/<owner>/<repo>/<commit>/...` URL), never by branch, so that they can't change under the hash.

A test finds its files with `florastack_testkit::fixture(set, name)` or `fixtures(set)`, and calls `skip!(Corpus, ...)` when the set is missing. To add a set, add a `[[fixture]]` with its source, license and attribution, list the files with `cargo xtask corpus --hash --inline <folder>`, and run `cargo xtask corpus <name>`. Only files whose license allows redistribution and testing belong here.

## Adding a local stack

For stacks that you can't or don't want to publish, make a folder `local/<name>/`, copy the frames into it and add a `stack.toml`:

```toml
subject = "insect"              # insect, flower, fungus, coin, landscape or microscopy
frame_order = "near-to-far"     # which way focus moves through the frames: near-to-far, far-to-near or unknown
description = "Weevil at 4x, rail, 120 frames, ISO 100"   # optional

# Optional: the frames in focus order. Without it, every image file in the folder is a frame, in file-name order.
# frames = ["IMG_0101.CR3", "IMG_0102.CR3"]

# Optional: what is known for sure, for judging results.
[ground_truth]
notes = "The antenna tips are sharp only in frames 90 to 100; frame 57 shows the left leg moving."
# reference = "retouched.tif"   # a file in the folder holding a trusted result; it is not taken as a frame
```

`cargo xtask corpus` checks every local stack (that `stack.toml` parses, that the frames it lists exist, that there is at least one frame) and lists it next to the downloaded ones. Local stacks are never downloaded, hashed or changed.

`cargo xtask quality` stacks every local stack by every method, with the downloaded ones (`--stack local/<name>` for one, `--no-local` for none), and scores the outputs against the `reference` when there is one. Its report and crops stay in the gitignored `target/quality/`, and local stacks never go into the committed baseline, `quality/baseline.json`.
