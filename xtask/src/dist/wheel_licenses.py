"""Adds license files to a built wheel: `python wheel_licenses.py WHEEL FILE...`.

`cargo xtask dist` runs it on the wheel maturin built (xtask/src/dist/notices.rs): maturin takes license files only from
the Python project's own folder, and FloraStack's are at the repository's top, with the third-party notices generated
at build time. Each FILE goes into the wheel's `.dist-info/licenses/` folder by its name, METADATA gains a
`License-File` line for it (core metadata 2.4, PEP 639), and RECORD is written again with every file's hash and size.
"""

import base64
import hashlib
import os
import sys
import zipfile


def record_line(name, data):
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode("ascii")
    return f"{name},sha256={digest},{len(data)}"


def main(wheel, files):
    with zipfile.ZipFile(wheel) as source:
        entries = [(info, source.read(info)) for info in source.infolist()]
    dist_infos = {info.filename.split("/")[0] for info, _ in entries if info.filename.split("/")[0].endswith(".dist-info")}
    if len(dist_infos) != 1:
        sys.exit(f"{wheel} has {len(dist_infos)} .dist-info folders, not one")
    dist_info = dist_infos.pop()
    metadata_name, record_name = f"{dist_info}/METADATA", f"{dist_info}/RECORD"

    added = []
    for path in files:
        name = os.path.basename(path)
        with open(path, "rb") as file:
            added.append((f"{dist_info}/licenses/{name}", name, file.read()))
    taken = {info.filename for info, _ in entries}
    for target, _, _ in added:
        if target in taken:
            sys.exit(f"{wheel} already has {target}")

    out = []
    for info, data in entries:
        if info.filename == record_name:
            continue
        if info.filename == metadata_name:
            text = data.decode("utf-8")
            head, separator, body = text.partition("\n\n")
            head = head.rstrip("\n") + "".join(f"\nLicense-File: {name}" for _, name, _ in added)
            data = (head + "\n" + separator[1:] + body).encode("utf-8") if separator else (head + "\n").encode("utf-8")
        out.append((info, data))
    for target, _, data in added:
        info = zipfile.ZipInfo(target, date_time=(1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        info.external_attr = 0o644 << 16
        out.append((info, data))

    lines = [record_line(info.filename, data) for info, data in out] + [f"{record_name},,"]
    record = zipfile.ZipInfo(record_name, date_time=(1980, 1, 1, 0, 0, 0))
    record.compress_type = zipfile.ZIP_DEFLATED
    record.external_attr = 0o644 << 16

    temporary = wheel + ".part"
    with zipfile.ZipFile(temporary, "w") as target:
        for info, data in out:
            target.writestr(info, data)
        target.writestr(record, "\n".join(lines) + "\n")
    os.replace(temporary, wheel)


if __name__ == "__main__":
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2:])
