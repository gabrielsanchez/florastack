# The browser build

The browser version ([In the browser](../user/browser.md)) is the desktop app's library, `florastack_app`, compiled
for `wasm32-unknown-unknown` with threads, and started on a page by eframe's web runner. There is no separate web
app: the same panels, the same engine, the same commands. This chapter says how it is built, what makes it work in a
browser, and how it is tested.

## Building

```sh
rustup toolchain install nightly-2026-11-01 --profile minimal --component rust-src
cargo install wasm-bindgen-cli --version <the wasm-bindgen version in Cargo.lock> --locked
cargo xtask web            # the site, in target/web-site
cargo xtask web --serve    # and serve it on http://127.0.0.1:8080/ with the headers it needs
cargo xtask web --test     # and run the headless browser test
```

`cargo xtask web --help` lists the options (`--dev` builds quicker and stacks slower). What it does
(`xtask/src/web.rs`):

1. **Builds the app's library as a `cdylib`** (`cargo rustc --crate-type cdylib`, so that native builds stay plain
   libraries) with the `web` profile, which is the release profile under another name. Threads need atomics and
   shared memory, which the standard library is built without, so a **nightly** toolchain rebuilds it
   (`-Z build-std`), with the target features and linker arguments in `RUSTFLAGS`: a memory imported, shared, and
   up to 4 GiB, and the thread-local storage exports wasm-bindgen's glue starts threads with. The nightly is the
   one the fuzz targets pin (`fuzz/rust-toolchain.toml`); a test keeps the two the same.
2. **Runs wasm-bindgen** (`--target web`) for the JavaScript glue. Its CLI must be exactly the version of the library
   in `Cargo.lock`, which the workspace pins with `=`; a test checks that the two stay together.
3. **Fills `packaging/web/index.html.in` and `worker.js.in`** with the product identity, and copies the hosting
   files (`_headers`, `.htaccess`), the licenses and an icon.

The `Dockerfile` at the root does the same in a container and serves the site with nginx
(`packaging/web/nginx.conf`); `packaging/web/README.md` is for whoever hosts it.

## What the browser changes

A page is a different machine, with three rules the native code never had to follow:

- **The main thread may not block.** A wait on the page's main thread (a contended lock, a channel's `recv`, a
  rayon call) traps, which ends the program.
- **Threads are Web Workers**, which start only once the thread that made them returns to the browser's event loop.
- **There is no file system, and no clock or process id** in the standard library: its calls fail, or panic.

florastack-core supplies what the standard library can't, and is the standard library on native targets:

| Module | Native | Browser |
|---|---|---|
| `fs` | `std::fs` | `MemoryFs`, an in-memory file system with `std::fs`'s functions and types |
| `thread` | `std::thread` | Web Workers on the module's shared memory (`worker.js` starts them), and `start_pool` for rayon |
| `time` | `std::time` | The page's `performance.now()` and `Date.now()`, through `web-time` |
| `sync` | `std::sync` | Locks that spin on the main thread (`wasm_sync`'s mutex), and a `OnceLock` initialized under one |
| `process` | `std::process::id` | A number drawn once per page |

The crates that run in the browser use these: `std::fs` and `Path::exists` and its kind are
`florastack_core::fs` and its `path_exists`, `path_is_file` and `path_is_dir`, `std::env::temp_dir` is
`fs::temp_dir`, `std::path::absolute` is `fs::absolute`, and clocks come from `florastack_core::time`.
(`wasm_sync`'s own `OnceLock` reads the standard library's private state, and spins forever with the nightly's: core's
`sync::OnceLock` is the standard library's, initialized under a spinning mutex instead.) Rayon is built with its `web_spin_lock` feature, so that
a rayon call made on the main thread spins until the pool's workers are done rather than traps; long work still
belongs in jobs, which run on workers.

Then, in the browser:

- **Jobs** run on the engine's `LaneExecutor`: one long-lived worker per lane, since workers are slow to start.
  `Engine::wait` doesn't exist; the app polls, as it always has. Closing a project asks its jobs to stop and refuses
  until they have, rather than wait for them.
- **Spilled tiles** go to the origin-private file system: the browser build's `TempFileStore` (florastack-core's
  `opfs.rs`) sends its reads and writes to one storage worker, which owns every store's file, since synchronous file
  access exists only in workers and opening a file is asynchronous.
- **Stacking on the GPU** uses WebGPU from a GPU worker that owns the device: wgpu's objects can't move between
  threads, and readbacks must be awaited, which a job can't do. The engine's `webgpu` module runs the device's
  self-test there and stacks through it, the job sending each frame and waiting for the answer; florastack-gpu's
  `finish_async` and `self_test_async` read results back by awaiting them. A stack whose working data outgrows the
  GPU's budget, which would need readbacks during the frames, starts again on the CPU, as a GPU failure does natively.
- **The app** (`apps/florastack/src/web.rs`) starts rayon's pool and the storage worker, then eframe's web runner on the
  page's canvas, drawn with WebGPU or WebGL. Files come from the hidden file input and from drops, are written to the
  in-memory file system's `/files`, and reach the app as paths; anything written under `/downloads` is downloaded.
  `dialogs.rs` hands out `/downloads` folders where the desktop's file dialogs pick folders. What needs the user's
  folders or devices is left out with `cfg`: opening and saving projects elsewhere, recent projects, the batch and
  capture tabs, the control channel. Workers never touch egui's locks: they ask for no repaints, and the app
  repaints on a timer while jobs run or images load.
- **Memory**: the module may grow to 4 GiB. Files given to the page may take half of that, and a stack checks that
  its frames and working data fit before it starts, with a message that says what to do when they don't.

## Testing

`cargo xtask web --test` is the acceptance test: the engine's `web_stack` example writes a 20-frame synthetic stack
(a layered scene from `florastack_core::testutil`) and stacks it natively; then `apps/florastack/tests/web/stack.mjs`,
with Playwright's library (installed outside the repository at the version `xtask/src/web.rs` pins, as the glTF
validator is), opens the site in a headless Chrome or Edge, gives the page the frames through its file input,
aligns and stacks them by the pyramid through `window.florastack.dispatch`, and downloads the output; the example
then compares it with the native one, which it must match within 40 dB of PSNR. `FLORASTACK_BROWSER` names another
browser: a Playwright channel (`chrome`, `msedge`, `chromium`) or a path.

The native unit tests cover what can be tested natively: the in-memory file system, the lane executor, the server's
paths. CI's `web` job (`.github/workflows/ci.yml`) builds the site and runs the test on Linux with the runner's
Chrome; `cargo xtask ci` runs the same step.
