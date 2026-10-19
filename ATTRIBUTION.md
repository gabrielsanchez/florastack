# Attribution

This file lists every third-party project that FloraStack takes code, data, assets or design from, and the main dependencies chosen for core jobs such as image codecs, RAW decoding and color management. FloraStack's own code is MIT OR Apache-2.0 (see [`LICENSE-MIT`](LICENSE-MIT), [`LICENSE-APACHE`](LICENSE-APACHE) and [`NOTICE`](NOTICE)).

When a change adds such a source, add its row in the same commit. Only permissively licensed sources and published papers may be used. Never take code, shaders or assets from proprietary focus stackers or from GPL/LGPL image projects.

| Name | Source | License | Used for |
|---|---|---|---|
| PhotoCraft | <https://github.com/storytold/photocraft> | MIT OR Apache-2.0 | Design reference for the engine-first workspace layout (library crates under `crates/`, thin `apps/`, `cargo xtask` tooling), the workspace lint settings and the crate-layering check. No code copied. |
| LightCraft | <https://github.com/storytold/lightcraft> | MIT OR Apache-2.0 | The same design references as PhotoCraft, plus the `rustfmt.toml` style. No code copied. |
