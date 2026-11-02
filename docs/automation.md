# Automation

All processing and project edits use the engine command registry. `florastack-cli commands` prints its command
names, help, examples, input schemas and result schemas. The same commands work in MCP, control and Python.

## MCP over stdio

Build `cargo build -p florastack-cli --release`. Configure an MCP client to launch:

```json
{"mcpServers":{"florastack":{"command":"/absolute/path/to/florastack-cli","args":["mcp"]}}}
```

On Windows the executable has an `.exe` suffix. The server uses the [official Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk).
Its stdout contains only MCP messages. Each registry command is a tool with its original name, parameter schema,
result schema and help. For example, call `project.new` with `{}`, `project.addFrames` with `{"paths":["/photos"]}`,
`align.run` with `{}`, then `stack.pyramid` with `{}`. Tool calls wait for jobs and return their final result as
structured content and JSON text. Engine errors are tool results with `isError: true`, including stable error codes.
An MCP cancellation asks the engine job to stop at its next checkpoint. Job progress is sent when the client supplies a progress token.

`florastack://project` is a JSON resource with the current project, path, dirty flag and jobs. Outputs with cached
previews add `florastack://outputs/<id>/thumbnail`, containing a base64 JPEG. URIs resolve only inside the current
project; arbitrary files cannot be read as resources. Read the project resource again after a command to refresh it.

By default MCP owns an isolated headless engine. `--device cpu` before `mcp` selects CPU processing. Unnamed projects
are kept in the CLI's temporary-project root; save one with `project.saveAs` to keep it elsewhere.

## Desktop control and bridging

Generate a token file with Python (or an equivalent cryptographic random generator):

```sh
python -c "import os,secrets; f=open(os.open('control-token',os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600),'w'); f.write(secrets.token_hex(32)); f.close()"
florastack --control --control-token-file control-token
```

Keep that file private. On Windows use a user-private directory/ACL. The app prints `control: 127.0.0.1:PORT` to
stderr, without the token. `--control-port PORT` chooses a fixed port; zero chooses an available port. Control is
disabled unless `--control` is supplied. It always binds to IPv4 loopback; clients accept only numeric loopback
addresses. There is no HTTP listener. Authentication is required on every request, before it reaches the UI thread.
The token grants the same local file, project and device capabilities as engine commands.

To bridge MCP to that app, launch:

```sh
florastack-cli mcp --connect 127.0.0.1:PORT --token-file control-token
```

Tools, resources and jobs then use the app's engine and undo history. The app remains responsive during jobs.

The control protocol is one UTF-8 JSON line and one response per TCP connection, ending with LF. For example:

```json
{"id":1,"token":"<file contents>","method":"command.dispatch","params":{"command":"project.info","params":{}}}
```

The response is `{"id":1,"result":...}` or `{"id":1,"error":{"code":"...","message":"..."}}`.
`command.dispatch` returns `{"result":...}` immediately or `{"job":ID}` for a job. Poll `job.get` with `{"job":ID}`;
read its `state`, `progress`, `result` and `error`. Cancel through `command.dispatch` with command `job.cancel`.

| Method | Parameters / result |
|---|---|
| `commands.list` | `{}` → registry definitions |
| `command.dispatch` | `{"command":"align.run","params":{}}` → result or job |
| `job.get` | `{"job":ID}` → current job state |
| `resources.list` / `resources.read` | `{}` / `{"uri":"florastack://project"}` → MCP-shaped resources/contents |
| `ui.state` | `{}` → tab, zoom, viewer readiness, frame/output counts |
| `ui.setView` | Optional `tab` (`stack`, `retouch`, `export`, `batch`, `capture`), `frame` or `output` ID, and `zoom` |
| `ui.screenshot` | `{"path":"/absolute/path/ui.png","overwrite":false}` → path, width, height after the PNG is complete |
| `ui.close` | `{"discardChanges":false}`; dirty projects require saving or explicit discard |

Requests and responses are limited to 24 MiB; thumbnail blobs to 4 MiB. A connection has a 30-second deadline,
with at most 16 connected clients/queued requests. Long commands return job IDs so they do not consume the deadline.
There are no automatic retries of mutations. Treat a transport timeout during a mutation as an uncertain outcome
and inspect project/job state before deciding whether to retry.

## Offscreen screenshots

`florastack --headless --control --control-token-file control-token` runs the same UI into a wgpu texture without
opening a window or needing a display server. `--width 1200 --height 720` sets its dimensions (up to 4096 per side).
A hardware or software adapter is required; Windows WARP and Mesa lavapipe work. `ui.screenshot` uses that rendered
texture, including the GPU image viewer. In a windowed app it captures the rendered viewport. Wait for
`ui.state.ready` after selecting an image before requesting its screenshot.
`--temporary-projects FOLDER` isolates unnamed projects from the normal desktop data directory; the screenshot script uses it.

The README images are generated by this script through control, including loading, aligning and stacking:

```sh
cargo build -p florastack
python scripts/readme_screenshots.py
# Or use your own stack:
python scripts/readme_screenshots.py --frames photos/001.tif photos/002.tif
```

The default demo is an original synthetic flower generated with NumPy, kept only in a temporary directory.
Only the screenshots are saved to `docs/images/`. No token, input frames or project files are committed.

## Python research pipelines

The separate `python/florastack` crate uses [PyO3](https://pyo3.rs/) and [maturin](https://www.maturin.rs/).
For source builds, install Python 3.9+ with its development library and the pinned Rust toolchain, then:

```sh
python -m venv .venv
# Activate .venv (on Windows: .venv\Scripts\activate; on Unix: source .venv/bin/activate)
python -m pip install 'maturin>=1.12,<2' numpy pytest
python -m maturin develop --manifest-path python/florastack/Cargo.toml --locked
python -m pytest python/florastack/tests
```

```python
import florastack

with florastack.Project() as project:
    project.load_frames(["near.tif", "middle.tif", "far.tif"])
    project.align(rotation=False)
    image = project.stack("pyramid")  # also "average" or "depth-map"
    print(image.shape, image.dtype)   # (height, width, channels), float32
    settings = project.run("stack.get")
# image remains valid here and can be passed to NumPy/SciPy routines.
```

Arrays own their data, are C-contiguous and retain full precision, linear encoding and the output's primaries
(RGB, or RGBA for wide framing with uncovered pixels). No display curve, quantization or color conversion is
applied. `project.output(ID)` reads an existing stacked output. Retouching/annotation rendering uses the export
commands. Processing defaults to CPU; `project.run("compute.set", {"device":"gpu"})` explicitly requests GPU use.
Native work releases the GIL, and Ctrl+C cancels a running job. Engine failures raise `RuntimeError`; malformed
JSON and invalid Python arguments raise Python exceptions. Use `with` or `close()` to release project locks.
`load_frames` returns the registry's `added` IDs and `skipped` files/reasons; check `skipped` for unreadable or duplicate inputs.

`cargo xtask python` builds and tests in `target/python-tests`, and is part of `cargo xtask ci` and CI on all three
platforms. `PYO3_PYTHON` selects a particular interpreter for Rust builds and the test environment's creation.
Wheel publication belongs to Phase 7.
