"""Focus stacking with owned, linear float32 NumPy outputs."""

import json
import os

from ._native import Engine as _Engine, __version__


class Project:
    """An isolated engine and temporary project; use as a context manager to close it promptly.

    Processing uses the CPU by default. ``run`` exposes every registry command,
    including compute.set for an explicit GPU choice. Paths are resolved by Rust.
    """

    def __init__(self):
        self._engine = _Engine()

    def run(self, command, params=None):
        """Run a registry command to completion, raising RuntimeError on engine errors."""
        return json.loads(self._engine.run(command, json.dumps({} if params is None else params, allow_nan=False)))

    def load_frames(self, paths):
        """Load image/RAW paths; return added IDs and skipped files with their reasons."""
        return self.run("project.addFrames", {"paths": [os.fsdecode(os.fspath(path)) for path in paths]})

    def align(self, **settings):
        """Align loaded frames. Optional settings use the align.set schema."""
        if settings:
            self.run("align.set", settings)
        return self.run("align.run")

    def stack(self, method="pyramid", **settings):
        """Stack aligned frames and return an owned (H, W, C) float32 array.

        Methods are average, pyramid and depth-map. Channels are RGB (RGBA for
        wide framing with uncovered pixels). Values are linear in the output's
        recorded primaries; no display curve or clipping is applied.
        """
        commands = {"average": "stack.average", "pyramid": "stack.pyramid", "depth-map": "stack.depthMap"}
        if method not in commands:
            raise ValueError("method must be average, pyramid or depth-map")
        result = self.run(commands[method], settings)
        return self.output(result["output"])

    def output(self, output_id):
        """Read a stacked output; the array remains valid after close()."""
        return self._engine.output(output_id)

    def close(self):
        self._engine.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()
