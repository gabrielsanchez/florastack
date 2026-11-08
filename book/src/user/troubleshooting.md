# Troubleshooting

Each problem below starts with what you see, then its usual causes and what to do about each. Look at the result at
100% (Ctrl+1): most of these show only there. [Choosing a method](methods.md#what-they-look-like) has crops of what
the methods do well and badly on a real stack.

## Halos

*A light or dark fringe along the edge of the subject, most often against a blurred background.*

- **The depth map at a near-to-far edge.** Each pixel comes from one frame, and near an edge the frame where the
  background is sharpest shows the subject's blurred edge spread over it. Stack by **Pyramid**, whose range limit holds
  each pixel within what the frames have there. Or keep the depth map and raise **Smoothing**, or
  [retouch](retouching.md) the edge from the frame where it is sharpest.
- **The pyramid with a lowered range limit.** Put **Range limit** (under **Advanced**) back at 1.
- **The weighted average's veil.** Out-of-focus frames leave a soft glow around the subject. Use the pyramid, or leave
  out the frames focused far behind the subject.

## Edge streaks

*Smeared, stretched or soft bands along the borders of the output.*

- **Wide framing.** Focusing changes the field of view a little (focus breathing), so the frames at one end of the
  stack see more than those at the other. With **Wide** framing, the borders come only from the frames that see them,
  which may all be out of focus there, and parts no frame covers are transparent. Use **Narrow** framing (the default),
  which crops to the narrow end, or **Common**, which crops to what every frame covers.
- **Frames that rotated or shifted without being corrected.** If **Rotation** or **Shift** is off and the camera
  moved, edges don't line up. Turn the term on and align again.
- **Light falling off toward the corners** that changes with focus. Turn on **Brightness**, which matches each frame's
  brightness to the master frame's.

## Dust trails

*A line or curve of small dark spots, the same spot repeated, often in a smooth area such as the sky or the
background.*

Dust on the sensor is at the same place in every frame. Alignment moves the frames to line up the subject, so the dust
spots land at a slightly different place in each, and the stack shows them all, in a row. Hot pixels make bright trails
the same way.

- **Detect dust and hot pixels**, under **Advanced** in the Stack tab, finds what stays at the same sensor position in
  every frame (from at least 5 frames) and paints it out before aligning. Align again after turning it on.
- **Check the mask**: `defects.export` writes it as an image (black spots on white). Draw your own, or correct the
  detected one, and import it with `defects.import` or `florastack-cli stack --dust-mask mask.png`.
- **Clean the sensor** before the next session ([Shooting for focus stacking](shooting.md#dust-and-hot-pixels)).

## Washed-out output

*The result looks flat, hazy or lighter than the frames.*

- **RAW frames are developed linearly.** FloraStack develops RAW files into linear, scene-referred color, without the
  tone curve and "look" your camera or raw converter adds, which belong after stacking. The result looks flatter than
  the camera's JPEG: apply your usual curve or preset in an editor, or export a **DNG** and develop it in your raw
  converter.
- **A viewer that ignores color profiles.** A file in a wide gamut (ProPhoto RGB, Rec. 2020) or a linear profile looks
  pale in a program that ignores the embedded profile. Export with the **sRGB** profile for the web and for such
  programs.
- **A 32-bit file with extended range.** Values kept beyond white need tone mapping in the editor; without it, they look
  clipped or pale. Turn off **Retain extended range**, or export 16-bit.
- **Haze from out-of-focus frames.** The weighted average blends every frame a little, and frames focused far beyond
  the subject add a veil. Leave out the frames flagged **out of focus**, or use the pyramid.

## Color shift

*Colors differ from the frames: a tint over the whole image, or colored fringes and blotches in places.*

- **Frames with different white balance.** JPEGs shot with automatic white balance can each have another color, and
  the stack mixes them. Fix the camera's white balance. RAW frames are developed with one white balance for the whole
  stack; choose it with `develop.set` (as shot, automatic, or a temperature and tint).
- **Decisions by channel.** The pyramid's **Decisions: By channel** picks detail for each color on its own, which can
  shift colors. Use **By luminance**, the default.
- **Colored speckle in dark, smooth areas** of a wavelet stack. The wavelet method keeps more of the finest detail than
  the pyramid, and in dark areas some of it is noise, which shows as fine colored speckle where the pyramid is clean.
  Stack by the pyramid, or retouch those areas from a pyramid output.
- **Colored fringes along edges** (lateral chromatic aberration). For RAW frames the develop step measures and
  corrects them by default (`"chromatic_aberration": "auto"`), and finds nothing to correct on a frame with too few
  strong edges: set the correction by hand with `develop.set` then. For other files, correct them in the raw
  converter before stacking.
- **RAW colors unlike the camera's JPEG.** FloraStack uses the camera's color matrix, without the maker's color
  styles. A camera without a known matrix develops with a neutral fallback, which `develop.get` reports. Convert the
  files to DNG, or to 16-bit TIFFs in your raw converter, for that converter's colors.
- **The pyramid in smooth areas** can move tones slightly. The **Depth map** keeps every pixel's color as one frame
  has it.

## Out-of-order stacks

*Many frames flagged with a low score or a limit, alignment that fails half way, or a result with soft bands and
doubled detail.*

The frames must be in focus order, from one end of the range to the other. Files from two cards, renamed files, or two
brackets in one folder break that order.

- **Sort frames by scale** repairs it: focusing changes the magnification steadily, so the frames' scales give their
  order. Run it from the command palette (Ctrl+K, "Sort frames by scale") or as the engine command
  `align.sortByScale`; it aligns the frames, sorts them and aligns again, as one undo step. It fails, changing nothing,
  when the scales differ too little to sort by, as on a rail that moves the subject rather than the camera.
- **Drag frames** in the frame list to put them in order by hand, then align again.
- **Several stacks in one folder** belong apart: **File > Import stacks from a folder…** finds where each begins
  ([Batch and watch folders](batch-and-watch.md#several-stacks-in-one-folder)).
- A stack processed from the wrong end, with frames rejected for reaching a limit, may need **Reverse the order**
  (under **Advanced**).

## Unsupported RAW files

*A RAW file is refused: "files of this kind can't be decoded yet: convert them to DNG (Adobe DNG Converter does) or to a
16-bit TIFF in your raw converter first".*

FloraStack reads DNG, Canon CR2 and CR3, Nikon NEF and NRW, Sony ARW, Fujifilm RAF, Panasonic RW2, Pentax PEF,
uncompressed Olympus ORF, Samsung SRW and a few more, but not every variant of every format: the compressed ORF files
that most Olympus and OM System cameras write, and Nikon's High Efficiency NEF, are among those it can't read yet.

- **Convert to DNG** with Adobe's free DNG Converter, which keeps the RAW data: FloraStack develops DNGs like any RAW
  file.
- **Or export 16-bit TIFFs** from your raw converter, with the same settings for every frame and no sharpening.
- **Or stack the camera's JPEGs** for a quick result.

A file reported as "damaged or not a valid" file is truncated or corrupt: copy it from the card again.

## Other problems

- **Frames flagged moved.** Part of the subject moved between frames: an insect's leg, a leaf in the wind. Play the
  [filmstrip](stack-tab.md#filmstrip) to see it; untick the frames, or retouch the place from one frame.
- **Low match scores** on a stack with little detail (a smooth surface, a microscope slide): lower **Lowest match
  score**, under **Advanced**.
- **The GPU isn't used.** `compute.info`, or the status line's `CPU`, says why: no adapter, or one that failed the
  self-test (whose message says what differed). Update the graphics driver and press **Test the GPU** in the
  preferences.
- **Stacking is slow or the computer sluggish.** See [GPU, memory and the cache](performance.md): a larger memory
  budget on a dedicated machine, a smaller one on a busy one, and a fast drive for the cache.
- **The desktop app prints nothing in a Windows terminal.** It is a windowed program: redirect its output
  (`florastack.exe --version | more`), or use `florastack-cli`.
- **Something else.** Run with `FLORASTACK_LOG=debug` set for a detailed log on standard error, and
  [open an issue](https://github.com/gabrielsanchez/florastack/issues/new/choose) with it, your system, and if you can,
  a sample of the stack.
