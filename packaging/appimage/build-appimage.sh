#!/usr/bin/env bash
# Builds lumen-text-x86_64.AppImage. Run from anywhere; output goes to packaging/appimage/out/.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$here/out"
appdir="$out/AppDir"

cd "$root"
cargo build --release --locked

rm -rf "$appdir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/lumen-text" "$appdir/usr/share/applications"
install -m755 target/release/lumen-text "$appdir/usr/bin/lumen-text"
cp -r assets "$appdir/usr/share/lumen-text/assets"

# Desktop file (AppImage wants one at the AppDir root, with an Icon= key).
{ grep -v '^Icon=' lumen-text.desktop; echo "Icon=lumen-text"; } > "$appdir/lumen-text.desktop"
cp "$appdir/lumen-text.desktop" "$appdir/usr/share/applications/lumen-text.desktop"

# Placeholder icon (256x256 solid PNG) generated with a tiny script. Replace with real art.
python3 - "$appdir/lumen-text.png" <<'PY'
import struct, sys, zlib
w = h = 256
def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
rows = b"".join(b"\x00" + bytes((0x2b, 0x6c, 0xb0)) * w for _ in range(h))
png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b""))
open(sys.argv[1], "wb").write(png)
PY

cat > "$appdir/AppRun" <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export LUMEN_ASSETS="${LUMEN_ASSETS:-$HERE/usr/share/lumen-text/assets}"
exec "$HERE/usr/bin/lumen-text" "$@"
APPRUN
chmod +x "$appdir/AppRun"

tool="$(command -v appimagetool || true)"
if [ -z "$tool" ] && [ -x "$out/appimagetool" ]; then tool="$out/appimagetool"; fi
if [ -n "$tool" ]; then
  ARCH=x86_64 "$tool" "$appdir" "$out/lumen-text-x86_64.AppImage"
  echo "Built $out/lumen-text-x86_64.AppImage"
else
  echo "AppDir ready at $appdir, but appimagetool was not found."
  echo "Get it with:"
  echo "  curl -L -o $out/appimagetool https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
  echo "  chmod +x $out/appimagetool"
  echo "then re-run this script (without FUSE: $out/appimagetool --appimage-extract-and-run)."
fi
