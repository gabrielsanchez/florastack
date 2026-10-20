# Test corpus

Real focus stacks for testing and scoring FloraStack. Two files in this folder are tracked: [`manifest.toml`](manifest.toml), which pins stacks to download, and this README. Everything else here is gitignored: downloads, local stacks and anything you put next to them. Never commit images to the repository.

```text
corpus/
├── manifest.toml           tracked: stacks to download, each file pinned by sha256
├── README.md               tracked: this file
├── stacks/<name>/          downloaded by `cargo xtask corpus`
└── local/<name>/           your own stacks, each with a stack.toml
    ├── stack.toml
    └── IMG_0001.CR3 ...
```

```sh
cargo xtask corpus               # download every stack in the manifest that is missing or fails its check, then list all stacks
cargo xtask corpus bee-head      # only that stack
cargo xtask corpus --check       # download nothing; fail if a pinned file is missing or wrong
cargo xtask corpus --hash DIR    # print the [[stack.files]] entries for the files in DIR
cargo xtask corpus --help
```

Downloads go through `curl` (it comes with Windows 10 and later, macOS and most Linux systems) with a generic User-Agent. Each file lands in a staging folder, is checked against its size and sha256, and only then moves into `stacks/<name>/`; a file that fails the check is deleted and the command fails. Files that already match are not downloaded again.

Tests that need corpus files call `florastack_testkit::skip!(Corpus, ...)` when they are missing, so `cargo xtask ci` lists them as skipped rather than letting them pass silently. Until real stacks are added, alignment and stacking are scored on the synthetic stacks of `florastack_core::testutil`, whose ground truth is exact.

## What to collect

The roadmap asks for 5 to 10 real stacks before Phase 2: a bristly insect, a flower, a coin and a handheld landscape at least, and a microscope stack if you have one. The manifest has a placeholder for each, describing what makes a good one. Prefer stacks you shot yourself or that come with a license that allows redistribution and testing (CC0, CC BY, public domain). RAW frames are best (the RAW decoders arrive in Phase 1), 16-bit TIFF next, JPEG last.

## Adding a stack to download

1. Put the frames where they have stable URLs that will not change, for example a release of a GitHub repository of your own, or a file host with permanent links.
2. Run `cargo xtask corpus --hash path/to/frames` and paste its `[[stack.files]]` entries into `manifest.toml` under a new `[[stack]]`, filling in each `url`. List the frames in focus order.
3. Fill in `name`, `subject`, `frame_order`, `source` (the page the stack comes from), `license` and, when the license asks for credit, `attribution`. `manifest.toml` documents every key and shows a complete example.
4. Run `cargo xtask corpus <name>`: it downloads the stack and checks every file. Commit the manifest change, never the files.

To replace a placeholder, delete its `placeholder = true` line and fill it in the same way.

## Adding a local stack

For stacks that you can't or don't want to publish, make a folder `local/<name>/`, copy the frames into it and add a `stack.toml`:

```toml
subject = "insect"              # insect, flower, coin, landscape or microscopy
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
