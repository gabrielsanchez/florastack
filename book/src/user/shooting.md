# Shooting for focus stacking

Most stacking problems start at the camera. A stack whose frames overlap in focus, match in exposure and stay still
stacks cleanly by any method; one with gaps, flicker or a moving subject leaves bands, blotches and ghosts that no
setting can fully repair. This chapter gathers what makes the difference.

## Cover the whole depth, with overlap

Each frame is sharp over a thin slice of the subject. Consecutive slices must overlap, or the result has soft bands
between them: a stripe of blur across a leg, a ring around a flower's center.

- **Start in front, end behind.** Begin a little in front of the nearest point you want sharp and end a little behind
  the farthest. The extra frames at each end cost little: alignment flags them **out of focus**, and you can untick
  them.
- **Choose the step.** With a camera's focus bracketing, use a small step (a finer "focus differential") at high
  magnification and a larger one for products and landscapes; too small only costs frames and time, too large leaves
  bands. With a rail, the **diffraction step calculator** in the [Capture tab](capture.md#physical-step-calculator)
  gives a step with 50% overlap from the magnification and the aperture: at 1:1 and f/4, about 70 µm; at 5:1 and f/2.8,
  about 12 µm.
- **Check the first stack.** Look at the result at 100% for soft bands, then make the step smaller if you find them.

## Aperture: don't stop down to gain depth

Stacking gives you depth of field; let the aperture give you sharpness. Most lenses are sharpest two or three stops
down from wide open. At macro magnifications the *effective* aperture is larger than the marked one, by the
magnification plus one: f/8 marked at 1:1 behaves like f/16, and diffraction softens every frame. Rather than closing
the aperture to cover the subject in fewer frames, shoot more frames at a sharper aperture. The step calculator
reports the size of the diffraction blur in pixels: under two pixels, the sensor resolves it; over several, fine
detail is lost to diffraction whatever the stacking.

## Keep exposure and color constant

- **Manual exposure.** Fix the aperture, shutter speed and ISO, so that every frame has the same brightness. The
  **Brightness** term of alignment corrects small differences (a flickering light, a drifting flash), not a camera
  changing its exposure between frames.
- **Fixed white balance** for JPEGs: with automatic white balance, each frame may come out a different color. RAW files
  are developed together with one white balance for the whole stack, so they don't have this problem.
- **RAW is best.** FloraStack develops RAW frames once for the whole stack into linear color, with the same settings
  for every frame. Camera JPEGs work, but carry the camera's sharpening, noise reduction and tone curve. A 16-bit TIFF
  from a raw converter works too, if every frame was exported with the same settings.

## Light

- **Constant or consistent.** Continuous light is simplest. Flash works when every frame gets the same power: give it
  time to recycle between frames, or lower its power so that it always can.
- **Diffuse it.** Large, soft light reduces specular highlights, which move as the focus changes and confuse the
  methods' choice of frame. A diffuser around a small subject helps more than any setting.
- **Watch for flicker.** LED and fluorescent lights can flicker at fast shutter speeds; the brightness term corrects
  some of it, a slower shutter speed or a flicker-free light the rest.

## Keep still

- **Steady the camera.** A tripod or copy stand, a remote release or self-timer, and an electronic shutter or a shutter
  delay keep frames from shaking. Alignment corrects shift, scale and rotation, but not blur within a frame.
- **Shoot living subjects fast.** A camera's focus bracketing at a high frame rate takes a stack in a second or two,
  before an insect moves. Alignment flags frames in which part of the subject **moved**; play the
  [filmstrip](stack-tab.md#filmstrip) to see what moved, and untick or retouch.
- **Mind the wind** outdoors: shield the subject, or wait for a lull.

## Dust and hot pixels

Dust on the sensor shows as a soft dark spot at the same place in every frame. Alignment moves the frames to line up
the subject, so in the stack those spots no longer line up with each other: one spot becomes a trail of spots. Hot
pixels (bright points, worse in long exposures and warm cameras) do the same.

- **Clean the sensor** before an important session, and check with a frame of a plain, evenly lit surface at a small
  aperture.
- **Let FloraStack find them.** **Detect dust and hot pixels**, under **Advanced** in the Stack tab, looks for spots at
  the same sensor position in every frame (it needs at least 5) and paints them out before aligning. Check what it
  found: `defects.export` writes the mask as an image, and you can draw your own (black spots on white, at the frames'
  size) and import it with `defects.import`, or with `--dust-mask` from the command line.
- **Shorter exposures** and a camera at room temperature make fewer hot pixels; many cameras can also map them out
  ("pixel mapping").

## Keep each stack together

Put each stack's frames in a folder of their own, in focus order. File names from the camera are usually in the order
of capture, which is the focus order. A folder that holds several stacks can be split by
[**File > Import stacks from a folder…**](batch-and-watch.md#several-stacks-in-one-folder), which finds where one
stack ends and the next begins.

## Settings to start from

The built-in presets (**Presets > Built-in presets**) set alignment and stacking for common kinds of subject:

| Preset | For |
|---|---|
| Insect macro | Insects and other small subjects, handheld or on a rail |
| Product shot | Still objects on a tripod under controlled light |
| Landscape | Near-to-far scenes on a tripod |
| Microscope | Stacks through a microscope, the stage moved between frames |

[Exporting, presets and preferences](export.md#presets) explains what each sets and how to make your own.
