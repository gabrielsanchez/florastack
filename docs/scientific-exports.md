# Scientific exports and annotations

The Export tab's **Scientific outputs and annotations** section uses the same registry commands as scripts and the
CLI. `export.set` saves project export settings; `export.run` can override them for one export. `output.save`, batch
and watch exports also write reproducibility manifests.

| Option | Artifact and values |
|---|---|
| `depth_map` | Untagged 16-bit gray TIFF: `sample / 65535 * (N - 1)` is the fractional index in the output's direct inputs, in compositing order. |
| `calibrated_depth` | Untagged 32-bit float gray TIFF, `-depth-um.tif`, with samples in micrometers. Requires `z_step_micrometers`. |
| `confidence_map` | Untagged 16-bit gray TIFF: normalized confidence, with 0.5 at the method's contrast threshold. This is not a probability or a depth uncertainty. |
| `aligned_frames` | 16-bit TIFFs in an `aligned` folder, developed, defect-corrected, aligned and multiplied by the recorded brightness gain. |
| `contribution_maps` | One untagged 16-bit gray TIFF per input frame, in a `contributions` folder: the normalized depth compositor weight, including feathered coverage and its nearest-covering-frame fallback. Weights sum to 1, within integer rounding, except at uncovered pixels, where all are zero. |
| `alignment_csv` | Per-frame capture and stacking indices, input path, master-to-input translation in pixels, isotropic scale, rotation in radians, gain and alignment score. CSV fields with paths are quoted. |

Calibrated depth uses the full-precision depth retained when stacking, without the 16-bit map's quantization. Z is
`z_origin_micrometers + capture_index * z_step_micrometers`, interpolated between the frames on either side of the
depth. Capture indices count from zero in the frame order recorded when the output was made, including disabled or
rejected frames. A negative step reverses travel. Set the frame order to capture order before stacking if files were
added out of order. Lens focus motor increments are not a physical travel calibration. A final slab depth indexes
slabs, so calibrated depth and per-frame contributions require a depth-map output made directly from frames.

Outputs retain their development settings, resolved RAW development, alignment, frame order and defect mask. Later
changes to those settings or to the project's frame order do not change their scientific exports. Older project
outputs lack this record: restack them to retain scientific precision and immutable processing state.

`calibration.get` and `calibration.set` manage a library of named objective/magnification profiles. Each measures
pixels per micrometer in the alignment master's input image. The engine accounts for a narrow canvas's scale when
rendering the bar. The library lives in `calibrations.json` beside the host's startup temporary-projects folder,
independently of projects and presets. Selecting a profile copies its name and value into `calibration_name` and
`pixels_per_micrometer` in the project's output settings. Subsequent library changes leave that measurement intact.
Use a JSON `null` to clear an optional step or lateral calibration with `export.set`.

`annotation.get` lists an output's layers; `annotation.set` replaces the list in one undo step. Layers are saved with
the project and can be shown, hidden or removed. Text labels use fractional image coordinates, an integer bitmap
font size and black or white ink. The bundled font supports Latin and Greek; unsupported characters are rejected.
Scale bars support all four corners, solid or bracket styles, a fixed length or an automatic 1/2/5 length fitting up
to a quarter of the image width. A visible bar requires a lateral calibration. Bars and labels that cannot fit
return an actionable error. `annotations: false` exports the output without rendering its layers. Rendering changes
only the exported output copy; depth, confidence, contribution maps and the stored image are unchanged.

Every export has an adjacent `<image filename>.json` sidecar, with a versioned format, software version, all
processing and export settings, original SHA-256 input hashes, immutable per-output input state, output dependencies,
retouch and annotation layers, coordinate conventions and artifact hashes. Non-regenerable masks are embedded;
output pixels and RAW inputs are not. Export preparation is cancellable; the completed sidecar is published last.
Existing image or sidecar files are refused unless overwrite is requested.

Replay requires the original inputs at their recorded absolute paths and the software version named by the sidecar:

```sh
florastack-cli reproduce 'exports/specimen.tif.json' --out reproduced
```

This dispatches `export.reproduce`. It verifies input and embedded-mask hashes, recomputes the stacking dependency
graph with recorded alignment and development, replays visible retouch and annotation layers and writes the same
export options into a separately chosen destination. It needs no original project or output cache and never writes
destination paths from the manifest. Replay uses the CPU for deterministic verification. CPU exports reproduce the
same decoded samples on the same software version; GPU floating-point arithmetic and platform codecs can differ
slightly. Artifact byte hashes include metadata and container details, so compare decoded samples when these vary.
