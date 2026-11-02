"""Acceptance through the installed maturin extension, with no image-library dependency."""
import struct
import zlib

import numpy as np
import pytest

from florastack import Project


def png(path, image):
    def chunk(name, data):
        return struct.pack(">I", len(data)) + name + data + struct.pack(">I", zlib.crc32(name + data))

    height, width, channels = image.shape
    assert channels == 3
    rows = b"".join(b"\0" + row.tobytes() for row in image)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
                     + chunk(b"sRGB", b"\0") + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


@pytest.fixture
def frames(tmp_path):
    y, x = np.indices((72, 96))
    paths = []
    for focus in range(4):
        texture = (x * 7 + y * 13 + x * y % 29) % 60
        detail = np.where(x // 24 == focus, (x + y) % 2 * 65, 32)
        sample = 60 + texture + detail
        image = np.stack((sample, sample + 7, sample - 9), axis=2).astype(np.uint8)
        path = tmp_path / f"focus {focus}.png"
        png(path, image)
        paths.append(path)
    return paths


@pytest.mark.parametrize("method", ["average", "pyramid", "depth-map"])
def test_synthetic_stack_shape_and_array_ownership(frames, method):
    with Project() as project:
        project.load_frames(frames)
        # Disable geometric correction to make the expected output dimensions exact.
        project.align(shift_x=False, shift_y=False, scale=False, rotation=False)
        result = project.stack(method)
        assert result.shape == (72, 96, 3)
        assert result.dtype == np.float32
        assert result.flags.c_contiguous
        assert np.isfinite(result).all()
        assert result.max() > 0.1
        saved = result.copy()
    # The ndarray owns the Rust allocation after the engine and its cache are gone.
    np.testing.assert_array_equal(result, saved)
    result[0, 0] = 0
    with pytest.raises(RuntimeError, match="closed"):
        project.run("project.info")


def test_failures_raise_exceptions(frames):
    with Project() as project:
        with pytest.raises(RuntimeError):
            project.stack()
        report = project.load_frames([frames[0].with_name("missing.png")])
        assert report["added"] == []
        assert len(report["skipped"]) == 1
        with pytest.raises(RuntimeError):
            project.run("command.thatDoesNotExist")
        with pytest.raises(RuntimeError):
            project.run("align.set", {"unknown": True})
        with pytest.raises(ValueError):
            project.stack("unknown")
