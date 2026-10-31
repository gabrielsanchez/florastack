# The desktop app's English strings (Project Fluent: https://projectfluent.org/). Every key here is used by the code,
# and every key the code uses is here: a test in src/i18n.rs checks both. A translation copies this file to
# locales/<language>/florastack.ftl; it may also override a command's title and summary from the engine's registry
# with `command-<name>-title` and `command-<name>-summary`, the name's dot written as a hyphen.

untitled = Untitled
title-unsaved = { $name } (unsaved)

## Menus

menu-file = File
menu-edit = Edit
menu-stack = Stack
menu-view = View
menu-help = Help
menu-recent = Recent projects
menu-recent-none = No recent projects
menu-about = About { $name }

## Actions: menu items, the command palette and Help > Shortcuts

action-new = New project
action-open = Open project…
action-add-frames = Add frames…
action-import-stacks = Import stacks from a folder…
action-save = Save
action-save-as = Save as…
action-close = Close project
action-quit = Quit
action-undo = Undo
action-undo-named = Undo { $what }
action-redo = Redo
action-redo-named = Redo { $what }
action-palette = Command palette…
action-align = Align frames
action-preview = Preview stack
action-stack = Stack frames
action-cancel-jobs = Cancel running jobs
action-zoom-fit = Fit to window
action-zoom-100 = Zoom to 100%
action-zoom-200 = Zoom to 200%
action-zoom-in = Zoom in
action-zoom-out = Zoom out
action-next-frame = Next frame
action-previous-frame = Previous frame
action-remove-frame = Remove frame
action-toggle-theme = Switch between dark and light
action-shortcuts = Keyboard shortcuts
action-preferences = Preferences…
action-brush-smaller = Smaller brush
action-brush-larger = Larger brush

## Workflow tabs

tab-stack = Stack
tab-retouch = Retouch
tab-export = Export
tab-batch = Batch

## Jobs

job-cancel = Cancel
job-cancelling = Cancelling…
job-eta = { $stage }, { $time } left

## Status line

notice-dismiss = Dismiss
notice-saved-as = Saved as { $path }
notice-no-stacks = No stacks found in that folder.
notice-exported = Exported { $count ->
    [one] one file
   *[other] { $count } files
} to { $folder }
device-gpu = GPU: { $name }
device-cpu = CPU

## Frame list

frames-heading = Frames
frames-empty = Drop frames or a folder here, or add them:
frames-enabled = Use this frame in stacks
frames-check = { $missing ->
    [0] { $changed ->
        [one] One frame's file changed since it was added.
       *[other] { $changed } frames' files changed since they were added.
    }
    [one] One frame's file is missing{ $changed ->
        [0] .
       *[other] , and { $changed } changed.
    }
   *[other] { $missing } frames' files are missing{ $changed ->
        [0] .
       *[other] , and { $changed } changed.
    }
}

flag-limit = limit
flag-limit-help = Alignment reached a limit (shift, scale or rotation) and would have gone beyond it.
flag-low-score = low score
flag-low-score-help = The frame matched its neighbor less well than the minimum score.
flag-repeating-pattern = repeating pattern
flag-repeating-pattern-help = A repeating pattern matches in several places: the alignment may be off by one period.
flag-no-match = no match
flag-no-match-help = The frame shares no detail with its neighbor.
flag-not-refined = not refined
flag-not-refined-help = Refining against the master frame didn't match well enough; the chained estimate stays.
flag-out-of-focus = out of focus
flag-out-of-focus-help = The frame has no in-focus detail. It is only flagged: leave it out if it adds nothing.
flag-moved = moved
flag-moved-help = Part of the subject moved against the neighboring frame.

## Stack tab

stack-no-project = Open a project or add frames to start.
step-frames = 1. Frames: { $count }
step-align = 2. Align
step-stack = 3. Stack
stack-align = Align
stack-preview = Preview
stack-run = Stack frames
stack-show-preview = Show the preview
tip-preview = Stacks a quarter-size version in a few seconds, to see what the full stack will look like. Changing a setting does it on its own.
tip-stack = Stacks the aligned frames at full size into a new output, in the background: you can keep changing settings and previewing meanwhile.
align-none = Not aligned yet.
align-summary = { $aligned } of { $count } frames aligned, { $flagged } flagged, { $rejected } rejected.
align-changed = The alignment settings changed since the frames were aligned: align them again.
align-missing = { $count ->
    [one] One frame isn't aligned yet: align again.
   *[other] { $count } frames aren't aligned yet: align again.
}
learn-more = Learn more

stack-method = Method
method-pyramid = Pyramid
method-depth-map = Depth map
method-average = Weighted average
method-both = Both (pyramid and depth map)
tip-method-pyramid = Picks the sharpest detail scale by scale. The best all-rounder, and the best where near and far detail overlap, such as hair, bristles and antennae. Can raise contrast and noise a little.
tip-method-depth-map = Takes each pixel from the frame where it is sharpest. Keeps colors and smooth backgrounds exactly as shot, and needs a contrast threshold. Best for subjects without overlapping detail; it can leave halos at edges.
tip-method-average = Blends every frame, each weighted by its sharpness. Fast and smooth, but softer than the others. Use it for quick results or simple subjects.
tip-method-both = Makes a pyramid and a depth-map output from the same frames, to compare them or to retouch one from the other.

setting-radius = Radius
setting-smoothing = Smoothing
tip-radius = How large an area, in pixels, sharpness is measured over. Raise it when noise is mistaken for detail; lower it to keep fine detail such as thin hairs.
tip-smoothing = How gradually the result moves from one frame to the next, in pixels. Raise it to hide seams and halos; lower it when fine detail looks smeared.
tip-radius-pyramid = The pyramid method has no radius or smoothing: it picks detail scale by scale. Its main setting, Grit, is under Advanced.

align-terms = Alignment
align-shift = Shift
align-scale = Scale
align-rotation = Rotation
align-brightness = Brightness
tip-align-shift = Corrects frames that moved sideways or up and down. Leave it on unless the camera was locked off and the subject can't move.
tip-align-scale = Corrects the change of magnification as focus moves (focus breathing) or as a rail moves the camera. Leave it on for most stacks.
tip-align-rotation = Corrects frames that turned slightly. Turn it off for stacks on a rail, which can't rotate, to make alignment faster and steadier.
tip-align-brightness = Matches each frame's brightness to the master's, for flickering lights or exposure that drifted.

setting-framing = Framing
framing-narrow = Narrow
framing-wide = Wide
framing-common = Common
tip-framing-narrow = Crops to the field of view of the stack's narrow end: every frame covers every pixel. The safe default.
tip-framing-wide = Keeps the widest field of view: the edges come from fewer frames, and parts no frame covers are transparent.
tip-framing-common = Crops to what every frame covers once aligned.

advanced = Advanced
setting-grit = Grit
tip-grit = How much the finest detail favors the single sharpest frame over a blend. Raise it for crisper fine texture; lower it to reduce speckled noise.
setting-range-limit = Range limit
tip-range-limit = Keeps each pixel within the brightness the frames have there, which stops the halos and overshoot the pyramid method can make. Lower it only to let contrast rise.
setting-levels = Levels
tip-levels = How many detail scales the pyramid has (0 chooses from the image's size). Rarely needs changing.
setting-tone = Tone
tone-compress = Compress
tone-clip = Clip
tone-keep = Keep (unclipped)
tip-tone = What happens to the few values the pyramid pushes beyond white or black: compressed back smoothly, clipped, or kept for editing later in a 32-bit file.
setting-decisions = Decisions
decisions-luminance = By luminance
decisions-channels = By channel
tip-decisions = Whether the sharpest detail is picked once for all colors, or for each color channel on its own. By channel can help with color fringes, but may shift colors.
setting-power = Power
tip-power = How strongly the sharpest frame wins in the blend. Raise it for a crisper result, lower it for a smoother one.
threshold-when-stacking = Its contrast threshold is set when you stack.

align-heading = Alignment
align-refine = Refine against the master
tip-align-refine = Aligns each frame again directly against the master frame, so that small errors don't add up along a long stack. Helps shallow stacks of similar frames most.
align-max-shift = Largest shift (%)
align-max-scale = Largest scale change (%)
align-max-rotation = Largest rotation (°)
tip-align-limits = How far alignment may move, scale or turn a frame relative to the master. Raise a limit when frames are flagged for reaching it.
align-min-score = Lowest match score
tip-align-min-score = How well a frame must match its neighbor to count as aligned, from 0 to 1. Lower it for difficult, low-detail stacks.
align-reject = Reject failed frames
tip-align-reject = Leaves out the frames whose alignment failed or reached a limit. Off, they are only flagged.
align-resolution = Alignment resolution
tip-align-resolution = The size, in pixels, frames are reduced to for alignment. Alignment is already sub-pixel; higher values cost time for little gain.
align-automatic-order = Start from the narrow end
tip-align-automatic-order = Aligns from the end of the stack with the narrower field of view, so that every frame covers the output.
align-reverse-order = Reverse the order
tip-align-reverse-order = Processes the stack from its other end.
align-min-sharp = Out-of-focus limit
tip-align-min-sharp = Flags frames with less in-focus detail than this, such as the blurred ones at the start and end of a stack, so that you can leave them out.
align-min-motion = Smallest movement flagged (px)
tip-align-min-motion = Flags frames in which part of the subject moved at least this many pixels against the frame before.
align-detect-defects = Detect dust and hot pixels
tip-align-detect-defects = Finds the dust and hot pixels that stay at the same place in every frame, and paints them out before aligning.
stack-reset = Reset stacking settings
align-reset = Reset alignment settings

threshold-title = Contrast threshold
threshold-text = Areas in black are below the threshold: too smooth or blurred to tell which frame is sharpest, so their depth is filled from around them. Raise it until noise and smooth background turn black, while the subject's detail stays visible.
threshold-auto = Automatic
threshold-percentile = Percentile
threshold-level = Level
tip-threshold-mode = Automatic picks a threshold from the image; Percentile fills at least that share of the image; Level sets the contrast directly.
tip-threshold = Drag to raise or lower the threshold; the picture follows.
threshold-measuring = Measuring focus…
threshold-below = { $share }% of the image is below the threshold.
threshold-continue = Continue

outputs-heading = Outputs
output-compare = Compare
tip-compare = Pick two outputs to compare them side by side or with a split line.
compare-split = Split
compare-side = Side by side
compare-hint = Pick two outputs to compare them.
made-auto = automatic threshold
made-percentile = fills { $value }%
made-level = threshold { $value }

## Filmstrip

film-play = Play
film-stop = Stop
film-as-shot = As shot
film-aligned = Aligned
tip-film = Drag up and down over the thumbnails to play the stack frame by frame, and spot what moved between frames. Escape returns to the viewer.
tip-film-as-shot = Plays the frames as they were shot rather than aligned: alignment's own corrections then show as movement.
film-caption = Frame { $index } of { $count }: { $name } ({ $how })
quality-master = master
quality-rejected = rejected
quality-score = match { $score }
quality-tip = Match score { $score } · shift { $x }, { $y } px · scale { $scale }% · rotation { $rotation }° · brightness ×{ $gain }

## Retouch tab

retouch-nothing = Stack the frames first: retouching paints onto an output.
retouch-output = Output
retouch-pick-output = Pick an output
retouch-opening = Opening the output for retouching…
retouch-hint = Pick an output to retouch in the panel on the right.
retouch-layers = Layers
retouch-no-layers = No layers yet: the first stroke makes one.
retouch-layer = { $name } ({ $strokes ->
    [one] one stroke
   *[other] { $strokes } strokes
})
tip-layer-visible = Show or hide this layer's strokes.
tip-layer-active = Strokes go into this layer.
retouch-add-layer = Add layer
retouch-remove-layer = Remove layer
retouch-source = Source
retouch-source-frame = Frame: { $name }
retouch-source-output = Output: { $name }
retouch-source-none = No source yet: pick a frame or an output, or Ctrl+click the output.
retouch-source-loading = Loading the source…
retouch-frame = Frame
retouch-other-output = Other output
retouch-pick = Pick…
retouch-follow = Follow the sharpest frame
tip-follow = While the pointer rests on a point of the output, the source becomes the frame where that point is sharpest.
retouch-jump-hint = Ctrl+click a point of the output to use the frame where it is sharpest.
retouch-map = Map sharpest frames
tip-map = Measures the frames' focus once, so that clicking a point can jump to the frame where it is sharpest.
retouch-mapping = Mapping the sharpest frames…
retouch-brush = Brush
brush-details = Details
brush-pixels = Pixels
brush-darken = Darken
brush-lighten = Lighten
brush-eraser = Eraser
tip-brush-details = Paints the source's detail, its brightness and color matched to the output so that strokes blend in, with a hard edge where there is fine detail. The brush for most retouching.
tip-brush-pixels = Paints the source exactly as it is, with the hardness you set.
tip-brush-darken = Paints the source only where it is darker than the result: dark bristles or hairs over a light background.
tip-brush-lighten = Paints the source only where it is lighter than the result: light hairs over a dark background.
tip-brush-eraser = Brings back the output as it was stacked.
brush-size = Size
tip-brush-size = The brush's diameter, in pixels of the output. [ and ] make it smaller and larger.
brush-hardness = Hardness
tip-brush-hardness = How much of the brush paints fully before its edge fades: low for a soft edge that blends, high for a crisp one.
brush-opacity = Opacity
tip-brush-opacity = How strongly a stroke paints.
brush-pressure = Use pen pressure
tip-brush-pressure = A pen's pressure makes the brush smaller, where the system reports it (Windows).
retouch-boost = Brightness boost
tip-boost = Shows both images brighter, to work in dark regions. It changes only what you see, not the output.

## Export tab

export-nothing = Stack the frames first: each stack is an output to export.
export-settings = Export settings
export-format = Format
tip-export-format = TIFF for editing; PNG for the web without loss; JPEG for sharing; DNG to develop the stack as a RAW file; OpenEXR for compositing, in 32-bit float.
export-depth = Bit depth
tip-export-depth = 16 bits keep smooth gradients through later editing; 8 bits are enough for a final picture; 32-bit float keeps every value, and the extended range.
export-depth-8 = 8-bit
export-depth-16 = 16-bit
export-depth-32 = 32-bit float
export-compression = Compression
tip-tiff-compression = Deflate and LZW lose nothing and make smaller files; uncompressed files open in every program.
tip-exr-compression = ZIP suits most pictures; PIZ suits noisy ones; all of them lose nothing.
tip-dng-compression = Lossless JPEG makes smaller files that lose nothing.
compression-none = None
compression-deflate = Deflate
compression-lzw = LZW
compression-zip = ZIP
compression-zips = ZIP, line by line
compression-piz = PIZ
compression-rle = RLE
compression-lossless-jpeg = Lossless JPEG
export-exr-half = Half floats
tip-exr-half = 16-bit floats: half the size, with less precision.
export-jpeg-quality = Quality
tip-jpeg-quality = 90 to 95 for a picture to keep, 75 to 85 for the web.
export-subsampling = Color detail
tip-jpeg-subsampling = Full color detail (4:4:4) keeps fine colored edges; 4:2:0 makes smaller files.
subsampling-444 = Full (4:4:4)
subsampling-422 = Half across (4:2:2)
subsampling-420 = Quarter (4:2:0)
export-dng-look = Look
tip-dng-look = Rendered keeps the colors as the stack has them; raw leaves them to the RAW developer.
dng-look-rendered = Rendered
dng-look-raw = Raw
export-extended-range = Retain extended range
tip-extended-range = Keeps the stack's values beyond black and white, before they were brought into range, for editing in 32-bit float. Only 32-bit float files can hold them.
export-profile = Color profile
export-profile-input = Keep the frames' profile
tip-profile-input = The profile the frames were shot or exported in, embedded in the file.
export-profile-srgb = sRGB
tip-profile-srgb = The web's and most screens' colors: the safest choice for sharing.
export-profile-pro-photo = Wide gamut (ProPhoto RGB)
export-profile-rec2020 = Wide gamut (Rec. 2020)
tip-profile-wide = Keeps saturated colors that sRGB can't hold, for editing; convert to sRGB before sharing.
export-metadata = Copy metadata from frame
tip-export-metadata = The camera, lens, exposure, date, place and keywords of the frame, in the file.
tip-metadata-frame = The frame, counted from 1 in the frame list's order, whose metadata the file gets.
export-template = File name
tip-export-template = The file's name without its extension. The tokens below are filled in.
export-tokens = Tokens: { $tokens }
export-alongside = Also export
export-depth-map = Depth map
tip-export-depth-map = Which frame each point is sharpest in, as a 16-bit gray TIFF beside the output: for masks and 3D.
export-confidence-map = Confidence map
tip-export-confidence-map = How sure the depth map is at each point, as a 16-bit gray TIFF beside the output.
export-maps-none = Only depth-map outputs have a depth map and a confidence map.
export-aligned-frames = Aligned frames
tip-export-aligned-frames = Every frame as it was aligned onto the output's canvas, as 16-bit TIFF files in a folder of their own: for retouching in other programs.
export-pick = Pick an output to export.
export-estimate = { $size } in { $files ->
    [one] one file
   *[other] { $files } files
}
export-estimate-about = About { $size } in { $files ->
    [one] one file
   *[other] { $files } files
}
tip-export-estimate = Compressed files' sizes depend on the picture: this is a guess.
export-overwrite = Replace files that exist
tip-export-overwrite = Off, an export that would replace a file stops and says which.
export-run = Export…
tip-export-run = Writes the output, and what goes with it, into a folder you pick.
export-remove = Remove output

## Presets

menu-presets = Presets
presets-built-in = Built-in presets
presets-apply-file = Apply a preset file…
presets-save = Save as preset…
presets-save-title = Save as preset
preset-apply-title = Apply { $name }
preset-apply-groups = The settings to take from it:
preset-apply = Apply
preset-name = Name
preset-description = Description
preset-save-groups = The settings to save:
preset-complete = Every setting, the defaults too
tip-preset-complete = On, applying the preset gives these settings whatever the project had. Off, it holds only what you changed, and layers over other settings.
preset-save = Save…
preset-group-develop = RAW develop
preset-group-align = Alignment
preset-group-stack = Stacking
preset-group-output = Output
notice-preset-applied = Applied { $name }.
notice-preset-saved = Saved the preset as { $path }
notice-preset-empty = { $name } holds no settings.

## Batch tab

batch-intro = Stacks every stack in a folder (a folder of stack folders, or one folder of frames to split) into projects and files, as the preset's output settings say, with a summary.
batch-input = Stacks folder
batch-output = Results folder
batch-preset = Preset
batch-preset-file = A preset file
batch-preset-path = Preset file
batch-method = Method: { $method } (as in the Stack tab)
batch-run = Run batch

## Preferences

prefs-title = Preferences
prefs-memory = Memory
prefs-memory-suggested = Use the suggested
prefs-memory-installed = This computer has { $installed } GB; { $suggested } MiB is suggested.
prefs-memory-unknown = The memory installed isn't known; { $suggested } MiB is suggested.
prefs-memory-help = Stacking keeps its work in memory up to this much. Beyond it, the work goes to the cache folder on disk: slower, but large stacks never run out of memory. A quarter of the memory installed leaves the rest for the system and your other programs.
prefs-compute = Processing
prefs-device-auto = The GPU when it passes its test, otherwise the CPU
tip-device-auto = The graphics card stacks when it computes what the CPU does, checked when FloraStack starts.
prefs-device-cpu = The CPU only
tip-device-cpu = Never the graphics card: when its driver misbehaves.
prefs-device-gpu = Any GPU, software ones too
tip-device-gpu = Software renderers compute correctly, but more slowly than the CPU: for testing.
prefs-test-gpu = Test the GPU
tip-test-gpu = Runs the GPU's self-test again, now: after a driver update, say.
prefs-gpu-passed = The GPU passed: { $name }
prefs-gpu-unused = The CPU stacks: { $reason }
prefs-cache = Cache
prefs-cache-help = Temporary projects and the work beyond the memory above go here. Temporary projects made before stay where they are.
prefs-cache-projects = Temporary projects
prefs-cache-tiles = Stacking's work on disk
prefs-cache-project = The open project's cache
prefs-cache-clean = Clean up
tip-cache-clean = Removes what stacks that crashed left on disk.
prefs-autosave = Autosave
prefs-minutes = {" "}min
prefs-autosave-help = after the first unsaved change; 0 for never.
prefs-appearance = Appearance
prefs-theme = Theme
prefs-theme-dark = Dark
prefs-theme-light = Light
prefs-language = Language
prefs-privacy = Privacy
prefs-no-telemetry = { $name } sends nothing anywhere: no telemetry, no usage statistics, no crash reports, no update checks. Your photos and settings stay on this computer.
prefs-ok = OK

## The viewer

view-hint = Drop frames, a folder or a project here.
view-loading = Loading…
drop-hint = Drop to open or add

## Banners

orphans-found = { $count ->
    [one] One old temporary project
   *[other] { $count } old temporary projects
} take { $size } MB.
orphans-trash = Move to the trash
orphans-keep = Keep

## Dialogs

cancel = Cancel
close = Close
browse = Browse…
unsaved-title = Unsaved changes
unsaved-text = The project has changes that aren't saved.
unsaved-save = Save
unsaved-discard = Don't save
save-as-name = Name
save-as-folder = In folder
save-as-save = Save
about-version = Version { $version }
about-text = Free, open-source focus stacking. Licensed under the MIT or Apache 2.0 license.
stacks-title = Stacks found
stacks-found = { $count } stacks in that folder. Pick one to add:
stacks-stack = { $first } and the following, { $count } frames

## Command palette

palette-hint = Type a command or an action…
palette-parameters = Parameters (JSON):
palette-run = Run
palette-back = Back
palette-nothing = Nothing matches.
