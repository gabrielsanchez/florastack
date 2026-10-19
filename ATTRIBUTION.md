# Attribution

This file lists every third-party project that FloraStack takes code, data, assets or design from, and the main dependencies chosen for core jobs such as image codecs, RAW decoding and color management. FloraStack's own code is MIT OR Apache-2.0 (see [`LICENSE-MIT`](LICENSE-MIT), [`LICENSE-APACHE`](LICENSE-APACHE) and [`NOTICE`](NOTICE)).

When a change adds such a source, add its row in the same commit. Only permissively licensed sources and published papers may be used. Never take code, shaders or assets from proprietary focus stackers or from GPL/LGPL image projects.

| Name | Source | License | Used for |
|---|---|---|---|
| PhotoCraft | <https://github.com/storytold/photocraft> | MIT OR Apache-2.0 | Design reference for the engine-first workspace layout (library crates under `crates/`, thin `apps/`, `cargo xtask` tooling), the workspace lint settings, the crate-layering check, the never-crash rules in `AGENTS.md` and the CI conventions (`--locked` builds, actions pinned to a commit). No code copied. |
| LightCraft | <https://github.com/storytold/lightcraft> | MIT OR Apache-2.0 | The same design references as PhotoCraft, plus the `rustfmt.toml` style and the design of `cargo xtask ci`. The code adapted from it has its own row. |
| LightCraft test skip log and CI report | <https://github.com/storytold/lightcraft/blob/d999e382bafbb5d5c54482fdb1d11f6a63bfdf69/crates/testkit/src/lib.rs> and <https://github.com/storytold/lightcraft/blob/d999e382bafbb5d5c54482fdb1d11f6a63bfdf69/xtask/src/ci_report.rs> (commit `d999e38`) | MIT OR Apache-2.0 | Code adapted into `crates/testkit/src/lib.rs` (the `skip!` macro and its JSON-lines skip log) and `xtask/src/ci.rs` (reading that log and summarizing the skipped tests by need and reason in `cargo xtask ci`). Copyright notice in `NOTICE`. |
