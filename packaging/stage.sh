#!/usr/bin/env bash
# Install the lumen-text file layout into a staging directory.
#
#   stage.sh <binary> <destdir> [prefix]
#
# prefix defaults to /usr; pass "" to get a bare bin/ + share/ tree (portable tarball).
# Layout (config::assets_dir() looks at <exe dir>/../share/lumen-text/assets, so the
# binary and the assets must keep this relative position in every package format):
#   <prefix>/bin/lumen-text
#   <prefix>/share/lumen-text/assets/
#   <prefix>/share/applications/lumen-text.desktop
#   <prefix>/share/icons/hicolor/scalable/apps/lumen-text.svg
#   <prefix>/share/icons/hicolor/<N>x<N>/apps/lumen-text.png   (N = 16..512)
#   <prefix>/share/doc/lumen-text/{README.md,LICENSE}
set -euo pipefail

[ $# -ge 2 ] || { echo "usage: $0 <binary> <destdir> [prefix]" >&2; exit 2; }
bin="$1"
dest="$2"
prefix="${3-/usr}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
p="${dest}${prefix}"

[ -f "$bin" ] || { echo "stage.sh: binary not found: $bin" >&2; exit 1; }
[ -f "$root/assets/language-manifest.json" ] || { echo "stage.sh: assets/ is incomplete" >&2; exit 1; }

install -d "$p/bin" "$p/share/lumen-text" "$p/share/applications" \
  "$p/share/icons/hicolor/scalable/apps" "$p/share/doc/lumen-text"
install -m755 "$bin" "$p/bin/lumen-text"
if command -v "${STRIP:-strip}" >/dev/null 2>&1; then
  "${STRIP:-strip}" --strip-unneeded "$p/bin/lumen-text" || true
fi
cp -r "$root/assets" "$p/share/lumen-text/assets"
install -m644 "$root/lumen-text.desktop" "$p/share/applications/lumen-text.desktop"
install -m644 "$here/icons/lumen-text.svg" "$p/share/icons/hicolor/scalable/apps/lumen-text.svg"
for png in "$here"/icons/png/lumen-text-*.png; do
  n="$(basename "$png" .png)"; n="${n#lumen-text-}"
  [ "$n" = 1024 ] && continue
  install -Dm644 "$png" "$p/share/icons/hicolor/${n}x${n}/apps/lumen-text.png"
done
install -m644 "$root/README.md" "$root/LICENSE" "$p/share/doc/lumen-text/"

# Normalise modes (the checkout's umask must not leak into packages).
find "$p/share" -type d -exec chmod 755 {} +
find "$p/share" -type f -exec chmod 644 {} +
