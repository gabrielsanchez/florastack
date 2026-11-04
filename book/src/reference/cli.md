# The command-line tool

`florastack-cli` does from a terminal or a script what the desktop app does: it aligns, stacks and exports, runs
batches and watch folders, and runs any [engine command](commands.md) on a project. Every subcommand has `--help` with
examples.

```text
florastack-cli [OPTIONS] <COMMAND>

  stack       Align, stack and export files, or the image files directly in a folder
  batch       Stack every stack in a folder, one saved project per stack
  watch       Stack a stack as it is shot into a folder, then finish it once it goes quiet
  run         Open a project and run engine commands on it, in order
  reproduce   Make an export again from its sidecar, checking the inputs' hashes
  commands    Print every engine command, with its help and JSON schemas, as JSON
  project     Create, inspect and clean up projects
  mcp         Serve every engine command as an MCP tool over standard input and output
```

Options for every subcommand:

| Option | |
|---|---|
| `--device auto\|cpu\|gpu` | Which device stacks: `auto` (the default) takes the GPU when a hardware adapter passes its self-test and the CPU otherwise; `gpu` takes software adapters too ([GPU, memory and the cache](../user/performance.md)) |
| `--temporary-projects FOLDER` | Where unnamed projects go (by default a folder named after the app ID in the system's temporary folder) |
| `--json-progress` | Progress, warnings and errors as JSON lines on standard error, for other programs to read |

Results go to standard output as JSON. Progress goes to standard error: a bar on a terminal, stage messages when
redirected, or the engine's events as JSON lines with `--json-progress`.

## stack

```sh
florastack-cli stack photos --out result.tif
florastack-cli stack frame1.tif frame2.tif --method depthmap --out depthmap.tif --depth 32f
florastack-cli stack photos --method both --out result.tif --crop common
florastack-cli stack photos --slab 15:5 --align shift,scale,rotation,brightness --preset macro.toml --raw-wb asshot --dust-mask dust.png --out result.tif
florastack-cli stack photos --out result.tif --json-progress 2> progress.jsonl
```

Inputs are files in focus order, or folders whose image and RAW files are read in name order. An unreadable or
duplicate input stops it with a diagnostic.

| Option | |
|---|---|
| `--out FILE` | The TIFF to write; an existing file is refused |
| `--method` | `pyramid` (the default), `depthmap`, `average`, or `both`, which writes `<name>-pyramid.tif` and `<name>-depthmap.tif` |
| `--depth 16\|32f` | 16-bit (the default) or 32-bit float samples |
| `--preset PRESET` | A preset file, or a built-in one as `builtin:insect-macro` (`product-shot`, `landscape`, `microscope`), applied first; the options below override it |
| `--align TERMS` | The terms to correct, from `shift`, `scale`, `rotation`, `brightness` |
| `--crop narrow\|wide\|common` | The framing |
| `--slab SIZE:OVERLAP` | [Stack in slabs](../user/slabbing.md) of SIZE frames sharing OVERLAP, by the method for both stages |
| `--raw-wb asshot` | The camera's white balance for the whole RAW stack |
| `--dust-mask FILE` | A defect mask: black spots on white, at the frames' size |

`stack` saves an unnamed project with the alignment and outputs, and its result names it. Run with files and nothing
else, as when photos are dragged onto the executable in Windows Explorer, it stacks them with the default settings
into `<first file>-stacked.tif`, beside the first file.

## batch and watch

```sh
florastack-cli batch --in stacks --out results --preset builtin:insect-macro --method depthmap
florastack-cli watch capture --out results --preset macro.toml --idle 10 --json-progress
```

| Option | |
|---|---|
| `--preset PRESET` | Required: a preset file or `builtin:<id>`, whose output settings say what is written |
| `--method` | `pyramid` (the default), `depthmap` or `average` |
| `--template` | The output file's name: `{stack}`, `{method}`, `{range}`, `{first}`, `{last}`, `{date}` (UTC), `{params}` (default `{stack}-{method}-{range}.tif`) |
| `--in FOLDER` (batch) | A folder of stack folders, or one folder of frames to split into stacks |
| `--out FOLDER` | Where the outputs, projects and `summary.csv` go; outside the input, and new (`watch` defaults to `<folder>-stacked`) |
| `--idle SECONDS` (watch) | Finish after this long without a new or changed file (default 5) |
| `--settle-ms MS` (watch) | How long a file must stay unchanged before it is read (default 1000) |
| `--poll-ms MS` (watch) | The longest time between scans of the folder, besides the system's file events (default 250) |

[Batch and watch folders](../user/batch-and-watch.md) explains both.

## run

```sh
florastack-cli project new Bee --folder .
florastack-cli run Bee.florastack --cmd project.addFrames --params '{"paths":["photos"]}' --cmd align.run --params '{}' --cmd stack.pyramid --params '{}'
florastack-cli run Bee.florastack --cmd project.info
```

Opens a project and runs [engine commands](commands.md) on it in the order given, waiting for each job, and prints one
JSON result per line. The first `--params` goes with the first `--cmd`, and so on; give one JSON object per command,
or none at all for `{}` each. It stops at the first error. The project is saved after every command has succeeded:
files written outside the project before an error stay, and the project's changes since its last save are not saved.

The examples use quoting that Bash and PowerShell accept; in `cmd.exe`, escape the JSON's quotes as that shell needs.

## commands, reproduce and mcp

- `florastack-cli commands > commands.json` prints the whole registry: every command's name, help, examples, flags
  and JSON schemas.
- `florastack-cli reproduce result.tif.json --out reproduced` makes an export again from its JSON sidecar, after
  checking that the original inputs are unchanged ([Scientific exports](../user/scientific-exports.md)).
- `florastack-cli mcp` serves the engine's commands as MCP tools; `--connect HOST:PORT --token-file FILE` bridges to a
  running desktop app instead ([Automation, MCP and Python](../user/automation.md)).

## project

```sh
florastack-cli project new Bee --folder photos      # a named project, Bee.florastack, in photos/
florastack-cli project new                          # an unnamed, temporary one
florastack-cli project info Bee.florastack          # describe it, without locking it
florastack-cli project clean-orphans --dry-run      # list unnamed projects unused for 30 days
florastack-cli project clean-orphans --days 30      # move them to the trash
```

Named projects are never cleaned up.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success, including `--help` and `--version` |
| 1 | A processing, project, file or output failure (in `batch`, any stack that failed) |
| 2 | Invalid arguments, an unknown command, invalid JSON or invalid command parameters |
| 130 | A job was cancelled |
