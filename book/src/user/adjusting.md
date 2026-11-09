# Adjusting the image

Once an output is stacked and retouched, the Adjust tab finishes it: tone, color and detail for the whole image, and
the same within masks, regions such as the subject, the background, a part of the subject at some depth, a gradient,
an ellipse or brush strokes. Adjustments are non-destructive: they are settings kept with the output in the project, the
output itself never changes, and every change is one undo step. Exports write the output with its adjustments, after
its retouch strokes and before its annotations.

## A first adjustment

1. **Open the output.** Open the **Adjust** tab: it opens the output selected in the outputs list, or the newest, and
   shows it with its adjustments. **Output** picks another one.
2. **Set the whole image.** The sliders under **Light**, **Color** and **Detail** adjust the whole image. The view
   follows as you drag, with a quick preview while the slider moves and a full one when you let go. A slider's whole
   drag is one undo step.
3. **Add a mask.** **Add mask** adds a mask of one shape (below) and selects it. The panel then shows the mask: its
   shapes, their settings, and its own sliders under **Mask adjustments**, which apply only where the mask selects.
   **Show mask** tints the selected mask's region red.
4. **Compare.** **Before** shows the output as it was, without its adjustments, at the same size.
5. **Undo.** Ctrl+Z (⌘Z on macOS) undoes the last change, a slider's drag, a mask added or removed, a handle moved or
   a brush stroke. **Reset all** removes every adjustment and mask of the output, as one step.

## The sliders

| Group | Slider | What it does |
|---|---|---|
| Light | **Exposure** | Brightness in stops: +1 is twice as bright. |
| | **Contrast** | Spreads the tones apart around middle gray, or draws them together. |
| | **Highlights**, **Shadows** | The bright and the dark tones' brightness: lower highlights bring back detail in them, higher shadows open them up. |
| | **Whites**, **Blacks** | The white and the black points. |
| Color | **Temperature**, **Tint** | White balance, from the output as stacked: warmer or cooler, magenta or green. |
| | **Vibrance** | Saturation that spares colors already saturated. The whole image only. |
| | **Saturation** | Every color alike; the lowest is black and white. |
| Detail | **Texture** | Contrast of fine detail and surface texture. |
| | **Clarity** | Contrast of mid-sized detail: punch, or softness. |
| | **Dehaze** | Removes haze, or adds it. |
| | **Sharpening** | With its **Radius**, **Detail** and **Masking**, which limits it to edges. |
| | **Noise reduction** | Luminance noise, with its **Detail**; and **Color noise**. |

A mask's own sliders are the same, without vibrance, with **Sharpness** and **Noise** for detail, and with an
**Amount**, how strongly the mask applies them.

## Masks and their shapes

A mask is made of one or more shapes. The first one's region is the mask's; **Add shape** adds another, which is
added to it, subtracted from it or intersected with it (**Add**, **Subtract**, **Intersect**). **Invert** selects what
a shape, or the whole mask, leaves out. **Refine edges** makes the mask's edges follow the image's, which a subject or
a brushed mask needs most.

- **Subject** and **Background**: what the stack has in focus, and what it doesn't. FloraStack finds the subject in
  the stacked image itself: a focus stack brings everything its frames had in focus into one sharp image and leaves
  the rest as soft as the frames saw it, so the subject is where the image has fine detail. It needs no map and no
  frames. A smooth part of a subject (a shiny shell, a wing's membrane) larger than a small gap can be left out, and a
  backdrop with fine detail of its own taken in: brush them in or out with a brush shape, subtracted or added.
- **Depth range**: the part of the subject in focus in a range of the stack's frames, **From** 0, the first frame, **To**
  1, the last (the nearest and the farthest, or the other way round, as the stack was shot), fading over **Feather**.
  It needs the stack's depth map: a depth-map output has its own; for the other methods, **Map the depth** measures the
  frames' focus once, at a quarter of their size, and keeps the map with the output (adding a depth range does it for
  you). An output's masks can select at most five different depth ranges.
- **Linear gradient**: full effect before its first handle, fading to none at its second; drag the handles on the
  image.
- **Radial gradient**: an ellipse, with handles for its center and its two radii, and its **Feather** and **Angle**.
- **Brush**: drag on the image to paint, with the brush's **Size**, **Feather** and **Flow**; **Erase** takes away what
  earlier strokes painted. Each stroke is one undo step.
- **Luminance range**: the tones from **Darkest** to **Brightest**, in stops: 0 is eight stops below white, 1 white
  (at 1, everything brighter too).
- **Color range**: the colors close to the ones sampled: **Pick a color**, then click the image; up to 16 samples, with
  a **Tolerance**.
- **Whole image**: everything, for adjustments that should come after other masks.

## Scripting

Every change goes through the engine's `adjust.*` commands (the [command reference](../reference/commands.md)), so
scripts, the control channel and MCP clients adjust outputs as the tab does: `adjust.set` takes all the settings at
once, `adjust.addMask`, `adjust.setMask`, `adjust.removeMask`, `adjust.moveMask` and `adjust.stroke` change masks, and
`output.save` or `export.run` write the result. The adjustments are part of an export's reproducibility sidecar, so
`export.reproduce` writes the same file again.
