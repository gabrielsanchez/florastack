# Capture

The Capture tab's primary path on every platform is a watched folder. Pick an input folder, a separate results folder and a preset (`builtin:insect-macro`, for example), then **Watch folder and stack**. A preset file path can be pasted into the preset field. Photos that finish writing are decoded, aligned and stacked into a running preview. The final project, output, preview and CSV are saved after capture goes quiet. The standalone watch command processes one stack and exits; set its idle timeout longer than the interval between photos, including exposure and transfer time.

Many current cameras offer in-camera focus bracketing. Set the focus range, number of images and increment on the camera, and use its supported tethering software to transfer every original to the watched folder. A camera that brackets only to its card can also be used: copy the finished bracket to the folder, or open it directly in the Stack tab. The calculator below cannot translate physical spacing into a manufacturer's arbitrary focus increment.

On Windows, configure the camera maker's tethering app to save into the watched folder: [Canon EOS Utility](https://www.usa.canon.com/support/eos-utilities), [Nikon NX Tether](https://downloadcenter.nikonimglib.com/en/products/578/NX_Tether.html), [Sony Imaging Edge Desktop Remote](https://support.d-imaging.sony.co.jp/app/imagingedge/en/instruction/4_5_remote.php), or [Fujifilm X Acquire](https://fujifilm-x.com/global/products/software/x-acquire/). Availability of tethered capture and automatic transfer depends on the model, connection, firmware and application. Check the maker's compatibility list. Built-in camera control uses libgphoto2 on Linux/macOS; Windows uses this watch-folder path and has the same serial rail controls.

For a standalone watcher, the CLI provides:

```sh
florastack-cli watch capture --out results --preset builtin:insect-macro --idle 30 --json-progress
```

Use a fresh results folder. Files must retain their size and modification time for the settling interval (one second by default), decode successfully and remain unchanged during the read. Use one final filename per photo. Partially written files are retried; replacing an already ingested file stops the run. Do not place results inside input. The running TIFF is shown in the viewer and exposed through job events with its ingested frame count. The top bar shows progress and cancellation.

## Camera control on Linux and macOS

The default build has no built-in camera control. Enable the optional feature on Linux or macOS:

```sh
cargo build -p florastack --features gphoto
cargo build -p florastack-cli --features gphoto
```

Windows builds must omit `gphoto`; enabling it there gives an explicit build error. The optional FFI module is excluded from Windows and default builds. There is no static link, LGPL Rust crate, or build-time dependency on libgphoto2 headers. Install a current libgphoto2 shared library with the OS package manager (`sudo apt install libgphoto2-6` on Debian/Ubuntu, `brew install libgphoto2` on macOS). Close competing photo importers and tether applications, connect exactly one supported camera, then **Connect camera**. Discovery is also available through `capture.camera` with `action=discover`; ambiguous connections are refused.

The library is LGPL-2.1. FloraStack loads `libgphoto2.so.6` on Linux or `libgphoto2.6.dylib` on macOS at run time (including Homebrew's usual absolute paths). To use a different compatible build, set `FLORASTACK_GPHOTO_LIBRARY` to its **absolute** shared-library path before starting the app. A bundled distribution must leave the library separately replaceable, include its license and satisfy the LGPL source-distribution requirements for that library; this repository bundles none. Its drivers, USB access and companion libraries remain the installed library's responsibility. Missing libraries, symbols, permissions or unsupported operations produce errors. [Public API](https://gphoto.sourceforge.io/doc/api/) and [camera support and focus controls](https://gphoto.sourceforge.io/doc/remote/).

**Refresh live view** retrieves a still live-view image; refresh again after focusing. With a supported `manualfocusdrive`, move by near/far unit pulses and mark both endpoints from that view. Turn off continuous autofocus, avoid shutter-triggered autofocus, and do not touch the focus ring or change camera focus settings. Marks count relative motor commands within the connection; they are not measured lens positions and cannot correct backlash, slip or nonuniform optical motion. Reconnects, focus errors and **Reset lens marks** invalidate both marks. Reset them after any uncertain movement. Unsupported camera/driver capabilities produce an error instead of pretending a move succeeded.

Select **Bracket between lens marks** and built-in camera capture. The photo count includes both endpoints: 21 photos means 20 intervals. Positions are integer motor pulses distributed across the marked span; there must be at least one pulse per interval. The sequence returns to the near mark, then moves toward far. Test the range and overlap on the actual lens before a long bracket.

Downloads retain their original encoded format and appear under unique `capture-00000-<random>.<extension>` names only after the temporary `.part` file has been flushed and published without replacing an existing file. The watcher handles these files exactly like tethered photos. Managed capture signals its end explicitly, so idle time during long exposures cannot end it. Cancellation stops future operations, waits for an in-flight blocking camera call to return and preserves any completed original; it cannot interrupt every camera's USB transaction or exposure. A failed sequence keeps originals but does not publish a partial final stack. Originals are incremental external effects and are not undoable project edits.

## Serial rails

Set the serial device (`COM3`, `/dev/ttyACM0`, etc.), baud rate, firmware dialect, calibration and safe travel limit. The line settings are 8N1 without flow control. Establish a safe physical zero **before connecting**. The travel limit is the absolute displacement allowed in either direction from that zero, not an automatic home or hardware end-stop. There is no automatic homing; use the controller's own procedure and hardware limits. Movement is refused outside the configured envelope. The command `capture.rail` with `action=ports` lists available ports.

Supported dialects are explicit, with no protocol detection:

| Adapter | Initialization | Relative movement and completion | Shutter | Stop |
|---|---|---|---|---|
| Marlin G-code, X axis | `G21` then `G91`, each `ok` | `G1 X<mm> F<mm/min>`, then `M400`; each must reply `ok` | External tethered photo | `M112` emergency stop; reset controller before reconnecting |
| FloraStack DIY Arduino line protocol | `HELLO` → `DONE` | `MOVE <signed integer steps>` → `DONE` **after physical movement** | `SHUTTER` → `DONE` after controller exposure/trigger cycle | `STOP`, processed immediately even during motion |
| Zaber ASCII | Unavailable stub; connection refused | Public protocol reserved for future hardware validation | — | — |

The DIY contract is a firmware interface to implement on an Arduino, not a claim that every Arduino rail already uses it. Commands and replies are newline-terminated ASCII, at most 256 reply bytes per line. `MOVE` uses calibrated micrometers per controller step and rounds to the nearest integer step; a nonzero request smaller than a representable step is refused. Keep the requested spacing an integer multiple of the calibration to avoid rounding error. Firmware must report an error with `ERROR <reason>` on failure and must never acknowledge completion merely when motion has been queued. `STOP` has no acknowledgement requirement and must stop motion without waiting for queued moves; any replies are discarded by reconnecting. The serial transport polls cancellation at most every 100 ms while reading, with an overall operation deadline; timeouts, malformed replies and disconnection invalidate the rail position and send a best-effort stop. Stop requires a reconnect at a known zero. Marlin's accepted busy/echo lines do not count as completion. References: [Marlin M400](https://marlinfw.org/docs/gcode/M400.html), [M112](https://marlinfw.org/docs/gcode/M112.html), and [Zaber's public ASCII manual](https://www.zaber.com/protocol-manual?protocol=ascii). Proprietary commercial protocols are not reverse-engineered.

For a managed rail sequence, use an empty input folder, select the photo count, signed rail spacing, settling time and trigger source. The first photo is taken at the current rail position; there are `photos-1` moves. The controller must finish each movement before settling begins. Choose:

- **Built-in camera capture** on supported Linux/macOS builds, for automatic capture and download.
- **Controller shutter and tethered file** for the DIY controller's shutter output, with a camera maker's tether application transferring the photo. Set the frame timeout to include the full exposure, transfer and preview processing.
- **Wait for an external photo at each position**, including on Windows, to trigger each photo from the camera or tether app. Wait for the rail to finish moving and settling before triggering. This workflow requires one photo per position; do not run an independent repeating capture or burst. The next move waits until the new file is decoded, aligned and previewed.

**Stop capture** dispatches job cancellation even while devices are busy; it stops subsequent shots/movements and sends a best-effort rail stop. The physical controller's emergency stop remains the immediate stop for an unresponsive device. A queued `capture.rail` stop cannot preempt an active device job; use `job.cancel` for that job. Stacking and normal heavy jobs can run alongside the device lane. A single device operation runs at a time.

## Physical step calculator

The calculator uses the diffraction (wave-optics) term in [Nikon MicroscopyU's depth-of-field equation](https://www.microscopyu.com/microscopy-basics/depth-of-field-and-depth-of-focus). It deliberately omits the geometric detector/circle-of-confusion term:

```text
λ = 0.55 µm (green light), refractive index n = 1 (air)
m = image size / object size
N = marked f-number at infinity, pupil magnification assumed 1
N_eff = N (1 + m)
NA ≈ m / (2 N_eff)                 [paraxial object-space numerical aperture]
diffraction depth = λ / NA² = 4 λ (N_eff / m)²  [µm, object space]
recommended step = diffraction depth / 2       [50% overlap]
sensor Airy diameter = 2.44 λ N_eff / pixel_pitch [pixels]
```

At 1:1, marked f/4 and 4 µm pixels: effective f/8, diffraction depth 140.8 µm, recommended spacing **70.4 µm**, Airy diameter 2.684 pixels. At 5:1, marked f/2.8 and 4 µm pixels: effective f/16.8, depth 24.83712 µm and spacing **12.41856 µm**. Changing pixel pitch changes the sampling report, not the wave-optics spacing. An Airy diameter under two pixels flags undersampling; diffraction over several pixels limits fine detail. Closing the aperture increases axial overlap but reduces lateral resolution.

This is a paraxial, unit-pupil, air model with a conservative fixed overlap, not a guarantee for every lens, wavelength, aperture, high-NA microscope objective or occluding subject. Nonunit pupil magnification, immersion, aberrations, focus breathing and physical motion alter the result. Validate a trial stack and reduce spacing if necessary. The result is rail translation along the optical axis, never lens motor pulses or a camera's arbitrary bracketing increment. Use a marked aperture, not an already compensated effective f-number.

## Engine commands and verification

`capture.info` reads cached connected state; `capture.calculate` is a pure query. `capture.camera`, `capture.rail` and `capture.run` are device-lane jobs with typed JSON schemas and validation. They do not alter the current project; managed ingest owns a separate project just as `watch.run` does. An example managed rail request:

```json
{"watch":{"input":"C:/Capture","output":"C:/Results","preset":"builtin:insect-macro"},"frames":20,"mode":"rail","trigger":"external","stepMicrometers":50,"settleMs":300,"frameTimeoutMs":120000}
```

Default builds/tests run on Windows, Linux and macOS; Linux/macOS additionally lint all targets and test the capture/engine crates with `--features gphoto`. Simulated camera and rail tests use JSON dispatch, real watched TIFF files, a live preview before shooting completes and a final stack, plus long exposures, cancellation, disconnection, external frame acknowledgements and lens marks. Serial parser and optical reference tests need no hardware. Physical camera/rail compatibility still requires testing the actual device and firmware.
