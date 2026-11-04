# Manual release checks

Automated tests can't hold a pen, open Lightroom or watch a 24-megapixel brush stroke. Before a release, run through
these lists on each platform, and note the platform, GPU, versions and stack with the results. The roadmap records
what has been checked.

## Retouching

Run through this list on a real stack (24 MP or larger, at least 30 frames, RAW files) on each platform. [Retouching](../user/retouching.md) describes what is being checked.

**Opening**

- [ ] The Retouch tab opens the selected output (or the newest) and shows it on the right, fitted.
- [ ] An output with strokes from an earlier session opens without a pause for recomputation, and shows its strokes.
- [ ] Opening another output closes the first; its strokes are still there when it is opened again.

**Source**

- [ ] Picking a frame in the panel shows it on the left, aligned with the output: features line up when you switch between them at 100%.
- [ ] Picking a frame a second time is immediate (it is kept in memory or the cache).
- [ ] Map sharpest frames finishes, and Ctrl+click on an antenna tip, an eye and the background picks three different, sensible frames.
- [ ] With Follow the sharpest frame on, resting on a point switches the source after a moment, and moving across the subject never switches it in the middle of a stroke.
- [ ] Another output (for example a depth map while retouching a pyramid) can be a source; an output of another framing is refused with a message.

**Painting**

- [ ] The brush's circle shows at the same point in both views, at the brush's size, at every zoom.
- [ ] A Details stroke from a frame that is brighter than the stack blends in without a visible edge or brightness step.
- [ ] A Details stroke across a hair or bristle brings it through crisply, without a soft ghost around it.
- [ ] Darken over dark bristles on a light background leaves the background between them untouched.
- [ ] Lighten does the opposite on light hairs over a dark background.
- [ ] Pixels copies exactly, with the hardness set.
- [ ] The Eraser brings back the output as stacked.
- [ ] The stroke shows while it is drawn, and doesn't change when the button is let go.
- [ ] Painting is smooth with a 200-pixel brush at 100% on a 24 MP output, and usable with a 1000-pixel brush.
- [ ] With a pen on Windows, pressure makes the brush smaller; on macOS and Linux, the brush keeps its full size (pens report no pressure there yet).
- [ ] The brightness boost brightens both views, and the exported file isn't brightened.

**Undo, layers and saving**

- [ ] Ctrl+Z undoes the last stroke only, and the region it covered shows what was under it, including strokes it covered; Ctrl+Shift+Z redoes it.
- [ ] Hiding a layer removes its strokes from the view; showing it brings them back exactly.
- [ ] Removing a layer can be undone.
- [ ] After saving, closing and opening the project again, the output shows all its strokes, and undo still works for new strokes.
- [ ] Exporting the output writes it retouched (open the file in another program).
- [ ] Deleting the project's `cache/` folder and opening the output again paints the strokes again to the same result (slower, but identical).

## Photo-editor integrations


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
