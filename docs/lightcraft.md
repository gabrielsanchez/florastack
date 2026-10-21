# The LightCraft dependency

FloraStack reads and writes image files, and decodes and develops camera RAW files, with the help of [LightCraft](https://github.com/storytold/lightcraft)'s crates (MIT OR Apache-2.0), rather than writing its own TIFF, EXIF, XMP and RAW code. This page says what is pinned, how the dependency is kept in bounds, and how to move the pin.

## What is pinned

LightCraft doesn't publish its crates to crates.io (the `lightcraft` crate there is an unrelated project), so they come from its git repository, all at one commit: `a2a41383c4fd62fa85a46ccd00998639221f2d3c`, "Release 0.10.0" on `main`. The commit is written once per crate in `[workspace.dependencies]` in the root `Cargo.toml`, and `Cargo.lock` records it.

| Crate | Features | Used by | What for |
|---|---|---|---|
| `lightcraft-tiff` | (has none) | codecs | Reading and writing TIFF directories: the EXIF block FloraStack writes, and the TIFF and DNG writers |
| `lightcraft-codecs` | `parallel` only | codecs, raw | The JPEG and PNG encoders, format sniffing, ICC profile parsing and writing, and the color-managed decoder used for CMYK files and LUT-based profiles; for `florastack-raw`, the linear Rec. 2020 profile that developed frames are tagged with (the same bytes `florastack-codecs` writes for that space) |
| `lightcraft-meta` | (has none) | codecs | Finding EXIF, IPTC and XMP in a file, parsing them, and merging XMP |
| `lightcraft-color` | (has none) | codecs, raw | Chromaticities, matrices, Bradford adaptation, and color temperature and tint |
| `lightcraft-raw` | `jxl` only | codecs, raw | For `florastack-codecs`, the lossless-JPEG encoder of compressed DNG. For `florastack-raw`, everything about RAW files: the decoders, normalization (black and white levels, DNG opcode lists, active area), the demosaics (bilinear, AHD, X-Trans), highlight reconstruction, the DNG color model and the measured camera matrices |
| `lightcraft-pipeline` | (has none) | raw | Three pieces of the develop pipeline that `florastack-raw` calls on their own: noise reduction (`local::apply_nr`), lateral chromatic-aberration estimation (`optics::estimate_lateral_ca`) and automatic white balance (`auto::auto_wb`). Its renderer, tone mapping, filmic curve and looks are never called: developed frames stay linear |
| `lightcraft-develop` | (has none) | raw | The edit model whose noise-reduction sliders `local::nr_params` reads, so that FloraStack's strengths map to LightCraft's parameters as LightCraft maps its own |
| `lightcraft-raster` | `parallel` (its default) | raw | The RGB image type of the steps above, and the resampler for sensors with non-square photosites |

`lightcraft-pipeline` brings `lightcraft-inpaint` and `lightcraft-geom` along (both pure Rust; `lightcraft-inpaint` ports PhotoCraft's inpainting code, which `NOTICE` credits). None of these is LightCraft's engine, catalog, GPU or UI crate, and `cargo tree -p florastack-raw -e normal,build` shows no `-sys` crate, `cc` or `cmake`.

`lightcraft-codecs`' default features add JPEG XL (`jxl`) and AVIF encoding (`avif`), which FloraStack doesn't need; `avif` would bring in rav1e and, through it, libfuzzer-sys, whose build compiles C++ with the `cc` crate. `lightcraft-raw`'s `jxl` feature (its default, which the workspace manifest turns off and `florastack-raw` turns back on) reads JPEG XL compressed DNGs (DNG 1.7), which newer versions of Adobe DNG Converter and Lightroom write, through jxl-oxide (pure Rust, MIT OR Apache-2.0, with brotli-decompressor, BSD-3-Clause or MIT). It is on because DNG is how users of other raw converters bring their files, and it costs no C code; Cargo's feature unification turns it on for `florastack-codecs` too, which never reads DNG. Neither crate pulls in a C library with the features above.

What `florastack-raw` leaves out of LightCraft's develop code on purpose, because stacking needs linear, scene-referred data and the same geometry in every frame: the pipeline's renderer with its tone map, filmic shoulder and looks; a DNG's own profile look (`ProfileHueSatMap`, `ProfileLookTable`, `ProfileToneCurve`, `ProfileGainTableMap`) and `BaselineExposure`; and the lens warps a file records (DNG `WarpRectilinear` and the cameras' own corrections), unless `DevelopSettings::lens_distortion` asks for them. The camera looks LightCraft's engine fits to a camera's embedded JPEG, and its bundled camera profiles, live in its engine crate, which FloraStack doesn't use: cameras without a color matrix of their own or in `lightcraft_raw::spectral` develop with a neutral fallback model, and `florastack-raw` says so (`ColorSource::Fallback`).

LightCraft's types never appear in FloraStack's public APIs. `florastack-codecs` wraps their errors in its opaque `BackendError` and exposes its own `ImageDecoder` and `ImageEncoder` traits; `florastack-raw` does the same with its own `BackendError`, its `RawDecoder` trait and the plain data of `RawFrame` (normalized samples, the color filter array, the DNG color model). So the pin can move, or another implementation replace a crate, without changing a caller: a new LightCraft commit touches `crates/raw/src/decoder.rs` and the steps in `develop.rs` and `color.rs` at most, and another RAW decoder implements `RawDecoder`.

## Policy exceptions

The dependency needs three exceptions to the dependency policy, each in `deny.toml` with its reason:

- **A git source.** `[sources] allow-git` lists `https://github.com/storytold/lightcraft`, and `required-git-spec = "rev"` makes every git dependency name a commit, never a branch or a tag.
- **The Independent JPEG Group's license**, allowed for `jpeg-encoder` alone, which `lightcraft-codecs` and `lightcraft-raw` use. It is permissive and asks only for the credit line that `NOTICE` gives.
- **Two quick-xml advisories, ignored.** `lightcraft-meta` uses quick-xml 0.38.4, which RUSTSEC-2026-0194 and RUSTSEC-2026-0195 cover. Neither reaches it: it reads attributes with `with_checks(false)`, so it never runs the duplicate-attribute check of the first, and it never uses `NsReader`, the subject of the second. quick-xml 0.41 fixes both, and LightCraft has the bump open (storytold/lightcraft#1464, and Dependabot's #2265). `.cargo/audit.toml` ignores the same two ids for `cargo audit`.

One more pin is FloraStack's own: `exr` is held at `=1.74.0`, because 1.74.1 and later depend on `pulp`, which brings in `paste`, an unmaintained crate (RUSTSEC-2024-0436). That dependency came with 1.74.1's DWA compression, so until the pin moves, DWA-compressed OpenEXR files are reported as unsupported, with the advice to save them with ZIP or PIZ compression. Revisit it when exr drops that dependency.

## Moving the pin

1. Pick a LightCraft commit on `main`, preferably a release, and read what changed in the eight crates since the current pin, and in its `docs/raw-parity.md` (which RAW formats and variants decode).
2. Replace the commit in all eight `lightcraft-*` lines of `[workspace.dependencies]` and build once (`cargo check --workspace`), so that Cargo records the new commit in `Cargo.lock`. Commit the lock file with the change.
3. Check what the new commit brings in. `cargo tree -p florastack-codecs -e normal,build` and `cargo tree -p florastack-raw -e normal,build` must not show a `-sys` crate, `cc` or `cmake` (at the current pin they show none), nor a LightCraft crate beyond those listed above (its engine, catalog, GPU and UI crates in particular); if a feature now needs one, turn it off or discuss it first (AGENTS.md, Pure Rust).
4. If the new commit uses quick-xml 0.41 or later, delete the two advisory ignores from `deny.toml` and `.cargo/audit.toml`.
5. Run `cargo xtask corpus --fixtures` and then `cargo xtask ci`. The codec tests read the PngSuite, TIFF, WebP and OpenEXR fixtures, and the RAW tests (`crates/raw/tests/corpus.rs`) one camera file per format, pinning the size, layout, color source and orientation each decodes to, so they catch a change in decoding. Such a change may be a fix: compare with LightCraft's `docs/raw-parity.md` before updating the expectation. Update the name that `LightCraftRaw` reports (`crates/raw/src/decoder.rs`) too.
6. For the browser build (roadmap 7.3), check the codecs and the RAW importer against its target: `rustup target add wasm32-unknown-unknown`, then `cargo check -p florastack-codecs -p florastack-raw --target wasm32-unknown-unknown`. CI doesn't run this yet: LightCraft's first five crates built for wasm32 at the current pin when it was chosen, but `florastack-codecs`, `florastack-raw` and the pipeline, develop and raster crates haven't been checked for that target.
7. Update the commit in `ATTRIBUTION.md`, `NOTICE` and this page.

## Vendoring or forking

The first build downloads LightCraft from GitHub into Cargo's git cache (`~/.cargo/git`). To build without network access, or if the repository goes away:

- **Vendor**: `cargo vendor` copies every dependency, LightCraft included, into a folder and prints the `[source]` configuration that points Cargo at it.
- **Fork**: push the pinned commit, plus any fix, to a repository you control, point the eight `git =` URLs at it, and add that URL to `allow-git` in `deny.toml`. Keep naming a `rev`.
- **Work on LightCraft locally**: a `[patch."https://github.com/storytold/lightcraft"]` section in the root `Cargo.toml` that points each crate at a local checkout (`path = "../lightcraft/crates/codecs"` and so on). Don't commit it.
