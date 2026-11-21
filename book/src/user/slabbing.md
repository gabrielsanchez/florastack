# Slabbing deep stacks

A stack of hundreds of frames, from a microscope or a rail at high magnification, can be stacked in two stages:
first in *slabs* of consecutive frames, each slab into an image of its own, then the slabs into the result. Each slab
covers a slice of the subject's depth, sharp where its frames are, and the final stage merges a few dozen slabs rather
than hundreds of frames.

Why slab:

- **Cleaner deep stacks.** Every frame a method reads adds a little of its noise and its out-of-focus blur to the
  decisions. Stacking slabs first keeps that local: a slab's blur comes only from frames near it in focus.
- **Faster second tries.** Each slab becomes an output of the project, kept with what it was made from. Stack again
  with other final settings, such as another contrast threshold, and only the slabs are read, not the frames.
- **Better retouching sources.** A slab is sharp over a whole slice of the subject, which makes it a better source to
  [retouch](retouching.md) from than any one of hundreds of frames.

For stacks of a few dozen frames, slabbing gains little: stack them directly.

## Slabs and overlap

The frames are split, in the order they are stacked, into slabs of **slab size** consecutive frames, neighboring slabs
sharing at least **overlap** frames, so that nothing between two slabs is lost. By default a slab holds the square root
of the number of frames, rounded up, and neighbors share a fifth of a slab: 200 frames make slabs of 15 frames, sharing
3. Each slab is stacked by the **slab method** and the slabs by the **final method**, the pyramid for both by default,
each with the project's stacking settings unless you give others.

Slabs are kept as 16-bit float files, which hold what the final stage needs of them (each value to about three
significant digits, and the values beyond white) in about 2.5 times less space than 32-bit float files: the 22 slabs
of a 300-frame stack of 24-megapixel frames take 2 to 3 GB. Where the frames don't all cover the picture (wide framing),
each slab fades out at its edge as its frames do, so the final stage blends across it without a seam.

The pyramid makes the cleanest final stage. A depth map of slabs can fill smooth areas from blurred slabs behind them
and leave dark specks along edges, so prefer the pyramid as the final method, or retouch those places from a slab.

## In the desktop app

Slabbing has no button of its own yet: run it from the command palette. Align the frames, press Ctrl+K (⌘K on macOS),
type "slabs" and pick **Stack in slabs**. Its parameters open as a form; untick every setting for the defaults, or
tick **Edit as JSON** and write them, for example:

```json
{ "slabSize": 12, "overlap": 3, "slabs": { "method": "pyramid" }, "final": { "method": "pyramid", "grit": 0.75 } }
```

Each slab appears in the outputs list ("Pyramid slab 3 of 15"), followed by the final output ("Slabbed pyramid"), all
added as one undo step.

## From the command line

`--slab SIZE:OVERLAP` stacks in slabs by the chosen method, for both stages:

```sh
florastack-cli stack photos --slab 15:3 --method pyramid --out result.tif
```

For different slab and final methods, run the engine command on a project:

```sh
florastack-cli run Specimen.florastack --cmd stack.slabbed --params '{"slabs":{"method":"pyramid"},"final":{"method":"depth-map","threshold":"percentile","percentile":40}}'
```

[`stack.slabbed`](../reference/commands.md#stackslabbed) lists every parameter.
