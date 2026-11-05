# Contributing to FloraStack

Thank you for helping. Bug reports with a sample stack, fixes, documentation and new features are all welcome. This
page says how to get started and what a change needs before it can be merged.

## Reporting a problem

[Open an issue](https://github.com/gabrielsanchez/florastack/issues/new/choose) and pick the form that fits: a bug, a
stacking result that looks wrong, or an idea. Stacking problems depend on the photos, so the forms ask for your
system, the image size, the frame count, the camera and, if you can share it, a sample stack: often a few frames are
enough to reproduce a problem. The [troubleshooting chapter](book/src/user/troubleshooting.md) may already have the
answer.

Report security problems privately, as [SECURITY.md](SECURITY.md) says, not in a public issue.

## Before you write code

- Read [AGENTS.md](AGENTS.md). It is the rule book for every change, by people and AI coding agents alike: the crate
  map and which crate may depend on which, the never-crash rules, the clean-room rule, the conventions and the
  per-change checklist.
- Look at [docs/ROADMAP.md](docs/ROADMAP.md) for what is done and what comes next, and the
  [developer guide](book/src/dev/architecture.md) for how the code is organized.
- For anything larger than a fix, open an issue first, so that we can agree on the approach before you spend time on it.

## Setting up

Install [rustup](https://rustup.rs); the toolchain is pinned in `rust-toolchain.toml` and installs itself on the first
build. On Windows you also need the Visual Studio C++ build tools ("Desktop development with C++"). The full checks
need a few more tools:

- Python 3.9 or newer with its development library, for the Python package's tests (`PYO3_PYTHON` selects the
  interpreter);
- `cargo install --locked cargo-deny cargo-audit`, for the dependency policy;
- Node.js and npm, for the glTF validator that one test uses;
- mdBook and lychee, for the book (`cargo xtask book` says which versions and how to install them).

The [README](README.md#building) lists the build commands.

## Making a change

1. Work on a branch of its own, in small commits that each build and pass the tests.
2. Add tests: every new command is dispatched from JSON in a test, every crash fix comes with a regression test that
   crashed before the fix, and a change that moves stacking quality on purpose updates the quality baseline
   (`cargo xtask quality --update-baseline`).
3. Update the documentation with the behavior: the [book](book/src/SUMMARY.md) chapter that describes it, the command
   reference when a command changes (`cargo xtask book --update-reference`), the README when building or running
   changes, and the roadmap.
4. Run `cargo xtask ci`, which runs everything CI runs, and fix what fails. Its summary lists the tests that were
   skipped for want of a GPU or corpus files; say which in your pull request.

## Pull requests

Describe what the change does and why, link the issue, say how you tested it (and on which systems), and add a
screenshot for a change to the desktop app. Keep unrelated changes for another pull request. CI runs on Linux, macOS
and Windows, and a pull request is merged once it passes and has been reviewed.

## Where code may come from

FloraStack's code is original or comes from sources whose licenses allow it, recorded in
[ATTRIBUTION.md](ATTRIBUTION.md):

- Never read or copy code from enfuse/enblend, CombineZP, darktable, RawTherapee, LibRaw, dcraw or rawler/rawloader
  (GPL and LGPL), not even to see how something is done. Work from the papers they build on, and cite the paper in the
  doc comment of the code that implements it.
- Never use the code, shaders or assets of proprietary stackers, nor reverse-engineer them.
- Code adapted from a permissively licensed project (focus-stack, PhotoCraft, LightCraft) gets a row in ATTRIBUTION.md
  and its copyright line in NOTICE, in the same commit, and a comment at the adapted code naming its source.
- New dependencies must be permissively licensed (`cargo deny check` enforces it) and preferably pure Rust.

## AI coding agents

Changes written with an AI coding agent are welcome under the same rules: AGENTS.md is written for agents too. The
person who opens the pull request is responsible for it, for having read it, and for the clean-room rule having been
followed.

## License

FloraStack is licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or the
[MIT license](LICENSE-MIT), at your option. Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in FloraStack by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.
