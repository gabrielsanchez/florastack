# GPU, memory and the cache

## The GPU

FloraStack stacks on the graphics card when it has one that computes what the CPU does, and on the CPU otherwise. When
it starts stacking for the first time, it picks the best adapter (through Vulkan, Metal or DirectX 12), compiles its
kernels and runs a self-test against the CPU's results; a card that fails is left alone. Nothing needs installing
beyond the graphics driver. The status line, at the bottom right of the window, says which device stacks: `GPU:` and
the card's name, or `CPU`.

The GPU's results agree with the CPU's within small, tested tolerances, not bit for bit. A GPU that fails part way
through a stack makes the stack start again on the CPU.

**File > Preferences > Processing** chooses:

- **The GPU when it passes its test, otherwise the CPU** (the default).
- **The CPU only**: when a driver misbehaves, or for results that are the same on every machine.
- **Any GPU, software ones too**: software renderers such as Mesa's lavapipe and Windows' WARP compute correctly, but
  more slowly than the CPU; for testing.

**Test the GPU** runs the self-test again, after a driver update say. From the command line, `--device auto|cpu|gpu`
does the same, and `compute.info` says which device was chosen, and why the CPU stacks when it does.

How much faster the GPU is depends on the card and the method. On a laptop RTX 3060 against 16 CPU threads, 100 frames
of 24 megapixels took 17 s rather than 94 s by weighted average, 19 s rather than 238 s by pyramid, and 31 s rather
than 112 s by depth map.

## Memory

Stacking reads one frame at a time and keeps only the method's running totals, so a stack of hundreds of frames needs
no more memory than one of ten. The totals for a large canvas still take room: FloraStack keeps them in memory up to a
budget and moves the rest to disk, tile by tile, beyond it. Large stacks then never run out of memory; they only slow
down.

**File > Preferences > Memory** sets the budget. The suggestion, a quarter of the memory installed, leaves the rest
for the system and your other programs. Raise it on a machine dedicated to stacking; lower it if the computer becomes
sluggish while a stack runs. On the GPU, the totals live in the card's memory, within a budget of their own, and move to
the computer's memory beyond it.

## The cache and temporary files

- **Temporary projects**, made when you drop photos on the window without naming a project, live in the app's data
  folder (on Windows under `%LOCALAPPDATA%`). After 30 days unused, FloraStack offers to move them to the trash.
  `florastack-cli project clean-orphans` does the same from the command line (`--dry-run` lists them first).
- **Stacking's work on disk**, the tiles beyond the memory budget, goes to the system's temporary folder, and is
  removed when the stack ends. A stack that crashed leaves its folder behind: **Clean up** in the preferences removes
  those.
- **A project's cache**, in its `cache/` and `previews/` folders, keeps developed frames, previews and results, so that
  nothing is computed twice. It is cleaned of what the project no longer uses when the project closes.

**File > Preferences > Cache** shows how much each takes and can move temporary projects and stacking's work to
another folder: a fast local drive with room to spare is best.
