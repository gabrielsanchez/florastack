# 3D reliefs and synthetic stereo

Select a **Depth map** output in the Export tab and open **3D model and stereo**. Build the 3D preview,
then drag to orbit, scroll to zoom, or double-click to reset. The viewer uses wgpu with a depth buffer.
Changing surface settings requires rebuilding the preview. Export buttons use the current settings.

The depth coordinate is the output's recorded input order: zero is nearest by default. Turn on **Reverse depth
direction** for far-to-near captures. Focus order alone does not determine physical distance or camera calibration;
these outputs are relative-depth illustrations, not metrology or a complete 3D reconstruction. Output snapshots,
including slab order, remain independent of later edits to the project's frame order. Visible retouch is included.

Minimum confidence removes uncertain pixels. Confidence-weighted box smoothing reduces depth noise without
restoring masked pixels. A coarse mesh also omits cells spanning a masked pixel. Maximum depth jump removes
triangles across large depth discontinuities. Relief height is a fraction of the image's longest side, which is
one model unit; positive Z points toward the viewer, X right, Y up. Image size and mesh grid limits bound memory
and file size; the input image and maps are still decoded at their original sizes. The mesh is an open relief.

- **GLB** writes `model.glb`, a glTF 2.0 container with an embedded sRGB PNG, UV coordinates and a double-sided,
  unlit material, preserving the composite's appearance.
- **OBJ** writes `model.obj`, `material.mtl` and `texture.png`. Keep the three files together. OBJ texture V is
  flipped to its bottom-left convention; GLB and the viewer use top-left UVs.
- **Parallel** writes `parallel.png` with the left eye first. **Cross-eyed** writes `cross-eyed.png` with the
  right eye first. **Red-cyan** writes `anaglyph.png`, taking red from left and green/blue from right.
- **Rocking GIF** writes `rocking.gif`, a smooth, endlessly looping sinusoidal camera excursion. The desktop
  defaults to 32 frames at 16 fps. GIF has a 256-color palette per frame and centisecond timing.

Baseline is a fraction of the output image width, from 0 to 0.25. Views converge at relative depth 0.5:
`x_view = x - camera_offset * baseline * width * (0.5 - depth)`, with camera offsets -0.5 (left) and +0.5 (right).
Near points move right in the left eye and left in the right eye; far points do the reverse. Foreground wins when
projected points overlap. Reprojection cannot reveal hidden surfaces: disocclusions and masked samples remain
transparent in PNG, and black in GIF. Keep the baseline small to limit gaps and stereo discomfort.

All work is available through cancellable engine jobs, including the CLI's generic `run` command:

```sh
florastack-cli run specimen.florastack --cmd parallax.mesh --params '{"output":3,"folder":"model","format":"glb","settings":{"confidence":0.5,"smoothing":2}}'
florastack-cli run specimen.florastack --cmd parallax.stereo --params '{"output":3,"folder":"stereo","mode":"cross-eyed","settings":{"baseline":0.04}}'
florastack-cli run specimen.florastack --cmd parallax.animate --params '{"output":3,"folder":"animation","frames":48,"fps":24}'
```

`parallax.preview` returns the path to a cached textured mesh for the app. All export commands refuse existing
files unless `overwrite` is true. Files are staged before publication, after the final cancellation checkpoint.
Choose a separate destination folder per output. These derivative exports do not currently carry the scientific
reproduction sidecar; keep the project or an `export.run` export to retain the processing record.

The GLB integration test uses Khronos' official validator, pinned to `gltf-validator@2.0.0-dev.3.10`. Install
Node.js/npm, then run `cargo xtask gltf-validator` once to fetch it outside the repository. `cargo xtask ci` and
CI prepare that fixture before testing. Direct test runs without it record a missing-fixture skip. The validator
test rejects both errors and warnings. Other tests check synthetic-stack parallax direction, confidence holes,
triangle winding, stereo packing, GIF decoding/timing, cancellation and hostile parameters.
