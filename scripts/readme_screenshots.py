"""Render README screenshots through the authenticated control channel, without opening a window.

Run after cargo build -p florastack: python scripts/readme_screenshots.py
NumPy is needed only for the original synthetic demo; --frames accepts an existing stack instead.
"""

import argparse
import json
import os
from pathlib import Path
import queue
import secrets
import socket
import struct
import subprocess
import tempfile
import threading
import time
import zlib


def demo(folder):
    import numpy as np

    y, x = np.mgrid[-1:1:384j, -4 / 3:4 / 3:512j]
    radius = np.sqrt(x * x + y * y)
    angle = np.arctan2(y, x)
    petal = 0.52 + 0.19 * np.cos(7 * angle)
    mask = 1 / (1 + np.exp(np.clip((radius - petal) * 150, -80, 80)))
    texture = 0.08 * np.sin(210 * radius + 8 * np.cos(7 * angle)) * np.cos(13 * angle)
    center = np.exp(-((radius / 0.16) ** 4))
    color = np.stack((0.75 + texture, 0.32 + texture, 0.49 + texture), axis=2)
    color = color * (1 - center[..., None]) + np.array([0.87, 0.66, 0.19]) * center[..., None]
    color = color * mask[..., None] + np.array([0.06, 0.13, 0.10]) * (1 - mask[..., None])
    depth = np.clip(radius / 0.8, 0, 1)
    fy, fx = np.meshgrid(np.fft.fftfreq(384), np.fft.rfftfreq(512), indexing="ij")
    spectrum = np.fft.rfft2(color, axes=(0, 1))
    blurred = np.fft.irfft2(spectrum * np.exp(-2 * np.pi**2 * 3.5**2 * (fy * fy + fx * fx))[..., None], s=(384, 512), axes=(0, 1))
    paths = []
    for i, focus in enumerate(np.linspace(0, 1, 8)):
        sharp = np.exp(-((depth - focus) / 0.17) ** 2)[..., None]
        image = np.rint(np.clip(color * sharp + blurred * (1 - sharp), 0, 1) * 255).astype(np.uint8)
        rows = b"".join(b"\0" + row.tobytes() for row in image)

        def chunk(name, data):
            return struct.pack(">I", len(data)) + name + data + struct.pack(">I", zlib.crc32(name + data))

        path = folder / f"Synthetic flower {i + 1:02}.png"
        path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 512, 384, 8, 2, 0, 0, 0))
                         + chunk(b"sRGB", b"\0") + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))
        paths.append(path)
    return paths


def capture(app, output, frames):
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="florastack-screenshots-") as temporary:
        folder = Path(temporary)
        token = secrets.token_hex(32)
        token_file = folder / "control-token"
        with open(os.open(token_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as file:
            file.write(token)
        files = [Path(path).resolve() for path in frames] if frames else demo(folder)
        process = subprocess.Popen([str(app.resolve()), "--headless", "--control", "--control-token-file", str(token_file),
                                    "--temporary-projects", str(folder / "projects")],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        lines = queue.Queue()

        def read_stderr():
            for line in process.stderr:
                lines.put(line.rstrip())
            lines.put(None)

        threading.Thread(target=read_stderr, daemon=True).start()
        try:
            deadline = time.monotonic() + 60
            while True:
                line = lines.get(timeout=max(0.1, deadline - time.monotonic()))
                if line is None:
                    raise RuntimeError("app exited before starting control")
                if line.startswith("control: "):
                    host, port = line.removeprefix("control: ").rsplit(":", 1)
                    address = (host, int(port))
                    break
                if time.monotonic() >= deadline:
                    raise TimeoutError("app did not start control")

            def request(method, params=None):
                message = {"id": 1, "token": token, "method": method, "params": {} if params is None else params}
                with socket.create_connection(address, timeout=35) as connection:
                    connection.sendall(json.dumps(message).encode() + b"\n")
                    with connection.makefile("rb") as reader:
                        response = json.loads(reader.readline(24 * 1024 * 1024))
                if "error" in response:
                    raise RuntimeError(response["error"])
                return response["result"]

            def run(command, params=None):
                result = request("command.dispatch", {"command": command, "params": {} if params is None else params})
                if "job" not in result:
                    return result["result"]
                deadline = time.monotonic() + 180
                while time.monotonic() < deadline:
                    job = request("job.get", {"job": result["job"]})
                    if job["state"] == "done":
                        return job["result"]
                    if job["state"] in ("failed", "cancelled"):
                        raise RuntimeError(job.get("error", job["state"]))
                    time.sleep(0.05)
                raise TimeoutError(command)

            run("compute.set", {"device": "cpu"})
            run("project.new")
            run("project.rename", {"name": "Synthetic flower" if not frames else "Focus stack"})
            run("project.addFrames", {"paths": [str(path) for path in files]})
            if not frames:
                run("align.set", {"shift_x": False, "shift_y": False, "scale": False, "rotation": False, "brightness": False})
            run("align.run")
            result = run("stack.pyramid")
            for tab in ("stack", "export"):
                request("ui.setView", {"tab": tab, "output": result["output"]})
                deadline = time.monotonic() + 30
                while not request("ui.state")["ready"]:
                    if time.monotonic() > deadline:
                        raise TimeoutError("viewer did not finish loading")
                    time.sleep(0.05)
                # Let layout, thumbnails and the viewer's fit animation settle before capture.
                time.sleep(0.5)
                path = (output / f"{tab}.png").resolve()
                request("ui.screenshot", {"path": str(path), "overwrite": True})
                print(path)
            request("ui.close", {"discardChanges": True})
            process.wait(timeout=10)
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=10)
            process.stderr.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, default=Path("target/debug/florastack.exe" if os.name == "nt" else "target/debug/florastack"))
    parser.add_argument("--out", type=Path, default=Path("docs/images"))
    parser.add_argument("--frames", nargs="+", default=[])
    args = parser.parse_args()
    capture(args.app, args.out, args.frames)
