"""Compare stacked outputs of one scene, from FloraStack or any other stacker, with numbers and matched crops.

Registers every output onto the grid of the smallest one, so that an output saved at another size or crop is compared
fairly, then measures detail by frequency band, with and without matching tones (a contrast boost raises every band
without adding detail), noise on the flattest background, tone and clipping, brightness beside the subject (halos) and
colour differences. With --frames it also aligns the source frames and scores each output against the sharpest frame
of every tile, which says what detail is real. It finishes by cropping the tiles where the baseline output falls
furthest behind the others and furthest ahead of them, and any --crop regions, beside the sharpest frame.

    python scripts/compare_outputs.py corpus/local/comparison --frames corpus/local/cricket-jpg
    python scripts/compare_outputs.py FloraStack=a.jpg Helicon=b.jpg --name ladybug --note "pyramid, grit 0" --crop eye:1600,1838

Inputs are image files, Label=file pairs, or folders of images. Needs NumPy, SciPy and Pillow, and reads what Pillow
reads as 8-bit RGB, so give it JPEG or PNG outputs and JPEG frames, not camera RAW files. A 20-megapixel stack of 15
frames takes about 5 minutes and 3 GB of memory; the frames' temporary files, about 25 MB each, are deleted at the end.

Every run keeps a report of its own, report.md, metrics.json and crops-*.png in <reports>/<name>/<date-time>/, and adds
its figures to <reports>/history.md (made from history.jsonl), so that comparisons add up over time; a report also says
what moved since the stack's last run. --note says what the run is about, such as the settings: outputs don't always
record them. Reports default to corpus/comparisons/, which git ignores and cargo clean leaves alone. Keep them out of
the repository: AGENTS.md allows comparing with proprietary stackers, but their output must not be committed. Don't put
them or the outputs in corpus/local/, where a folder without a stack.toml fails cargo xtask corpus.

Reading the results:
- Outputs are read as stored, ignoring their EXIF orientation (--output-exif applies it), and frames with it applied,
  because stackers rarely agree on that tag. An error about too few matching tiles usually means the two disagree.
- Outputs bigger than the grid are filtered as they are scaled down and the smallest is not, which raises its noise
  and finest-band figures a little beside the others. Compare outputs of the same size directly.
- The "subject" is what has colour against a neutral background (--subject-chroma sets the threshold). On a colourful
  background, read the whole-image figures instead.
- Most differences between good stacks are a few percent, and one stack is one stack: confirm a small lead by eye.
"""

import argparse
from datetime import datetime
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys

import numpy as np
from PIL import Image, ImageDraw, ImageOps
from scipy import ndimage as ni

Image.MAX_IMAGE_PIXELS = None

REPOSITORY = Path(__file__).resolve().parents[1]
EXTENSIONS = {".jpg", ".jpeg", ".png", ".tif", ".tiff", ".webp", ".bmp"}
LUMA = np.array([0.2126, 0.7152, 0.0722], np.float32)
BANDS = ((0.6, 1.2), (1.2, 2.4), (2.4, 4.8), (4.8, 9.6))
HALO_BINS = ((1, 3), (3, 6), (6, 10), (10, 16), (16, 24), (24, 36), (36, 60), (60, 100))
TILE = 96
FRAME_TILE = 64
UNCOVERED = 0.5
# How far from anything an output doesn't cover the measures stay: the coarsest band's filter reads this far.
MEASURE_MARGIN = 32
# A frame far from most of the scene's focus matches in few tiles, and a loose fit would score outputs against the wrong pixels.
MAX_FRAME_RESIDUAL = 2.0


def read(path, apply_exif):
    """Reads an image as float RGB in 0..1."""
    with Image.open(path) as image:
        image = ImageOps.exif_transpose(image) if apply_exif else image
        return np.asarray(image.convert("RGB"), dtype=np.float32) / 255


def gray(rgb):
    return rgb @ LUMA


def band_pass(image, sigma=1.5):
    return ni.gaussian_filter(image, sigma) - ni.gaussian_filter(image, sigma * 6)


def detail(image):
    """The band the tile scores use: fine detail, above noise and below what resampling changes."""
    return ni.gaussian_filter(image, 0.8) - ni.gaussian_filter(image, 2.4)


def correlate(a, b):
    """The shift of b against a, and its normalized correlation.

    Plain cross-correlation of band-passed tiles: phase correlation locks onto the JPEG block grid of two images of
    different scale, which shows up as false residuals in stripes.
    """
    window = np.outer(np.hanning(a.shape[0]), np.hanning(a.shape[1])).astype(np.float32)
    a = (a - a.mean()) * window
    b = (b - b.mean()) * window
    surface = np.fft.fftshift(np.fft.irfft2(np.fft.rfft2(a).conj() * np.fft.rfft2(b), s=a.shape))
    surface /= np.sqrt((a * a).sum() * (b * b).sum()) + 1e-12
    y, x = np.unravel_index(np.argmax(surface), surface.shape)
    if not (0 < y < surface.shape[0] - 1 and 0 < x < surface.shape[1] - 1):
        return 0.0, 0.0, 0.0

    def vertex(before, peak, after):
        curve = before - 2 * peak + after
        return 0.0 if curve == 0 else 0.5 * (before - after) / curve

    dy = vertex(surface[y - 1, x], surface[y, x], surface[y + 1, x])
    dx = vertex(surface[y, x - 1], surface[y, x], surface[y, x + 1])
    return y + dy - a.shape[0] // 2, x + dx - a.shape[1] // 2, float(surface[y, x])


def tile_shifts(ref, moved, size, step, threshold, limit):
    """Tile centers and their shifts, for the tiles that match well enough."""
    points, shifts = [], []
    for y in range(0, ref.shape[0] - size + 1, step):
        for x in range(0, ref.shape[1] - size + 1, step):
            a = ref[y : y + size, x : x + size]
            if a.std() < 0.002:
                continue
            dy, dx, peak = correlate(a, moved[y : y + size, x : x + size])
            if peak > threshold and abs(dx) < limit and abs(dy) < limit:
                points.append((x + size / 2, y + size / 2))
                shifts.append((dx, dy))
    return np.array(points, float).reshape(-1, 2), np.array(shifts, float).reshape(-1, 2)


def robust_fit(design, targets, floor):
    """Least squares that drops outliers, and the fit's residuals on what it kept."""
    keep = np.ones(len(design), bool)
    for _ in range(6):
        solution, *_ = np.linalg.lstsq(design[keep], targets[keep], rcond=None)
        residual = np.hypot(*(design @ solution - targets).T)
        keep = residual < max(3 * np.median(residual[keep]), floor)
        if keep.sum() < design.shape[1]:
            keep = residual <= np.sort(residual)[design.shape[1] - 1]
    return solution, residual[keep]


def polynomial(x, y, width, height, degree):
    u = 2 * x / width - 1
    v = 2 * y / height - 1
    return np.stack([u**i * v**j for i in range(degree + 1) for j in range(degree + 1 - i)], -1)


def affine_sample(image, matrix, shape):
    yy, xx = np.mgrid[0 : shape[0], 0 : shape[1]].astype(np.float32)
    x = matrix[0, 0] * xx + matrix[0, 1] * yy + matrix[0, 2]
    y = matrix[1, 0] * xx + matrix[1, 1] * yy + matrix[1, 2]
    return ni.map_coordinates(image, [y, x], order=1, mode="nearest")


def sample(image, x, y, blur):
    """Samples a gray or RGB image at x, y, after the blur that scaling down needs."""
    if image.ndim == 2:
        source = ni.gaussian_filter(image, blur) if blur > 0 else image
        return ni.map_coordinates(source, [y, x], order=3, mode="nearest")
    return np.stack([sample(image[..., c], x, y, blur) for c in range(3)], -1)


def align(ref, image, threshold, passes):
    """Maps the reference grid into image: coarse affine at quarter size, then a smooth polynomial field.

    The field takes up what an affine can't, such as the warps a stacker applies to its frames. Returns None when too
    few tiles match, or x, y, the blur to scale down with, and the fit's statistics.
    """
    height, width = ref.shape
    size_ratio = image.shape[1] / width
    small_ref = ni.zoom(ref, 0.25, order=1)
    small = ni.zoom(image, 0.25 / size_ratio, order=1)
    target = band_pass(small_ref, 1.0)
    matrix = np.array([[1, 0, 0], [0, 1, 0], [0, 0, 1]], float)
    for _ in range(4):
        moved = band_pass(affine_sample(small, matrix, small_ref.shape), 1.0)
        points, shifts = tile_shifts(target, moved, 128, 64, 0.3, 40)
        if len(points) < 6:
            return None
        design = np.c_[points, np.ones(len(points))]
        solution, _ = robust_fit(design, points + shifts, 0.4)
        matrix = matrix @ np.vstack([solution.T, [0, 0, 1]])

    full = matrix[:2].copy()
    full[:, :2] *= size_ratio
    full[:, 2] *= size_ratio / 0.25
    scale = float(np.sqrt(abs(np.linalg.det(full[:, :2]))))
    blur = 0.6 * np.sqrt(max(scale * scale - 1, 0))

    yy, xx = np.mgrid[0:height, 0:width].astype(np.float32)
    x = full[0, 0] * xx + full[0, 1] * yy + full[0, 2]
    y = full[1, 0] * xx + full[1, 1] * yy + full[1, 2]
    target = band_pass(ref)
    gy, gx = np.mgrid[0:height:32, 0:width:32].astype(np.float32)
    tiles, residual = 0, np.zeros(1)
    for _ in range(passes):
        points, shifts = tile_shifts(target, band_pass(sample(image, x, y, blur)), 192, 96, threshold, 12)
        tiles = len(points)
        if tiles < 12:
            break
        # Out-of-focus frames match in few tiles: a high degree would swing wildly between them.
        degree = 5 if tiles >= 100 else 3 if tiles >= 40 else 1
        solution, residual = robust_fit(polynomial(*points.T, width, height, degree), shifts, 0.25)
        field = polynomial(gx.ravel(), gy.ravel(), width, height, degree) @ solution
        for axis, coordinate in enumerate((x, y)):
            grid = field[:, axis].reshape(gx.shape)
            coordinate += ni.zoom(grid, (height / gx.shape[0], width / gx.shape[1]), order=1)[:height, :width] * scale
    stats = {
        "scale": round(scale, 4),
        "tiles": tiles,
        "residualMedianPx": round(float(np.median(residual)), 3),
        "residualP95Px": round(float(np.percentile(residual, 95)), 3),
    }
    return x, y, blur, stats


def to_lab(rgb):
    c = np.where(rgb <= 0.04045, rgb / 12.92, ((rgb + 0.055) / 1.055) ** 2.4)
    matrix = np.array([[0.4124, 0.3576, 0.1805], [0.2126, 0.7152, 0.0722], [0.0193, 0.1192, 0.9505]], np.float32)
    xyz = c @ matrix.T / np.array([0.9505, 1, 1.089], np.float32)
    f = np.where(xyz > 0.008856, np.cbrt(xyz), 7.787 * xyz + 16 / 116)
    return np.stack([116 * f[..., 1] - 16, 500 * (f[..., 0] - f[..., 1]), 200 * (f[..., 1] - f[..., 2])], -1)


def tone_match(image, reference, valid):
    """Maps image's tones onto reference's by quantiles of the low-passed images, leaving detail for comparison.

    Only over valid pixels: the gray that fills what an output doesn't cover would skew that output's curve alone.
    """
    q = np.linspace(0.001, 0.999, 400)
    source = np.quantile(ni.gaussian_filter(image, 1.5)[valid][::16], q)
    target = np.quantile(ni.gaussian_filter(reference, 1.5)[valid][::16], q)
    source = np.maximum.accumulate(source + np.arange(400) * 1e-7)
    # At slope 1 beyond the quantiles: a flat end would clip the extremes' detail, even of an image matched to itself.
    source = np.concatenate(([source[0] - 1], source, [source[-1] + 1]))
    target = np.concatenate(([target[0] - 1], target, [target[-1] + 1]))
    return np.interp(image, source, target).astype(np.float32)


def tile_mean(image, size):
    h = image.shape[0] // size * size
    w = image.shape[1] // size * size
    return image[:h, :w].reshape(h // size, size, w // size, size).mean((1, 3))


def metadata(path):
    """What a file says about itself, including the settings some stackers leave in it."""
    with Image.open(path) as image:
        info = {
            "size": list(image.size),
            "orientation": image.getexif().get(274),
            "iccBytes": len(image.info.get("icc_profile", b"")),
        }
    with open(path, "rb") as file:
        text = re.findall(rb"[ -~]{6,}", file.read(400_000))
    pattern = r"(?i)method|helicon|zerene|pmax|dmap|florastack|software"
    info["notes"] = sorted({t.decode("latin1").strip()[:80] for t in text if re.search(pattern, t.decode("latin1"))})[:6]
    return info


def collect(inputs):
    """Label and path of every output."""
    items = []
    for item in inputs:
        label, separator, rest = item.partition("=")
        path = Path(rest if separator else item)
        if path.is_dir():
            items += [(None, p) for p in sorted(path.iterdir()) if p.suffix.lower() in EXTENSIONS]
        elif path.is_file():
            items.append((label if separator else None, path))
        else:
            sys.exit(f"error: no such file or folder: {path}")
    stems = [p.stem for label, p in items if label is None]
    prefix = 0
    if len(stems) > 1:
        common = stems[0]
        for stem in stems[1:]:
            while not stem.startswith(common):
                common = common[:-1]
        prefix = len(common)
    labels = [label or p.stem[prefix:] or p.stem for label, p in items]
    if len(items) < 2 or len(set(labels)) != len(labels):
        sys.exit(f"error: needs two or more outputs with different labels, got {labels}")
    return list(zip(labels, (p for _, p in items)))


def subject_masks(labs, images, chroma):
    """A coarse mask for band statistics and a tight one for distances from the subject's edge."""
    mean = np.mean([np.hypot(lab[..., 1], lab[..., 2]) for lab in labs.values()], 0)
    coarse = ni.binary_opening(ni.gaussian_filter(mean, 6) > chroma, iterations=4)
    coarse = ni.binary_dilation(coarse, iterations=8)
    spread = np.mean([np.hypot(v[..., 0] - v[..., 2], v[..., 0] - v[..., 1]) for v in images.values()], 0)
    tight = ni.binary_opening(ni.gaussian_filter(spread, 2) > 0.05, iterations=3)
    tight = ni.binary_fill_holes(ni.binary_closing(tight, iterations=6))
    parts, count = ni.label(tight)
    if count:
        sizes = ni.sum(tight, parts, range(1, count + 1))
        tight = np.isin(parts, 1 + np.where(sizes > 20000)[0])
    return coarse, tight


def measure(images, grays, valid, baseline, chroma):
    labs = {k: to_lab(v) for k, v in images.items()}
    subject, tight = subject_masks(labs, images, chroma)
    on, off = subject & valid, ~subject & valid
    if not on.any() or not off.any():
        on = off = valid
    matched = {k: g if k == baseline else tone_match(g, grays[baseline], valid) for k, g in grays.items()}
    result = {"subjectFraction": round(float(subject.mean()), 3), "outputs": {}, "pairs": {}}

    def bands(g):
        out = []
        for low, high in BANDS:
            b = ni.gaussian_filter(g, low) - ni.gaussian_filter(g, high)
            out.append({"subject": round(1000 * float(b[on].std()), 2), "background": round(1000 * float(b[off].std()), 2)})
        return out

    distance = ni.distance_transform_edt(~tight).astype(np.float32)
    subject_tiles = tile_mean(subject.astype(np.float32), TILE) > 0.5
    covered_tiles = tile_mean(valid.astype(np.float32), TILE) > 0.99
    for k, g in grays.items():
        lab = labs[k]
        fine = g - ni.gaussian_filter(g, 1)
        energy = np.sqrt(tile_mean(fine * fine, TILE))[~subject_tiles & covered_tiles]
        half = ni.laplace(ni.zoom(ni.gaussian_filter(matched[k], 1.0), 0.5, order=1))
        half_valid = ni.zoom(valid.astype(np.float32), 0.5, order=1) > 0.9
        half_subject = (ni.zoom(subject.astype(np.float32), 0.5, order=1) > 0.5) & half_valid
        result["outputs"][k] = {
            "bands": bands(g),
            "bandsToneMatched": bands(matched[k]),
            "halfSizeDetail": round(1000 * float(half[half_subject if half_subject.any() else half_valid].std()), 2),
            "noiseFloor": round(1000 * float(np.percentile(energy, 5)), 3) if energy.size else None,
            "meanLab": [round(float(c), 2) for c in lab[on].mean(0)],
            "lightness": [round(float(c), 1) for c in np.percentile(lab[..., 0][valid], [0.5, 99.5])],
            "nearBlackPercent": round(100 * float((images[k].max(-1)[valid] <= 2 / 255).mean()), 3),
            "nearWhitePercent": round(100 * float((images[k].min(-1)[valid] >= 253 / 255).mean()), 3),
            "lumaByDistance": [round(255 * float(g[(distance >= a) & (distance < b) & valid].mean()), 2) for a, b in HALO_BINS],
        }
    energy = {k: np.sqrt(tile_mean(detail(g) ** 2, TILE)) for k, g in matched.items()}
    names = list(grays)
    for i, a in enumerate(names):
        for b in names[i + 1 :]:
            delta = np.sqrt(((ni.gaussian_filter(labs[a], (8, 8, 0)) - ni.gaussian_filter(labs[b], (8, 8, 0))) ** 2).sum(-1))
            ratio = energy[a][covered_tiles] / energy[b][covered_tiles]
            result["pairs"][f"{a} / {b}"] = {
                "deltaEMean": round(float(delta[valid].mean()), 2),
                "deltaEP95": round(float(np.percentile(delta[valid], 95)), 2),
                "detailRatioMedian": round(float(np.median(ratio)), 3),
            }
    others = [e for k, e in energy.items() if k != baseline]
    behind = np.log2(energy[baseline] / np.maximum.reduce(others))
    ahead = np.log2(energy[baseline] / np.minimum.reduce(others))
    behind[~covered_tiles] = 0
    ahead[~covered_tiles] = 0
    result["baselineAgainstBest"] = {
        "medianRatio": round(float(2 ** np.median(behind[covered_tiles])), 3),
        "tilesBelow0.8Percent": round(100 * float((2 ** behind[covered_tiles] < 0.8).mean()), 2),
    }
    return result, subject, behind, ahead


def spaced_tiles(score, count):
    """The tiles of the highest score, apart from one another."""
    picks = []
    for index in np.argsort(-score.ravel()):
        y, x = divmod(int(index), score.shape[1])
        if all(abs(y - a) + abs(x - b) > 3 for a, b in picks):
            picks.append((y, x))
        if len(picks) == count:
            break
    return [(x * TILE + TILE // 2, y * TILE + TILE // 2) for y, x in picks]


class Frames:
    """The source frames on the grid, as half-float files on disk: fifteen in memory would not fit on a small machine."""

    def __init__(self, folder, ref, work):
        self.paths, self.names, self.stats = [], [], []
        files = sorted(p for p in Path(folder).iterdir() if p.suffix.lower() in EXTENSIONS)
        if not files:
            sys.exit(f"error: no frames in {folder}")
        work.mkdir(parents=True, exist_ok=True)
        for i, path in enumerate(files):
            image = gray(read(path, True))
            aligned = align(ref, image, 0.3, 2)
            if aligned is None:
                print(f"  {path.name}: too few matching tiles, left out")
                continue
            x, y, blur, stats = aligned
            if stats["residualMedianPx"] > MAX_FRAME_RESIDUAL:
                print(f"  {path.name}: aligned too loosely to be a reference, left out: {stats}")
                continue
            target = work / f"{i:03}.npy"
            np.save(target, sample(image, x, y, blur).astype(np.float16))
            self.paths.append(target)
            self.names.append(path.name)
            self.stats.append({"frame": path.name, **stats})
            print(f"  {path.name}: {stats}", flush=True)
        if not self.paths:
            sys.exit("error: no frame matched the outputs: do the frames and outputs share an orientation? (--output-exif)")

    def load(self, index):
        return np.load(self.paths[index]).astype(np.float32)

    def sharpest(self, x0, y0, size):
        """The index of the frame sharpest in the middle of a region (its corners may be another subject)."""
        inner = size // 4
        region = np.s_[y0 + inner : y0 + size - inner, x0 + inner : x0 + size - inner]
        return int(np.argmax([detail(np.load(p, mmap_mode="r")[region].astype(np.float32)).std() for p in self.paths]))


def score_frames(frames, grays, valid, subject):
    """Each output's detail against the frame that is sharpest in each tile, after each tile's own small shift."""
    size = FRAME_TILE
    energy = np.array([np.sqrt(tile_mean(detail(frames.load(i)) ** 2, size)) for i in range(len(frames.paths))])
    best = np.argmax(energy, 0)
    runner_up = np.sort(energy, 0)[-2] if len(energy) > 1 else energy[0] * 0
    peaked = np.take_along_axis(energy, best[None], 0)[0] > 1.25 * runner_up
    passed = {k: detail(g) for k, g in grays.items()}
    correlation = {k: np.zeros(best.shape, np.float32) for k in grays}
    kept = {k: np.zeros(best.shape, np.float32) for k in grays}
    height, width = valid.shape
    for i in range(len(frames.paths)):
        frame = detail(frames.load(i))
        for ty, tx in zip(*np.where(best == i)):
            y0, x0 = max(ty * size - 16, 0), max(tx * size - 16, 0)
            y1, x1 = min(ty * size + size + 16, height), min(tx * size + size + 16, width)
            a = frame[y0:y1, x0:x1]
            for k, image in passed.items():
                b = image[y0:y1, x0:x1]
                dy, dx, _ = correlate(a, b)
                if abs(dy) > 3 or abs(dx) > 3:
                    dy = dx = 0
                shifted = ni.shift(b, (-dy, -dx), order=1, mode="nearest")[4:-4, 4:-4].ravel()
                truth = a[4:-4, 4:-4].ravel()
                if truth.std() > 0 and shifted.std() > 0:
                    correlation[k][ty, tx] = np.corrcoef(truth, shifted)[0, 1]
                kept[k][ty, tx] = shifted.std() / (truth.std() + 1e-9)
    covered = tile_mean(valid.astype(np.float32), size) > 0.99
    on_subject = tile_mean(subject.astype(np.float32), size) > 0.5
    groups = {"all": covered, "subject": covered & on_subject, "background": covered & ~on_subject, "inFocusTiles": covered & peaked}
    result = {"sharpestFrameTiles": dict(zip(frames.names, np.bincount(best.ravel(), minlength=len(frames.paths)).tolist()))}
    for name, mask in groups.items():
        if mask.any():
            result[name] = {
                k: {
                    "correlation": round(float(np.median(correlation[k][mask])), 3),
                    "correlationP10": round(float(np.percentile(correlation[k][mask], 10)), 3),
                    "detailKept": round(float(np.median(kept[k][mask])), 3),
                }
                for k in grays
            }
    if covered.any() and peaked[covered].any():
        wins = np.argmax(np.stack([correlation[k] for k in grays]), 0)[covered & peaked]
        result["closestToFrames"] = dict(zip(grays, np.bincount(wins, minlength=len(grays)).tolist()))
    return result


def crop_sheet(path, regions, images, holes, frames, size=300, zoom=1):
    """Rows of matched crops, one per region, the outputs and then the sharpest frame in gray.

    `holes` says what each output doesn't cover, which shows gray here and nowhere else: painted into the images, its
    edge would reach the measures through the band filters.
    """
    columns = len(images) + (1 if frames else 0)
    cell = size * zoom
    sheet = Image.new("RGB", (columns * (cell + 10), len(regions) * (cell + 20)), "white")
    draw = ImageDraw.Draw(sheet)
    height, width = next(iter(images.values())).shape[:2]
    for row, (label, cx, cy) in enumerate(regions):
        x0 = min(max(cx - size // 2, 0), width - size)
        y0 = min(max(cy - size // 2, 0), height - size)
        crop = np.s_[y0 : y0 + size, x0 : x0 + size]
        cells = [(k, np.where(holes[k][crop][..., None], UNCOVERED, v[crop]) if k in holes else v[crop]) for k, v in images.items()]
        if frames:
            index = frames.sharpest(x0, y0, size)
            shot = frames.load(index)[crop]
            reference = gray(next(iter(images.values()))[crop])
            shot = shot * (np.median(reference) / max(float(np.median(shot)), 1e-3))
            cells.append((f"{frames.names[index]} (gray)", np.repeat(shot[..., None], 3, 2)))
        for column, (name, pixels) in enumerate(cells):
            tile = Image.fromarray((np.clip(pixels, 0, 1) * 255).astype(np.uint8))
            tile = tile.resize((cell, cell), Image.LANCZOS) if zoom != 1 else tile
            sheet.paste(tile, (column * (cell + 10), row * (cell + 20) + 16))
            draw.text((column * (cell + 10) + 2, row * (cell + 20) + 2), f"{name} - {label} @{cx},{cy}", fill="black")
    sheet.save(path)


def table(header, rows):
    lines = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    return lines + ["| " + " | ".join(str(c) for c in row) + " |" for row in rows]


def report(path, data, baseline):
    outputs = data["metrics"]["outputs"]
    names = list(outputs)
    lines = [f"# {data['name']}: {', '.join(names)}", "", f"{data['date']}, script at {data['commit']}.", ""]
    lines += [f"Note: {data['note']}", ""] if data["note"] else []
    lines += [f"Grid: {data['grid'][0]}x{data['grid'][1]}, from {data['gridOutput']}. Baseline: {baseline}.", ""]
    lines += ["Keep this report out of the repository (AGENTS.md).", ""]
    lines += changes(data)
    lines += ["## Files", ""]
    rows = [[k, "x".join(map(str, v["size"])), v["orientation"], v["iccBytes"], "; ".join(v["notes"])] for k, v in data["files"].items()]
    lines += table(["Output", "Size", "Orientation", "ICC bytes", "Notes in the file"], rows)
    lines += ["", "## Registration", ""]
    rows = [[k, v["scale"], v["tiles"], v["residualMedianPx"], v["residualP95Px"]] for k, v in data["registration"].items()]
    lines += table(["Output", "Scale", "Tiles", "Residual median (px)", "p95 (px)"], rows)
    lines += ["", "## Measures", ""]
    rows = []
    for title, get in (
        ("Finest band, subject (x1000)", lambda o: o["bands"][0]["subject"]),
        ("Finest band, subject, tone-matched", lambda o: o["bandsToneMatched"][0]["subject"]),
        ("Finest band, background, tone-matched", lambda o: o["bandsToneMatched"][0]["background"]),
        ("Coarsest band, subject (contrast, not detail)", lambda o: o["bands"][-1]["subject"]),
        ("Half-size detail, tone-matched", lambda o: o["halfSizeDetail"]),
        ("Noise floor, flattest background (lower is cleaner)", lambda o: o["noiseFloor"]),
        ("Lightness p0.5 / p99.5", lambda o: " / ".join(map(str, o["lightness"]))),
        ("Near black %", lambda o: o["nearBlackPercent"]),
        ("Near white %", lambda o: o["nearWhitePercent"]),
    ):
        rows.append([title] + [get(outputs[k]) for k in names])
    lines += table(["Measure"] + names, rows)
    lines += ["", "## Pairs", ""]
    rows = [[k, v["detailRatioMedian"], v["deltaEMean"], v["deltaEP95"]] for k, v in data["metrics"]["pairs"].items()]
    lines += table(["Pair", "Tile detail ratio, tone-matched", "Mean dE76 (low-passed)", "p95"], rows)
    against = data["metrics"]["baselineAgainstBest"]
    lines += ["", f"{baseline} against the better of the others, tile by tile: median {against['medianRatio']}x, "]
    lines[-1] += f"{against['tilesBelow0.8Percent']}% of tiles below 0.8x."
    lines += ["", "## Brightness beside the subject", "", "Mean luma (0-255) by distance outside the subject's edge. A halo"]
    lines[-1] += " is a step near the edge that the others lack; an even offset is tone.\n"
    bins = [f"{a}-{b}px" for a, b in HALO_BINS]
    lines += table(["Output"] + bins, [[k] + outputs[k]["lumaByDistance"] for k in names])
    frames = data.get("frames")
    if frames:
        lines += ["", "## Against the source frames", "", "Median correlation of fine detail with each tile's sharpest frame, "]
        lines[-1] += "and the detail kept against that frame. Detail kept isn't tone-matched, so a contrast boost raises it.\n"
        rows = []
        for group in ("all", "subject", "background", "inFocusTiles"):
            if group in frames:
                rows.append([group] + [f"{frames[group][k]['correlation']} / {frames[group][k]['detailKept']}" for k in names])
        lines += table(["Tiles"] + names, rows)
        if "closestToFrames" in frames:
            lines += ["", f"Closest to the frames, in-focus tiles won: {frames['closestToFrames']}."]
        lines += ["", f"Tiles each frame was sharpest in: {frames['sharpestFrameTiles']}."]
    lines += ["", "## Crops", "", "Gray in a crop is where that output has no pixels. Frame crops are matched in brightness only."]
    lines += ["", *[f"- `{name}`" for name in data["crops"]], ""]
    path.write_text("\n".join(lines), encoding="utf-8")


def summary(data):
    """A run's line in the history: what's worth following from one comparison to the next."""
    frames = data.get("frames", {}).get("all", {})
    outputs = {}
    for label, output in data["metrics"]["outputs"].items():
        outputs[label] = {
            "detail": output["bandsToneMatched"][0]["subject"],
            "halfSizeDetail": output["halfSizeDetail"],
            "noise": output["noiseFloor"],
            "frameMatch": frames.get(label, {}).get("correlation"),
            "nearBlackPercent": output["nearBlackPercent"],
        }
    keys = ("date", "name", "note", "commit", "baseline", "report")
    return {**{k: data[k] for k in keys}, "outputs": outputs, "againstBest": data["metrics"]["baselineAgainstBest"]["medianRatio"]}


def read_history(path):
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def changes(data):
    """What moved since this stack's last run, for the outputs both have. Lines for the report."""
    before, now = data.get("previous"), summary(data)
    if not before:
        return ["## Since the last run", "", "This stack's first run.", ""]
    rows = []
    for label, output in now["outputs"].items():
        old = before["outputs"].get(label)
        if old:
            cells = []
            for key in ("detail", "halfSizeDetail", "noise", "frameMatch"):
                a, b = old.get(key), output.get(key)
                cells.append(f"{a} -> {b} ({100 * (b / a - 1):+.1f}%)" if a and b is not None else "-")
            rows.append([label] + cells)
    lines = ["## Since the last run", "", f"Against {before['date']}" + (f" ({before['note']})." if before["note"] else "."), ""]
    if not rows:
        return lines + ["No output has the same label as in that run.", ""]
    return lines + table(["Output", "Detail", "Half-size detail", "Noise", "Match to frames"], rows) + [""]


def write_history(root, entry):
    """Adds a run to history.jsonl and writes history.md again from it, a table per stack, latest run first."""
    source = root / "history.jsonl"
    with open(source, "a", encoding="utf-8") as file:
        file.write(json.dumps(entry) + "\n")
    runs = read_history(source)
    lines = ["# Stack comparisons", "", "Written by scripts/compare_outputs.py from history.jsonl. Bold marks each run's baseline."]
    lines += ["Against best: the baseline's fine detail, tile by tile, over the better of the others.", ""]
    keys = ("detail", "halfSizeDetail", "noise", "frameMatch", "nearBlackPercent")
    for name in sorted({run["name"] for run in runs}):
        rows = []
        for run in sorted((r for r in runs if r["name"] == name), key=lambda r: r["date"], reverse=True):
            for index, (label, output) in enumerate(run["outputs"].items()):
                shown = f"**{label}**" if label == run["baseline"] else label
                date, note, best = (run["date"], run["note"], run["againstBest"]) if index == 0 else ("", "", "")
                link = f"[report]({run['report']})" if index == 0 else ""
                rows.append([date, note, shown, *[output[k] if output[k] is not None else "-" for k in keys], best, link])
        header = ["Date", "Note", "Output", "Detail", "Half-size", "Noise", "Match to frames", "Near black %", "Against best", ""]
        lines += [f"## {name}", ""] + table(header, rows) + [""]
    (root / "history.md").write_text("\n".join(lines), encoding="utf-8")


def script_commit():
    """The repository's commit, so that figures from an older script can be told apart."""
    done = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=REPOSITORY, capture_output=True, text=True, check=False)
    return done.stdout.strip() or "unknown"


def parse_crop(text):
    match = re.fullmatch(r"(?:(.+):)?(\d+),(\d+)", text)
    if not match:
        raise argparse.ArgumentTypeError(f"expected [LABEL:]X,Y, got {text}")
    return match[1] or "region", int(match[2]), int(match[3])


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("inputs", nargs="+", help="output images, Label=image pairs or folders of them")
    parser.add_argument("--frames", help="folder of the source frames, to score the outputs against what was shot")
    parser.add_argument("--name", help="the stack, which groups its runs (default: the frames' folder, or the outputs')")
    parser.add_argument("--note", default="", help="what this run is about, such as the settings the outputs were made with")
    parser.add_argument("--reports", type=Path, default=REPOSITORY / "corpus" / "comparisons", help="where reports and the history go")
    parser.add_argument("--baseline", help="the output to look for weaknesses in (default: FloraStack's, or the first)")
    parser.add_argument("--crop", type=parse_crop, action="append", default=[], metavar="[LABEL:]X,Y", help="also crop here, in grid pixels")
    parser.add_argument("--output-exif", action="store_true", help="apply the outputs' EXIF orientation")
    parser.add_argument("--subject-chroma", type=float, default=9, help="Lab chroma above which a pixel is subject")
    args = parser.parse_args()

    items = collect(args.inputs)
    labels = [label for label, _ in items]
    baseline = args.baseline or next((k for k in labels if "flora" in k.lower()), labels[0])
    if baseline not in labels:
        sys.exit(f"error: no output is labelled {baseline}: {labels}")
    name = args.name or (Path(args.frames).resolve().name if args.frames else items[0][1].resolve().parent.name)
    now = datetime.now()
    out = args.reports / name / now.strftime("%Y-%m-%d-%H%M%S")
    out.mkdir(parents=True, exist_ok=True)
    print(f"comparing {labels}, baseline {baseline}, into {out}")

    raw = {label: read(path, args.output_exif) for label, path in items}
    grid_label = min(raw, key=lambda k: raw[k].shape[0] * raw[k].shape[1])
    ref = gray(raw[grid_label])
    height, width = ref.shape
    images, registration, holes = {}, {}, {}
    valid = np.ones((height, width), bool)
    valid[:8], valid[-8:], valid[:, :8], valid[:, -8:] = False, False, False, False
    for label, image in raw.items():
        if label == grid_label:
            images[label] = image
            registration[label] = {"scale": 1.0, "tiles": 0, "residualMedianPx": 0.0, "residualP95Px": 0.0}
            continue
        aligned = align(ref, gray(image), 0.5, 3)
        if aligned is None:
            sys.exit(f"error: too few matching tiles between {label} and {grid_label}: same scene? same orientation? (--output-exif)")
        x, y, blur, stats = aligned
        images[label] = np.clip(sample(image, x, y, blur), 0, 1)
        inside = (x >= 8) & (y >= 8) & (x <= image.shape[1] - 9) & (y <= image.shape[0] - 9)
        holes[label] = ~inside
        valid &= inside
        registration[label] = stats
        print(f"  {label}: {stats}", flush=True)
    del raw
    valid = ni.minimum_filter(valid.astype(np.uint8), size=2 * MEASURE_MARGIN + 1).astype(bool)
    images = {label: images[label] for label in labels}
    grays = {k: gray(v) for k, v in images.items()}

    metrics, subject, behind, ahead = measure(images, grays, valid, baseline, args.subject_chroma)
    earlier = [run for run in read_history(args.reports / "history.jsonl") if run["name"] == name]
    data = {
        "name": name,
        "date": now.strftime("%Y-%m-%d %H:%M"),
        "note": args.note,
        "commit": script_commit(),
        "report": (out / "report.md").relative_to(args.reports).as_posix(),
        "previous": max(earlier, key=lambda run: run["date"]) if earlier else None,
        "grid": [width, height],
        "gridOutput": grid_label,
        "baseline": baseline,
        "files": {label: metadata(path) for label, path in items},
        "registration": registration,
        "metrics": metrics,
    }

    frames = None
    work = out / "frames.tmp"
    try:
        if args.frames:
            print("aligning the frames")
            frames = Frames(args.frames, ref, work)
            data["frameRegistration"] = frames.stats
            print("scoring against the frames")
            data["frames"] = score_frames(frames, grays, valid, subject)
        sheets = {
            "crops-behind.png": [("behind", x, y) for x, y in spaced_tiles(-behind, 6)],
            "crops-ahead.png": [("ahead", x, y) for x, y in spaced_tiles(ahead, 4)],
        }
        if args.crop:
            sheets["crops-chosen.png"] = args.crop
        for file, regions in sheets.items():
            crop_sheet(out / file, regions, images, holes, frames)
        data["crops"] = list(sheets)
    finally:
        shutil.rmtree(work, ignore_errors=True)

    (out / "metrics.json").write_text(json.dumps(data, indent=2), encoding="utf-8")
    report(out / "report.md", data, baseline)
    write_history(args.reports, summary(data))
    print(f"wrote {out / 'report.md'} and {args.reports / 'history.md'}")


if __name__ == "__main__":
    main()
