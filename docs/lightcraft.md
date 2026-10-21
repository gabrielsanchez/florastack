# The LightCraft dependency

FloraStack reads and writes image files with the help of [LightCraft](https://github.com/storytold/lightcraft)'s low-level crates (MIT OR Apache-2.0), rather than writing its own TIFF, EXIF and XMP code. This page says what is pinned, how the dependency is kept in bounds, and how to move the pin.

## What is pinned

LightCraft doesn't publish its crates to crates.io (the `lightcraft` crate there is an unrelated project), so they come from its git repository, all at one commit: `a2a41383c4fd62fa85a46ccd00998639221f2d3c`, "Release 0.10.0" on `main`. The commit is written once per crate in `[workspace.dependencies]` in the root `Cargo.toml`, and `Cargo.lock` records it.

| Crate | Features | What `florastack-codecs` uses it for |
|---|---|---|
| `lightcraft-tiff` | (has none) | Reading and writing TIFF directories: the EXIF block FloraStack writes, and the TIFF and DNG writers |
| `lightcraft-codecs` | `parallel` only | The JPEG and PNG encoders, format sniffing, ICC profile parsing and writing, and the color-managed decoder used for CMYK files and LUT-based profiles |
| `lightcraft-meta` | (has none) | Finding EXIF, IPTC and XMP in a file, parsing them, and merging XMP |
| `lightcraft-color` | (has none) | Chromaticities, matrices and Bradford adaptation |
| `lightcraft-raw` | none of its defaults | The lossless-JPEG encoder for compressed DNG; Prompt 1.3 adds RAW decoding through it in `florastack-raw` |

`lightcraft-codecs`' default features add JPEG XL (`jxl`) and AVIF encoding (`avif`), which FloraStack doesn't need; `avif` would bring in rav1e and, through it, libfuzzer-sys, whose build compiles C++ with the `cc` crate. `lightcraft-raw`'s default `jxl` feature reads JPEG XL compressed DNGs, which Prompt 1.3 may turn on. Neither crate pulls in a C library with the features above.

LightCraft's types never appear in FloraStack's public APIs. `florastack-codecs` wraps their errors in its opaque `BackendError` and exposes its own `ImageDecoder` and `ImageEncoder` traits, so the pin can move, or another implementation replace a crate, without changing a caller.

## Policy exceptions

The dependency needs three exceptions to the dependency policy, each in `deny.toml` with its reason:

- **A git source.** `[sources] allow-git` lists `https://github.com/storytold/lightcraft`, and `required-git-spec = "rev"` makes every git dependency name a commit, never a branch or a tag.
- **The Independent JPEG Group's license**, allowed for `jpeg-encoder` alone, which `lightcraft-codecs` and `lightcraft-raw` use. It is permissive and asks only for the credit line that `NOTICE` gives.
- **Two quick-xml advisories, ignored.** `lightcraft-meta` uses quick-xml 0.38.4, which RUSTSEC-2026-0194 and RUSTSEC-2026-0195 cover. Neither reaches it: it reads attributes with `with_checks(false)`, so it never runs the duplicate-attribute check of the first, and it never uses `NsReader`, the subject of the second. quick-xml 0.41 fixes both, and LightCraft has the bump open (storytold/lightcraft#1464, and Dependabot's #2265). `.cargo/audit.toml` ignores the same two ids for `cargo audit`.

One more pin is FloraStack's own: `exr` is held at `=1.74.0`, because 1.74.1 and later depend on `pulp`, which brings in `paste`, an unmaintained crate (RUSTSEC-2024-0436). That dependency came with 1.74.1's DWA compression, so until the pin moves, DWA-compressed OpenEXR files are reported as unsupported, with the advice to save them with ZIP or PIZ compression. Revisit it when exr drops that dependency.

## Moving the pin

1. Pick a LightCraft commit on `main`, preferably a release, and read what changed in the five crates since the current pin.
2. Replace the commit in all five `lightcraft-*` lines of `[workspace.dependencies]` and build once (`cargo check --workspace`), so that Cargo records the new commit in `Cargo.lock`. Commit the lock file with the change.
3. Check what the new commit brings in. `cargo tree -p florastack-codecs -e normal,build` must not show a `-sys` crate, `cc` or `cmake` (at the current pin it shows none); if a feature now needs one, turn it off or discuss it first (AGENTS.md, Pure Rust).
4. If the new commit uses quick-xml 0.41 or later, delete the two advisory ignores from `deny.toml` and `.cargo/audit.toml`.
5. Run `cargo xtask corpus --fixtures` and then `cargo xtask ci`. The codec tests read the PngSuite, TIFF, WebP and OpenEXR fixtures, so they catch a change in decoding.
6. For the browser build (roadmap 7.3), check the codecs against its target: `rustup target add wasm32-unknown-unknown`, then `cargo check -p florastack-codecs --target wasm32-unknown-unknown`. CI doesn't run this yet: LightCraft's five crates built for wasm32 at the current pin when it was chosen, but `florastack-codecs` itself hasn't been checked for that target.
7. Update the commit in `ATTRIBUTION.md`, `NOTICE` and this page.

## Vendoring or forking

The first build downloads LightCraft from GitHub into Cargo's git cache (`~/.cargo/git`). To build without network access, or if the repository goes away:

- **Vendor**: `cargo vendor` copies every dependency, LightCraft included, into a folder and prints the `[source]` configuration that points Cargo at it.
- **Fork**: push the pinned commit, plus any fix, to a repository you control, point the five `git =` URLs at it, and add that URL to `allow-git` in `deny.toml`. Keep naming a `rev`.
- **Work on LightCraft locally**: a `[patch."https://github.com/storytold/lightcraft"]` section in the root `Cargo.toml` that points each crate at a local checkout (`path = "../lightcraft/crates/codecs"` and so on). Don't commit it.
