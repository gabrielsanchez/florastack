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
stack-method = Method
method-pyramid = Pyramid
method-depth-map = Depth map
method-average = Weighted average
stack-settings = Settings
setting-radius = Radius
setting-smoothing = Smoothing
setting-power = Power
setting-grit = Grit
setting-range-limit = Range limit
setting-tone = Tone
tone-compress = Compress
tone-clip = Clip
tone-keep = Keep (unclipped)
setting-decisions = Decisions
decisions-luminance = By luminance
decisions-channels = By channel
setting-threshold = Threshold
threshold-auto = Automatic
threshold-level = Level
threshold-percentile = Percentile
setting-level = Level
setting-percentile = Percentile
stack-reset = Reset to defaults
stack-align = Align
stack-preview = Preview
stack-run = Stack frames
stack-align-first = Align the frames to preview and stack them.
stack-show-preview = Show the preview
outputs-heading = Outputs

## Retouch tab

retouch-later = Retouching brushes arrive in a later version.

## Export tab

export-nothing = Stack the frames first: each stack is an output to export.
export-format = Format
export-pick = Pick an output to export.
export-save = Save as file…
export-remove = Remove output

## Batch tab

batch-intro = Stacks every stack in a folder (a folder of stack folders, or one folder of frames to split) into projects and TIFF files, with a summary.
batch-input = Stacks folder
batch-output = Results folder
batch-preset = Preset file
batch-method = Method: { $method } (as in the Stack tab)
batch-run = Run batch

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
