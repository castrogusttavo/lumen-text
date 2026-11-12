#!/usr/bin/env python3
"""Pack packaging/icons/png/lumen-text-<N>.png into a multi-size .ico (PNG-compressed entries, Vista+).

    make-ico.py <out.ico>
"""
import pathlib
import struct
import sys

png_dir = pathlib.Path(__file__).resolve().parent.parent / "icons" / "png"
sizes = [16, 24, 32, 48, 64, 128, 256]
images = [(n, (png_dir / f"lumen-text-{n}.png").read_bytes()) for n in sizes]
out = bytearray(struct.pack("<HHH", 0, 1, len(images)))
offset = 6 + 16 * len(images)
for n, data in images:
    out += struct.pack("<BBBBHHII", n % 256, n % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
for _, data in images:
    out += data
pathlib.Path(sys.argv[1]).write_bytes(bytes(out))
