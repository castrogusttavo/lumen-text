#!/usr/bin/env python3
"""SVG -> alpha mask (.a8) converter for lumen-text icons.

usage: gen-icons.py <svg_dir> <out_dir> [size=48]

Each SVG is rasterized with `rsvg-convert` (black strokes/fills on transparent) and only the
alpha channel is kept: file = [w, h, w*h alpha bytes]. At runtime the mask is scaled to the
requested size and tinted with the theme color, so one icon works in light and dark themes.
"""
import os, struct, subprocess, sys, tempfile, zlib

def png_alpha(path):
    d = open(path, 'rb').read()
    pos, idat, w, h, ct, bd = 8, b'', 0, 0, 0, 8
    while pos < len(d):
        n, t = struct.unpack('>I4s', d[pos:pos + 8])
        c = d[pos + 8:pos + 8 + n]
        pos += 12 + n
        if t == b'IHDR':
            w, h, bd, ct = struct.unpack('>IIBB', c[:10])
        elif t == b'IDAT':
            idat += c
    assert bd == 8 and ct in (6, 4), f"unsupported PNG (bitdepth {bd}, color type {ct})"
    bpp = 4 if ct == 6 else 2
    raw = zlib.decompress(idat)
    stride, prev, out, i = w * bpp, bytearray(w * bpp), bytearray(), 0
    for _ in range(h):
        f = raw[i]; line = bytearray(raw[i + 1:i + 1 + stride]); i += 1 + stride
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b = prev[x]
            c = prev[x - bpp] if x >= bpp else 0
            if f == 1: line[x] = (line[x] + a) & 255
            elif f == 2: line[x] = (line[x] + b) & 255
            elif f == 3: line[x] = (line[x] + ((a + b) >> 1)) & 255
            elif f == 4:
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[x] = (line[x] + pr) & 255
        out += bytes(line[bpp - 1::bpp])
        prev = line
    return w, h, bytes(out)

def main():
    src, dst = sys.argv[1], sys.argv[2]
    size = int(sys.argv[3]) if len(sys.argv) > 3 else 48
    os.makedirs(dst, exist_ok=True)
    n = 0
    for name in sorted(os.listdir(src)):
        if not name.endswith('.svg'):
            continue
        with tempfile.NamedTemporaryFile(suffix='.png') as tmp:
            r = subprocess.run(['rsvg-convert', '-w', str(size), '-h', str(size), os.path.join(src, name), '-o', tmp.name], capture_output=True)
            if r.returncode != 0:
                print('skip', name, r.stderr.decode()[:80]); continue
            w, h, a = png_alpha(tmp.name)
        open(os.path.join(dst, name[:-4] + '.a8'), 'wb').write(bytes([w, h]) + a)
        n += 1
    print(f'{n} icons -> {dst}')

main()
