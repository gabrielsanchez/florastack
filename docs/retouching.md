# Retouching

Stacking can't always tell which frame is right: a bristle in front of a blurred background, a halo along an edge, a hair that moved between frames. The Retouch tab fixes those by hand, painting from a frame (or from another output) onto the result. Retouching is non-destructive: each stroke is a record kept in the project, layer by layer, and the result is the output with its strokes painted over it, so strokes can be undone, layers hidden, and the work picked up again in a later session.

## How it works

- **Two views.** The source is on the left and the output being retouched on the right, with zoom and pan locked together. The brush's circle shows at the same point in both, so you can see what a stroke will bring before you paint.
- **The source.** Pick a frame or another output in the panel. Or Ctrl+click a point of the output, and the source becomes the frame where that point is sharpest. With **Follow the sharpest frame** on, resting the pointer on a point does the same. Depth-map outputs know their sharpest frames already; for other outputs, **Map sharpest frames** measures the frames' focus once and keeps the map with the output.
- **Painting.** Drag on the output to paint; the stroke shows as you draw it and becomes one undo step when you let go. Pan with the middle or right button, or with Space and the left button; zoom with the wheel. `[` and `]` make the brush smaller and larger.
- **Layers.** Strokes go into the layer picked in the panel, or the top one; the first stroke makes "Layer 1". Hide a layer to see the output without its strokes.

## Brushes

- **Details** (the default) paints the source's detail with its brightness and color matched to the output's, so that a frame a little brighter or tinted than the stack paints seamlessly. Its edge is hard where there is fine detail (so a hair comes through whole, without a ghost) and as soft as its hardness elsewhere (so it blends).
- **Pixels** paints the source exactly as it is.
- **Darken** and **Lighten** paint only where the source is darker (or lighter) than the result: a dark bristle over a light background, without touching the background between the bristles.
- **Eraser** brings back the output as it was stacked.

Every brush has a size (its diameter, in pixels of the output), a hardness (how much of it paints fully before its edge fades) and an opacity. With **Use pen pressure** on, a pen's pressure makes the brush smaller where the system reports it. The **Brightness boost** shows both views brighter for working in dark regions; it changes only what you see.

## What is kept

- Strokes are kept in `project.json`, each with its brush, settings, source and the pointer's path, in pixels of the output. Painting them again over the same images gives the same pixels, bit for bit, which is what makes undo exact and later sessions reliable.
- When the project is saved (and when another output is opened for retouching), the retouched result is cached with the output, so that opening it again needs no recomputation. Frames used as sources are cached too, warped onto the output's canvas, so that they are read rather than decoded again.
- Exporting a retouched output (Export tab, `output.save`) writes it with its visible strokes.

## Manual checklist

Run through this list on a real stack (24 MP or larger, at least 30 frames, RAW files) on each platform before a release. Note the platform, GPU and stack with the results.

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
