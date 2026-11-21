# Batch and watch folders

Two ways to stack without sitting at the Stack tab: a **batch** stacks every stack in a folder, one after the other,
and a **watch folder** stacks a stack while it is being shot, as each photo arrives. Both take their settings from a
[preset](export.md#presets), and both are in the desktop app (the **Batch** and **Capture** tabs) and the command-line
tool (`batch` and `watch`).

## Several stacks in one folder

A day's shooting often leaves several stacks in one folder. **File > Import stacks from a folder…** finds where one
stack ends and the next begins, and lists them; pick one to add its frames. It looks at what changes between
consecutive files: a gap in capture time, another scene, a jump in framing or in magnification. `project.findStacks`
is the same as an engine command.

## Batch

The **Batch** tab takes a folder of stacks, a results folder and a preset, and **Run batch** stacks each one:

- **A folder of stack folders**: each immediate subfolder is a stack.
- **One folder of frames**: it is split into stacks as above.

Each stack becomes a named project and an output file in the results folder, as the preset's output settings say (a
16-bit TIFF unless they say otherwise, with the maps and aligned frames they ask for). The method is the Stack tab's.
`summary.csv` in the results folder records each stack: its frame count, method, processing time, alignment warnings
(frames that moved or were out of focus among them), project, output and any error. A stack that fails is recorded and
the batch goes on.

From the command line:

```sh
florastack-cli batch --in stacks --out results --preset builtin:insect-macro --method depthmap
```

The results folder must be outside the input, and new: existing files and an existing `summary.csv` are refused, and
nothing appears there until the batch publishes it, so a cancelled batch leaves nothing half-written. `--template`
names the output files (default `{stack}-{method}-{range}.tif`): `{stack}` is the stack's name, `{method}` the method,
`{first}` and `{last}` the first and last frames' file names, `{range}` both joined, `{date}` the processing date
(`YYYY-MM-DD`, UTC) and `{params}` the method's settings. The command exits with code 1 when any stack failed.

### An interrupted batch

Until it publishes, a batch works in a `.queue` folder inside the results folder, and records there each stack it has
finished. When the batch is interrupted (the computer crashed or lost power, the process was killed, or an error
stopped it), run it again with the same input, results folder, preset, method and template: it carries on from the
first stack it hadn't finished, and its result's `resumed` says how many it kept. Run with other settings, it refuses
and says why; `--restart` (`restart` for `batch.run`) discards the interrupted batch's work and starts again.
Cancelling a batch discards its work, as asked.

## Watch folder

A watch folder stacks a stack as it is shot: each photo that arrives in the folder is read, aligned to the ones before
and stacked into a running preview, and when no new photo has come for a while, the stack is finished at full quality
and saved. It is the way to stack while tethered, on every platform; [Capture](capture.md) shows how to point a
camera's tethering software at the folder.

In the **Capture** tab, pick the watched folder, a separate results folder and a preset, then **Watch folder and
stack**. From the command line:

```sh
florastack-cli watch capture --out results --preset builtin:insect-macro --idle 30 --json-progress
```

How it decides that a photo has arrived:

- A file must keep its size and modification time for the **settling** interval (`--settle-ms`, one second by
  default), decode, and stay unchanged while it is read. A file that is still being written is tried again later.
  Temporary names are ignored until the file is renamed with an image extension.
- The watcher wakes on the system's file events and also checks the folder regularly, so network shares and folders
  without events work too. It watches the folder's own files, not subfolders, and includes the photos already there.
- Capture waits for its first photo, then finishes once no new or changed photo has arrived for the **idle** time
  (`--idle`, 5 seconds by default), counted from the moment the latest photo's preview is ready, however long it took:
  set it longer than the time between photos, exposure and transfer included. A
  photo replaced after it was read stops the capture with an error.

When it finishes, the frames are aligned again as a whole, and the project, the stacked TIFF, a preview,
`summary.csv` and `captures.json` (which frames each capture took) are saved in the results folder (by default a
folder beside the watched one, named `<folder>-stacked`). With `--json-progress`, each update's event carries the
running preview's path and frame count (`job.preview.path`, `job.preview.frames`), for other programs to show.

### Several stacks in one session

Each watch makes one stack. To shoot stack after stack into the same folder, keep the watch armed:

```sh
florastack-cli watch capture --out results --preset builtin:insect-macro --idle 30 --captures 0
```

`--captures N` makes N stacks, one after another, each started by the next photo after the one before has finished;
`0` keeps watching until the process is stopped (Ctrl+C), which leaves the stack in progress for
the next watch to resume (below). Each stack leaves out the photos of the ones before it, is
numbered after them (`capture-capture`, `capture-capture-2`, …) and adds its row to `summary.csv`. `--append` joins a
new watch to the stacks already in a results folder in the same way (`append` for `watch.run`).

### An interrupted capture

A capture records each photo it takes in the results folder's `.queue` folder. When it is interrupted (a crash, a
power cut, a stopped process), run the watch again with the same folders, preset, method and template: it opens the
capture where it was, with the photos it had previewed, takes the ones that arrived since, and finishes once the
idle time passes. A photo changed or removed since then stops it with an error; `--restart` starts the capture
afresh instead. Stopping a watch from the Capture tab, which cancels it, discards the capture in progress.
