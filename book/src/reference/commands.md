<!-- Generated from the engine's command registry by crates/engine/tests/reference.rs. Don't edit it by hand: after changing a command, run `cargo xtask book --update-reference`. -->

# Engine commands

Everything FloraStack does is one of these commands: the desktop app's buttons and menus, the command-line tool's subcommands, the command palette (Ctrl+K), MCP tools, the desktop control channel and the Python package's `Project.run` all call them. Each takes its parameters as a JSON object and returns a JSON object. `florastack-cli run` runs them on a project, and `florastack-cli commands` prints this list as JSON, with the complete schemas. [Automation, MCP and Python](../user/automation.md) shows how to call them.

- A **query** reads and returns at once; an **instant** command changes the session and returns; a **job** runs in the background, on one of the engine's lanes, and returns its result when it ends.
- An **undoable** command's changes to the project are one undo step.
- Parameters left out keep their defaults; unknown parameters are refused.

| Area | Commands |
|---|---|
| [adjust](#adjust) | [`adjust.addMask`](#adjustaddmask), [`adjust.close`](#adjustclose), [`adjust.describe`](#adjustdescribe), [`adjust.get`](#adjustget), [`adjust.mapDepth`](#adjustmapdepth), [`adjust.model`](#adjustmodel), [`adjust.moveMask`](#adjustmovemask), [`adjust.open`](#adjustopen), [`adjust.preview`](#adjustpreview), [`adjust.removeMask`](#adjustremovemask), [`adjust.reset`](#adjustreset), [`adjust.sampleColor`](#adjustsamplecolor), [`adjust.select`](#adjustselect), [`adjust.set`](#adjustset), [`adjust.setMask`](#adjustsetmask), [`adjust.setModel`](#adjustsetmodel), [`adjust.stroke`](#adjuststroke) |
| [align](#align) | [`align.get`](#alignget), [`align.info`](#aligninfo), [`align.reset`](#alignreset), [`align.run`](#alignrun), [`align.set`](#alignset), [`align.sortByScale`](#alignsortbyscale) |
| [annotation](#annotation) | [`annotation.get`](#annotationget), [`annotation.set`](#annotationset) |
| [batch](#batch) | [`batch.run`](#batchrun) |
| [cache](#cache) | [`cache.info`](#cacheinfo), [`cache.removeAbandoned`](#cacheremoveabandoned), [`cache.setFolder`](#cachesetfolder) |
| [calibration](#calibration) | [`calibration.get`](#calibrationget), [`calibration.set`](#calibrationset) |
| [capture](#capture) | [`capture.calculate`](#capturecalculate), [`capture.camera`](#capturecamera), [`capture.info`](#captureinfo), [`capture.rail`](#capturerail), [`capture.run`](#capturerun) |
| [compute](#compute) | [`compute.info`](#computeinfo), [`compute.set`](#computeset), [`compute.test`](#computetest) |
| [defects](#defects) | [`defects.clear`](#defectsclear), [`defects.detect`](#defectsdetect), [`defects.export`](#defectsexport), [`defects.import`](#defectsimport), [`defects.info`](#defectsinfo) |
| [develop](#develop) | [`develop.get`](#developget), [`develop.reset`](#developreset), [`develop.set`](#developset) |
| [edit](#edit) | [`edit.history`](#edithistory), [`edit.redo`](#editredo), [`edit.undo`](#editundo) |
| [export](#export) | [`export.estimate`](#exportestimate), [`export.get`](#exportget), [`export.reproduce`](#exportreproduce), [`export.reset`](#exportreset), [`export.run`](#exportrun), [`export.set`](#exportset) |
| [filmstrip](#filmstrip) | [`filmstrip.list`](#filmstriplist), [`filmstrip.update`](#filmstripupdate) |
| [job](#job) | [`job.cancel`](#jobcancel), [`job.list`](#joblist) |
| [output](#output) | [`output.list`](#outputlist), [`output.remove`](#outputremove), [`output.rename`](#outputrename), [`output.render`](#outputrender), [`output.save`](#outputsave) |
| [parallax](#parallax) | [`parallax.animate`](#parallaxanimate), [`parallax.mesh`](#parallaxmesh), [`parallax.preview`](#parallaxpreview), [`parallax.stereo`](#parallaxstereo) |
| [preset](#preset) | [`preset.apply`](#presetapply), [`preset.list`](#presetlist), [`preset.read`](#presetread), [`preset.save`](#presetsave) |
| [project](#project) | [`project.addFrames`](#projectaddframes), [`project.checkFrames`](#projectcheckframes), [`project.close`](#projectclose), [`project.findOrphans`](#projectfindorphans), [`project.findStacks`](#projectfindstacks), [`project.frames`](#projectframes), [`project.info`](#projectinfo), [`project.moveFrames`](#projectmoveframes), [`project.new`](#projectnew), [`project.open`](#projectopen), [`project.removeFrames`](#projectremoveframes), [`project.rename`](#projectrename), [`project.save`](#projectsave), [`project.saveAs`](#projectsaveas), [`project.setFramesEnabled`](#projectsetframesenabled), [`project.trashOrphans`](#projecttrashorphans) |
| [retouch](#retouch) | [`retouch.addLayer`](#retouchaddlayer), [`retouch.cancelPaint`](#retouchcancelpaint), [`retouch.close`](#retouchclose), [`retouch.mapFocus`](#retouchmapfocus), [`retouch.open`](#retouchopen), [`retouch.paint`](#retouchpaint), [`retouch.removeLayer`](#retouchremovelayer), [`retouch.setLayerVisible`](#retouchsetlayervisible), [`retouch.setSource`](#retouchsetsource), [`retouch.sharpest`](#retouchsharpest), [`retouch.stroke`](#retouchstroke) |
| [stack](#stack) | [`stack.average`](#stackaverage), [`stack.depthMap`](#stackdepthmap), [`stack.get`](#stackget), [`stack.preview`](#stackpreview), [`stack.pyramid`](#stackpyramid), [`stack.reset`](#stackreset), [`stack.set`](#stackset), [`stack.slabbed`](#stackslabbed), [`stack.threshold`](#stackthreshold), [`stack.wavelet`](#stackwavelet) |
| [watch](#watch) | [`watch.run`](#watchrun) |

## adjust

### `adjust.addMask`

**Add mask.** Adds a mask on top of an output's, as one undo step.

The mask is given a new id, which the result returns; an `id` given is ignored. Its components are combined in order: the first one's region, then each next one added (`add`), taken away (`subtract`) or intersected (`intersect`). Shapes: `whole`, `linear` (`start`, `end`), `radial` (`center`, `radius_x`, `radius_y`, `angle`, `feather`), `brush` (`strokes`), `luminance` (`low`, `high`, `low_feather`, `high_feather`), `color` (`samples`, from `adjust.sampleColor`, and `refine`), and from the stack's focus `subject`, `background` and `depth` (`from`, `to`, `feather`). Coordinates are fractions of the output's width and height.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `mask` | object | **Required.** The mask: its name, components and adjustments. |
| `mask.adjust` | object | The adjustments it applies where it selects. Default: `{"amount":100.0,"blacks":0.0,"clarity":0.0,"contrast":0.0,"dehaze":0.0,"exposure":0.0,"highlights":0.0,"noise":0.0,"saturation":0.0,"shadows":0.0,"sharpness":0.0,"temperature":0.0,"texture":0.0,"tint":0.0,"whites":0.0}`. |
| `mask.adjust.amount` | number, from 0.0 to 100.0 | How strongly the mask applies its adjustments, from 0 (not at all) to 100. Default: `100.0`. |
| `mask.adjust.blacks` | number, from -100.0 to 100.0 | The black point. Default: `0.0`. |
| `mask.adjust.clarity` | number, from -100.0 to 100.0 | Contrast of mid-sized detail. Default: `0.0`. |
| `mask.adjust.contrast` | number, from -100.0 to 100.0 | Contrast. Default: `0.0`. |
| `mask.adjust.dehaze` | number, from -100.0 to 100.0 | Removes haze (positive) or adds it (negative). Default: `0.0`. |
| `mask.adjust.exposure` | number, from -5.0 to 5.0 | Exposure in stops, from -5 to 5. Default: `0.0`. |
| `mask.adjust.highlights` | number, from -100.0 to 100.0 | Brightness of the bright tones. Default: `0.0`. |
| `mask.adjust.noise` | number, from -100.0 to 100.0 | Reduces noise (positive) or keeps more grain (negative). Default: `0.0`. |
| `mask.adjust.saturation` | number, from -100.0 to 100.0 | Saturation; -100 is black and white. Default: `0.0`. |
| `mask.adjust.shadows` | number, from -100.0 to 100.0 | Brightness of the dark tones. Default: `0.0`. |
| `mask.adjust.sharpness` | number, from -100.0 to 100.0 | Sharpens (positive) or softens (negative). Default: `0.0`. |
| `mask.adjust.temperature` | number, from -100.0 to 100.0 | Warmer (positive) or cooler (negative). Default: `0.0`. |
| `mask.adjust.texture` | number, from -100.0 to 100.0 | Contrast of fine detail and surface texture. Default: `0.0`. |
| `mask.adjust.tint` | number, from -100.0 to 100.0 | Magenta (positive) or green (negative). Default: `0.0`. |
| `mask.adjust.whites` | number, from -100.0 to 100.0 | The white point. Default: `0.0`. |
| `mask.components` | array of object | Its shapes, combined in order: the first one's region, then each next one added, subtracted or intersected. Default: `[]`. |
| `mask.id` | integer | The mask's id, unique among the output's masks and never reused: commands name a mask by it. Default: `0`. |
| `mask.invert` | boolean | Whether it selects what its components leave out instead. Default: `false`. |
| `mask.name` | string | Its name, at most 64 characters. Default: `""`. |
| `mask.refine` | number, from 0.0 to 100.0 | How closely its edges follow the image's own, from 0 (as drawn) to 100. Default: `0.0`. |
| `mask.visible` | boolean | Whether its adjustments apply. A hidden mask is kept but changes nothing. Default: `true`. |
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `adjustments` | object | An output's adjustments: the whole image's, then each mask's in order. |
| `mask` | integer | The mask's id. |
| `neutral` | boolean | Whether they leave the output as it is. |
| `output` | integer | An output's id in its project. |

Examples:

```json
{"mask":{"adjust":{"exposure":0.4},"components":[{"shape":{"angle":0,"center":[0.5,0.5],"feather":60,"kind":"radial","radius_x":0.25,"radius_y":0.2}}],"name":"Center"},"output":3}
{"mask":{"adjust":{"saturation":-30},"components":[{"shape":{"kind":"whole"}}]},"output":3}
```

### `adjust.close`

**Close adjusting.** Closes the output open for adjusting.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `closed` | boolean | Whether an output was open for adjusting. |

Example:

```json
{}
```

### `adjust.describe`

**AI describe.** Selects everything a description names in the output open for adjusting, with the AI model, into a mask, as one undo step.

A short noun phrase works best: `insect`, `flower`, `the red berries`. The selection goes into the mask's first `described` shape, or a new one. Fails, changing nothing, when nothing matches. Needs the model (`adjust.model`).

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `mask` | integer | **Required.** The mask's id, as `adjust.get` lists it. |
| `text` | string | **Required.** What to select, at most 200 characters: a short noun phrase. |

Result:

| Name | Type | Description |
|---|---|---|
| `component` | integer |  |
| `loaded` | boolean | Whether the model had to load or read the image first (the slow part). |
| `mask` | integer | The mask, and the index of the shape the selection went into. |

Example:

```json
{"mask":1,"text":"insect"}
```

### `adjust.get`

**Adjustments.** An output's adjustments: the whole image's and its masks'.

Every setting is listed, neutral ones included; `neutral` says whether they change anything. The whole image's (`global`) are applied first, then each visible mask's (`masks`) where it selects.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `adjustments` | object | An output's adjustments: the whole image's, then each mask's in order. |
| `neutral` | boolean | Whether they leave the output as it is. |
| `output` | integer | An output's id in its project. |

Example:

```json
{"output":3}
```

### `adjust.mapDepth`

**Map the stack's depth.** Measures the depth of the output open for adjusting from its frames' focus, for its depth masks.

Measures the output's frames' focus at a quarter of their size, as the depth-map method's first pass does, and keeps the depth map it makes as the output's `focus-depth` map: at each point, which frame was in focus there. A depth-map output needs none: its own depth map tells. Exports that need it and find none measure it themselves. The subject and background masks need no map: the subject is found in the output itself. Runs beside other jobs.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `height` | integer |  |
| `measured` | boolean | Whether the frames were measured (`false`: the output's map was there already). |
| `output` | integer | An output's id in its project. |
| `width` | integer | The map's size, a quarter of the output's for a measured one. |

Example:

```json
{}
```

### `adjust.model`

**AI model.** Whether Meta's SAM 3 model, which AI selections need, is installed, and where.

FloraStack doesn't ship the model: its files (`model.safetensors`, `vocab.json`, `merges.txt`) are the `facebook/sam3` checkpoint on Hugging Face, under Meta's SAM License, which you accept there before downloading them. Then `adjust.setModel` names their folder.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | Nothing | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `bytes` | integer |  |
| `download` | string | Where the files come from, and their license. |
| `files` | array of string | The files, and the weights' size in bytes. |
| `folder` | string | The folder named, if any. |
| `installed` | boolean | Whether the folder named holds the model's files (their sizes; `adjust.setModel` checked their hash). |
| `license` | string |  |
| `problem` | string | Why the folder named doesn't hold the model, when it doesn't. |

Example:

```json
{}
```

### `adjust.moveMask`

**Move mask.** Moves one of an output's masks to another place in their order, in one undo step.

Masks apply in order, the first one first; `index` 0 is the first place, and an index past the end the last.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `index` | integer | **Required.** Its new place: 0 is the first, and an index past the end the last. |
| `mask` | integer | **Required.** The mask's id, as `adjust.get` lists it. |
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `adjustments` | object | An output's adjustments: the whole image's, then each mask's in order. |
| `mask` | integer | The mask's id. |
| `neutral` | boolean | Whether they leave the output as it is. |
| `output` | integer | An output's id in its project. |

Example:

```json
{"index":0,"mask":2,"output":3}
```

### `adjust.open`

**Open for adjusting.** Opens an output for adjusting, so that `adjust.preview` can show its adjustments.

Reads the output with its visible retouch strokes (the output open for retouching as it is, or the result cached) and keeps it in memory, ready to render. One output is open for adjusting at a time; opening another closes the one open. Runs beside other jobs.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `depth` | boolean | Whether the stack's depth map is known, for the depth masks. |
| `height` | integer |  |
| `output` | integer | An output's id in its project. |
| `width` | integer |  |

Example:

```json
{"output":3}
```

### `adjust.preview`

**Preview adjustments.** Renders the output open for adjusting with its adjustments, reduced to fit a view.

The preview fits in `width` by `height` pixels (never enlarged). When the output's retouch strokes changed since it was read, it is read again first. `draft` renders quickly while a slider moves, skipping the most expensive refinements; `overlay` shows a mask's region tinted red; `before` also renders the output without its adjustments at the same size, for comparing. Apps draw the result; the job's result gives its size. Runs beside other jobs; a newer call replaces one still running.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `before` | boolean | Also renders the output without its adjustments, at the same size, for comparing. Default: `false`. |
| `draft` | boolean | A quick render while a slider moves. Default: `false`. |
| `height` | integer, from 1 to 16384 | **Required.** The largest height, in pixels, from 1 to 16384. |
| `overlay` | integer | A mask whose region is shown tinted red, by id. |
| `width` | integer, from 1 to 16384 | **Required.** The largest width, in pixels, from 1 to 16384. |

Result:

| Name | Type | Description |
|---|---|---|
| `height` | integer |  |
| `output` | integer | An output's id in its project. |
| `width` | integer |  |

Examples:

```json
{"height":1000,"width":1600}
{"draft":true,"height":600,"overlay":2,"width":800}
{"before":true,"height":1000,"width":1600}
```

### `adjust.removeMask`

**Remove mask.** Removes one of an output's masks, in one undo step.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `mask` | integer | **Required.** The mask's id, as `adjust.get` lists it. |
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `adjustments` | object | An output's adjustments: the whole image's, then each mask's in order. |
| `mask` | integer | The mask's id. |
| `neutral` | boolean | Whether they leave the output as it is. |
| `output` | integer | An output's id in its project. |

Example:

```json
{"mask":1,"output":3}
```

### `adjust.reset`

**Reset adjustments.** Removes an output's adjustments and masks, in one undo step.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `adjustments` | object | An output's adjustments: the whole image's, then each mask's in order. |
| `neutral` | boolean | Whether they leave the output as it is. |
| `output` | integer | An output's id in its project. |

Example:

```json
{"output":3}
```

### `adjust.sampleColor`

**Sample color.** The color of the output open for adjusting at a point, for a color-range mask.

`x` and `y` are fractions of the output's width and height. The color is in `OkLab`, after the whole image's adjustments, averaged over a few pixels: what a `color` shape's `samples` hold. `null` outside the image.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `x` | number, from -10.0 to 10.0 | **Required.** From 0 (the left edge) to 1 (the right edge). |
| `y` | number, from -10.0 to 10.0 | **Required.** From 0 (the top edge) to 1 (the bottom edge). |

Result:

| Name | Type | Description |
|---|---|---|
| `color` | array of number | Lightness, then the green-red and blue-yellow axes; `null` outside the image. |

Example:

```json
{"x":0.42,"y":0.6}
```

### `adjust.select`

**AI select.** Selects an object of the output open for adjusting from clicks on it, with the AI model, into a mask, as one undo step.

The clicks (`at`: fractions of the output's width and height) are on the object, or, with `include` false, on what to leave out of it; send them all each time. The selection goes into the mask's first `object` shape, or a new one. The first selection on an output loads the model and reads the image, which takes tens of seconds on a CPU; later ones about a second. Needs the model (`adjust.model`).

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `clicks` | array of object | **Required.** The clicks, all of them, at most 64: on the object, or with `include` false on what to leave out of it. |
| `mask` | integer | **Required.** The mask's id, as `adjust.get` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `component` | integer |  |
| `loaded` | boolean | Whether the model had to load or read the image first (the slow part). |
| `mask` | integer | The mask, and the index of the shape the selection went into. |

Examples:

```json
{"clicks":[{"at":[0.48,0.52]}],"mask":1}
{"clicks":[{"at":[0.48,0.52]},{"at":[0.7,0.2],"include":false}],"mask":1}
```

### `adjust.set`

**Set adjustments.** Replaces an output's adjustments, the whole image's and its masks', in one undo step.

Settings left out take their neutral defaults. Sliders run from -100 to 100 (0 neutral) unless their description says otherwise: exposure in stops from -5 to 5, sharpening from 0 to 150, noise reduction from 0 to 100. Masks keep their ids; a new mask's id must be one no mask of the output has. Adjustments apply after the output's retouch strokes, to exports too.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `adjustments` | object | **Required.** The adjustments: the whole image's (`global`) and the masks (`masks`). What is left out is neutral. |
| `adjustments.global` | object | The adjustments of the whole image. Default: `{"blacks":0.0,"clarity":0.0,"color_noise":0.0,"contrast":0.0,"dehaze":0.0,"exposure":0.0,"highlights":0.0,"noise_detail":50.0,"noise_reduction":0.0,"saturation":0.0,"shadows":0.0,"sharpen_detail":25.0,"sharpen_masking":0.0,"sharpen_radius":1.0,"sharpening":0.0,"temperature":0.0,"texture":0.0,"tint":0.0,"vibrance":0.0,"whites":0.0}`. |
| `adjustments.global.blacks` | number, from -100.0 to 100.0 | The black point. Default: `0.0`. |
| `adjustments.global.clarity` | number, from -100.0 to 100.0 | Contrast of mid-sized detail: punch when positive, softness when negative. Default: `0.0`. |
| `adjustments.global.color_noise` | number, from 0.0 to 100.0 | Color noise reduction, from 0 (none) to 100. Default: `0.0`. |
| `adjustments.global.contrast` | number, from -100.0 to 100.0 | Contrast of the tones around middle gray. Default: `0.0`. |
| `adjustments.global.dehaze` | number, from -100.0 to 100.0 | Removes haze (positive) or adds it (negative). Default: `0.0`. |
| `adjustments.global.exposure` | number, from -5.0 to 5.0 | Exposure in stops, from -5 to 5. Default: `0.0`. |
| `adjustments.global.highlights` | number, from -100.0 to 100.0 | Brightness of the bright tones, recovering detail in them when negative. Default: `0.0`. |
| `adjustments.global.noise_detail` | number, from 0.0 to 100.0 | How much detail luminance noise reduction keeps, from 0 to 100. Default: `50.0`. |
| `adjustments.global.noise_reduction` | number, from 0.0 to 100.0 | Luminance noise reduction, from 0 (none) to 100. Default: `0.0`. |
| `adjustments.global.saturation` | number, from -100.0 to 100.0 | Saturation of every color alike; -100 is black and white. Default: `0.0`. |
| `adjustments.global.shadows` | number, from -100.0 to 100.0 | Brightness of the dark tones, opening them up when positive. Default: `0.0`. |
| `adjustments.global.sharpen_detail` | number, from 0.0 to 100.0 | How much sharpening favors fine detail over edges, from 0 to 100. Default: `25.0`. |
| `adjustments.global.sharpen_masking` | number, from 0.0 to 100.0 | Limits sharpening to edges, from 0 (everywhere) to 100 (only the strongest edges). Default: `0.0`. |
| `adjustments.global.sharpen_radius` | number, from 0.5 to 3.0 | The size of the detail sharpening works on, in pixels, from 0.5 to 3. Default: `1.0`. |
| `adjustments.global.sharpening` | number, from 0.0 to 150.0 | Sharpening, from 0 (none) to 150. Default: `0.0`. |
| `adjustments.global.temperature` | number, from -100.0 to 100.0 | White balance: warmer (positive) or cooler (negative), on a scale of mireds around the image as stacked. Default: `0.0`. |
| `adjustments.global.texture` | number, from -100.0 to 100.0 | Contrast of fine detail and surface texture. Default: `0.0`. |
| `adjustments.global.tint` | number, from -100.0 to 100.0 | White balance: magenta (positive) or green (negative). Default: `0.0`. |
| `adjustments.global.vibrance` | number, from -100.0 to 100.0 | Saturation that spares colors already saturated, and skin tones. Default: `0.0`. |
| `adjustments.global.whites` | number, from -100.0 to 100.0 | The white point. Default: `0.0`. |
| `adjustments.masks` | array of object | The masks, each a region with adjustments of its own, applied on top of the whole image's. Default: `[]`. |
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `adjustments` | object | An output's adjustments: the whole image's, then each mask's in order. |
| `neutral` | boolean | Whether they leave the output as it is. |
| `output` | integer | An output's id in its project. |

Examples:

```json
{"adjustments":{"global":{"clarity":20,"exposure":0.3,"vibrance":15}},"output":3}
{"adjustments":{"masks":[{"adjust":{"exposure":-0.5},"components":[{"shape":{"end":[0.5,0.4],"kind":"linear","start":[0.5,0]}}],"id":1,"name":"Sky"}]},"output":3}
```

### `adjust.setMask`

**Set mask.** Replaces one of an output's masks, the one with the same id, in one undo step.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `mask` | object | **Required.** The mask: its name, components and adjustments. |
| `mask.adjust` | object | The adjustments it applies where it selects. Default: `{"amount":100.0,"blacks":0.0,"clarity":0.0,"contrast":0.0,"dehaze":0.0,"exposure":0.0,"highlights":0.0,"noise":0.0,"saturation":0.0,"shadows":0.0,"sharpness":0.0,"temperature":0.0,"texture":0.0,"tint":0.0,"whites":0.0}`. |
| `mask.adjust.amount` | number, from 0.0 to 100.0 | How strongly the mask applies its adjustments, from 0 (not at all) to 100. Default: `100.0`. |
| `mask.adjust.blacks` | number, from -100.0 to 100.0 | The black point. Default: `0.0`. |
| `mask.adjust.clarity` | number, from -100.0 to 100.0 | Contrast of mid-sized detail. Default: `0.0`. |
| `mask.adjust.contrast` | number, from -100.0 to 100.0 | Contrast. Default: `0.0`. |
| `mask.adjust.dehaze` | number, from -100.0 to 100.0 | Removes haze (positive) or adds it (negative). Default: `0.0`. |
| `mask.adjust.exposure` | number, from -5.0 to 5.0 | Exposure in stops, from -5 to 5. Default: `0.0`. |
| `mask.adjust.highlights` | number, from -100.0 to 100.0 | Brightness of the bright tones. Default: `0.0`. |
| `mask.adjust.noise` | number, from -100.0 to 100.0 | Reduces noise (positive) or keeps more grain (negative). Default: `0.0`. |
| `mask.adjust.saturation` | number, from -100.0 to 100.0 | Saturation; -100 is black and white. Default: `0.0`. |
| `mask.adjust.shadows` | number, from -100.0 to 100.0 | Brightness of the dark tones. Default: `0.0`. |
| `mask.adjust.sharpness` | number, from -100.0 to 100.0 | Sharpens (positive) or softens (negative). Default: `0.0`. |
| `mask.adjust.temperature` | number, from -100.0 to 100.0 | Warmer (positive) or cooler (negative). Default: `0.0`. |
| `mask.adjust.texture` | number, from -100.0 to 100.0 | Contrast of fine detail and surface texture. Default: `0.0`. |
| `mask.adjust.tint` | number, from -100.0 to 100.0 | Magenta (positive) or green (negative). Default: `0.0`. |
| `mask.adjust.whites` | number, from -100.0 to 100.0 | The white point. Default: `0.0`. |
| `mask.components` | array of object | Its shapes, combined in order: the first one's region, then each next one added, subtracted or intersected. Default: `[]`. |
| `mask.id` | integer | The mask's id, unique among the output's masks and never reused: commands name a mask by it. Default: `0`. |
| `mask.invert` | boolean | Whether it selects what its components leave out instead. Default: `false`. |
| `mask.name` | string | Its name, at most 64 characters. Default: `""`. |
| `mask.refine` | number, from 0.0 to 100.0 | How closely its edges follow the image's own, from 0 (as drawn) to 100. Default: `0.0`. |
| `mask.visible` | boolean | Whether its adjustments apply. A hidden mask is kept but changes nothing. Default: `true`. |
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `adjustments` | object | An output's adjustments: the whole image's, then each mask's in order. |
| `mask` | integer | The mask's id. |
| `neutral` | boolean | Whether they leave the output as it is. |
| `output` | integer | An output's id in its project. |

Example:

```json
{"mask":{"adjust":{"clarity":30},"components":[{"shape":{"kind":"whole"}}],"id":1,"name":"Center","visible":false},"output":3}
```

### `adjust.setModel`

**Set the AI model's folder.** Checks a folder of SAM 3's files and remembers it for AI selections.

Checks that the folder holds `model.safetensors` (the official checkpoint: its size and SHA-256, which takes a while over 3.4 GB), `vocab.json` and `merges.txt`, then remembers it in the user's settings. The files stay where they are.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | Nothing | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `folder` | string | **Required.** The folder holding `model.safetensors`, `vocab.json` and `merges.txt`. |

Result:

| Name | Type | Description |
|---|---|---|
| `folder` | string |  |

Example:

```json
{"folder":"C:/Users/me/Downloads/sam3"}
```

### `adjust.stroke`

**Brush stroke.** Paints a brush stroke onto a mask, as one undo step.

The stroke goes on top of the mask's brush component `component` (an index into its components), or of its last brush component, or of a new one added when it has none. Its `points` are fractions of the output's width and height, its `size` a fraction of its long edge; `erase` takes away what earlier strokes of the component painted.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `component` | integer | The brush component it goes onto, by its index among the mask's components; its last brush component when left out, or a new one when it has none. |
| `mask` | integer | **Required.** The mask's id, as `adjust.get` lists it. |
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |
| `stroke` | object | **Required.** The stroke. |
| `stroke.erase` | boolean | Whether it erases what earlier strokes painted. Default: `false`. |
| `stroke.feather` | number, from 0.0 to 100.0 | How softly its edge fades, from 0 (hard) to 100. Default: `50.0`. |
| `stroke.flow` | number, from 0.0 to 100.0 | How much each dab adds, from 0 to 100. Default: `100.0`. |
| `stroke.points` | array of array of number | The pointer's path, as fractions of the image's width and height. Default: `[]`. |
| `stroke.size` | number, from 0.0 to 1.0 | The brush's radius, as a fraction of the image's long edge, above 0 and at most 1. Default: `0.03`. |

Result:

| Name | Type | Description |
|---|---|---|
| `component` | integer |  |
| `mask` | integer | The mask's id, and the index of the component the stroke went onto. |
| `strokes` | integer | How many strokes the component has now. |

Examples:

```json
{"mask":1,"output":3,"stroke":{"feather":60,"flow":100,"points":[[0.2,0.3],[0.25,0.32],[0.3,0.36]],"size":0.04}}
{"component":0,"mask":1,"output":3,"stroke":{"erase":true,"points":[[0.22,0.31]],"size":0.02}}
```

## align

### `align.get`

**Alignment settings.** The stack's alignment, ordering, framing and focus settings.

Every setting, with the keys the preferences don't set at their defaults: which terms are corrected and their limits, the processing order (`automatic_order`, `reverse_order`), the output's framing (`narrow`, `wide` or `common`) and the out-of-focus threshold (`min_sharp_fraction`).

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `settings` | object | Every setting, with the defaults filled in. |

Example:

```json
{}
```

### `align.info`

**Alignment.** The stack's alignment as the project holds it: each frame's, the processing order and the output's framings.

For each aligned frame: its transform from the master's pixels and its terms (scale, rotation, shift), its brightness gain and score, its status and flags, its sharp-content fraction and what moved in it against its neighbor. For the run: the master, the frames' size, the narrow end, the processing order and the settings it ran with. For each framing (narrow, wide, common): the size of the output it gives and the transform from its pixels to the master's, or why the frames can't be framed that way; `framing` is the one the settings choose.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `aligned` | boolean | Whether the project holds an alignment run. |
| `framing` | `"narrow"` \| `"wide"` \| `"common"` | The framing the settings choose. |
| `run` | object | The run, when there is one. |

Example:

```json
{}
```

### `align.reset`

**Reset alignment settings.** Puts every alignment setting back at its default.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `settings` | object | Every setting, with the defaults filled in. |

Example:

```json
{}
```

### `align.run`

**Align frames.** Aligns the enabled frames and orders the stack from its narrow end.

Reads and develops every enabled frame in turn and aligns each to its neighbor, correcting the terms the settings say. Then the end of the stack with the narrower field of view becomes its start, and its frame the master that the others are aligned to (unless `automatic_order` is off, or `reverse_order` on), and every frame's sharp-content fraction is measured: the frames with no in-focus detail are flagged `out-of-focus`, for you to disable with `project.setFramesEnabled`. Frames that aren't enabled lose any alignment they had. The result lists the master, the order and the flagged and rejected frames; `align.info` has every frame's details. Runs as a job; the alignment is recorded in one undo step when it ends, or not at all when it is cancelled.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `defects` | object | The defect mask this run detected and inpainted, when the settings asked for one (`detect_defects`) and the project had none. |
| `frameOrder` | array of integer | After sorting by scale: the project's frame order now. |
| `master` | integer | The frame the others are aligned to, which the output is framed on. |
| `moved` | array of integer | The frames in which part of the subject moved against their neighbor: `filmstrip.list` shows where. |
| `narrowEnd` | `"first"` \| `"last"` \| `null` | The end of the stack with the narrower field of view, when the frames' scales tell. |
| `order` | array of integer | The aligned frames, in the order the stack is processed. |
| `outOfFocus` | array of integer | The frames with no in-focus detail: flagged for you to disable. |
| `rejected` | array of integer | The frames whose alignment couldn't be trusted, and keep their neighbor's (`align.info` says why). |
| `start` | `"first"` \| `"last"` | The end the stack is processed from. |

Example:

```json
{}
```

### `align.set`

**Set alignment settings.** Changes some of the stack's alignment, ordering, framing and focus settings.

Only the keys given change, as when a preset is applied: `{"framing": "wide"}` keeps the widest field of view and leaves the rest as it is. The result is checked whole, and a misspelled key is an error. The framing applies at once, since it follows from the stored alignment (`align.info`); the other settings apply when the stack is next aligned.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `automatic_order` | boolean | Process the stack from the end with the narrower field of view, found from the scales of its first and last frames: the frames are aligned to that end's frame, and with the default framing every pixel of the output is covered by every frame. Off, the stack is processed from its first frame. Default: `true`. |
| `brightness` | boolean | Match each frame's brightness to the master's with a gain. Default: `true`. |
| `detect_defects` | boolean | Before aligning, when the project has no defect mask, detect one from the frames (dust and hot pixels, which stay at the same sensor pixels) and inpaint it in every frame. Off by default: review a detected mask before stacking with it (`defects.rs`). A mask the project has, detected or supplied, is always inpainted. Default: `false`. |
| `framing` | `"narrow"` \| `"wide"` \| `"common"` | The output's field of view: `narrow` crops to the narrow end's (every frame covers it), `wide` keeps the widest, marking what each frame doesn't cover in its coverage mask, and `common` crops to what every frame covers once aligned. Default: `"narrow"`. |
| `max_rotation` | number, from 0.0 to 45.0 | The largest rotation relative to the master, in degrees, from 0 to 45. Default: `5.0`. |
| `max_scale` | number, from 0.0 to 50.0 | The largest change of scale relative to the master, in percent, from 0 to 50. Default: `15.0`. |
| `max_shift` | number, from 0.0 to 50.0 | The largest shift relative to the master, in percent of the frame's width horizontally and of its height vertically, from 0 to 50. Default: `10.0`. |
| `min_motion` | number, from 0.1 to 100.0 | The smallest local movement flagged, in pixels of the frames, from 0.1 to 100: a region of a frame that moved at least this much against its neighbor, once aligned, is marked in its motion map, and a frame with such a region is flagged as moved (`motion.rs`). Movement is measured on a level of at most 1024 pixels, and below three quarters of a pixel there it can't be told from the apparent motion of defocus and noise: on larger frames, the threshold is at least that many frame pixels (4.4 on a 6000-pixel frame). Default: `2.0`. |
| `min_score` | number, from 0.0 to 1.0 | The lowest score at which a frame counts as matched, from 0 to 1: the correlation of the frame's band-passed luminance with its reference's once aligned. On the synthetic test stacks, neighbors score above 0.9 and a frame of another scene near 0. Default: `0.3`. |
| `min_sharp_fraction` | number, from 0.0 to 1.0 | A frame is flagged as out of focus when its sharp-content fraction, the share of the stack's in-focus detail on which it is among the sharpest frames, is less than this, from 0 to 1 (0 flags none). Such frames, common at the start and the end of a stack, add nothing but blur; they are only flagged, for the user to exclude. Default: `0.002`. |
| `refine` | boolean | After chaining each frame to its neighbor, align it again directly against the master, starting from the chained estimate, so that the small errors of a long chain don't add up. The direct estimate is kept only when it scores at least `min_score`: frames focused far from the master share little detail with it, so this helps shallow stacks of similar frames most. Default: `false`. |
| `reject` | boolean | Reject the frames whose alignment hit a limit, scored below `min_score` or found no match: a rejected frame keeps the transform of the frame it was compared with, and the next frame is aligned to the last frame that wasn't rejected. Off, such frames are only flagged. Default: `true`. |
| `resolution` | integer, from 64 to 16384 | The longer side, in pixels, of the finest level alignment works at, from 64 to 16384: larger frames are reduced to it first. Alignment is sub-pixel at that level, so higher values cost time for little gain. Default: `2048`. |
| `reverse_order` | boolean | Process the stack from the other end: from its wide end with `automatic_order`, from its last frame without. Default: `false`. |
| `rotation` | boolean | Correct rotation. Default: `true`. |
| `scale` | boolean | Correct changes of scale: focus breathing, or the magnification a rail changes as it moves. Default: `true`. |
| `shift_x` | boolean | Correct horizontal shifts. Default: `true`. |
| `shift_y` | boolean | Correct vertical shifts. Default: `true`. |
| `white_balance` | boolean | Match each frame's color to the master's with a gain per color channel, which changes the color and not the brightness: for JPEGs shot with automatic white balance, and light that changes color. Off by default. Default: `false`. |

Result:

| Name | Type | Description |
|---|---|---|
| `settings` | object | Every setting, with the defaults filled in. |

Examples:

```json
{"framing":"wide"}
{"brightness":false,"rotation":false}
{"automatic_order":false,"min_sharp_fraction":0.005,"reverse_order":true}
```

### `align.sortByScale`

**Sort frames by scale.** Puts the enabled frames in the order of their scale, then aligns them in that order.

Repairs a stack shot or imported out of order. Focus breathing, or a rail, changes the magnification steadily with the focus, so the frames' scales give their focus order. A first pass aligns the frames as they are, to measure their scales; the frames are then sorted by scale, in the direction that keeps the stack's present trend, and aligned again in that order, as `align.run` does. Disabled frames keep their places in the list. Fails, changing nothing, when the scales differ too little to sort by. Runs as a job, which reads every frame twice; the new order and the alignment are one undo step.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `defects` | object | The defect mask this run detected and inpainted, when the settings asked for one (`detect_defects`) and the project had none. |
| `frameOrder` | array of integer | After sorting by scale: the project's frame order now. |
| `master` | integer | The frame the others are aligned to, which the output is framed on. |
| `moved` | array of integer | The frames in which part of the subject moved against their neighbor: `filmstrip.list` shows where. |
| `narrowEnd` | `"first"` \| `"last"` \| `null` | The end of the stack with the narrower field of view, when the frames' scales tell. |
| `order` | array of integer | The aligned frames, in the order the stack is processed. |
| `outOfFocus` | array of integer | The frames with no in-focus detail: flagged for you to disable. |
| `rejected` | array of integer | The frames whose alignment couldn't be trusted, and keep their neighbor's (`align.info` says why). |
| `start` | `"first"` \| `"last"` | The end the stack is processed from. |

Example:

```json
{}
```

## annotation

### `annotation.get`

**Annotation layers.** Lists an output's non-destructive annotation layers.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `output` | integer | **Required.** Output ID from output.list. |

Result:

| Name | Type | Description |
|---|---|---|
| `layers` | array of object |  |

Example:

```json
{"output":3}
```

### `annotation.set`

**Set annotation layers.** Replaces an output's annotation layers in one undo step.

Layers are rendered onto exported copies only. Scale bars use the export's pixels_per_micrometer calibration; automatic lengths use 1, 2 or 5 times a power of ten. Text positions are fractions of the output size. Hidden layers stay saved. An empty list removes every layer.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `layers` | array of object | **Required.** Complete layer list, bottom first, at most 256 layers. |
| `output` | integer | **Required.** Output ID from output.list. |

Result:

| Name | Type | Description |
|---|---|---|
| `layers` | array of object |  |

Example:

```json
{"layers":[{"annotation":{"kind":"scale-bar"},"name":"Scale","textSize":16,"visible":true,"white":true}],"output":3}
```

## batch

### `batch.run`

**Process batch.** Queues stacks as separate projects, exports their outputs and writes a summary CSV.

Immediate subfolders are stacks; a folder with images directly inside is auto-split with project.findStacks. Mixed layouts are refused. Each stack receives the preset, a TOML file or a built-in one as `builtin:<id>` (`preset.list`), including depth-map contrast thresholds; each output is exported as the preset's output settings say (`export.run`: a 16-bit TIFF unless they say otherwise, with the maps and aligned frames they ask for), the template's extension giving way to their format's. Failures are recorded and the queue continues. Outputs and projects are staged until the cancellable work ends, then published without replacing existing files. summary.csv lists stack, frames, method, time in seconds, warnings, project, output and error. Templates accept {stack}, {method}, {range}, {first}, {last}, {date} (UTC YYYY-MM-DD), and {params} (the method's effective settings).

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | No | Nothing | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `input` | string | **Required.** The folder of stack folders, or one folder of frames to auto-split. |
| `method` | `"pyramid"` \| `"depth-map"` \| `"average"` \| `"wavelet"` | Method to run for each stack; pyramid by default. Default: `"pyramid"`. |
| `output` | string | **Required.** Destination for individual projects, TIFFs and summary.csv; existing entries are refused. |
| `preset` | string | **Required.** TOML preset applied to each project, including its depth-map contrast threshold; or a built-in one as `builtin:<id>` (`preset.list`). |
| `split` | object | Boundary detection settings for a flat folder; omitted settings use their defaults. Default: `{"gap":null,"maxScaleStep":5.0,"maxShift":10.0,"minScore":0.3,"minSimilarity":0.9}`. |
| `split.gap` | number, at least 0.0 | A gap in capture time longer than this, in seconds, cuts the batch; `None` chooses it from the batch: 30 s or 8 times its median gap, whichever is longer. |
| `split.maxScaleStep` | number, from 0.0 to 50.0 | A change of scale between matching neighbors beyond this, in percent, is a new framing. Default: `5.0`. |
| `split.maxShift` | number, from 0.0 to 50.0 | A shift between matching neighbors beyond this, in percent of the frame's diagonal, is a new framing. Default: `10.0`. |
| `split.minScore` | number, from 0.0 to 1.0 | Neighbors that match with a lower score than this, from 0 to 1, and aren't similar, are of different scenes. Default: `0.3`. |
| `split.minSimilarity` | number, from -1.0 to 1.0 | Neighbors whose low-passed luminance correlates less than this, from -1 to 1, and that don't match, are of different scenes. Default: `0.9`. |
| `template` | string | A TIFF filename with {stack}, {method}, {range}, {first}, {last}, {date} and {params} tokens. Default: `"{stack}-{method}-{range}.tif"`. |

Result:

| Name | Type | Description |
|---|---|---|
| `failed` | integer | Number of failed entries; a CLI reports failure when nonzero. |
| `stacks` | array of object | Each queue entry, in processing order. |
| `summary` | string | The CSV summary file. |

Examples:

```json
{"input":"C:/Stacks","output":"C:/Results","preset":"C:/Presets/macro.toml"}
{"input":"C:/Stacks","output":"C:/Results","preset":"builtin:insect-macro"}
```

## cache

### `cache.info`

**Cache sizes.** How much the caches take: temporary projects, spilled tiles and the open project's cache.

Walks the folders and adds up their files: the temporary projects' folder (unnamed projects, which orphan cleanup moves to the trash once they are old), the tile folder (tiles of running stacks, and those crashed runs left), and the open project's `cache/` and `previews/` folders (results kept so that nothing is computed twice; cleaned of what the project no longer refers to when it closes).

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | Nothing | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `project` | object | The open project's `cache/` and `previews/` folders; none without a project. |
| `temporaryProjects` | object | Temporary projects; none when the engine has no folder for them. |
| `tiles` | object | Tiles of running stacks, and of crashed runs. In the system's temporary directory, only FloraStack's tile folders count. |

Example:

```json
{}
```

### `cache.removeAbandoned`

**Remove abandoned tile files.** Removes the tile folders that crashed runs left in the tile folder.

Stacks keep their accumulators' tiles in a folder of the tile folder (the system's temporary directory unless `compute.set` chose another) when they exceed their memory budget, and remove it when they end; a run that crashes leaves it behind. This removes every such folder that no run holds and that hasn't changed for a minute (a folder from before tile folders had locks, once it is a day old). They hold nothing a run can use again, so they are deleted, not moved to the trash. The desktop app runs this at startup.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | Nothing | Files outside the project |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `folder` | string | The folder looked in: the tile folder. |
| `removed` | array of string |  |

Example:

```json
{}
```

### `cache.setFolder`

**Set cache folder.** Puts temporary projects and spilled tiles in a folder of your choice.

Temporary projects go into `projects` and spilled tiles into `tiles` under `folder`, made if they don't exist, for projects and stacks started from now on: a project open now stays where it is. A fast local drive with room to spare is best. For this engine; it isn't stored in the project.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | Nothing | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `folder` | string | **Required.** The folder: temporary projects go into its `projects` folder, spilled tiles into its `tiles` folder. |

Result:

| Name | Type | Description |
|---|---|---|
| `project` | object | The open project's `cache/` and `previews/` folders; none without a project. |
| `temporaryProjects` | object | Temporary projects; none when the engine has no folder for them. |
| `tiles` | object | Tiles of running stacks, and of crashed runs. In the system's temporary directory, only FloraStack's tile folders count. |

Example:

```json
{"folder":"D:/FloraStack cache"}
```

## calibration

### `calibration.get`

**Calibration profiles.** Lists the user's objective and magnification calibrations.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | Nothing | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `profiles` | array of object | The full library, at most 64 uniquely named calibrations. An empty list removes the library. |

Example:

```json
{}
```

### `calibration.set`

**Save calibration profiles.** Replaces the user's library of calibration profiles.

The library is saved as calibrations.json beside the host's temporary-projects folder, independently of projects and presets. Select a profile in the Export tab to copy its value into that project's export settings. Editing the library then leaves existing measurements reproducible.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | Nothing | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `profiles` | array of object | **Required.** The full library, at most 64 uniquely named calibrations. An empty list removes the library. |

Result:

| Name | Type | Description |
|---|---|---|
| `profiles` | array of object | The full library, at most 64 uniquely named calibrations. An empty list removes the library. |

Example:

```json
{"profiles":[{"name":"10x","pixelsPerMicrometer":2.5}]}
```

## capture

### `capture.calculate`

**Calculate rail step.** Recommends diffraction-based physical rail spacing and explains sensor sampling.

Uses 550 nm light, air, pupil ratio 1, N_eff=N(1+m), NA=m/(2 N_eff), wave depth=λ/NA² and 50% overlap. Pixel pitch assesses Airy-disk sampling, not a circle of confusion. Approximate paraxial model; see book/src/user/capture.md. Motor pulses cannot be converted to this spacing.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | Nothing | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `aperture` | number, from 0.5 to 64 | **Required.** Marked f-number at infinity, before extension (not the camera's effective f-number). |
| `magnification` | number, from 0.01 to 100 | **Required.** Image size divided by object size, e.g. 1 for 1:1 macro. |
| `pixelPitch` | number, from 0.1 to 100 | **Required.** Sensor pixel spacing in micrometers. |

Result:

| Name | Type | Description |
|---|---|---|
| `airyDiameterPixels` | number |  |
| `depthMicrometers` | number |  |
| `effectiveAperture` | number |  |
| `explanation` | string |  |
| `stepMicrometers` | number |  |

Example:

```json
{"aperture":4,"magnification":1,"pixelPitch":4}
```

### `capture.camera`

**Control camera.** Discovers, connects or operates a supported camera on a device worker.

Actions: discover, connect (exactly one camera), disconnect, liveView (returns a temporary JPEG path), focus (signed unit pulses), markNear, markFar, invalidateMarks. Marks exist only within this connection and count relative commands, not absolute optical positions. Disable continuous autofocus and do not touch the focus ring; invalidate marks whenever uncertain. A focus error invalidates both marks. gphoto on Linux/macOS only; other builds use the watch-folder workflow.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the device lane | No | Nothing | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `action` | `"discover"` \| `"connect"` \| `"disconnect"` \| `"liveView"` \| `"focus"` \| `"markNear"` \| `"markFar"` \| `"invalidateMarks"` | **Required.** discover, connect, disconnect, liveView, focus, markNear, markFar or invalidateMarks. |
| `pulses` | integer, from -10000 to 10000 | Signed unit motor pulses for focus only; positive moves toward far. Default: `0`. |

Result:

| Name | Type | Description |
|---|---|---|
| `devices` | array of string |  |
| `focus` | object | Session-relative focus marks. Manual focus, autofocus, reconnects and any movement error invalidate both marks. |
| `path` | string |  |

Examples:

```json
{"action":"discover"}
{"action":"focus","pulses":1}
```

### `capture.info`

**Capture devices.** Reports connected devices, capabilities and relative lens marks.

Does no device I/O; reports busy while a worker owns the devices. Camera control requires a gphoto build on Linux or macOS. Manual focus or autofocus invalidates relative marks: use invalidateMarks before marking again.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | Nothing | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `busy` | boolean |  |
| `camera` | object |  |
| `cameraControl` | boolean |  |
| `focus` | object | Session-relative focus marks. Manual focus, autofocus, reconnects and any movement error invalidate both marks. |
| `rail` | boolean |  |

Example:

```json
{}
```

### `capture.rail`

**Control rail.** Lists serial ports, connects a rail, moves it or stops it.

Actions: ports, connect (settings required), disconnect, move (micrometers), stop. Establish a safe physical zero before connecting and set the travel limit accordingly. Marlin uses relative X millimeters and M400; Arduino uses calibrated integer steps. Stop is best-effort, invalidates position and requires reconnect; use job.cancel for an active device job, which stops at its next I/O checkpoint. Zaber ASCII is explicitly unavailable. No proprietary protocols are inferred.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the device lane | No | Nothing | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `action` | `"ports"` \| `"connect"` \| `"disconnect"` \| `"move"` \| `"stop"` | **Required.** ports, connect, disconnect, move or stop. |
| `micrometers` | number, from -1000000 to 1000000 | Signed displacement for move only, in micrometers from the current position. Default: `0.0`. |
| `settings` | object | Required only for connect; origin must already be physically established. |
| `settings.baud` | integer, from 1200 to 1000000 | **Required.** Firmware baud rate (8 data bits, no parity, one stop bit). |
| `settings.feed` | number, from 0.01 to 10000 | **Required.** Marlin feed rate, millimeters per minute. |
| `settings.micrometersPerStep` | number, from 0.001 to 10000 | **Required.** Calibrated micrometers per full controller step, used for Arduino MOVE quantization. |
| `settings.port` | string | **Required.** Serial device, e.g. COM3 or /dev/ttyACM0. |
| `settings.protocol` | `"marlin"` \| `"arduino"` \| `"zaber-ascii"` | **Required.** Explicit firmware dialect; not auto-detected. |
| `settings.timeoutMs` | integer, from 100 to 3600000 | **Required.** Timeout for one completed movement or shutter operation, milliseconds. |
| `settings.travelMicrometers` | number, from 0.001 to 1000000 | **Required.** Maximum absolute displacement from the connection's user-established zero, in micrometers. |

Result:

| Name | Type | Description |
|---|---|---|
| `devices` | array of string |  |
| `focus` | object | Session-relative focus marks. Manual focus, autofocus, reconnects and any movement error invalidate both marks. |
| `path` | string |  |

Examples:

```json
{"action":"ports"}
{"action":"move","micrometers":50}
```

### `capture.run`

**Capture and stack.** Shoots a lens or rail sequence into the watched folder while stacking previews.

Requires an empty input folder, output outside it and a preset. mode=rail takes frames photos separated by stepMicrometers after settling; mode=lens takes frames photos including both relative near/far marks (frames-1 intervals). Trigger=camera downloads each original atomically with a unique filename; controller sends DIY SHUTTER then waits for external ingest; external waits for a new readable frame before advancing. External/controller workflows require one photo per position and frameTimeoutMs greater than exposure plus transfer/processing time. Managed completion overrides watch.idleSeconds while shooting. On cancellation or failure, stops further movement/shots, joins hardware work and keeps completed originals. A blocking libgphoto2 call finishes before cancellation can stop the next operation; final stack publication is atomic as for watch.run.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the device lane | No | Nothing | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `frameTimeoutMs` | integer, from 100 to 3600000 | External/controller timeout per new ingested frame, including exposure, transfer and preview processing. Default: `120000`. |
| `frames` | integer, from 2 to 10001 | **Required.** Number of photos; lens mode includes both marked endpoints (frames-1 intervals). |
| `mode` | `"rail"` \| `"lens"` | **Required.** rail for physical spacing, lens for relative near/far motor marks. |
| `settleMs` | integer, from 0 to 60000 | Vibration settling after completed movement, milliseconds. Default: `300`. |
| `stepMicrometers` | number, from -1000000 to 1000000 | Signed physical spacing in micrometers for rail mode; zero for lens mode. Default: `0.0`. |
| `trigger` | `"camera"` \| `"controller"` \| `"external"` | **Required.** camera downloads, controller triggers the DIY shutter, external waits for a tethered image. |
| `watch` | object | **Required.** Watch-folder ingest, preview, preset and final output settings. idleSeconds applies only after shooting ends. |
| `watch.idleSeconds` | number, from 0.1 to 86400 | Seconds without a new or changed frame before finalization; waits at least settleMs too. Default: `5.0`. |
| `watch.input` | string | **Required.** Folder populated by a tethered camera or its capture application. |
| `watch.method` | `"pyramid"` \| `"depth-map"` \| `"average"` \| `"wavelet"` | Method used for the running preview and the final output. Default: `"pyramid"`. |
| `watch.output` | string | **Required.** Folder for the saved project, TIFF and summary.csv, outside input. |
| `watch.pollMs` | integer, from 10 to 5000 | Maximum milliseconds between directory scans, including when native events are unavailable. Default: `250`. |
| `watch.preset` | string | **Required.** TOML preset, including contrast thresholds for unattended depth-map runs; or a built-in one as `builtin:<id>`. |
| `watch.settleMs` | integer, from 50 to 60000 | Milliseconds a file's size and modification time must remain unchanged before decoding. Default: `1000`. |
| `watch.template` | string | A portable image filename; accepts batch.run's template tokens, its extension following the preset's format. Default: `"{stack}-{method}-{range}.tif"`. |

Result:

| Name | Type | Description |
|---|---|---|
| `failed` | integer | Number of failed entries; a CLI reports failure when nonzero. |
| `stacks` | array of object | Each queue entry, in processing order. |
| `summary` | string | The CSV summary file. |

Example:

```json
{"frames":10,"mode":"rail","stepMicrometers":50,"trigger":"external","watch":{"input":"C:/Capture","output":"C:/Results","preset":"builtin:insect-macro"}}
```

## compute

### `compute.info`

**Compute device.** Which device stacks, the GPU or the CPU, and why, and how much memory stacking may use.

The compute preference (`auto`, `cpu` or `gpu`), the device it chose (`gpu` or `cpu`) with the GPU's adapter, or the reason the CPU stacks, and the GPU memory budget of a stack's accumulators. The first call in a process (or the first stack) probes the GPU: a device on the best adapter, every kernel compiled, and a self-test against the CPU's results; with `auto` a software adapter isn't taken. `memory`: the memory a stack's working data may take on the host (beyond it, tiles go to the tile folder on disk), the memory installed, and the budget suggested for this machine.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | Nothing | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `adapter` | object | The GPU's adapter, when stacking on the GPU. |
| `budgetMb` | integer | The GPU memory a stack's accumulators may take, in MiB. |
| `device` | string | What stacks: `gpu` or `cpu`. |
| `memory` | object | The memory stacking may use on the host, and where it spills beyond it. |
| `preference` | `"auto"` \| `"cpu"` \| `"gpu"` | The preference: `auto`, `cpu` or `gpu`. |
| `reason` | string | Why the CPU stacks, when it does. |

Example:

```json
{}
```

### `compute.set`

**Set compute device.** Chooses which device stacks, and how much memory stacking may use.

`device`: `auto` (the default: the GPU when a hardware adapter passes the self-test, the CPU otherwise), `cpu` (always the CPU, whatever the machine has) or `gpu` (software adapters such as lavapipe and WARP taken too, which are slower than the CPU). `budgetMb`: the GPU memory a stack's accumulators may take; beyond it, tiles move to the host between frames. `memoryMb`: the memory a stack's working data may take on the host; beyond it, tiles move to the tile folder on disk, which is slower. `tileFolder`: that folder (empty for the system's temporary folder). The preferences apply to stacks started from now on, for this engine; they aren't stored in the project. Returns what `compute.info` does.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | Nothing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `budgetMb` | integer, from 64 to 1048576 | The GPU memory budget of a stack's accumulators, in MiB, from 64 to 1048576. |
| `device` | `"auto"` \| `"cpu"` \| `"gpu"` | `auto`, `cpu` or `gpu`. |
| `memoryMb` | integer, from 64 to 1048576 | The memory a stack's working data may take on the host, in MiB, from 64 to 1048576. |
| `tileFolder` | string | The folder tiles beyond the memory budget go to; empty for the system's temporary folder. |

Result:

| Name | Type | Description |
|---|---|---|
| `adapter` | object | The GPU's adapter, when stacking on the GPU. |
| `budgetMb` | integer | The GPU memory a stack's accumulators may take, in MiB. |
| `device` | string | What stacks: `gpu` or `cpu`. |
| `memory` | object | The memory stacking may use on the host, and where it spills beyond it. |
| `preference` | `"auto"` \| `"cpu"` \| `"gpu"` | The preference: `auto`, `cpu` or `gpu`. |
| `reason` | string | Why the CPU stacks, when it does. |

Examples:

```json
{"device":"cpu"}
{"budgetMb":2048,"device":"auto"}
{"memoryMb":4096,"tileFolder":"D:/Cache/tiles"}
```

### `compute.test`

**Test the GPU.** Probes the GPU again and runs its self-test, now.

What `compute.info` does on its first call, again: a device on the best adapter the preference allows, every kernel compiled, and a self-test against the CPU's results, after a driver update or a GPU change, say. `device` is the preference to test: the engine's when left out, and `auto` when that is `cpu`. Returns what `compute.info` would with that preference; the engine's preference stays as it is.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | Nothing | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `device` | `"auto"` \| `"cpu"` \| `"gpu"` | `auto` (hardware adapters) or `gpu` (software ones too); the engine's preference when left out, `auto` when that is `cpu`. |

Result:

| Name | Type | Description |
|---|---|---|
| `adapter` | object | The GPU's adapter, when stacking on the GPU. |
| `budgetMb` | integer | The GPU memory a stack's accumulators may take, in MiB. |
| `device` | string | What stacks: `gpu` or `cpu`. |
| `memory` | object | The memory stacking may use on the host, and where it spills beyond it. |
| `preference` | `"auto"` \| `"cpu"` \| `"gpu"` | The preference: `auto`, `cpu` or `gpu`. |
| `reason` | string | Why the CPU stacks, when it does. |

Examples:

```json
{}
{"device":"gpu"}
```

## defects

### `defects.clear`

**Remove defect mask.** Removes the stack's defect mask, so that no pixel is inpainted.

Its file stays until the project is closed, so that undo can bring it back. Align again for it to apply.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `cleared` | boolean | False when the project had none: nothing changed. |

Example:

```json
{}
```

### `defects.detect`

**Detect dust and hot pixels.** Finds the dust and hot pixels of the enabled frames and makes them the stack's defect mask.

Reads up to 32 of the enabled frames, spread over the stack (at least 5 are needed), and marks the pixels that are darker than all around them by the same factor in every frame (dust) or brighter than their neighbors in most frames (hot pixels), at the same sensor pixels, grown to cover the faint rim of each dust shadow. Review the mask before stacking with it: `defects.export` writes it as an image to look at or edit, and `defects.import` reads it back. Replaces the project's mask, as one undo step; align again for it to apply.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `summary` | object | What a stored defect mask is: the summary its cache key holds, so that it can be told without reading the mask. |

Example:

```json
{}
```

### `defects.export`

**Export defect mask.** Writes the stack's defect mask as an 8-bit PNG of black spots on white.

For you to look at, edit in any image editor and read back with `defects.import`. An existing file is replaced only with `overwrite`.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | No | An open project | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `overwrite` | boolean | Replace the file if it exists. Default: `false`. |
| `path` | string | **Required.** The PNG file to write. |

Result:

| Name | Type | Description |
|---|---|---|
| `bytes` | integer | Its size. |
| `path` | string |  |

Examples:

```json
{"path":"C:/Stacks/dust.png"}
{"overwrite":true,"path":"C:/Stacks/dust.png"}
```

### `defects.import`

**Import defect mask.** Makes an image of black spots on white the stack's defect mask.

The image must have the frames' size: black (or any value below half) where a pixel is defective, white elsewhere; alpha is ignored. Any format FloraStack reads will do, such as a PNG drawn over a frame, or one that `defects.export` wrote and you edited. Replaces the project's mask, as one undo step; align again for it to apply.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `path` | string | **Required.** The image to read: black spots on white, the frames' size. |

Result:

| Name | Type | Description |
|---|---|---|
| `summary` | object | What a stored defect mask is: the summary its cache key holds, so that it can be told without reading the mask. |

Example:

```json
{"path":"C:/Stacks/dust.png"}
```

### `defects.info`

**Defect mask.** Whether the stack has a defect mask, where it came from and what it masks.

The defect mask marks the dust and hot pixels that sit at the same sensor pixels in every frame. The alignment jobs inpaint it in every frame before aligning them, and stacking will read the frames the same way. `detect` says whether `align.run` detects a mask when the project has none (the `detect_defects` alignment setting).

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `detect` | boolean | Whether `align.run` detects a mask when the project has none. |
| `mask` | boolean | Whether the project has a defect mask. |
| `summary` | object | What it is, when it has one. |

Example:

```json
{}
```

## develop

### `develop.get`

**Develop settings.** The develop settings of the stack's RAW frames, and how they were resolved.

`settings` are the preferences with every key they don't set at its default. `resolved` is the development worked out on a reference frame (one white balance and one chromatic-aberration correction for every frame), when the project has one; `current` says whether it was resolved with these settings.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `resolved` | object | The stack's development as resolved on its reference frame. |
| `settings` | object | Every setting, with the defaults filled in. |

Example:

```json
{}
```

### `develop.reset`

**Reset develop settings.** Puts every develop setting back at its default.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `settings` | object | Every setting, with the defaults filled in. |

Example:

```json
{}
```

### `develop.set`

**Set develop settings.** Changes some of the develop settings of the stack's RAW frames.

Only the keys given change, as when a preset is applied: `{"exposure": 0.5}` changes the exposure and keeps the rest, and a nested table (`noise_reduction`) merges key by key. The result is checked whole. A misspelled key is an error. They apply to camera RAW and DNG frames; TIFF and other image files are imported unchanged.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `chromatic_aberration` | `"off"` \| `"auto"` \| object | Lateral chromatic aberration correction: the red and blue planes magnified relative to green about the image's center, by a factor of `1 + scale`. Default: `"auto"`. |
| `chromatic_aberration.manual` | object | **Required.** |
| `chromatic_aberration.manual.blue` | number, from -0.01 to 0.01 | **Required.** |
| `chromatic_aberration.manual.red` | number, from -0.01 to 0.01 | **Required.** |
| `demosaic` | `"fast"` \| `"high-quality"` | The demosaic of output frames; previews always use [`Demosaic::Fast`]. Default: `"high-quality"`. |
| `exposure` | number, from -10.0 to 10.0 | Exposure in stops (EV), from -10 to 10: a gain of `2^exposure`, linear like everything else. Default: `0.0`. |
| `highlights` | `"reconstruct"` \| `"off"` | What happens to highlights the sensor clipped. Default: `"reconstruct"`. |
| `lens_distortion` | boolean | Apply the lens distortion correction the file records (DNG `WarpRectilinear`, or a camera's own). Off by default: distortion changes with focus distance, so correcting it changes each frame's geometry differently, which fights alignment. Alignment's scale and shift absorb the focus breathing instead. Default: `false`. |
| `noise_reduction` | object | Light noise reduction: strengths from 0 (off, the default) to 1, as LightCraft's (and Lightroom's) 0 to 100 sliders. Off by default because both filters are nonlinear: the luminance filter smooths fine texture, which stacking reads as focus, and the chroma filter keeps every sample at zero or above, which lifts the noise floor of the darkest areas a little. Stacking averages noise away anyway; turn it on for frames shot at high ISO. Default: `{"color":0.0,"luminance":0.0}`. |
| `noise_reduction.color` | number, from 0.0 to 1.0 | Smoothing of color (chroma) noise, keeping luminance. Default: `0.0`. |
| `noise_reduction.luminance` | number, from 0.0 to 1.0 | Edge-aware smoothing of luminance. It smooths fine texture too, which stacking reads as focus, so keep it low. Default: `0.0`. |
| `white_balance` | `"as-shot"` \| `"auto"` \| object | Which white the frames are balanced for. Default: `"as-shot"`. |
| `white_balance.custom` | object | **Required.** |
| `white_balance.custom.temperature` | number, from 2000.0 to 50000.0 | **Required.** |
| `white_balance.custom.tint` | number, from -150.0 to 150.0 | **Required.** |

Result:

| Name | Type | Description |
|---|---|---|
| `settings` | object | Every setting, with the defaults filled in. |

Examples:

```json
{"exposure":0.5}
{"noise_reduction":{"color":0.3},"white_balance":{"custom":{"temperature":5200,"tint":3}}}
{"demosaic":"fast","lens_distortion":true,"white_balance":"auto"}
```

## edit

### `edit.history`

**Undo history.** Lists what undo and redo would do, and whether the project has unsaved changes.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `dirty` | boolean | Whether the project has unsaved changes. |
| `redo` | array of object | The redo steps, the next to redo first. |
| `undo` | array of object | The undo steps, the next to undo first. |

Example:

```json
{}
```

### `edit.redo`

**Redo.** Redoes the last change undone.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | A project open for editing | The project or the session |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `label` | string | Its label: the title of the command that made it. |

Example:

```json
{}
```

### `edit.undo`

**Undo.** Undoes the last change to the project.

Every undoable command is one step; a retouch stroke is one stroke. Refused while a running job depends on what the step would change.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | A project open for editing | The project or the session |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `label` | string | Its label: the title of the command that made it. |

Example:

```json
{}
```

## export

### `export.estimate`

**Estimate export size.** About how many bytes exporting an output will write, and in how many files.

From the output's size and the settings (the project's, or the ones given): exact for uncompressed files, an estimate from typical compression otherwise, with the extras the settings ask for.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `output` | integer | **Required.** The output, as `output.list` lists it. |
| `settings` | object | Settings on top of the project's, for this estimate only. |
| `settings.aligned_frames` | boolean | Write every frame aligned onto the output's canvas alongside, as 16-bit TIFF files in a folder. Default: `false`. |
| `settings.alignment_csv` | boolean | Write master-to-input shift, scale, rotation, brightness gain and score as CSV. Default: `false`. |
| `settings.annotations` | boolean | Render visible annotation layers on the output copy. Default: `true`. |
| `settings.calibrated_depth` | boolean | Write full-precision calibrated depth as float TIFF values in micrometers. Default: `false`. |
| `settings.calibration_name` | string | Name of the selected objective or magnification, retained with its measured value. Default: `""`. |
| `settings.confidence_map` | boolean | Write the confidence map (a depth-map output's) alongside, as a 16-bit TIFF. Default: `false`. |
| `settings.contribution_maps` | boolean | Write a normalized 16-bit contribution TIFF per input of a depth-map output. Default: `false`. |
| `settings.depth` | `"u8"` \| `"u16"` \| `"f32"` | `u8`, `u16` or `f32`, as the format holds: JPEG 8-bit, PNG 8 or 16, TIFF 8, 16 or 32-bit float, DNG 16 or 32-bit float, OpenEXR 32-bit float. Default: `"u16"`. |
| `settings.depth_map` | boolean | Write the depth map (a depth-map output's) alongside, as a 16-bit TIFF. Default: `false`. |
| `settings.dng_compression` | `"uncompressed"` \| `"lossless-jpeg"` \| `"deflate"` | DNG's compression: `uncompressed`, `lossless-jpeg` (16-bit files) or `deflate` (32-bit float files). Either of the last two stands for the other at the depth it can't store. Default: `"lossless-jpeg"`. |
| `settings.dng_look` | `"rendered"` \| `"raw"` | DNG's look: `rendered` or `raw`. Default: `"rendered"`. |
| `settings.dng_preview` | boolean | A small sRGB picture of the image at the front of a DNG file, for file browsers to show. Default: `true`. |
| `settings.exr_compression` | `"none"` \| `"rle"` \| `"zips"` \| `"zip"` \| `"piz"` | OpenEXR's compression: `none`, `rle`, `zips`, `zip` or `piz`. Default: `"zip"`. |
| `settings.exr_half` | boolean | OpenEXR with half floats rather than full ones. Default: `false`. |
| `settings.extended_range` | boolean | In 32-bit float files, keep values beyond [0, 1] (the result from before tone compression). Default: `false`. |
| `settings.format` | `"tiff"` \| `"png"` \| `"jpeg"` \| `"dng"` \| `"open-exr"` | `tiff`, `png`, `jpeg`, `dng` or `open-exr`. Default: `"tiff"`. |
| `settings.jpeg_quality` | integer, from 1 to 100 | JPEG's quality, from 1 to 100. Default: `95`. |
| `settings.jpeg_subsampling` | `"s422"` \| `"s420"` \| `"s444"` | JPEG's chroma subsampling: `s444`, `s422` or `s420`. Default: `"s444"`. |
| `settings.metadata` | boolean | Copy a frame's metadata (EXIF, IPTC, XMP) into the file. Default: `true`. |
| `settings.metadata_frame` | integer, at least 1 | The frame whose metadata is copied: its position among the output's frames in the project's order (the filmstrip's), from 1 (the first); beyond the last, the last. Default: `1`. |
| `settings.pixels_per_micrometer` | number | Lateral calibration in master-image pixels per micrometer; copied from a profile or measured directly. |
| `settings.profile` | `"input"` \| `"srgb"` \| object \| `"untagged"` | The color profile: `input` (the frames' own), `srgb`, `{"wide": "pro-photo"}`, `{"wide": "rec2020"}` or `untagged`. Default: `"input"`. |
| `settings.profile.wide` | `"pro-photo"` \| `"rec2020"` | **Required.** A wide-gamut working space for [`OutputProfile::Wide`]. |
| `settings.template` | string | The file's name without its extension: {project}, {name}, {method}, {frames}, {first}, {last}, {date} and {id} are filled in. Default: `"{project} {name}"`. |
| `settings.tiff_compression` | `"none"` \| `"lzw"` \| `"deflate"` | TIFF's compression: `none`, `lzw` or `deflate`. Default: `"deflate"`. |
| `settings.z_origin_micrometers` | number | Z of the first captured frame, in micrometers. Default: `0.0`. |
| `settings.z_step_micrometers` | number | Signed capture step in micrometers, in the recorded capture order; unset when unknown. |

Result:

| Name | Type | Description |
|---|---|---|
| `approximate` | boolean | Whether `bytes` is a guess (compressed files), rather than known but for the headers, the profile and the metadata (a few KiB). |
| `bytes` | integer | About how many bytes, every file included. |
| `files` | integer | How many files. |
| `name` | string | The main file's name. |

Examples:

```json
{"output":3}
{"output":3,"settings":{"depth":"u8","format":"jpeg"}}
```

### `export.get`

**Export settings.** The project's output settings: format, depth, profile, metadata, file names and extras.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `settings` | object | What outputs are exported as: the preferences' `output` group, which presets carry. See `export.set`'s help. |

Example:

```json
{}
```

### `export.reproduce`

**Reproduce an export.** Recomputes an export from its sidecar and original input files.

Verifies the manifest version, software version, original SHA-256 input hashes and embedded masks. Replays each stacking method with its recorded development and alignment, then retouch and annotation layers and the original export settings. No original project or output cache is needed. Destination paths are selected here, never taken from the sidecar. CPU/GPU arithmetic can differ slightly when the device changes.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | No | Nothing | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `folder` | string | **Required.** New destination folder. Original paths in the sidecar are never written. |
| `overwrite` | boolean | Replace destination files if they exist. Default: `false`. |
| `sidecar` | string | **Required.** Versioned sidecar beside the original exported image. |

Result:

| Name | Type | Description |
|---|---|---|
| `result` | any |  |

Example:

```json
{"folder":"C:/Reproduced","sidecar":"C:/Exports/result.tif.json"}
```

### `export.reset`

**Reset export settings.** Puts every output setting back at its default.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `settings` | object | What outputs are exported as: the preferences' `output` group, which presets carry. See `export.set`'s help. |

Example:

```json
{}
```

### `export.run`

**Export output.** Writes an output into a folder with the output settings, and its extras alongside.

The file is named by the settings' template, with the format's extension; the depth and confidence maps (a depth-map output's) are written beside it as 16-bit TIFF files ending in `-depth` and `-confidence`, and the frames aligned onto the output's canvas as 16-bit TIFF files in a folder ending in ` aligned`. Settings given apply to this export only, on top of the project's. Existing files are refused unless `overwrite`. Each file is written whole and then renamed, so a failure leaves no partial file. Scientific extras include a float TIFF in micrometers, per-frame contribution maps and alignment CSV; visible annotation layers are rendered only onto the output copy. Every export includes a settings/version/input-hash JSON sidecar, published last; `export.reproduce` replays it. Runs as a job.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | No | An open project | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `folder` | string | **Required.** The folder the files go into, made if it doesn't exist. |
| `output` | integer | **Required.** The output, as `output.list` lists it. |
| `overwrite` | boolean | Replace files that exist. Default: `false`. |
| `settings` | object | Settings on top of the project's, for this export only. |
| `settings.aligned_frames` | boolean | Write every frame aligned onto the output's canvas alongside, as 16-bit TIFF files in a folder. Default: `false`. |
| `settings.alignment_csv` | boolean | Write master-to-input shift, scale, rotation, brightness gain and score as CSV. Default: `false`. |
| `settings.annotations` | boolean | Render visible annotation layers on the output copy. Default: `true`. |
| `settings.calibrated_depth` | boolean | Write full-precision calibrated depth as float TIFF values in micrometers. Default: `false`. |
| `settings.calibration_name` | string | Name of the selected objective or magnification, retained with its measured value. Default: `""`. |
| `settings.confidence_map` | boolean | Write the confidence map (a depth-map output's) alongside, as a 16-bit TIFF. Default: `false`. |
| `settings.contribution_maps` | boolean | Write a normalized 16-bit contribution TIFF per input of a depth-map output. Default: `false`. |
| `settings.depth` | `"u8"` \| `"u16"` \| `"f32"` | `u8`, `u16` or `f32`, as the format holds: JPEG 8-bit, PNG 8 or 16, TIFF 8, 16 or 32-bit float, DNG 16 or 32-bit float, OpenEXR 32-bit float. Default: `"u16"`. |
| `settings.depth_map` | boolean | Write the depth map (a depth-map output's) alongside, as a 16-bit TIFF. Default: `false`. |
| `settings.dng_compression` | `"uncompressed"` \| `"lossless-jpeg"` \| `"deflate"` | DNG's compression: `uncompressed`, `lossless-jpeg` (16-bit files) or `deflate` (32-bit float files). Either of the last two stands for the other at the depth it can't store. Default: `"lossless-jpeg"`. |
| `settings.dng_look` | `"rendered"` \| `"raw"` | DNG's look: `rendered` or `raw`. Default: `"rendered"`. |
| `settings.dng_preview` | boolean | A small sRGB picture of the image at the front of a DNG file, for file browsers to show. Default: `true`. |
| `settings.exr_compression` | `"none"` \| `"rle"` \| `"zips"` \| `"zip"` \| `"piz"` | OpenEXR's compression: `none`, `rle`, `zips`, `zip` or `piz`. Default: `"zip"`. |
| `settings.exr_half` | boolean | OpenEXR with half floats rather than full ones. Default: `false`. |
| `settings.extended_range` | boolean | In 32-bit float files, keep values beyond [0, 1] (the result from before tone compression). Default: `false`. |
| `settings.format` | `"tiff"` \| `"png"` \| `"jpeg"` \| `"dng"` \| `"open-exr"` | `tiff`, `png`, `jpeg`, `dng` or `open-exr`. Default: `"tiff"`. |
| `settings.jpeg_quality` | integer, from 1 to 100 | JPEG's quality, from 1 to 100. Default: `95`. |
| `settings.jpeg_subsampling` | `"s422"` \| `"s420"` \| `"s444"` | JPEG's chroma subsampling: `s444`, `s422` or `s420`. Default: `"s444"`. |
| `settings.metadata` | boolean | Copy a frame's metadata (EXIF, IPTC, XMP) into the file. Default: `true`. |
| `settings.metadata_frame` | integer, at least 1 | The frame whose metadata is copied: its position among the output's frames in the project's order (the filmstrip's), from 1 (the first); beyond the last, the last. Default: `1`. |
| `settings.pixels_per_micrometer` | number | Lateral calibration in master-image pixels per micrometer; copied from a profile or measured directly. |
| `settings.profile` | `"input"` \| `"srgb"` \| object \| `"untagged"` | The color profile: `input` (the frames' own), `srgb`, `{"wide": "pro-photo"}`, `{"wide": "rec2020"}` or `untagged`. Default: `"input"`. |
| `settings.profile.wide` | `"pro-photo"` \| `"rec2020"` | **Required.** A wide-gamut working space for [`OutputProfile::Wide`]. |
| `settings.template` | string | The file's name without its extension: {project}, {name}, {method}, {frames}, {first}, {last}, {date} and {id} are filled in. Default: `"{project} {name}"`. |
| `settings.tiff_compression` | `"none"` \| `"lzw"` \| `"deflate"` | TIFF's compression: `none`, `lzw` or `deflate`. Default: `"deflate"`. |
| `settings.z_origin_micrometers` | number | Z of the first captured frame, in micrometers. Default: `0.0`. |
| `settings.z_step_micrometers` | number | Signed capture step in micrometers, in the recorded capture order; unset when unknown. |

Result:

| Name | Type | Description |
|---|---|---|
| `files` | array of object |  |
| `warnings` | array of string | What the files leave out or couldn't hold. |

Examples:

```json
{"folder":"C:/Stacks/Exports","output":3}
{"folder":"C:/Stacks","output":3,"overwrite":true,"settings":{"aligned_frames":true}}
```

### `export.set`

**Set export settings.** Changes some of the project's output settings.

Only the keys given change, as when a preset is applied. `format`: `tiff`, `png`, `jpeg`, `dng` or `open-exr`; `depth`: `u8`, `u16` or `f32`, as the format holds; then each format's options (`tiff_compression`, `exr_compression` and `exr_half`, `jpeg_quality` and `jpeg_subsampling`, `dng_compression` and `dng_look`); `profile`: `input` (the frames' own), `srgb`, `{"wide": "pro-photo"}` or `{"wide": "rec2020"}`, or `untagged`; `metadata` and `metadata_frame` (the output's frame, from 1 in the filmstrip's order, whose EXIF, IPTC and XMP the file gets); `extended_range` (32-bit float files keep values beyond [0, 1]); `template`, the file name without its extension, with the tokens {project}, {name}, {method}, {frames}, {first}, {last}, {date} and {id}; and `depth_map`, `confidence_map` and `aligned_frames`, written alongside. Scientific options: `calibrated_depth` with signed `z_step_micrometers` and `z_origin_micrometers`, `contribution_maps`, `alignment_csv`, `annotations`, and `pixels_per_micrometer` with `calibration_name`. Null clears an optional step or lateral calibration. The keys are the preset files' `[output]` keys.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `aligned_frames` | boolean | Write every frame aligned onto the output's canvas alongside, as 16-bit TIFF files in a folder. Default: `false`. |
| `alignment_csv` | boolean | Write master-to-input shift, scale, rotation, brightness gain and score as CSV. Default: `false`. |
| `annotations` | boolean | Render visible annotation layers on the output copy. Default: `true`. |
| `calibrated_depth` | boolean | Write full-precision calibrated depth as float TIFF values in micrometers. Default: `false`. |
| `calibration_name` | string | Name of the selected objective or magnification, retained with its measured value. Default: `""`. |
| `confidence_map` | boolean | Write the confidence map (a depth-map output's) alongside, as a 16-bit TIFF. Default: `false`. |
| `contribution_maps` | boolean | Write a normalized 16-bit contribution TIFF per input of a depth-map output. Default: `false`. |
| `depth` | `"u8"` \| `"u16"` \| `"f32"` | `u8`, `u16` or `f32`, as the format holds: JPEG 8-bit, PNG 8 or 16, TIFF 8, 16 or 32-bit float, DNG 16 or 32-bit float, OpenEXR 32-bit float. Default: `"u16"`. |
| `depth_map` | boolean | Write the depth map (a depth-map output's) alongside, as a 16-bit TIFF. Default: `false`. |
| `dng_compression` | `"uncompressed"` \| `"lossless-jpeg"` \| `"deflate"` | DNG's compression: `uncompressed`, `lossless-jpeg` (16-bit files) or `deflate` (32-bit float files). Either of the last two stands for the other at the depth it can't store. Default: `"lossless-jpeg"`. |
| `dng_look` | `"rendered"` \| `"raw"` | DNG's look: `rendered` or `raw`. Default: `"rendered"`. |
| `dng_preview` | boolean | A small sRGB picture of the image at the front of a DNG file, for file browsers to show. Default: `true`. |
| `exr_compression` | `"none"` \| `"rle"` \| `"zips"` \| `"zip"` \| `"piz"` | OpenEXR's compression: `none`, `rle`, `zips`, `zip` or `piz`. Default: `"zip"`. |
| `exr_half` | boolean | OpenEXR with half floats rather than full ones. Default: `false`. |
| `extended_range` | boolean | In 32-bit float files, keep values beyond [0, 1] (the result from before tone compression). Default: `false`. |
| `format` | `"tiff"` \| `"png"` \| `"jpeg"` \| `"dng"` \| `"open-exr"` | `tiff`, `png`, `jpeg`, `dng` or `open-exr`. Default: `"tiff"`. |
| `jpeg_quality` | integer, from 1 to 100 | JPEG's quality, from 1 to 100. Default: `95`. |
| `jpeg_subsampling` | `"s422"` \| `"s420"` \| `"s444"` | JPEG's chroma subsampling: `s444`, `s422` or `s420`. Default: `"s444"`. |
| `metadata` | boolean | Copy a frame's metadata (EXIF, IPTC, XMP) into the file. Default: `true`. |
| `metadata_frame` | integer, at least 1 | The frame whose metadata is copied: its position among the output's frames in the project's order (the filmstrip's), from 1 (the first); beyond the last, the last. Default: `1`. |
| `pixels_per_micrometer` | number | Lateral calibration in master-image pixels per micrometer; copied from a profile or measured directly. |
| `profile` | `"input"` \| `"srgb"` \| object \| `"untagged"` | The color profile: `input` (the frames' own), `srgb`, `{"wide": "pro-photo"}`, `{"wide": "rec2020"}` or `untagged`. Default: `"input"`. |
| `profile.wide` | `"pro-photo"` \| `"rec2020"` | **Required.** A wide-gamut working space for [`OutputProfile::Wide`]. |
| `template` | string | The file's name without its extension: {project}, {name}, {method}, {frames}, {first}, {last}, {date} and {id} are filled in. Default: `"{project} {name}"`. |
| `tiff_compression` | `"none"` \| `"lzw"` \| `"deflate"` | TIFF's compression: `none`, `lzw` or `deflate`. Default: `"deflate"`. |
| `z_origin_micrometers` | number | Z of the first captured frame, in micrometers. Default: `0.0`. |
| `z_step_micrometers` | number | Signed capture step in micrometers, in the recorded capture order; unset when unknown. |

Result:

| Name | Type | Description |
|---|---|---|
| `settings` | object | What outputs are exported as: the preferences' `output` group, which presets carry. See `export.set`'s help. |

Examples:

```json
{"depth":"u8","format":"jpeg","jpeg_quality":92,"profile":"srgb"}
{"depth_map":true,"template":"{project}-{method}-{date}"}
```

## filmstrip

### `filmstrip.list`

**Filmstrip.** Every frame's previews, as shot and aligned, with its flags and the motion found in it.

For each frame, in the project's order: its preview as shot (developed, its defects inpainted) and aligned onto the output's canvas, small JPEGs of at most 512 pixels a side, as paths to read; its flags (such as `moved` and `out-of-focus`); and what alignment found moved in it against its neighbor: a score, the largest displacement and the regions, in frame pixels. A preview that is missing or out of date is left out and counted in `stale`: `filmstrip.update` makes them.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `frames` | array of object |  |
| `framing` | `"narrow"` \| `"wide"` \| `"common"` | The framing the aligned previews show. |
| `side` | integer | The previews' longest side, in pixels. |
| `stale` | integer | How many previews are missing or out of date: `filmstrip.update` makes them. |

Example:

```json
{}
```

### `filmstrip.update`

**Update filmstrip.** Makes the frames' previews that are missing or out of date.

Reads and develops the frames whose preview is missing or out of date (their file, the develop settings or the defect mask changed), and aligns every aligned frame's preview onto the canvas of the framing set, which needs no frame read. A frame whose file can't be read is listed in `unreadable` and keeps its previews. `align.run` makes them too, as it reads the frames. Runs beside other jobs as a preview; a newer call replaces an older one still running. Previews are cache files, not edits: no undo step.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `alignedPreviews` | integer |  |
| `previews` | integer | How many previews it made, as shot and aligned. |
| `unreadable` | array of object | The frames whose file couldn't be read (moved, deleted or damaged), with why: their previews stay as they were. |

Example:

```json
{}
```

## job

### `job.cancel`

**Cancel job.** Cancels a job.

A queued job ends at once; a running one stops at its next checkpoint, and its result is discarded, so the project stays as it was. A job that is writing a file can't be cancelled any more (`cancelled` is then false).

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | Nothing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `job` | integer | **Required.** The job's id, as dispatching its command returned it. |

Result:

| Name | Type | Description |
|---|---|---|
| `cancelled` | boolean | False when no such job is queued or running, or when it can't be stopped any more. |
| `job` | integer | A job's id, unique in its engine. |

Example:

```json
{"job":3}
```

### `job.list`

**List jobs.** Lists the jobs queued and running, then the last ones that ended, with their progress and results.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | Nothing | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `jobs` | array of object |  |

Example:

```json
{}
```

## output

### `output.list`

**List outputs.** Lists the outputs, oldest first: how each was made, its maps and its retouch layers.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `outputs` | array of object |  |

Example:

```json
{}
```

### `output.remove`

**Remove output.** Removes an output from the project.

Its cache files stay until the project is closed, so that undo can bring it back whole.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `output` | integer | An output's id in its project. |

Example:

```json
{"output":4}
```

### `output.rename`

**Rename output.** Renames an output.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `name` | string | **Required.** Its new name: at most 200 characters, no control characters. |
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `name` | string |  |
| `output` | integer | An output's id in its project. |

Example:

```json
{"name":"Pyramid, grit 0.5","output":4}
```

### `output.render`

**Render output.** Renders an output as it is written: with its visible retouch strokes and its adjustments.

For viewers: the result is a lossless image in the project's cache, whose `path` the result gives, kept as the output's `result` map and keyed by what it is made from, so that rendering it again reads it back until a stroke or an adjustment changes. An output with neither is its own image, with nothing rendered. Runs beside other jobs; a newer call replaces one still running.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `how` | string | How it came: `stacked` (nothing to render: the output's image), `cached` (rendered before) or `rendered`. |
| `output` | integer | An output's id in its project. |
| `path` | string | The image file to show: the result in the cache, or the output's own image. |

Example:

```json
{"output":4}
```

### `output.save`

**Save output.** Writes an output to an image file, in the format, depth and color profile given.

The metadata (EXIF, IPTC, XMP) comes from `metadataFrom`, or by default from the first of the output's frames, with the frame count in XMP. With `extended_range` in the spec and a 32-bit float depth (TIFF or OpenEXR), values outside [0, 1] are kept: for an output whose tone compression brought it into range (the pyramid method's), the result from before it is written ("retain extended range"). With `map`, one of the output's maps (`output.list` names them) is written instead, such as a depth-map output's `depth` and `confidence` maps: maps are data, so the spec's profile must be `untagged`, and a 16-bit depth keeps every level. The file is written whole to a temporary file and then renamed, so a failed write leaves nothing behind; an existing file is replaced only with `overwrite`. Runs as a job, which can be cancelled until writing starts.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | No | An open project | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `map` | string | One of the output's maps to write instead of the output, such as `depth` or `confidence`; the spec's profile must then be `untagged`. |
| `metadata` | boolean | Copy that frame's metadata (EXIF, IPTC, XMP) into the file; on by default. Off, the file has only what FloraStack writes itself (the frame count). Default: `true`. |
| `metadataFrom` | integer | The frame whose metadata the file gets; by default the first of the output's frames. |
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |
| `overwrite` | boolean | Replace the file if it exists. Default: `false`. |
| `path` | string | **Required.** The file to write. |
| `spec` | object | **Required.** The format, its options, the bit depth and the color profile. |
| `spec.depth` | `"u8"` \| `"u16"` \| `"f32"` | **Required.** The sample type of an output file. |
| `spec.extended_range` | boolean | Keep float values outside `[0, 1]` (the "retain extended range" option, for people who tone-map themselves). Integer files always clip. Default: `false`. |
| `spec.format` | object | **Required.** An output format with its options. |
| `spec.profile` | `"input"` \| `"srgb"` \| object \| `"untagged"` | The color profile of an output file. Default: `"input"`. |
| `spec.profile.wide` | `"pro-photo"` \| `"rec2020"` | **Required.** A wide-gamut working space for [`OutputProfile::Wide`]. |

Result:

| Name | Type | Description |
|---|---|---|
| `bytes` | integer | Its size. |
| `path` | string |  |
| `sidecar` | string | Reproducibility manifest written beside this image. |
| `warnings` | array of string | What the file couldn't hold or carry, such as metadata the format has no place for. |

Examples:

```json
{"output":4,"path":"C:/Stacks/bee.tif","spec":{"depth":"u16","format":{"compression":"deflate","format":"tiff"}}}
{"output":4,"overwrite":true,"path":"C:/Stacks/bee.jpg","spec":{"depth":"u8","format":{"format":"jpeg","quality":92,"subsampling":"s444"},"profile":"srgb"}}
{"output":5,"path":"C:/Stacks/bee.exr","spec":{"depth":"f32","extended_range":true,"format":{"compression":"zip","format":"open-exr","half":false}}}
{"map":"depth","output":6,"path":"C:/Stacks/bee-depth.tif","spec":{"depth":"u16","format":{"compression":"deflate","format":"tiff"},"profile":"untagged"}}
```

## parallax

### `parallax.animate`

**Export rocking animation.** Exports a looping back-and-forth parallax animation as GIF.

A sinusoidal camera excursion uses the stereo baseline. Frames are rendered and encoded one at a time, with infinite looping; unknown disocclusions appear black. GIF timing is in centiseconds. The selected output and its depth/confidence maps are read from one immutable snapshot.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | No | An open project | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `folder` | string | **Required.** Destination folder, created when needed. |
| `format` | `"glb"` \| `"obj"` \| `null` | Mesh format: glb or obj (mesh command only). |
| `fps` | integer, from 1 to 50 | Animation frames per second, 1 to 50 (animation command only; default 16). |
| `frames` | integer, from 4 to 120 | Animation frame count, 4 to 120 (animation command only; default 32). |
| `mode` | `"parallel"` \| `"cross-eyed"` \| `"anaglyph"` \| `null` | Stereo packing: parallel, cross-eyed or anaglyph (stereo command only). |
| `output` | integer | **Required.** The depth-map output to export. |
| `overwrite` | boolean | Replace existing destination files. Default: `false`. |
| `settings` | object | Confidence, smoothing, depth direction, relief, resolution and baseline. Default: `{"baseline":0.03999999910593033,"confidence":0.5,"maxDepthJump":0.25,"maxSide":1024,"meshSide":256,"relief":0.20000000298023224,"reverseDepth":false,"smoothing":2}`. |
| `settings.baseline` | number, from 0.0 to 0.25 | Stereo camera separation as a fraction of image width, from zero to 0.25. Default: `0.03999999910593033`. |
| `settings.confidence` | number, from 0.0 to 1.0 | Minimum confidence, from zero to one. Samples below it remain holes after smoothing. Default: `0.5`. |
| `settings.maxDepthJump` | number, from 0.0 to 1.0 | Maximum normalized depth difference across a triangle; larger discontinuities leave holes. Default: `0.25`. |
| `settings.maxSide` | integer, from 16 to 2048 | Longest texture or rendered image side, in pixels, from 16 to 2048. Default: `1024`. |
| `settings.meshSide` | integer, from 2 to 512 | Longest mesh grid side, from 2 to 512. Default: `256`. |
| `settings.relief` | number, from 0.001 to 1.0 | Mesh relief height as a fraction of the image's longest side, from 0.001 to 1. Default: `0.20000000298023224`. |
| `settings.reverseDepth` | boolean | Reverse focus order when the first input frame is farthest from the camera. Default: `false`. |
| `settings.smoothing` | integer, from 0 to 16 | Confidence-weighted smoothing radius in rendered pixels, from 0 to 16. Default: `2`. |

Result:

| Name | Type | Description |
|---|---|---|
| `files` | array of string |  |

Example:

```json
{"folder":"exports","fps":16,"frames":32,"output":3}
```

### `parallax.mesh`

**Export 3D mesh.** Exports a textured relative-depth relief as GLB or OBJ.

GLB embeds its PNG texture. OBJ writes model.obj, material.mtl and texture.png into the chosen folder. The surface uses confidence masking and weighted smoothing; depth discontinuities can leave holes. These are relative focus depths, not metric geometry. Existing files require overwrite.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | No | An open project | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `folder` | string | **Required.** Destination folder, created when needed. |
| `format` | `"glb"` \| `"obj"` \| `null` | Mesh format: glb or obj (mesh command only). |
| `fps` | integer, from 1 to 50 | Animation frames per second, 1 to 50 (animation command only; default 16). |
| `frames` | integer, from 4 to 120 | Animation frame count, 4 to 120 (animation command only; default 32). |
| `mode` | `"parallel"` \| `"cross-eyed"` \| `"anaglyph"` \| `null` | Stereo packing: parallel, cross-eyed or anaglyph (stereo command only). |
| `output` | integer | **Required.** The depth-map output to export. |
| `overwrite` | boolean | Replace existing destination files. Default: `false`. |
| `settings` | object | Confidence, smoothing, depth direction, relief, resolution and baseline. Default: `{"baseline":0.03999999910593033,"confidence":0.5,"maxDepthJump":0.25,"maxSide":1024,"meshSide":256,"relief":0.20000000298023224,"reverseDepth":false,"smoothing":2}`. |
| `settings.baseline` | number, from 0.0 to 0.25 | Stereo camera separation as a fraction of image width, from zero to 0.25. Default: `0.03999999910593033`. |
| `settings.confidence` | number, from 0.0 to 1.0 | Minimum confidence, from zero to one. Samples below it remain holes after smoothing. Default: `0.5`. |
| `settings.maxDepthJump` | number, from 0.0 to 1.0 | Maximum normalized depth difference across a triangle; larger discontinuities leave holes. Default: `0.25`. |
| `settings.maxSide` | integer, from 16 to 2048 | Longest texture or rendered image side, in pixels, from 16 to 2048. Default: `1024`. |
| `settings.meshSide` | integer, from 2 to 512 | Longest mesh grid side, from 2 to 512. Default: `256`. |
| `settings.relief` | number, from 0.001 to 1.0 | Mesh relief height as a fraction of the image's longest side, from 0.001 to 1. Default: `0.20000000298023224`. |
| `settings.reverseDepth` | boolean | Reverse focus order when the first input frame is farthest from the camera. Default: `false`. |
| `settings.smoothing` | integer, from 0 to 16 | Confidence-weighted smoothing radius in rendered pixels, from 0 to 16. Default: `2`. |

Result:

| Name | Type | Description |
|---|---|---|
| `files` | array of string |  |

Example:

```json
{"folder":"exports/model","format":"glb","output":3}
```

### `parallax.preview`

**View 3D relief.** Builds a textured relief for the orbit viewer.

Uses the selected output's immutable depth and confidence maps and its visible retouch. Frame zero is nearest unless reverseDepth; geometry is relative, not metric. Samples below the confidence threshold remain holes. The preview is stored in the project cache.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `output` | integer | **Required.** The depth-map output to view. |
| `settings` | object | Confidence, smoothing, depth direction, relief and resolution. Default: `{"baseline":0.03999999910593033,"confidence":0.5,"maxDepthJump":0.25,"maxSide":1024,"meshSide":256,"relief":0.20000000298023224,"reverseDepth":false,"smoothing":2}`. |
| `settings.baseline` | number, from 0.0 to 0.25 | Stereo camera separation as a fraction of image width, from zero to 0.25. Default: `0.03999999910593033`. |
| `settings.confidence` | number, from 0.0 to 1.0 | Minimum confidence, from zero to one. Samples below it remain holes after smoothing. Default: `0.5`. |
| `settings.maxDepthJump` | number, from 0.0 to 1.0 | Maximum normalized depth difference across a triangle; larger discontinuities leave holes. Default: `0.25`. |
| `settings.maxSide` | integer, from 16 to 2048 | Longest texture or rendered image side, in pixels, from 16 to 2048. Default: `1024`. |
| `settings.meshSide` | integer, from 2 to 512 | Longest mesh grid side, from 2 to 512. Default: `256`. |
| `settings.relief` | number, from 0.001 to 1.0 | Mesh relief height as a fraction of the image's longest side, from 0.001 to 1. Default: `0.20000000298023224`. |
| `settings.reverseDepth` | boolean | Reverse focus order when the first input frame is farthest from the camera. Default: `false`. |
| `settings.smoothing` | integer, from 0 to 16 | Confidence-weighted smoothing radius in rendered pixels, from 0 to 16. Default: `2`. |

Result:

| Name | Type | Description |
|---|---|---|
| `output` | integer | An output's id in its project. |
| `path` | string |  |

Example:

```json
{"output":3}
```

### `parallax.stereo`

**Export synthetic stereo.** Exports parallel or cross-eyed stereo pairs, or a red-cyan anaglyph PNG.

Parallel camera reprojection converges at relative depth 0.5. Baseline is a fraction of image width; smaller depths move right in the left eye and left in the right eye. Cross-eyed output swaps the eyes. Unknown disocclusions and masked samples are transparent; no hidden surfaces are invented. Use reverseDepth for far-to-near captures.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | No | An open project | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `folder` | string | **Required.** Destination folder, created when needed. |
| `format` | `"glb"` \| `"obj"` \| `null` | Mesh format: glb or obj (mesh command only). |
| `fps` | integer, from 1 to 50 | Animation frames per second, 1 to 50 (animation command only; default 16). |
| `frames` | integer, from 4 to 120 | Animation frame count, 4 to 120 (animation command only; default 32). |
| `mode` | `"parallel"` \| `"cross-eyed"` \| `"anaglyph"` \| `null` | Stereo packing: parallel, cross-eyed or anaglyph (stereo command only). |
| `output` | integer | **Required.** The depth-map output to export. |
| `overwrite` | boolean | Replace existing destination files. Default: `false`. |
| `settings` | object | Confidence, smoothing, depth direction, relief, resolution and baseline. Default: `{"baseline":0.03999999910593033,"confidence":0.5,"maxDepthJump":0.25,"maxSide":1024,"meshSide":256,"relief":0.20000000298023224,"reverseDepth":false,"smoothing":2}`. |
| `settings.baseline` | number, from 0.0 to 0.25 | Stereo camera separation as a fraction of image width, from zero to 0.25. Default: `0.03999999910593033`. |
| `settings.confidence` | number, from 0.0 to 1.0 | Minimum confidence, from zero to one. Samples below it remain holes after smoothing. Default: `0.5`. |
| `settings.maxDepthJump` | number, from 0.0 to 1.0 | Maximum normalized depth difference across a triangle; larger discontinuities leave holes. Default: `0.25`. |
| `settings.maxSide` | integer, from 16 to 2048 | Longest texture or rendered image side, in pixels, from 16 to 2048. Default: `1024`. |
| `settings.meshSide` | integer, from 2 to 512 | Longest mesh grid side, from 2 to 512. Default: `256`. |
| `settings.relief` | number, from 0.001 to 1.0 | Mesh relief height as a fraction of the image's longest side, from 0.001 to 1. Default: `0.20000000298023224`. |
| `settings.reverseDepth` | boolean | Reverse focus order when the first input frame is farthest from the camera. Default: `false`. |
| `settings.smoothing` | integer, from 0 to 16 | Confidence-weighted smoothing radius in rendered pixels, from 0 to 16. Default: `2`. |

Result:

| Name | Type | Description |
|---|---|---|
| `files` | array of string |  |

Example:

```json
{"folder":"exports","mode":"parallel","output":3,"settings":{"baseline":0.04}}
```

## preset

### `preset.apply`

**Apply preset.** Applies a preset, a file or a built-in one, on top of the project's preferences.

Tables merge key by key and other values replace what was there, so presets layer: one for develop settings, another for output. `groups` applies only some of the preset's groups (a partial preset). Give `path` (a TOML file) or `builtin` (an id from `preset.list`). Every group is checked before anything changes.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `builtin` | string | A built-in preset's id, as `preset.list` lists them, in place of `path`. |
| `groups` | array of `"develop"` \| `"align"` \| `"stack"` \| `"output"` | The groups to apply; all of the preset's when left out. |
| `path` | string | The preset file (TOML). |

Result:

| Name | Type | Description |
|---|---|---|
| `groups` | array of `"develop"` \| `"align"` \| `"stack"` \| `"output"` | The groups it set. |
| `name` | string | Its name, if it has one. |

Examples:

```json
{"path":"C:/Presets/rail.toml"}
{"groups":["develop"],"path":"C:/Presets/insect.toml"}
{"builtin":"insect-macro"}
```

### `preset.list`

**Built-in presets.** The presets shipped with FloraStack: each one's id, name, description and groups.

Apply one with `preset.apply` and its `builtin` id; in `batch.run` and `watch.run`, name it as the preset `builtin:<id>`.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | Nothing | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `presets` | array of object |  |

Example:

```json
{}
```

### `preset.read`

**Read preset.** Reads a preset file: its name, description and preferences.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | Nothing | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `path` | string | **Required.** The preset file (TOML). |

Result:

| Name | Type | Description |
|---|---|---|
| `description` | string |  |
| `name` | string |  |
| `preferences` | object | Its preferences, by group, as the file has them. |

Example:

```json
{"path":"C:/Presets/rail.toml"}
```

### `preset.save`

**Save preset.** Saves the project's preferences, or some groups of them, as a preset file.

The file is TOML, written atomically. An existing file is replaced only with `overwrite`. By default it holds only the settings changed from their defaults, so that it layers over what a project has; with `complete`, every setting of its groups, the defaults included, so that applying it gives these settings whatever the project had.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | An open project | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `complete` | boolean | Save every setting of the groups, the defaults included, rather than only those changed from them. Default: `false`. |
| `description` | string | What it is for. |
| `groups` | array of `"develop"` \| `"align"` \| `"stack"` \| `"output"` | The groups to save; all the project sets when left out. |
| `name` | string | The preset's name, for the presets list. |
| `overwrite` | boolean | Replace the file if it exists. Default: `false`. |
| `path` | string | **Required.** The preset file to write (TOML). |

Result:

| Name | Type | Description |
|---|---|---|
| `groups` | array of `"develop"` \| `"align"` \| `"stack"` \| `"output"` | The groups it holds. |
| `path` | string |  |

Examples:

```json
{"groups":["develop"],"name":"Bee","path":"C:/Presets/bee.toml"}
{"complete":true,"name":"Rail","path":"C:/Presets/rail.toml"}
```

## project

### `project.addFrames`

**Add frames.** Adds image and camera RAW files to the end of the frame list.

`paths` lists files and folders; a folder adds the frame files directly in it (by extension, sorted by name). Every file is read once, to check that FloraStack reads it, to hash it and to keep its metadata summary. Files that can't be read, or that are in the project already (the same path or the same contents), are reported in `skipped`. Runs as a job; the frames are added in one undo step when it ends, or not at all when it is cancelled.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `paths` | array of string | **Required.** Files and folders (at most 100000): a folder adds the frame files directly in it, sorted by name. |

Result:

| Name | Type | Description |
|---|---|---|
| `added` | array of integer | The new frames' ids, in order. |
| `skipped` | array of object | The files not added, and why. |

Examples:

```json
{"paths":["C:/Stacks/bee/IMG_0001.CR3","C:/Stacks/bee/IMG_0002.CR3"]}
{"paths":["C:/Stacks/bee"]}
```

### `project.checkFrames`

**Check frames.** Lists the frames whose files are missing or changed since they were added.

Each frame's file is compared with what was added, by its size and modification time, and by its contents when the time changed. A missing frame can still be stacked from the cache where it was aligned; a changed one must be added and aligned again. The desktop app asks this when a project opens.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `checked` | integer | How many frames were checked. |
| `frames` | array of object | The ones missing, changed or unreadable; empty when every file is the one added. |

Example:

```json
{}
```

### `project.close`

**Close project.** Closes the open project.

Refused while the project has unsaved changes, unless `discardChanges`. Its running jobs are cancelled and waited for; then the cache files that nothing refers to are removed, the undo history is dropped and the project folder's lock is released.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | Nothing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `discardChanges` | boolean | Close it even if it has unsaved changes, which are lost. Default: `false`. |

Result:

| Name | Type | Description |
|---|---|---|
| `cleaned` | integer | How many cache files that nothing referred to were removed. |
| `path` | string | The folder of the project closed; none when no project was open. |

Examples:

```json
{}
{"discardChanges":true}
```

### `project.findOrphans`

**Find old temporary projects.** Lists the temporary projects old enough to be moved to the trash.

Orphans are unnamed projects in the temporary-projects folder that no FloraStack has open and that haven't been saved or opened for `days` days (30 by default).

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | Nothing | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `days` | integer, from 0 to 36500 | How many days unused a temporary project must be (30 by default), at most 36500. |

Result:

| Name | Type | Description |
|---|---|---|
| `folder` | string | The temporary-projects folder looked in. |
| `orphans` | array of object |  |

Examples:

```json
{}
{"days":7}
```

### `project.findStacks`

**Find stacks.** Finds where each stack starts in a batch of files holding several, shot one after another.

`paths` lists files and folders, as `project.addFrames` takes them: a folder gives the frame files directly in it, sorted by name. Every file is read once, in that order, and compared with the one before it: a new stack starts where their capture times are further apart than `split.gap` seconds (by default 30 s, or 8 times the batch's median gap when that is longer), where the next frame shows another scene (it doesn't match the one before, scoring below `split.minScore`, and doesn't look like it once blurred, correlating below `split.minSimilarity`), where the framing jumps (the scale by more than `split.maxScaleStep` percent, or the view by more than `split.maxShift` percent of the diagonal) or where the frame size changes. Frames of one stack differ only in focus and a little in scale, so out-of-focus frames at a stack's ends stay with it. Needs no project and changes none: the result lists each stack's files, and every boundary with its reasons. Files that can't be read are listed in `skipped` and left out. Runs as a job.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | No | Nothing | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `paths` | array of string | **Required.** Files and folders (at most 100000), in order: a folder gives the frame files directly in it, sorted by name. |
| `split` | object | When the batch is cut: the gap in capture time, in seconds (`gap`, chosen from the batch when left out), and the limits on neighbors (`minScore`, `minSimilarity`, `maxScaleStep`, `maxShift`); each left out is the default. Default: `{"gap":null,"maxScaleStep":5.0,"maxShift":10.0,"minScore":0.3,"minSimilarity":0.9}`. |
| `split.gap` | number, at least 0.0 | A gap in capture time longer than this, in seconds, cuts the batch; `None` chooses it from the batch: 30 s or 8 times its median gap, whichever is longer. |
| `split.maxScaleStep` | number, from 0.0 to 50.0 | A change of scale between matching neighbors beyond this, in percent, is a new framing. Default: `5.0`. |
| `split.maxShift` | number, from 0.0 to 50.0 | A shift between matching neighbors beyond this, in percent of the frame's diagonal, is a new framing. Default: `10.0`. |
| `split.minScore` | number, from 0.0 to 1.0 | Neighbors that match with a lower score than this, from 0 to 1, and aren't similar, are of different scenes. Default: `0.3`. |
| `split.minSimilarity` | number, from -1.0 to 1.0 | Neighbors whose low-passed luminance correlates less than this, from -1 to 1, and that don't match, are of different scenes. Default: `0.9`. |

Result:

| Name | Type | Description |
|---|---|---|
| `boundaries` | array of object | Where each stack after the first starts, and why. |
| `gap` | number | The gap in capture time that cuts the batch, in seconds: the one given, or the one chosen from the batch; absent when no file has a capture time. |
| `skipped` | array of object | The files left out, and why. |
| `stacks` | array of object | The stacks, in the order of their files. |

Examples:

```json
{"paths":["C:/Stacks/2024-05-01"]}
{"paths":["C:/Stacks/2024-05-01"],"split":{"gap":60,"maxScaleStep":3}}
```

### `project.frames`

**List frames.** Lists the frames in order, with their files, hashes and metadata summaries.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `frames` | array of object |  |

Example:

```json
{}
```

### `project.info`

**Project information.** Describes the open project: its name, folder, frames, outputs and undo state.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `created` | string | RFC 3339. |
| `dirty` | boolean | Whether it has unsaved changes. |
| `enabledFrames` | integer |  |
| `frames` | integer |  |
| `migratedFrom` | integer |  |
| `modified` | string | When it was last saved, RFC 3339. |
| `name` | string | None for a temporary project. |
| `outputs` | integer |  |
| `path` | string |  |
| `readOnly` | boolean |  |
| `redo` | string | What a redo would redo now. |
| `software` | string | The software that last saved it. |
| `temporary` | boolean |  |
| `undo` | string | What an undo would undo now. |

Example:

```json
{}
```

### `project.moveFrames`

**Move frames.** Moves frames to another place in the frame list.

The frames, in the order given, go before the frame now at position `to` of the list without them (from 0; past the end means at the end).

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `frames` | array of integer | **Required.** The frames to move, in the order they go in. |
| `to` | integer | **Required.** Where they go: before the frame at this position of the list without them, from 0. |

Result:

| Name | Type | Description |
|---|---|---|
| `order` | array of integer |  |

Example:

```json
{"frames":[7],"to":0}
```

### `project.new`

**New project.** Creates a project folder and opens the new project, closing the project that is open.

With a `name`, the project folder is `<name>.florastack` in `folder`; without one, the project is temporary (unnamed) and goes in the temporary-projects folder, where orphan cleanup offers to move it to the trash once it is old and still unnamed. The project that is open is closed first, which is refused while it has unsaved changes, unless `discardChanges`.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | Nothing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `discardChanges` | boolean | Close the open project even if it has unsaved changes, which are lost. Default: `false`. |
| `folder` | string | The folder to make the project folder in. Needed with `name`, and only with it. |
| `name` | string | The project's name; its folder is `<name>.florastack` in `folder`. Leave it out for a temporary project. |

Result:

| Name | Type | Description |
|---|---|---|
| `cleaned` | integer | How many cache files that nothing referred to were removed. |
| `migratedFrom` | integer | The project format version the file was upgraded from, if it was older. |
| `name` | string |  |
| `path` | string | The project folder. |
| `readOnly` | boolean |  |
| `temporary` | boolean |  |

Examples:

```json
{"folder":"C:/Stacks","name":"Bee head"}
{}
```

### `project.open`

**Open project.** Opens a project folder, closing the project that is open.

`path` is the `.florastack` folder or its `project.json`. A project from an older version is upgraded in memory and written in the current format at the next save. With `readOnly`, the project isn't locked against other editors and can't be changed. Cache files that nothing in the project refers to any more are removed.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | Nothing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `discardChanges` | boolean | Close the open project even if it has unsaved changes, which are lost. Default: `false`. |
| `path` | string | **Required.** The `.florastack` project folder, or its `project.json`. |
| `readOnly` | boolean | Open it for reading only: not locked against other editors, and it can't be changed or saved. Default: `false`. |

Result:

| Name | Type | Description |
|---|---|---|
| `cleaned` | integer | How many cache files that nothing referred to were removed. |
| `migratedFrom` | integer | The project format version the file was upgraded from, if it was older. |
| `name` | string |  |
| `path` | string | The project folder. |
| `readOnly` | boolean |  |
| `temporary` | boolean |  |

Examples:

```json
{"path":"C:/Stacks/Bee head.florastack"}
{"path":"C:/Stacks/Bee head.florastack","readOnly":true}
```

### `project.removeFrames`

**Remove frames.** Removes frames from the frame list.

The files stay where they are. Outputs made from the frames keep their pixels and their record of what they were made from.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `frames` | array of integer | **Required.** The frames' ids, as `project.frames` lists them. |

Result:

| Name | Type | Description |
|---|---|---|
| `changed` | integer |  |

Example:

```json
{"frames":[3,4]}
```

### `project.rename`

**Rename project.** Gives the open project a name, which makes a temporary project permanent.

The project folder keeps its name; only the name in project.json changes.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `name` | string | **Required.** The new name: at most 200 characters, no control characters. |

Result:

| Name | Type | Description |
|---|---|---|
| `name` | string |  |

Example:

```json
{"name":"Bee head, 2026"}
```

### `project.save`

**Save project.** Saves the open project to its project.json.

Saving is atomic: a failed save (a full disk) leaves the last good save as it was. The undo history stays.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | A project open for editing | The project or the session |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `path` | string |  |
| `savedAt` | string | When, in RFC 3339. |

Example:

```json
{}
```

### `project.saveAs`

**Save project as.** Saves the open project in another folder, where it stays open.

The project goes to `<folder>/<name>.florastack`, named after the project: name a temporary project first with `project.rename`. It is saved first. A temporary project is moved out of the temporary-projects folder (copied instead where moving isn't possible, as to another drive); a named one is copied, and the original stays as it was saved. The undo history stays. Refused while a job is running, onto a folder that exists, and for a project whose frames are stored relative to its folder when the new folder is elsewhere.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | A project open for editing | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `folder` | string | **Required.** The folder to save the project in, made if needed: the project goes to `<folder>/<name>.florastack`. |

Result:

| Name | Type | Description |
|---|---|---|
| `moved` | boolean | Whether it was moved (a temporary project) rather than copied. |
| `path` | string | The project folder it is open in. |

Example:

```json
{"folder":"C:/Users/me/Pictures/Stacks"}
```

### `project.setFramesEnabled`

**Enable or disable frames.** Chooses whether frames go into stacks.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `enabled` | boolean | **Required.** Whether they go into stacks. |
| `frames` | array of integer | **Required.** The frames. |

Result:

| Name | Type | Description |
|---|---|---|
| `changed` | integer |  |

Example:

```json
{"enabled":false,"frames":[1,2]}
```

### `project.trashOrphans`

**Move old temporary projects to the trash.** Moves orphaned temporary projects to the trash.

Each path is checked again before it is moved: it must still be an orphan in the temporary-projects folder. Nothing is deleted; the projects can be restored from the trash.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | Nothing | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `days` | integer, from 0 to 36500 | How many days unused each must still be (30 by default). |
| `paths` | array of string | **Required.** The project folders, as `project.findOrphans` lists them. |

Result:

| Name | Type | Description |
|---|---|---|
| `failed` | array of object |  |
| `trashed` | array of string |  |

Example:

```json
{"paths":["C:/Users/me/AppData/Local/FloraStack/projects/untitled-20260901120000-0.florastack"]}
```

## retouch

### `retouch.addLayer`

**Add retouch layer.** Adds an empty retouch layer on top of an output's.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `name` | string | Its name; "Layer n" when left out. |
| `output` | integer | **Required.** The output, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `layer` | integer | A retouch layer's id in its project. |
| `output` | integer | An output's id in its project. |

Example:

```json
{"name":"Antennae","output":3}
```

### `retouch.cancelPaint`

**Cancel live stroke.** Takes back the stroke being drawn.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | A project open for editing | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `cancelled` | boolean |  |

Example:

```json
{}
```

### `retouch.close`

**Close retouching.** Closes the output open for retouching, caching its result.

The result is cached as the output's `retouched` map, so that opening it again needs no recomputation.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `closed` | boolean | Whether an output was open for retouching. |

Example:

```json
{}
```

### `retouch.mapFocus`

**Map sharpest frames.** Maps each point of the output open for retouching to its sharpest frame.

Measures the output's frames' focus at a quarter of their size, as the depth-map method's first pass does, and keeps which frame is sharpest at each point as the output's `sharpest` map, for `retouch.sharpest`. A depth-map output needs none: its depth map tells. Runs beside other jobs.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `frames` | integer | The frames it maps to. |
| `height` | integer |  |
| `width` | integer | The map's size, a quarter of the output's on each side. |

Example:

```json
{}
```

### `retouch.open`

**Open for retouching.** Opens an output for retouching, with its visible strokes painted.

Reads the output and paints its visible retouch strokes over it, or reads the result cached when the project was last saved, and the map of each point's sharpest frame when the output has one (a depth map's own, or `retouch.mapFocus`'s). Then `retouch.setSource` picks what strokes paint from, `retouch.stroke` paints one, and `edit.undo` takes it back. One output is open for retouching at a time; opening another caches and closes the one open.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `output` | integer | **Required.** The output's id, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `height` | integer |  |
| `output` | integer | An output's id in its project. |
| `rendered` | string | How its strokes got onto it: `cached` (read back), `painted` (painted again) or `none` (it has none). |
| `sharpest` | boolean | Whether it can tell each point's sharpest frame. |
| `skipped` | integer |  |
| `strokes` | integer | Its visible strokes, and how many of them couldn't be painted (their source is gone). |
| `width` | integer |  |

Example:

```json
{"output":3}
```

### `retouch.paint`

**Paint live.** Paints part of a stroke as it is drawn, without committing it.

`begin` starts a stroke with the settings and samples given; later calls add their samples to it. The result shows the stroke so far exactly as `retouch.stroke` will paint it, which commits it; `retouch.cancelPaint` takes it back. Returns the region that changed, in output pixels.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | No | A project open for editing | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `begin` | boolean | Starts a new stroke, in place of one being drawn. Default: `false`. |
| `stroke` | object | **Required.** The stroke: with `begin`, its settings and first samples; later, the samples to add (its settings must stay the same). |
| `stroke.brush` | `"details"` \| `"pixels"` \| `"darken"` \| `"lighten"` \| `"eraser"` | **Required.** A brush. See the crate documentation. |
| `stroke.hardness` | number, from 0.0 to 1.0 | **Required.** How much of the brush, from its center out, paints fully before its edge fades, from 0 (a soft edge all the way) to 1 (a hard edge). |
| `stroke.opacity` | number, from 0.0 to 1.0 | **Required.** How strongly it paints, from 0 to 1. |
| `stroke.samples` | array of array of number | **Required.** The pointer's path: `[x, y, pressure]` samples, in pixels of the output (from its top left corner) and pressure from 0 to 1 (1 without a pen), at most 100000. Pressure scales the brush's size. |
| `stroke.size` | number, from 1.0 to 4000.0 | **Required.** The brush's diameter, in pixels of the output, from 1 to 4000. |
| `stroke.source` | object | Where it paints from: `{"frame": id}` or `{"output": id}`; none for the eraser. |
| `stroke.version` | integer | How the record is rendered: 1, this crate's [`STROKE_VERSION`]. Default: `1`. |

Result:

| Name | Type | Description |
|---|---|---|
| `region` | array of integer | The region painted, `[x, y, width, height]` in output pixels; none when nothing changed. |

Example:

```json
{"begin":true,"stroke":{"brush":"details","hardness":0.5,"opacity":1,"samples":[[100,120,1]],"size":40,"source":{"frame":7}}}
```

### `retouch.removeLayer`

**Remove retouch layer.** Removes a retouch layer and its strokes.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `layer` | integer | **Required.** The layer, as `output.list` lists the output's layers. |
| `output` | integer | **Required.** The output, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `layer` | integer | A retouch layer's id in its project. |
| `output` | integer | An output's id in its project. |

Example:

```json
{"layer":5,"output":3}
```

### `retouch.setLayerVisible`

**Show retouch layer.** Shows or hides a retouch layer's strokes.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `layer` | integer | **Required.** The layer, as `output.list` lists the output's layers. |
| `output` | integer | **Required.** The output, as `output.list` lists it. |
| `visible` | boolean | **Required.** Whether its strokes show. |

Result:

| Name | Type | Description |
|---|---|---|
| `layer` | integer | A retouch layer's id in its project. |
| `output` | integer | An output's id in its project. |

Example:

```json
{"layer":5,"output":3,"visible":false}
```

### `retouch.setSource`

**Set retouch source.** Picks the frame or output that strokes paint from.

A frame is read and developed as stacking reads it, its brightness gain applied, and warped onto the output's canvas, then kept in the cache, so that it is read rather than decoded the next time; another output must have the output's size. Give `frame` or `output`. Runs beside other jobs; a newer call replaces one still running.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `frame` | integer | A frame, as `project.frames` lists it. |
| `output` | integer | Another output of the same size, as `output.list` lists it. |

Result:

| Name | Type | Description |
|---|---|---|
| `loaded` | string | How it was loaded: `memory` (kept since it was last used), `cache` or `decoded`. |
| `source` | object | Where a brush paints from: a frame of the stack, warped onto the output's canvas, or another output of the same size (a slab, or a stack by another method). |

Examples:

```json
{"frame":7}
{"output":2}
```

### `retouch.sharpest`

**Sharpest frame.** The frame where a point of the output is sharpest.

From the depth map of a depth-map output, or the map `retouch.mapFocus` made. `x` and `y` are in pixels of the output.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `x` | number | **Required.** In pixels of the output, from its left edge. |
| `y` | number | **Required.** In pixels of the output, from its top edge. |

Result:

| Name | Type | Description |
|---|---|---|
| `frame` | integer | The frame; none where no frame covers the point, or beyond the output. |
| `index` | integer | Its position among the output's frames, in the order they were stacked. |

Example:

```json
{"x":1200.5,"y":840}
```

### `retouch.stroke`

**Retouch stroke.** Paints a stroke onto the output open for retouching, as one undo step.

The stroke goes on top of `layer`'s strokes, or of the top layer's, in a new layer when the output has none. Its samples are `[x, y, pressure]` in pixels of the output, rounded to a hundredth of a pixel. Brushes: `details` (the source's detail, its brightness and color matched to the output's, with a hard edge where there is fine detail), `pixels` (the source as it is), `darken` and `lighten` (only where the source is darker or lighter), and `eraser` (back to the output as stacked; no source). When it is the stroke `retouch.paint` drew, it is committed as painted.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `layer` | integer | The layer it goes into, which must be visible; by default the top layer, or a new one when there is none. |
| `stroke` | object | **Required.** The stroke: its brush, source, size, hardness, opacity and samples. |
| `stroke.brush` | `"details"` \| `"pixels"` \| `"darken"` \| `"lighten"` \| `"eraser"` | **Required.** A brush. See the crate documentation. |
| `stroke.hardness` | number, from 0.0 to 1.0 | **Required.** How much of the brush, from its center out, paints fully before its edge fades, from 0 (a soft edge all the way) to 1 (a hard edge). |
| `stroke.opacity` | number, from 0.0 to 1.0 | **Required.** How strongly it paints, from 0 to 1. |
| `stroke.samples` | array of array of number | **Required.** The pointer's path: `[x, y, pressure]` samples, in pixels of the output (from its top left corner) and pressure from 0 to 1 (1 without a pen), at most 100000. Pressure scales the brush's size. |
| `stroke.size` | number, from 1.0 to 4000.0 | **Required.** The brush's diameter, in pixels of the output, from 1 to 4000. |
| `stroke.source` | object | Where it paints from: `{"frame": id}` or `{"output": id}`; none for the eraser. |
| `stroke.version` | integer | How the record is rendered: 1, this crate's [`STROKE_VERSION`]. Default: `1`. |

Result:

| Name | Type | Description |
|---|---|---|
| `layer` | integer | A retouch layer's id in its project. |
| `output` | integer | An output's id in its project. |
| `region` | array of integer | The region painted, in output pixels. |
| `strokes` | integer | The layer's strokes now. |

Examples:

```json
{"stroke":{"brush":"details","hardness":0.5,"opacity":1,"samples":[[100,120,1],[140,130,0.8]],"size":40,"source":{"frame":7}}}
{"layer":5,"stroke":{"brush":"eraser","hardness":0,"opacity":0.5,"samples":[[300,200,1]],"size":80}}
```

## stack

### `stack.average`

**Stack by weighted average.** Stacks the aligned frames by weighted average into a new output.

Blends every frame into every pixel, each weighted by its sharpness there: the Laplacian's energy, averaged over `radius` pixels and raised to `power`, the weights smoothed over `smoothing` pixels. Fast, and a good fallback for simple subjects; where near and far detail overlap, the pyramid method is sharper. Uses the project's settings (`stack.get`), or the ones given for this stack only, and the stored alignment, framed as the alignment settings say (`align.info`): align the stack first with `align.run`, and again after adding or enabling frames. Frames whose alignment was rejected are left out. Pixels that no frame covers, which only wide framing has, are transparent. Runs as a job; the output is added in one undo step when it ends, or not at all when it is cancelled.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `name` | string | The output's name; "Weighted average" when left out. |
| `power` | number, from 0.5 to 4.0 | How strongly the sharpest frame wins, from 0.5 to 4; the project's when left out. |
| `radius` | number, from 0.0 to 64.0 | The radius sharpness is measured over, in pixels, from 0 to 64; the project's (`stack.get`) when left out. |
| `smoothing` | number, from 0.0 to 64.0 | How smoothly the blend moves from one frame to the next, in pixels, from 0 to 64; the project's when left out. |

Result:

| Name | Type | Description |
|---|---|---|
| `depth` | object | The depth-map method's threshold and the pixels it filled. |
| `frames` | array of integer | The frames stacked, in the order they were added. |
| `framing` | `"narrow"` \| `"wide"` \| `"common"` | The output's field of view. See the module documentation. |
| `height` | integer |  |
| `levels` | integer | The pyramid's or the wavelet method's number of levels. |
| `output` | integer | The new output. |
| `range` | object | How the pyramid's or the wavelet method's result was brought into range. |
| `settings` | object | The settings it was stacked with, and the framing. |
| `skipped` | array of integer | The enabled frames left out, because their alignment was rejected. |
| `uncovered` | integer | How many pixels no frame covers: transparent in the output (only wide framing has them). |
| `width` | integer | Its size, in pixels. |

Examples:

```json
{}
{"name":"Average, smooth","radius":2.0,"smoothing":2.0}
```

### `stack.depthMap`

**Stack by depth map.** Stacks the aligned frames by the depth-map method into a new output.

Finds, for every pixel, the frame in which it is sharpest, smooths that depth map with an edge-aware filter and takes each pixel from the frames at its depth, blending the two on either side: every pixel's color and contrast are its frames', for smooth tones and accurate color. Where the confidence (how clearly the sharpest frame stands out) is below the threshold, as in an out-of-focus background or on a smooth surface, the depth is filled from the confident pixels around instead. `radius` is the estimation radius the focus is measured over, `smoothing` the reach of the depth map's smoothing, and `threshold` how the contrast threshold is chosen: `auto` (from the statistics: the pixels whose depth follows noise, or else the 10th percentile), `level` (the absolute `level`) or `percentile` (`percentile` percent of the pixels). Reads every frame twice. The output keeps the depth map (the frame index scaled to 16 bits, 0 for the first frame) and the confidence map (0.5 at the threshold) as its `depth` and `confidence` maps, which `output.save` writes with `map`. Where near and far detail overlap (hair, bristles), the pyramid method is sharper. Uses the project's settings (`stack.get`), or the ones given for this stack only, and the stored alignment, as `stack.average` does. Runs as a job; the output is added in one undo step when it ends, or not at all when it is cancelled.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `level` | number, at least 0.0 | The threshold's absolute level, from 0, for `"threshold": "level"`; the project's when left out. |
| `name` | string | The output's name; "Depth map" when left out. |
| `percentile` | number, from 0.0 to 100.0 | The share of the pixels to fill at least, in percent from 0 to 100, for `"threshold": "percentile"`; the project's when left out. |
| `radius` | number, from 0.0 to 64.0 | The estimation radius, in pixels, from 0 to 64; the project's (`stack.get`) when left out. |
| `smoothing` | number, from 0.0 to 256.0 | The depth map's smoothing radius, in pixels, from 0 to 256; the project's when left out. |
| `threshold` | `"auto"` \| `"level"` \| `"percentile"` | How the contrast threshold is chosen: auto, level or percentile; the project's when left out. |

Result:

| Name | Type | Description |
|---|---|---|
| `depth` | object | The depth-map method's threshold and the pixels it filled. |
| `frames` | array of integer | The frames stacked, in the order they were added. |
| `framing` | `"narrow"` \| `"wide"` \| `"common"` | The output's field of view. See the module documentation. |
| `height` | integer |  |
| `levels` | integer | The pyramid's or the wavelet method's number of levels. |
| `output` | integer | The new output. |
| `range` | object | How the pyramid's or the wavelet method's result was brought into range. |
| `settings` | object | The settings it was stacked with, and the framing. |
| `skipped` | array of integer | The enabled frames left out, because their alignment was rejected. |
| `uncovered` | integer | How many pixels no frame covers: transparent in the output (only wide framing has them). |
| `width` | integer | Its size, in pixels. |

Examples:

```json
{}
{"name":"Depth map, more filled","percentile":50,"threshold":"percentile"}
{"radius":3.0,"smoothing":16.0}
```

### `stack.get`

**Stacking settings.** The stacking methods' settings.

Every setting, with the keys the preferences don't set at their defaults: for the weighted average (`average`), the radius sharpness is measured over, the smoothing of the blend and the power that sets how strongly the sharpest frame wins; for the pyramid method (`pyramid`), the number of levels, the grit suppression, the decisions (luminance or channels), the range limit, the tone compression and the coarse limit; for the depth-map method (`depth_map`), the estimation radius, the smoothing radius and the contrast threshold: how it is chosen (`threshold`: auto, level or percentile), and its `level` and `percentile`; for the wavelet method (`wavelet`), the number of levels, the filter bank, the activity window, the consistency filter, the low-pass rule, the grit suppression, the noise floor, the range limit and the tone compression.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Query | No | An open project | Nothing |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `settings` | object | Every setting, with the defaults filled in. |

Example:

```json
{}
```

### `stack.preview`

**Preview stack.** Stacks the aligned frames at a quarter of their size, as a quick preview.

The weighted average of `stack.average`, or with `method` "pyramid", "depth-map" or "wavelet" the method of `stack.pyramid`, `stack.depthMap` or `stack.wavelet`, on frames reduced to a quarter of their size on each side with the settings scaled to match, so that it shows in seconds what the full stack will look like. Takes the same settings as that method's command. The result is a linear 32-bit float TIFF in the project's `previews/` folder, as a path, kept until the project is closed: no output, no undo step. Runs beside other jobs as a preview; a newer call replaces an older one still running, so call it after every change of a setting.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `coarseLimit` | integer, from 0 to 1000 | The pyramid's coarse limit in frames, or `null` for off; the project's when left out. |
| `consistency` | boolean | Whether the wavelet method's consistency filter is on; the project's when left out. |
| `decisions` | `"luminance"` \| `"channels"` | The pyramid's decisions, luminance or channels; the project's when left out. |
| `grit` | number, from 0.0 to 1.0 | The pyramid's or the wavelet method's grit suppression, 0 or from 0.25 to 1; the project's when left out. |
| `level` | number, at least 0.0 | The depth-map method's threshold level, from 0; the project's when left out. |
| `levels` | integer, from 0 to 10 | The pyramid's or the wavelet method's levels at full size, 0 to 10 (0 chooses); the project's when left out. |
| `lowpass` | `"focus"` \| `"average"` \| `"select"` | How the wavelet method makes the coarsest low-pass: focus, average or select; the project's when left out. |
| `method` | `"average"` \| `"pyramid"` \| `"depth-map"` \| `"wavelet"` | The method, "average", "pyramid", "depth-map" or "wavelet"; the settings below are that method's. Left out, the command's own: "average" for `stack.preview`, "pyramid" for the slabs of `stack.slabbed` and for its final stack. |
| `noiseFloor` | number, from 0.0 to 2.0 | The wavelet method's noise floor, in percent of white at full size, from 0 (off) to 2; the project's when left out. |
| `percentile` | number, from 0.0 to 100.0 | The depth-map method's threshold percentile, from 0 to 100; the project's when left out. |
| `power` | number, from 0.5 to 4.0 | The weighted average's power, from 0.5 to 4; the project's when left out. |
| `radius` | number, from 0.0 to 64.0 | The weighted average's or the depth-map method's radius, in pixels of the full-size frames, from 0 to 64; the project's (`stack.get`) when left out. |
| `rangeLimit` | number, from 0.0 to 1.0 | The pyramid's or the wavelet method's range limit, from 0 to 1; the project's when left out. |
| `smoothing` | number, from 0.0 to 256.0 | The weighted average's smoothing, in pixels of the full-size frames, from 0 to 64, or the depth-map method's, from 0 to 256; the project's when left out. |
| `threshold` | `"auto"` \| `"level"` \| `"percentile"` | How the depth-map method's contrast threshold is chosen: auto, level or percentile; the project's when left out. |
| `tone` | `"compress"` \| `"clip"` \| `"keep"` | The pyramid's or the wavelet method's tone compression: compress, clip or keep; the project's when left out. |
| `wavelet` | `"bior22"` \| `"db2"` | The wavelet method's filter bank: `bior22` or `db2`; the project's when left out. |
| `window` | integer, from 3 to 5 | The wavelet method's activity window, 3 or 5 samples; the project's when left out. |

Result:

| Name | Type | Description |
|---|---|---|
| `frames` | array of integer | The frames stacked, in the order they were added. |
| `framing` | `"narrow"` \| `"wide"` \| `"common"` | The output's field of view. See the module documentation. |
| `height` | integer |  |
| `method` | string | The method, "average", "pyramid", "depth-map" or "wavelet". |
| `path` | string | The preview: a linear 32-bit float TIFF, with an alpha channel when some pixels are covered by no frame. |
| `settings` | object | The settings at full scale, and the framing. |
| `skipped` | array of integer | The enabled frames left out, because their alignment was rejected. |
| `uncovered` | integer | How many pixels no frame covers. |
| `width` | integer | Its size, in pixels: a quarter of the full stack's on each side. |

Examples:

```json
{}
{"smoothing":3.0}
{"grit":0.25,"method":"pyramid"}
{"level":0.002,"method":"depth-map","threshold":"level"}
{"method":"wavelet","window":5}
```

### `stack.pyramid`

**Stack by pyramid.** Stacks the aligned frames by the pyramid method into a new output.

Decomposes every frame into a Laplacian pyramid and keeps, at every level and position, the coefficients of the frame with the most local energy: the finest detail of every frame survives where near and far structure overlap (hair, bristles, antennae), without the veils the weighted average leaves around out-of-focus edges. The finest level is blended to keep noise down (`grit`), choices are made on luminance or on each channel (`decisions`), every pixel is held within the range the frames span there (`rangeLimit`), and the result is brought into [0, 1] as `tone` says; when that changes any pixel, the result before it is kept with the output for "retain extended range" files (`output.save` with `extendedRange` and 32-bit float). `coarseLimit` (off by default) keeps the coarse levels' choices within that many frames of the frame in focus there. Uses the project's settings (`stack.get`), or the ones given for this stack only, and the stored alignment, as `stack.average` does. Runs as a job; the output is added in one undo step when it ends, or not at all when it is cancelled.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `coarseLimit` | integer, from 0 to 1000 | The coarse limit in frames, 0 to 1000, or `null` for off; the project's when left out. |
| `decisions` | `"luminance"` \| `"channels"` | Whether choices are made on luminance or on each channel; the project's when left out. |
| `grit` | number, from 0.0 to 1.0 | Grit suppression at the finest level, 0 (off) or from 0.25 to 1; the project's when left out. |
| `levels` | integer, from 0 to 10 | The number of pyramid levels, 1 to 10, or 0 to choose from the canvas's size; the project's (`stack.get`) when left out. |
| `name` | string | The output's name; "Pyramid" when left out. |
| `rangeLimit` | number, from 0.0 to 1.0 | How strictly each pixel is held within the frames' range there, from 0 to 1; the project's when left out. |
| `tone` | `"compress"` \| `"clip"` \| `"keep"` | How the result is brought into [0, 1]: compress, clip or keep; the project's when left out. |

Result:

| Name | Type | Description |
|---|---|---|
| `depth` | object | The depth-map method's threshold and the pixels it filled. |
| `frames` | array of integer | The frames stacked, in the order they were added. |
| `framing` | `"narrow"` \| `"wide"` \| `"common"` | The output's field of view. See the module documentation. |
| `height` | integer |  |
| `levels` | integer | The pyramid's or the wavelet method's number of levels. |
| `output` | integer | The new output. |
| `range` | object | How the pyramid's or the wavelet method's result was brought into range. |
| `settings` | object | The settings it was stacked with, and the framing. |
| `skipped` | array of integer | The enabled frames left out, because their alignment was rejected. |
| `uncovered` | integer | How many pixels no frame covers: transparent in the output (only wide framing has them). |
| `width` | integer | Its size, in pixels. |

Examples:

```json
{}
{"grit":0.25,"name":"Pyramid, more detail"}
{"decisions":"channels","tone":"keep"}
```

### `stack.reset`

**Reset stacking settings.** Puts every stacking setting back at its default.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters: none.

Result:

| Name | Type | Description |
|---|---|---|
| `settings` | object | Every setting, with the defaults filled in. |

Example:

```json
{}
```

### `stack.set`

**Set stacking settings.** Changes some of the stacking methods' settings.

Only the keys given change, as when a preset is applied: `{"average": {"radius": 2}}` changes the weighted average's radius and leaves the rest as it is. The result is checked whole, and a misspelled key is an error. The settings apply to the next stack and preview.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Instant | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `average` | object | The weighted average's: `radius`, `smoothing` and `power`. Default: `{"power":3.0,"radius":3.0,"smoothing":8.0}`. |
| `average.power` | number, from 0.5 to 4.0 | How strongly the sharpest frame wins, from 0.5 to 4: the power the sharpness is raised to before the weights are normalized. 1 blends in proportion to sharpness; higher values come closer to picking one frame per pixel. Default: `3.0`. |
| `average.radius` | number, from 0.0 to 64.0 | The radius over which sharpness is measured: the standard deviation, in pixels, of the window the Laplacian's energy is averaged over, from 0 to 64. Larger is steadier on noise and smooth surfaces, smaller follows fine detail more closely. Default: `3.0`. |
| `average.smoothing` | number, from 0.0 to 64.0 | How smoothly the blend moves from one frame to the next across the image: the standard deviation, in pixels, of the blur of each frame's weights, from 0 to 64. Larger hides the seams between frames, smaller keeps edges between near and far parts crisper. Default: `8.0`. |
| `depth_map` | object | The depth-map method's: `radius`, `smoothing`, `threshold`, `level` and `percentile`. Default: `{"level":0.001,"percentile":30.0,"radius":2.0,"smoothing":8.0,"threshold":"auto"}`. |
| `depth_map.level` | number, at least 0.0 | The threshold's level of confidence when `threshold` is `level`, from 0, in units of linear luminance contrast. Default: `0.001`. |
| `depth_map.percentile` | number, from 0.0 to 100.0 | The share of the pixels, in percent from 0 to 100, whose depth is filled at least when `threshold` is `percentile`. Default: `30.0`. |
| `depth_map.radius` | number, from 0.0 to 64.0 | The estimation radius: the standard deviation, in pixels, of the window the focus measure is averaged over, from 0 to 64. Larger is steadier on noise and smooth surfaces, smaller follows the edges between near and far parts more closely. Default: `2.0`. |
| `depth_map.smoothing` | number, from 0.0 to 256.0 | The smoothing radius: the reach, in pixels, of the edge-aware filter that smooths the depth map and carries it into the pixels below the threshold, from 0 to 256. Default: `8.0`. |
| `depth_map.threshold` | `"auto"` \| `"level"` \| `"percentile"` | How the contrast threshold is chosen: `auto` (the default), `level` (an absolute level) or `percentile` (a share of the pixels). Default: `"auto"`. |
| `pyramid` | object | The pyramid method's: `levels`, `grit`, `decisions`, `range_limit`, `tone` and `coarse_limit`. Default: `{"decisions":"luminance","grit":0.5,"levels":0,"range_limit":1.0,"tone":"compress"}`. |
| `pyramid.coarse_limit` | integer, from 0 to 1000 | Off by default: the most frames the coarse levels' choices may be from the depth-consistent frame (the one whose finest-level energy is largest there), from 0 to 1000. |
| `pyramid.decisions` | `"luminance"` \| `"channels"` | Whether choices are made on luminance (one choice for all channels, so colors stay together) or on each channel. Default: `"luminance"`. |
| `pyramid.grit` | number, from 0.0 to 1.0 | Grit suppression at the finest level, from 0.25 (closest to picking) to 1 (blends most), or 0 (off: the sharpest frame's coefficient is picked, as at the other levels, which keeps the most fine detail and builds up the most noise). The finest level's coefficients are blended with weights of their energy raised to `1 / grit`. Default: `0.5`. |
| `pyramid.levels` | integer, from 0 to 10 | The number of pyramid levels, from 1 to 10; 0 chooses from the canvas's size (about 16 samples on the short side of the coarsest level: 5 for 720x540, 7 for 20 MP). More levels decide coarser structure by selection rather than by averaging the frames. Default: `0`. |
| `pyramid.range_limit` | number, from 0.0 to 1.0 | How strictly each pixel is held within the range the frames span there, from 0 (off) to 1 (never darker or brighter than every frame): removes the inversion halos and color casts of selection. Default: `1.0`. |
| `pyramid.tone` | `"compress"` \| `"clip"` \| `"keep"` | How the result is brought into `[0, 1]`: `compress` (a local shoulder for highlights, negatives to 0), `clip` or `keep` (values outside stay, for those who tone-map themselves). Default: `"compress"`. |
| `wavelet` | object | The wavelet method's: `levels`, `wavelet`, `window`, `consistency`, `lowpass`, `grit`, `range_limit` and `tone`. Default: `{"consistency":true,"grit":0.5,"levels":0,"lowpass":"focus","noise_floor":0.4,"range_limit":1.0,"tone":"compress","wavelet":"bior22","window":3}`. |
| `wavelet.consistency` | boolean | Whether a majority filter replaces isolated picks in each level's choices. Default: `true`. |
| `wavelet.grit` | number, from 0.0 to 1.0 | Grit suppression at the finest level, from 0.25 (closest to picking) to 1 (blends most), or 0 (off: the sharpest frame's coefficients are picked, as at the other levels). Default: `0.5`. |
| `wavelet.levels` | integer, from 0 to 10 | The number of levels, from 1 to 10; 0 chooses from the canvas's size, as the pyramid does (5 for 720x540, 7 for 20 MP). More levels decide coarser structure by selection rather than by averaging the frames. Default: `0`. |
| `wavelet.lowpass` | `"focus"` \| `"average"` \| `"select"` | How the coarsest low-pass is made: `focus`, `average` or `select`. Default: `"focus"`. |
| `wavelet.noise_floor` | number, from 0.0 to 2.0 | The noise floor of the finest level's blend, in percent of white in gamma-encoded values, from 0 (off) to 2: the frames' noise, below which activity counts as noise. Raise it for noisy frames (high ISO); too high, it blends away fine detail. Default: `0.4`. |
| `wavelet.range_limit` | number, from 0.0 to 1.0 | How strictly each pixel is held within the range the frames span there, from 0 (off) to 1 (never darker or brighter than every frame). Default: `1.0`. |
| `wavelet.tone` | `"compress"` \| `"clip"` \| `"keep"` | How the result is brought into `[0, 1]`: `compress`, `clip` or `keep`. Default: `"compress"`. |
| `wavelet.wavelet` | `"bior22"` \| `"db2"` | The filter bank: `bior22`, symmetric, or `db2`. Default: `"bior22"`. |
| `wavelet.window` | integer, from 3 to 5 | The side of the window activity is measured over, 3 or 5 samples. 3 keeps a frame's choice closest to what it has in focus. Default: `3`. |

Result:

| Name | Type | Description |
|---|---|---|
| `settings` | object | Every setting, with the defaults filled in. |

Examples:

```json
{"average":{"radius":2.0}}
{"pyramid":{"grit":0.25,"tone":"clip"}}
{"depth_map":{"percentile":40,"threshold":"percentile"}}
{"wavelet":{"lowpass":"average","window":5}}
```

### `stack.slabbed`

**Stack in slabs.** Stacks the aligned frames in overlapping slabs by one method, then the slabs by another.

For very deep stacks. Splits the frames, in the order they are stacked, into slabs of `slabSize` consecutive frames, neighbors sharing at least `overlap` frames, stacks each slab with the `slabs` method and the slabs with the `final` method (the pyramid by default, for both), each with the project's settings (`stack.get`) under the ones given for this stack, as `stack.preview` takes them: `{"method": "pyramid", "grit": 0.25}`. By default a slab holds the square root of the number of frames, rounded up, and neighbors share a fifth of a slab. Slabs are stacked one at a time, in the memory of one stack. Each slab becomes an output of its own, made from its frames, for retouching to take pixels from; the final output is made from the slabs' outputs. With `reuse` (the default), a slab the project has an output of already, made from the same frames, development, alignment and settings, is taken from that output instead of being stacked again: run it again with other `final` settings and only the slabs' outputs are stacked, not the frames. Uses the stored alignment, as `stack.average` does. Runs as a job; the outputs are added in one undo step when it ends, or not at all when it is cancelled.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `final` | object | The final stack's method and its settings; the pyramid method, with the project's settings, when left out. |
| `final.coarseLimit` | integer, from 0 to 1000 | The pyramid's coarse limit in frames, or `null` for off; the project's when left out. |
| `final.consistency` | boolean | Whether the wavelet method's consistency filter is on; the project's when left out. |
| `final.decisions` | `"luminance"` \| `"channels"` | The pyramid's decisions, luminance or channels; the project's when left out. |
| `final.grit` | number, from 0.0 to 1.0 | The pyramid's or the wavelet method's grit suppression, 0 or from 0.25 to 1; the project's when left out. |
| `final.level` | number, at least 0.0 | The depth-map method's threshold level, from 0; the project's when left out. |
| `final.levels` | integer, from 0 to 10 | The pyramid's or the wavelet method's levels at full size, 0 to 10 (0 chooses); the project's when left out. |
| `final.lowpass` | `"focus"` \| `"average"` \| `"select"` | How the wavelet method makes the coarsest low-pass: focus, average or select; the project's when left out. |
| `final.method` | `"average"` \| `"pyramid"` \| `"depth-map"` \| `"wavelet"` | The method, "average", "pyramid", "depth-map" or "wavelet"; the settings below are that method's. Left out, the command's own: "average" for `stack.preview`, "pyramid" for the slabs of `stack.slabbed` and for its final stack. |
| `final.noiseFloor` | number, from 0.0 to 2.0 | The wavelet method's noise floor, in percent of white at full size, from 0 (off) to 2; the project's when left out. |
| `final.percentile` | number, from 0.0 to 100.0 | The depth-map method's threshold percentile, from 0 to 100; the project's when left out. |
| `final.power` | number, from 0.5 to 4.0 | The weighted average's power, from 0.5 to 4; the project's when left out. |
| `final.radius` | number, from 0.0 to 64.0 | The weighted average's or the depth-map method's radius, in pixels of the full-size frames, from 0 to 64; the project's (`stack.get`) when left out. |
| `final.rangeLimit` | number, from 0.0 to 1.0 | The pyramid's or the wavelet method's range limit, from 0 to 1; the project's when left out. |
| `final.smoothing` | number, from 0.0 to 256.0 | The weighted average's smoothing, in pixels of the full-size frames, from 0 to 64, or the depth-map method's, from 0 to 256; the project's when left out. |
| `final.threshold` | `"auto"` \| `"level"` \| `"percentile"` | How the depth-map method's contrast threshold is chosen: auto, level or percentile; the project's when left out. |
| `final.tone` | `"compress"` \| `"clip"` \| `"keep"` | The pyramid's or the wavelet method's tone compression: compress, clip or keep; the project's when left out. |
| `final.wavelet` | `"bior22"` \| `"db2"` | The wavelet method's filter bank: `bior22` or `db2`; the project's when left out. |
| `final.window` | integer, from 3 to 5 | The wavelet method's activity window, 3 or 5 samples; the project's when left out. |
| `name` | string | The final output's name; "Slabbed pyramid" (after the final method) when left out. |
| `overlap` | integer, from 0 to 100000 | How many frames neighboring slabs share at least, fewer than a slab holds; a fifth of a slab, at least 1, when left out. |
| `reuse` | boolean | Whether a slab the project has an output of already, made from the same frames with the same settings, is taken from that output instead of being stacked again; true when left out. |
| `slabSize` | integer, from 2 to 100000 | The frames in a slab, from 2, fewer than the frames stacked; the square root of their number, rounded up, when left out. |
| `slabs` | object | The slabs' method and its settings, as `stack.preview` takes them; the pyramid method, with the project's settings, when left out. |
| `slabs.coarseLimit` | integer, from 0 to 1000 | The pyramid's coarse limit in frames, or `null` for off; the project's when left out. |
| `slabs.consistency` | boolean | Whether the wavelet method's consistency filter is on; the project's when left out. |
| `slabs.decisions` | `"luminance"` \| `"channels"` | The pyramid's decisions, luminance or channels; the project's when left out. |
| `slabs.grit` | number, from 0.0 to 1.0 | The pyramid's or the wavelet method's grit suppression, 0 or from 0.25 to 1; the project's when left out. |
| `slabs.level` | number, at least 0.0 | The depth-map method's threshold level, from 0; the project's when left out. |
| `slabs.levels` | integer, from 0 to 10 | The pyramid's or the wavelet method's levels at full size, 0 to 10 (0 chooses); the project's when left out. |
| `slabs.lowpass` | `"focus"` \| `"average"` \| `"select"` | How the wavelet method makes the coarsest low-pass: focus, average or select; the project's when left out. |
| `slabs.method` | `"average"` \| `"pyramid"` \| `"depth-map"` \| `"wavelet"` | The method, "average", "pyramid", "depth-map" or "wavelet"; the settings below are that method's. Left out, the command's own: "average" for `stack.preview`, "pyramid" for the slabs of `stack.slabbed` and for its final stack. |
| `slabs.noiseFloor` | number, from 0.0 to 2.0 | The wavelet method's noise floor, in percent of white at full size, from 0 (off) to 2; the project's when left out. |
| `slabs.percentile` | number, from 0.0 to 100.0 | The depth-map method's threshold percentile, from 0 to 100; the project's when left out. |
| `slabs.power` | number, from 0.5 to 4.0 | The weighted average's power, from 0.5 to 4; the project's when left out. |
| `slabs.radius` | number, from 0.0 to 64.0 | The weighted average's or the depth-map method's radius, in pixels of the full-size frames, from 0 to 64; the project's (`stack.get`) when left out. |
| `slabs.rangeLimit` | number, from 0.0 to 1.0 | The pyramid's or the wavelet method's range limit, from 0 to 1; the project's when left out. |
| `slabs.smoothing` | number, from 0.0 to 256.0 | The weighted average's smoothing, in pixels of the full-size frames, from 0 to 64, or the depth-map method's, from 0 to 256; the project's when left out. |
| `slabs.threshold` | `"auto"` \| `"level"` \| `"percentile"` | How the depth-map method's contrast threshold is chosen: auto, level or percentile; the project's when left out. |
| `slabs.tone` | `"compress"` \| `"clip"` \| `"keep"` | The pyramid's or the wavelet method's tone compression: compress, clip or keep; the project's when left out. |
| `slabs.wavelet` | `"bior22"` \| `"db2"` | The wavelet method's filter bank: `bior22` or `db2`; the project's when left out. |
| `slabs.window` | integer, from 3 to 5 | The wavelet method's activity window, 3 or 5 samples; the project's when left out. |

Result:

| Name | Type | Description |
|---|---|---|
| `depth` | object | The final depth map's threshold and the pixels it filled. |
| `finalSettings` | object | The settings a stack was made with, as its result reports them. |
| `frames` | array of integer | The frames stacked, in the order they were added. |
| `framing` | `"narrow"` \| `"wide"` \| `"common"` | The output's field of view. See the module documentation. |
| `height` | integer |  |
| `output` | integer | The final output, made from the slabs' outputs. |
| `overlap` | integer |  |
| `range` | object | How the result was brought into range, when a pyramid made the slabs or the final stack. |
| `skipped` | array of integer | The enabled frames left out, because their alignment was rejected. |
| `slabSettings` | object | The settings the slabs and the final stack were made with, and the framing. |
| `slabSize` | integer | The frames in a slab, and how many neighboring slabs share at least. |
| `slabs` | array of object | The slabs, in order. |
| `uncovered` | integer | How many pixels no slab covers: transparent in the output (only wide framing has them). |
| `width` | integer | Its size, in pixels. |

Examples:

```json
{}
{"final":{"method":"depth-map","percentile":40,"threshold":"percentile"},"name":"Slabbed depth map"}
{"final":{"grit":0.75},"overlap":3,"slabSize":12,"slabs":{"grit":0.25}}
```

### `stack.threshold`

**Show contrast threshold.** Shows which parts of the stack fall below the depth map's contrast threshold.

The depth-map method follows each pixel's sharpest frame only where that frame stands out clearly; below the contrast threshold, in smooth or blurred areas, it fills the depth from the confident pixels around. This shows where that happens, at a quarter of the frames' size: the sharpest frame's luminance at each pixel, black below the threshold, transparent where no frame covers it, as an 8-bit TIFF in the project's `previews/` folder (each call removes the one before), with the threshold's level and the share of the pixels below it. Takes the threshold settings of `stack.depthMap`, the project's (`stack.get`) when left out. The first call reads every frame, as the depth-map method's first pass does; later calls with the same frames, alignment, develop settings and radius reuse what it measured and take milliseconds, so call it on every move of a threshold slider. Runs beside other jobs as a preview; a newer call replaces an older one still running.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the preview lane | No | An open project | Nothing |

Parameters:

| Name | Type | Description |
|---|---|---|
| `level` | number, at least 0.0 | The threshold's absolute level, from 0, for `"threshold": "level"`; the project's when left out. |
| `percentile` | number, from 0.0 to 100.0 | The share of the pixels to fill at least, in percent from 0 to 100, for `"threshold": "percentile"`; the project's when left out. |
| `radius` | number, from 0.0 to 64.0 | The estimation radius, in pixels of the full-size frames, from 0 to 64; the project's (`stack.get`) when left out. |
| `threshold` | `"auto"` \| `"level"` \| `"percentile"` | How the contrast threshold is chosen: auto, level or percentile; the project's when left out. |

Result:

| Name | Type | Description |
|---|---|---|
| `below` | number | The share of the covered pixels below it, from 0 to 1: their depth is filled from around them. |
| `height` | integer |  |
| `level` | number | The threshold's level of contrast, which `"threshold": "level"` takes. |
| `measured` | boolean | Whether the frames' focus was measured again, rather than reused from the call before. |
| `path` | string | The picture: an 8-bit TIFF, gray with an alpha channel. |
| `width` | integer | Its size, in pixels: a quarter of the full stack's on each side. |

Examples:

```json
{}
{"percentile":40,"threshold":"percentile"}
{"level":0.002,"threshold":"level"}
```

### `stack.wavelet`

**Stack by wavelet.** Stacks the aligned frames by the wavelet method into a new output.

Splits every frame by a shift-invariant wavelet transform and keeps, at every level and position, the detail of the frame with the most local activity, measured on luminance over a `window` of 3 or 5 samples: like the pyramid method, it keeps the finest detail where near and far structure overlap, and on real stacks it keeps a little more of it, with about as much noise. `levels` (0 chooses from the canvas's size) and `wavelet` (the filter bank, `bior22` or `db2`) shape the transform, `consistency` replaces isolated choices by the choice around them, `lowpass` makes the coarsest structure from the frames in focus there (`focus`), their average (`average`, which lightens dark objects only the sharp frames show) or the frame chosen most often (`select`), `noiseFloor` is the frames' noise in percent of white (0.4 by default, 0 off; raise it for high-ISO frames), below which the finest level blends frames evenly rather than picking the noisiest, and `grit`, `rangeLimit` and `tone` work as the pyramid's do. Reads every frame twice, stacks on the CPU only, and takes several times the pyramid's time and about twice its memory. Uses the project's settings (`stack.get`), or the ones given for this stack only, and the stored alignment, as `stack.average` does. Runs as a job; the output is added in one undo step when it ends, or not at all when it is cancelled.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | Yes | A project open for editing | The project or the session |

Parameters:

| Name | Type | Description |
|---|---|---|
| `consistency` | boolean | Whether a majority filter replaces isolated choices at each level; the project's when left out. |
| `grit` | number, from 0.0 to 1.0 | Grit suppression at the finest level, 0 (off) or from 0.25 to 1; the project's when left out. |
| `levels` | integer, from 0 to 10 | The number of levels, 1 to 10, or 0 to choose from the canvas's size; the project's (`stack.get`) when left out. |
| `lowpass` | `"focus"` \| `"average"` \| `"select"` | How the coarsest low-pass is made: focus, average or select; the project's when left out. |
| `name` | string | The output's name; "Wavelet" when left out. |
| `noiseFloor` | number, from 0.0 to 2.0 | The frames' noise in percent of white, from 0 (off) to 2, below which the finest level blends frames evenly; the project's when left out. |
| `rangeLimit` | number, from 0.0 to 1.0 | How strictly each pixel is held within the frames' range there, from 0 to 1; the project's when left out. |
| `tone` | `"compress"` \| `"clip"` \| `"keep"` | How the result is brought into [0, 1]: compress, clip or keep; the project's when left out. |
| `wavelet` | `"bior22"` \| `"db2"` | The filter bank: `bior22` (symmetric) or `db2`; the project's when left out. |
| `window` | integer, from 3 to 5 | The side of the window activity is measured over, 3 or 5 samples; the project's when left out. |

Result:

| Name | Type | Description |
|---|---|---|
| `depth` | object | The depth-map method's threshold and the pixels it filled. |
| `frames` | array of integer | The frames stacked, in the order they were added. |
| `framing` | `"narrow"` \| `"wide"` \| `"common"` | The output's field of view. See the module documentation. |
| `height` | integer |  |
| `levels` | integer | The pyramid's or the wavelet method's number of levels. |
| `output` | integer | The new output. |
| `range` | object | How the pyramid's or the wavelet method's result was brought into range. |
| `settings` | object | The settings it was stacked with, and the framing. |
| `skipped` | array of integer | The enabled frames left out, because their alignment was rejected. |
| `uncovered` | integer | How many pixels no frame covers: transparent in the output (only wide framing has them). |
| `width` | integer | Its size, in pixels. |

Examples:

```json
{}
{"name":"Wavelet, wider window","window":5}
{"name":"Wavelet, high ISO","noiseFloor":1.0}
{"lowpass":"average","tone":"keep"}
```

## watch

### `watch.run`

**Watch capture folder.** Ingests settled frames, aligns arrivals and updates a preview until capture goes quiet.

Watches files directly inside input, including existing images, using native events plus polling. A frame must have unchanged size and modification time for settleMs and decode successfully, with unchanged metadata before and after the read. Truncated or locked files are retried until the quiet timeout, then reported as warnings. Files rewritten after ingest are refused. New frames extend the neighbor alignment chain; job events expose the latest preview path and frame count. After idleSeconds without a new or changed frame, runs final alignment and exports one stack and its project plus summary.csv. Waits for the first frame indefinitely. Output must be outside input. Template tokens are the same as batch.run; preset depth-map thresholds need no interaction.

| Runs | Undoable | Requires | Changes |
|---|---|---|---|
| Job, on the heavy lane | No | Nothing | Files outside the project |

Parameters:

| Name | Type | Description |
|---|---|---|
| `idleSeconds` | number, from 0.1 to 86400 | Seconds without a new or changed frame before finalization; waits at least settleMs too. Default: `5.0`. |
| `input` | string | **Required.** Folder populated by a tethered camera or its capture application. |
| `method` | `"pyramid"` \| `"depth-map"` \| `"average"` \| `"wavelet"` | Method used for the running preview and the final output. Default: `"pyramid"`. |
| `output` | string | **Required.** Folder for the saved project, TIFF and summary.csv, outside input. |
| `pollMs` | integer, from 10 to 5000 | Maximum milliseconds between directory scans, including when native events are unavailable. Default: `250`. |
| `preset` | string | **Required.** TOML preset, including contrast thresholds for unattended depth-map runs; or a built-in one as `builtin:<id>`. |
| `settleMs` | integer, from 50 to 60000 | Milliseconds a file's size and modification time must remain unchanged before decoding. Default: `1000`. |
| `template` | string | A portable image filename; accepts batch.run's template tokens, its extension following the preset's format. Default: `"{stack}-{method}-{range}.tif"`. |

Result:

| Name | Type | Description |
|---|---|---|
| `failed` | integer | Number of failed entries; a CLI reports failure when nonzero. |
| `stacks` | array of object | Each queue entry, in processing order. |
| `summary` | string | The CSV summary file. |

Example:

```json
{"idleSeconds":5,"input":"C:/Capture","output":"C:/Results","preset":"C:/Presets/macro.toml"}
```
