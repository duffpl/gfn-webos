#!/usr/bin/env python3
"""Build a webOS .ipk deterministically: `ar` container of debian-binary, control.tar.gz, data.tar.gz.

Fixed owner, mtime and member order so the same inputs always give the same bytes (PlxNative ci/mkipk.py).
`usr/palm/packages/<id>/packageinfo.json` is required: appInstallService reads it to learn which app ids
a package owns, and an install without it registers nothing.

usage: mkipk.py <binary> <out-dir> [extra-executable ...]

Extra executables (e.g. opennow-core) are installed next to the main binary under their own names.
"""
import gzip
import io
import json
import struct
import sys
import tarfile
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MTIME = 0


def tar_gz(entries):
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as tar:
        dirs = sorted({str(Path(name).parents[i]) for name, _, _ in entries
                       for i in range(len(Path(name).parents) - 1)})
        for d in dirs:
            info = tarfile.TarInfo("./" + d)
            info.type, info.mode, info.mtime = tarfile.DIRTYPE, 0o755, MTIME
            tar.addfile(info)
        for name, data, mode in sorted(entries):
            info = tarfile.TarInfo("./" + name)
            info.size, info.mode, info.mtime = len(data), mode, MTIME
            tar.addfile(info, io.BytesIO(data))
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", mtime=MTIME) as gz:
        gz.write(raw.getvalue())
    return out.getvalue()


def ar(members):
    out = bytearray(b"!<arch>\n")
    for name, data in members:
        out += f"{name:<16}{MTIME:<12}{0:<6}{0:<6}{0o100644:<8o}{len(data):<10}`\n".encode()
        out += data + (b"\n" if len(data) % 2 else b"")
    return bytes(out)


def solid_png(size, rgb):
    row = b"\x00" + bytes(rgb) * size
    chunk = lambda kind, body: struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(row * size)) + chunk(b"IEND", b""))


def main():
    binary, out_dir, extras = Path(sys.argv[1]), Path(sys.argv[2]), [Path(p) for p in sys.argv[3:]]
    appinfo = json.loads((ROOT / "pkg/appinfo.json").read_text())
    app_id, version = appinfo["id"], appinfo["version"]
    app_dir = f"usr/palm/applications/{app_id}"
    packageinfo = {"id": app_id, "version": version, "app": app_id, "loc_name": appinfo["title"]}
    data = [
        (f"{app_dir}/appinfo.json", json.dumps(appinfo, indent=2).encode(), 0o644),
        (f"{app_dir}/{appinfo['main']}", binary.read_bytes(), 0o755),
        (f"{app_dir}/icon.png", solid_png(80, (0x76, 0xB9, 0x00)), 0o644),
        # The Inter fonts are compiled into the app binary; OFL 1.1 requires its text to travel with them.
        (f"{app_dir}/licenses/OFL-Inter.txt", (ROOT / "assets/fonts/OFL.txt").read_bytes(), 0o644),
        (f"usr/palm/packages/{app_id}/packageinfo.json", json.dumps(packageinfo, indent=2).encode(), 0o644),
    ] + [(f"{app_dir}/{extra.name}", extra.read_bytes(), 0o755) for extra in extras]
    installed_kb = sum(len(body) for _, body, _ in data) // 1024 + 1
    control = (f"Package: {app_id}\nVersion: {version}\nSection: misc\nPriority: optional\n"
               f"Architecture: arm\nInstalled-Size: {installed_kb}\nMaintainer: gfn-webos\n"
               f"License: GPL-3.0-or-later\nDescription: {appinfo['appDescription']}\n"
               f"webOS-Package-Format-Version: 2\nwebOS-Packager-Version: gfnwebos-mkipk/1\n")
    ipk = ar([("debian-binary", b"2.0\n"),
              ("control.tar.gz", tar_gz([("control", control.encode(), 0o644)])),
              ("data.tar.gz", tar_gz(data))])
    out_dir.mkdir(parents=True, exist_ok=True)
    target = out_dir / f"{app_id}_{version}_arm.ipk"
    target.write_bytes(ipk)
    print(target)


if __name__ == "__main__":
    main()
