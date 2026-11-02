# Photo-editor integrations

The Lightroom Classic plugin and darktable script render the selected photos with
their edits, stack the rendered TIFFs through the CLI, and import one result beside
the originals. Other editors can pass files to the desktop app or use the CLI recipe
below. A saved preset controls alignment and stack parameters; the integration's
method selector chooses pyramid, depth map or weighted average.

## Build and install

Build both executables and prepare the integrations from the repository root:

```sh
cargo build -p florastack -p florastack-cli --release
python scripts/package_integrations.py
```

The packaging script needs Python 3.11+ (standard library only) and reads the
product identity from `branding.toml`. It creates `target/integrations/` containing
`FloraStack.lrplugin/`, `darktable/` and the desktop entry. `--out FOLDER` selects
another destination. Copy generated packages somewhere permanent before enabling
them; source folders alone lack the generated branding and shared bridge modules.
Paths below use the current names in `branding.toml`.

### Lightroom Classic (Windows and macOS)

Follow the [plugin installation instructions](../packaging/lightroom/README.md).
In the Export dialog choose **Export To > FloraStack**, enter the absolute path to
`florastack-cli.exe` on Windows or `florastack-cli` on macOS, and choose a preset and
method. GUI applications may have a different `PATH` from your terminal, so use an
absolute executable path. Presets accept `builtin:insect-macro`,
`builtin:product-shot`, `builtin:landscape`, `builtin:microscope`, or an absolute
TOML filename. Save these fields as a Lightroom export preset for reuse.

Select at least two still photos in focus order. Export always uses full-size,
16-bit, ZIP-compressed sRGB TIFFs, includes embedded metadata, and disables output
sharpening and watermarks. The plugin waits for every rendition before stacking.
The finished TIFF is added to the catalog and stacked below the first original.
Photos from several folders are allowed; the first original determines the result
folder and metadata source. Lightroom cloud does not support these Lua plugins.

The service uses Adobe's documented
[export provider lifecycle](https://developer.adobe.com/lightroom-classic/).
It requires SDK 6 or newer; actual host compatibility still needs the manual checks
below. Extra third-party export filters can change rendered pixels and metadata;
disable them for a reproducible stack.

### darktable (Lua-enabled desktop builds)

Copy the contents of generated `target/integrations/darktable/` into the `lua/`
directory under darktable's configuration directory. The resulting layout is:

```text
lua/
  florastack.lua
  florastack/
    Branding.lua
    Bridge.lua
```

The configuration directory is normally `~/.config/darktable` on Linux,
`~/.config/darktable` on macOS, or `%LOCALAPPDATA%\darktable` on Windows; launch
with `darktable -d lua` to diagnose loading, and use the configuration directory
reported by your installation if it differs. Add this line to `luarc` in that
configuration directory, then restart darktable:

```lua
require "florastack"
```

In **Preferences > Lua options**, set the CLI executable, preset and method. In the
export panel choose **Stack with FloraStack** as the storage target, **TIFF** as the
format, a consistent output profile (sRGB is a portable choice), and include EXIF,
IPTC and XMP in the metadata options. Select at least two frames and export. The
script forces 16-bit output and unlimited dimensions before rendering. Only TIFF
is offered. It sorts original paths lexically, then image IDs to break ties, and
numbers the staged TIFFs in that order, regardless of export completion order.
Use zero-padded filenames that follow focus order, and a consistent edit/profile
across the selection. It imports the successful output and groups it with the first
original. On failure it reports an error in darktable and leaves diagnostics.

This is an original script using the documented
[storage API](https://docs.darktable.org/lua/stable/lua.api.manual/darktable/darktable.register_storage/),
[TIFF parameters](https://docs.darktable.org/lua/stable/lua.api.manual/types/dt_imageio_module_format_data_tiff/)
and [database import](https://docs.darktable.org/lua/stable/lua.api.manual/darktable/darktable.database/).
No darktable implementation or existing Lua script code was used. Sandboxed builds
must be able to execute the CLI and read/write the selected photo directories.

## Files, metadata and failures

For an original named `near.raw`, the result is `near-stacked.tif` beside it. A
repeat run chooses `near-stacked-2.tif`, and so on. Existing results, sidecars and
input folders are preserved. The sibling `near-stacked-inputs/` contains numbered
rendered TIFFs, `cli.log` (stderr) and `result.json` (stdout, including the CLI's
saved project path). Only that run's staged TIFFs are passed to the CLI, so existing
results and unrelated photos cannot become additional input frames. A CLI failure,
incomplete export or missing output prevents import; an import failure leaves the
finished TIFF available for manual import. The originals are never modified.

The engine copies embedded EXIF/IPTC/XMP from the first input and adds its software
and frame-count metadata. Catalog-only fields must first be included by the
editor's TIFF export. The CLI writes a 16-bit TIFF with its default output profile
and a reproducibility `.tif.json` sidecar. The CLI's `stack` command uses the chosen
preset's processing settings; its explicit output depth and default TIFF export
specification take precedence over the preset's `[output]` group.

Keep the `-inputs/` directory while you need to reopen the saved project or run
`florastack-cli reproduce`: those records refer to the rendered frames. Once you
only need the finished TIFF, you may remove its input directory yourself. Failed
or canceled runs may retain a partial input directory. Do not remove inputs while
the CLI is running. Lightroom releases its own temporary renditions after export;
darktable manages its temporary exports separately from our retained copies.
On Windows the darktable script also retains `stack.cmd` in that folder, working
around the [documented restriction on multiple quoted arguments](https://docs.darktable.org/lua/stable/lua.scripts.api.manual/dtutils.system/windows_command/)
in its process API without an extra Lua library.

The editors remain responsive while their documented process APIs wait for the
CLI. Their Cancel buttons cannot terminate an already running child process:
wait for it to exit. Lightroom checks cancellation before invoking the CLI and
again before import; darktable refuses incomplete export sets. Use the standalone
app when you need interactive cancellation during stacking.

All shell arguments are quoted. POSIX paths may include spaces, quotes, dollar
signs and shell punctuation. Windows arguments containing `%`, `!`, `"` or `^` are
rejected because `cmd.exe` can reinterpret them; use a different path or the CLI
directly. NULs and newlines are refused on every platform.

## digiKam and generic Open With

On Linux, put the desktop executable on `PATH` and install the generated entry:

```sh
mkdir -p ~/.local/share/applications
cp target/integrations/io.github.gabrielsanchez.FloraStack.desktop ~/.local/share/applications/
update-desktop-database ~/.local/share/applications
```

Select several files in digiKam or a file manager and use **Open With > Open with
FloraStack**. The entry uses `%F`, passing all local files to one desktop process.
The app loads them into one temporary project; align, stack and export in the app.
On Windows/macOS choose the desktop executable/application with the editor's Open
With action, or pass the selected files on its command line. Availability and
multi-selection behavior depend on the editor. Open With passes existing files;
to include uncommitted editor adjustments, export TIFF copies first.

For unattended stacking, export one selection to a fresh folder of full-size
16-bit TIFFs, with one common ICC profile and embedded metadata. Use filenames in
focus order. Put the result outside that folder to keep future inputs unambiguous:

```sh
florastack-cli stack "/photos/bee-rendered" --preset builtin:insect-macro --method pyramid --depth 16 --out "/photos/bee-stacked.tif"
```

Or pass explicit files in focus order:

```sh
florastack-cli stack "/photos/near.tif" "/photos/far.tif" --preset "/presets/macro.toml" --method depthmap --depth 16 --out "/photos/stack.tif"
```

Check exit status 0 and the output's existence before importing it back into the
editor. Existing outputs are refused. In PowerShell invoke an absolute executable
with `& 'C:\Program Files\FloraStack\florastack-cli.exe' stack ...`. The desktop entry
opens a project for interactive work; these recipes produce the finished TIFF.

## LightCraft

LightCraft documents **Settings > General > External Editing** and **Photo > Edit
In > Edit in Other Editor** in its
[release notes](https://github.com/storytold/lightcraft/blob/main/docs/whats-new.md).
Set TIFF/16-bit there when handing individual developed copies to another editor.
That per-photo workflow does not establish a multi-frame selection handoff or
automatic import of a separately named stack result. For a whole focus stack,
export the selection as numbered TIFFs and use Open With or the CLI recipe above,
then import the result into LightCraft.

For automation LightCraft exposes an
[MCP server](https://github.com/storytold/lightcraft/blob/main/docs/mcp.md).
Start `lightcraft-cli mcp /photos/shoot` for a headless session, or start the desktop
app with `lightcraft --control 7980` and bridge it with
`lightcraft-cli mcp --connect 127.0.0.1:7980`. Consult the installed release's help
for control-channel options. Use one library owner at a time; bridge the open app
when it already owns the library.

An MCP client with local process execution can orchestrate the round trip:

1. Use LightCraft's `query_photos` to identify frames, order their IDs by focus,
   and export each through `export`, for example
   `{"id": 42, "path": "/photos/bee-rendered/000001.tif", "format": "tiff", "bitDepth": 16, "colorSpace": "srgb"}`.
   Export every frame at full size to a fresh directory and wait for each result.
2. Run the FloraStack CLI recipe with that directory and the chosen preset; wait
   for exit status 0. LightCraft's MCP server provides photo commands; the
   orchestrator supplies the local CLI invocation.
3. Call LightCraft's `import` with `{"paths": ["/photos/bee-stacked.tif"]}`.
   Review metadata and group the output with its originals in LightCraft.

The MCP recipe uses LightCraft's current documented helper parameters. Discover
tools and command help on the installed release before automating it. Our own
[MCP server](automation.md) is also available for clients that prefer engine
commands to launching `stack`.

## Validation and manual acceptance checklist

With Lua and [Luacheck](https://github.com/lunarmodules/luacheck) installed:

```sh
python scripts/package_integrations.py
luacheck packaging
lua packaging/tests/integrations.lua target/integrations
desktop-file-validate target/integrations/io.github.gabrielsanchez.FloraStack.desktop
```

The Lua checks mock each editor's documented API. They cover argument quoting,
collision handling, frame order, TIFF settings, successful import, failed renders,
incomplete exports, cancellation, CLI failures and missing output. They cannot
validate real host rendering, catalog metadata or platform-specific process APIs.
Run the following before declaring a host/platform tested; record editor version,
OS, CLI version, preset, frame count and result for each run:

- [ ] **Lightroom, Windows and macOS:** install the generated plugin, select three
  developed RAW frames in focus order, save/reload the export preset, and export.
  Check full-size 16-bit TIFF inputs, baked edits and one correctly rendered result.
  Verify catalog import below the first original and a result beside it on disk.
- [ ] **darktable, Linux/macOS/Windows Lua builds:** install via `luarc`, set
  preferences, select TIFF with embedded metadata, and export at least three RAW
  frames. Check 16-bit/full-size inputs even after selecting 8-bit/reduced export
  settings, deterministic filename order, import, grouping and the output folder.
- [ ] **Metadata/color:** give the first frame a caption, keyword, copyright and
  capture time; verify embedded EXIF/IPTC/XMP, ICC profile, software and frame count
  after import. Check orientation, colors and pixel dimensions. Confirm private
  metadata follows the chosen editor export policy.
- [ ] **Presets/methods:** try a built-in and a custom TOML preset, and pyramid,
  depth map and average. Check the selected alignment/stack settings in the saved
  project and distinguish changes from the editor's baked development.
- [ ] **Paths/collisions:** use spaces, Unicode, apostrophes and `&` in paths;
  repeat the export and verify earlier output/sidecar/input folders survive.
  Try identically named originals in different directories. On Windows verify
  `%`, `!`, `"` and `^` fail with a readable error before process launch.
- [ ] **Failure/cancellation:** test one photo, an unavailable original, a missing
  executable, invalid preset/method, read-only destination and full disk. Cancel
  during rendering and let any running child finish. Verify there is no partial
  import and originals survive. Inspect `cli.log` after a processing failure, and
  manually import a successful result after deliberately preventing catalog import.
- [ ] **Reopen/reproduce:** use the project path in `result.json` to reopen it with
  retained inputs, and reproduce the TIFF from its JSON sidecar to a fresh folder.
- [ ] **digiKam/generic:** install/validate the Linux desktop entry, select multiple
  files, and verify one app project contains every selected frame. Run the CLI
  recipe on exported TIFFs, import the result and check its metadata.
- [ ] **LightCraft:** run the TIFF export/CLI/import recipe; then use its installed
  MCP helpers for the same selection and compare the outputs and metadata. Check
  the external-editor path separately on the installed release.

Implementation validation is recorded in the roadmap. These host checks remain
manual; no editor has been claimed tested merely because the mocked checks pass.
