"""Make the user guide's before/after crops from a real stack, through the CLI.

Stacks the frames by every method (pyramid, depth map, weighted average, wavelet) in a temporary project, exports the
outputs and the frames aligned onto the outputs' canvas, then crops each output at the regions where the methods
differ most, beside the frame that is sharpest there: what one photo shows against what each method makes of the
stack.

Run after cargo build -p florastack-cli --release (a debug build works, more slowly):
    python scripts/method_crops.py corpus/local/mosquito-jpg --out book/src/images/methods
Needs NumPy and Pillow. --region X,Y crops there instead of choosing (repeatable; output pixels, the top-left corner).
The frames are read in name order. Only the crops are written to --out, as JPEG files, and a regions.json that says
where each crop is and which frame it shows; the project and the exports stay in a temporary folder.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

import numpy as np
from PIL import Image

METHODS = (("pyramid", "stack.pyramid"), ("depth-map", "stack.depthMap"), ("average", "stack.average"), ("wavelet", "stack.wavelet"))


def cli(app, temporary, *args):
    """Runs the CLI and returns its JSON results: one value per line of standard output."""
    command = [str(app), "--temporary-projects", str(temporary), *[str(a) for a in args]]
    done = subprocess.run(command, capture_output=True, text=True, encoding="utf-8")
    if done.returncode != 0:
        raise RuntimeError(f"{' '.join(command)} failed ({done.returncode}):\n{done.stderr}")
    return [json.loads(line) for line in done.stdout.splitlines() if line.strip()]


def luminance(path):
    with Image.open(path) as image:
        rgb = np.asarray(image.convert("RGB"), dtype=np.float32) / 255.0
    return rgb @ np.array([0.2126, 0.7152, 0.0722], dtype=np.float32)


def box(plane, radius):
    """Mean over a (2 radius + 1) square, by integral image, with the edges clamped."""
    padded = np.pad(plane, radius + 1, mode="edge").astype(np.float64)
    s = padded.cumsum(0).cumsum(1)
    n = 2 * radius + 1
    total = s[n:, n:] - s[:-n, n:] - s[n:, :-n] + s[:-n, :-n]
    return (total / (n * n))[: plane.shape[0], : plane.shape[1]].astype(np.float32)


def window_sums(plane, side, step):
    """The sum of `plane` over each side x side window whose corner is a multiple of `step`, by corner."""
    s = np.pad(plane.astype(np.float64), ((1, 0), (1, 0))).cumsum(0).cumsum(1)
    ys = np.arange(0, plane.shape[0] - side + 1, step)
    xs = np.arange(0, plane.shape[1] - side + 1, step)
    y, x = np.meshgrid(ys, xs, indexing="ij")
    return y, x, s[y + side, x + side] - s[y, x + side] - s[y + side, x] + s[y, x]


def choose(planes, side, count):
    """The `count` windows, without overlap, where the outputs' fine detail differs most and there is detail at all."""
    detail = [p - box(p, 6) for p in planes]
    spread = np.std(np.stack(detail), axis=0)
    texture = np.mean(np.abs(np.stack(detail)), axis=0)
    y, x, differs = window_sums(spread, side, side // 4)
    _, _, textured = window_sums(texture, side, side // 4)
    score = differs * np.sqrt(textured / max(textured.max(), 1e-9))
    chosen = []
    for i in np.argsort(score, axis=None)[::-1]:
        cy, cx = int(y.flat[i]), int(x.flat[i])
        if all(abs(cy - oy) >= side or abs(cx - ox) >= side for oy, ox in chosen):
            chosen.append((cy, cx))
        if len(chosen) == count:
            break
    return [(cx, cy) for cy, cx in chosen]


def sharpness(path, regions, side):
    """A frame's detail energy in each region: the variance of its luminance less its local mean."""
    plane = luminance(path)
    out = []
    for x, y in regions:
        crop = plane[y : y + side, x : x + side]
        out.append(float(np.mean((crop - box(crop, 3)) ** 2)))
    return out


def save_crop(path, region, side, destination):
    x, y = region
    with Image.open(path) as image:
        image.convert("RGB").crop((x, y, x + side, y + side)).save(destination, quality=92, subsampling=0, optimize=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("frames", type=Path, help="a folder of frames")
    parser.add_argument("--out", type=Path, required=True)
    exe = "florastack-cli.exe" if os.name == "nt" else "florastack-cli"
    parser.add_argument("--app", type=Path, default=Path("target/release") / exe)
    parser.add_argument("--side", type=int, default=320, help="the crops' side, in output pixels (default 320)")
    parser.add_argument("--count", type=int, default=3, help="how many regions to choose (default 3)")
    parser.add_argument("--region", action="append", default=[], help="X,Y of a region to crop instead of choosing")
    args = parser.parse_args()
    app = args.app.resolve()
    if not app.is_file():
        parser.error(f"{app} doesn't exist: build the CLI first, or name it with --app")
    args.out.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="florastack-crops-") as temporary:
        work = Path(temporary)
        cli(app, work / "temp", "project", "new", "Crops", "--folder", work)
        project = work / "Crops.florastack"
        steps = ["--cmd", "project.addFrames", "--params", json.dumps({"paths": [str(args.frames.resolve())]}),
                 "--cmd", "align.run", "--params", "{}"]
        for _, command in METHODS:
            steps += ["--cmd", command, "--params", "{}"]
        results = cli(app, work / "temp", "run", project, *steps)
        outputs = {name: results[2 + i]["result"]["output"] for i, (name, _) in enumerate(METHODS)}

        exports = []
        for name, output in outputs.items():
            settings = {"format": "png", "depth": "u8", "metadata": False, "template": name, "aligned_frames": name == "pyramid"}
            folder = work / "export" / name
            exports += ["--cmd", "export.run", "--params", json.dumps({"output": output, "folder": str(folder), "settings": settings})]
        cli(app, work / "temp", "run", project, *exports)
        images = {name: work / "export" / name / f"{name}.png" for name in outputs}
        frames = sorted(p for p in (work / "export" / "pyramid").rglob("*.tif"))
        if not frames:
            raise RuntimeError("the export wrote no aligned frames")

        side = args.side
        if args.region:
            regions = [tuple(int(v) for v in r.split(",")) for r in args.region]
        else:
            regions = choose([luminance(images[name]) for name in outputs], side, args.count)
        energy = [sharpness(frame, regions, side) for frame in frames]
        record = []
        for i, region in enumerate(regions, start=1):
            best = max(range(len(frames)), key=lambda f: energy[f][i - 1])
            save_crop(frames[best], region, side, args.out / f"r{i}-frame.jpg")
            for name in outputs:
                save_crop(images[name], region, side, args.out / f"r{i}-{name}.jpg")
            record.append({"region": i, "x": region[0], "y": region[1], "side": side, "frame": best + 1, "frames": len(frames)})
            print(f"region {i}: x {region[0]}, y {region[1]}; sharpest frame {best + 1} of {len(frames)}")
        (args.out / "regions.json").write_text(json.dumps({"stack": args.frames.name, "regions": record}, indent=2) + "\n")


if __name__ == "__main__":
    main()
