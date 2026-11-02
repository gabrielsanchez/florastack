"""Build installable editor integrations using the identity in branding.toml (Python 3.11+)."""

import argparse
import json
from pathlib import Path
import shutil
import tomllib


def build(destination: Path) -> None:
    root = Path(__file__).resolve().parents[1]
    identity = tomllib.loads((root / "branding.toml").read_text(encoding="utf-8"))
    name = identity["display_name"]
    app_id = identity["app_id"]
    if "OWNER" in app_id:
        raise ValueError("set the publisher in branding.toml before packaging integrations")
    cli = identity["binaries"]["cli"]
    gui = identity["binaries"]["gui"]
    # These values enter Lua literals, desktop-entry fields and directory names.
    for value in (name, app_id, cli, gui):
        if not value or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-_ " for c in value):
            raise ValueError("integration branding must use ASCII letters, numbers, spaces, dots, hyphens or underscores")
    branding = "-- Generated from branding.toml by scripts/package_integrations.py.\nreturn {\n"
    for key, value in (("name", name), ("app_id", app_id), ("cli", cli)):
        branding += f"    {key} = {json.dumps(value)},\n"
    branding += "}\n"
    source = root / "packaging"
    lightroom = destination / f"{name}.lrplugin"
    darktable = destination / "darktable"
    for directory in (lightroom, darktable / "florastack"):
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "Branding.lua").write_text(branding, encoding="utf-8")
        shutil.copyfile(source / "common" / "Bridge.lua", directory / "Bridge.lua")
    for filename in ("Info.lua", "ExportServiceProvider.lua"):
        shutil.copyfile(source / "lightroom" / filename, lightroom / filename)
    shutil.copyfile(source / "darktable" / "florastack.lua", darktable / "florastack.lua")
    desktop = (source / "linux" / "open-with.desktop.in").read_text(encoding="utf-8")
    for token, value in (("@DISPLAY_NAME@", name), ("@GUI_BINARY@", gui)):
        desktop = desktop.replace(token, value)
    (destination / f"{app_id}.desktop").write_text(desktop, encoding="utf-8")
    print(f"Editor integrations: {destination.resolve()}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=Path(__file__).resolve().parents[1] / "target" / "integrations")
    build(parser.parse_args().out)
