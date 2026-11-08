# Writing the documentation

The user and developer guide is an [mdBook](https://rust-lang.github.io/mdBook/) in `book/`: `book/book.toml`
configures it, `book/src/SUMMARY.md` is its table of contents, and each chapter is a Markdown file under
`book/src/user/`, `book/src/reference/` or `book/src/dev/`. The pages read the same on GitHub, so keep them plain
Markdown, without mdBook-only syntax.

## Building and checking it

```sh
cargo xtask book                       # build it into target/book/ and check every link
cargo xtask book --online              # check links to other sites too
cargo xtask book --update-reference    # write the engine-command reference again
```

`cargo xtask book` builds the book with mdBook, then checks with [lychee](https://lychee.cli.rs) every link in it and
in the repository's other Markdown files (the README, AGENTS.md, CONTRIBUTING.md, SECURITY.md, the roadmap and the
rest): links between pages, links to headings within them, and links to the repository's own files. Those are written
as GitHub URLs (`https://github.com/<owner>/florastack/blob/main/...`), so that they work in the published book too,
and checked against the files of your checkout, without a network request. Links to other sites are checked only with
`--online`, since a site that is down for a minute shouldn't fail CI. CI runs it in the `docs` job, and
`cargo xtask ci` with the other steps.

It needs mdBook 0.5.4 and lychee 0.24.2, the versions CI uses:

```sh
cargo install --locked mdbook --version 0.5.4
cargo install --locked lychee --version 0.24.2
```

or their release builds, from [mdBook's](https://github.com/rust-lang/mdBook/releases) and
[lychee's](https://github.com/lycheeverse/lychee/releases) release pages, on your `PATH`. `mdbook serve book` shows
the book in a browser while you write, rebuilding on every save.

## The engine-command reference

[Engine commands](../reference/commands.md) is generated from the registry by `crates/engine/tests/reference.rs`: every
command's name, texts, flags, parameters, result and examples. The test fails when the committed page differs from
what the registry gives, so a change to a command (its help, a parameter's doc comment, an example) updates the page in
the same commit: run `cargo xtask book --update-reference` and commit the result. Never edit the page by hand.

## Screenshots

The screenshots of the desktop app are made through its control channel, offscreen, by
`scripts/readme_screenshots.py`: it starts the app headless, adds the frames, aligns and stacks them, and captures each
tab, as WebP files in `book/src/images/`, which the README uses too.

```sh
cargo build -p florastack
python scripts/readme_screenshots.py --frames corpus/local/mosquito-jpg/*.JPG --name Mosquito
```

Make them again when a tab they show changes visibly. It needs Pillow for WebP (`--format png` writes PNG files
without it). [Automation, MCP and Python](../user/automation.md#offscreen-screenshots) describes the control channel.

## The method crops

The before/after crops of [Choosing a method](../user/methods.md) come from `scripts/method_crops.py`, which stacks a
real stack by every method through the command-line tool, exports the outputs and the aligned frames, and crops each
at the regions where the methods differ most, beside the frame that is sharpest there:

```sh
cargo build -p florastack-cli --release
python scripts/method_crops.py corpus/local/mosquito-jpg --out book/src/images/methods
```

It needs NumPy and Pillow. `--region X,Y` crops at chosen places instead. It writes `regions.json` beside the crops,
which records where each crop is and which frame it shows. The book's crops are at the regions chosen before the
wavelet method was added, which its text describes: to make them again, pass those regions with
`--region 1840,2080 --region 2000,2400 --region 2720,1360`.

## Photos

The book's photos (the screenshots' and the crops' stacks) are the project maintainer's, licensed under CC BY 4.0 and
recorded in ATTRIBUTION.md. Use only photos whose license allows it, credit them as their license asks, and record
them there too. The corpus's local stacks stay out of the repository: only crops and screenshots of them, at the
sizes the book shows, may be committed, with their owner's permission.

## Style

Write for the reader of that chapter: the user guide for photographers, who may never open a terminal; the developer
guide for contributors. Prefer short sentences and concrete steps, name buttons and menus as the app shows them, in
bold, and link to the chapter that says more rather than repeating it. When behavior changes, the chapter that
describes it changes in the same commit.
