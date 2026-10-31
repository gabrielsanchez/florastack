# The Stack tab

The Stack tab, on the right of the desktop app's window, takes a stack of photos from frames to finished image in three steps: add the frames, align them, and stack them. Its settings are always on screen; the ones most stacks never need are under **Advanced**. Every setting has a tooltip, and its **?** opens its section of this page.

Changing a stacking setting renders a quarter-size preview in a few seconds, once the frames are aligned. A full-size stack runs in the background: you can keep changing settings and previewing while it runs, and each stack you make is kept in the outputs list.

## Workflow

1. **Frames.** Drop photos or a folder of them on the window, or use **Add frames**. RAW files, DNG, TIFF, JPEG and PNG work.
2. **Align.** Alignment finds how each frame moved, turned and changed scale against its neighbor, and corrects it. Each frame in the list then shows how well it matched (from 0 to 1, with a bar), whether it is the master frame the others are aligned to, and whether it was rejected; its tooltip gives the correction. Frames flagged as out of focus, moved or past a limit are worth a look: untick a frame to leave it out, and align again after changing which frames are in.
3. **Stack.** **Preview** shows a quarter-size version; **Stack frames** makes the full-size output. With the depth-map method, the contrast threshold comes first.

## Method

- **Pyramid** picks the sharpest detail scale by scale. It is the best all-rounder, and the best where near and far detail overlap, such as hair, bristles and antennae. It can raise contrast and noise a little.
- **Depth map** takes each pixel from the frame where it is sharpest. Colors and smooth backgrounds stay exactly as shot, but edges between near and far parts can show halos. It asks for its [contrast threshold](#contrast-threshold) before stacking.
- **Weighted average** blends every frame, each weighted by its sharpness: fast and smooth, but softer than the others.
- **Both** makes a pyramid output and a depth-map output from the same frames, to compare them, or to retouch one from the other later.

## Radius

How large an area, in pixels of the full-size frames, sharpness is measured over (weighted average and depth map). A larger radius ignores noise but blurs the boundary between frames; a smaller one follows fine detail such as thin hairs. Raise it when noise is mistaken for detail. The pyramid method has no radius.

## Smoothing

How gradually the result moves from one frame to the next, in pixels. Raise it to hide seams and halos where the sharp frame changes; lower it when fine detail looks smeared. The pyramid method has no smoothing.

## Alignment

Which corrections alignment makes:

- **Shift**: frames that moved sideways or up and down. Leave it on unless the camera was locked off and nothing could move.
- **Scale**: the change of magnification as focus moves (focus breathing) or as a rail moves the camera. Leave it on for most stacks.
- **Rotation**: frames that turned slightly. Turn it off for stacks on a rail, which can't rotate, to make alignment faster and steadier.
- **Brightness**: matches each frame's brightness to the master's, for flickering lights or drifting exposure.

Changing these needs a new alignment: the tab says so, and **Align** runs it again.

## Framing

The output's field of view, which applies when stacking (no new alignment needed):

- **Narrow** crops to the field of view of the stack's narrow end, so that every frame covers every pixel. The safe default.
- **Wide** keeps the widest field of view; its edges come from fewer frames, and parts no frame covers are transparent.
- **Common** crops to what every frame covers once aligned.

## Contrast threshold

The depth-map method follows each pixel's sharpest frame only where that frame stands out clearly. In smooth or blurred areas, such as an out-of-focus background, no frame does, and following noise there makes blotches. Below the contrast threshold, a pixel's depth is filled from the confident pixels around it instead.

When you stack by depth map, the tab shows the threshold first, over a picture of the stack: areas in **black** are below it. Raise the threshold until noise and smooth background turn black while the subject's detail stays visible, then press **Continue**.

- **Automatic** picks a threshold from the image.
- **Percentile** fills at least that share of the image.
- **Level** sets the contrast directly, for repeating a threshold across stacks.

The first picture takes as long as a quarter-size preview, since every frame is read; after that the picture follows the slider at once.

## Grit

The pyramid's finest detail can favor the single sharpest frame or a blend of the sharpest ones. Raise grit for crisper fine texture; lower it to reduce speckled noise in smooth areas.

## Range limit

Keeps each pixel of a pyramid stack within the brightness its frames have there, which stops the halos and overshoot the method can otherwise make around edges. Lower it only to let local contrast rise.

## Levels

How many detail scales the pyramid has; 0 chooses from the image's size. It rarely needs changing.

## Tone

What happens to the few values the pyramid pushes beyond white or black: **Compress** brings them back smoothly, **Clip** cuts them off, and **Keep** leaves them, for editing later in a 32-bit file.

## Decisions

Whether the pyramid picks the sharpest detail once for all colors (**By luminance**, the default) or for each color channel on its own (**By channel**), which can help with color fringes but may shift colors.

## Power

How strongly the sharpest frame wins in the weighted average. Raise it for a crisper result, lower it for a smoother one.

## Refine

After aligning each frame to its neighbor, align it again directly against the master frame, so that small errors don't add up along a long stack. It helps shallow stacks of similar frames most; deep stacks, whose far frames share little detail with the master, keep their chained alignment.

## Limits

How far alignment may move (in percent of the frame), scale (in percent) or turn (in degrees) a frame relative to the master. A frame that would go beyond a limit is flagged, and rejected when **Reject failed frames** is on. Raise a limit when good frames are flagged for reaching it.

## Match score

How well a frame must match its neighbor to count as aligned, from 0 to 1. Neighbors in a good stack score above 0.9. Lower it for difficult stacks with little detail.

## Reject

With **Reject failed frames** on, frames whose alignment failed or reached a limit are left out of the stack. Off, they are only flagged.

## Resolution

The longer side, in pixels, that frames are reduced to for alignment. Alignment is sub-pixel at that size already, so higher values cost time for little gain.

## Order

By default the stack is processed from its end with the narrower field of view, so that every frame covers the output. **Reverse the order** processes it from the other end.

## Out of focus

Frames with less in-focus detail than this share are flagged as out of focus: typically the blurred frames at the start and end of a stack, which add nothing but blur. They are only flagged; untick them to leave them out.

## Motion

Frames in which part of the subject moved at least this many pixels against the frame before are flagged as moved. Play the [filmstrip](#filmstrip) to see what moved.

## Defects

**Detect dust and hot pixels** finds the specks that stay at the same place on the sensor in every frame and paints them out before aligning. Off by default: check a detected mask before relying on it.

## Filmstrip

Drag up and down over the thumbnails in the frame list to play the stack frame by frame in the viewer, or press **Play**. Frames play aligned by default, so that whatever still moves is the subject: an insect's leg, a leaf in the wind. **As shot** plays them before alignment. Press Escape to return to the viewer.

## Outputs

Every stack is kept in the outputs list with its method and main parameters; the full parameters are in its tooltip. Click an output to view it. Tick **Compare** on two outputs to compare them, either with a **Split** line that you drag across the view, or **Side by side**, with zoom and pan shared.
