#!/usr/bin/env bash
# Build lumen-text-<version>-<arch>.AppImage from an already built binary.
#   build-appimage.sh <binary> <version> <arch> <outdir>      arch: x86_64 | aarch64
#
# appimagetool is taken from $APPIMAGETOOL, PATH, or downloaded next to the output (cached).
# It runs with APPIMAGE_EXTRACT_AND_RUN=1 so FUSE is not required (CI containers).
# Set LUMEN_NO_APPIMAGETOOL=1 to only assemble the AppDir.
set -euo pipefail
[ $# -eq 4 ] || { echo "usage: $0 <binary> <version> <arch> <outdir>" >&2; exit 2; }
bin="$(realpath "$1")"; version="$2"; arch="$3"; out="$4"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
case "$arch" in x86_64|aarch64) ;; *) echo "unsupported arch: $arch" >&2; exit 2 ;; esac

mkdir -p "$out"; out="$(cd "$out" && pwd)"
appdir="$out/AppDir"
rm -rf "$appdir"
"$here/../stage.sh" "$bin" "$appdir" /usr

# AppImage wants the .desktop file, the icon and AppRun at the AppDir root.
cp "$appdir/usr/share/applications/lumen-text.desktop" "$appdir/lumen-text.desktop"
cp "$appdir/usr/share/icons/hicolor/scalable/apps/lumen-text.svg" "$appdir/lumen-text.svg"
ln -s lumen-text.svg "$appdir/.DirIcon"

# The binary already resolves <exe dir>/../share/lumen-text/assets, which is correct inside the
# mounted image; LUMEN_ASSETS is set anyway so the lookup never depends on the process name.
cat > "$appdir/AppRun" <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export LUMEN_ASSETS="${LUMEN_ASSETS:-$HERE/usr/share/lumen-text/assets}"
exec "$HERE/usr/bin/lumen-text" "$@"
APPRUN
chmod 755 "$appdir/AppRun"

if [ "${LUMEN_NO_APPIMAGETOOL:-0}" = 1 ]; then
  echo "AppDir ready at $appdir"; exit 0
fi

tool="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
if [ -z "$tool" ]; then
  # appimagetool itself only exists for the build host's architecture here.
  host="$(uname -m)"
  tool="$out/appimagetool-$host.AppImage"
  if [ ! -x "$tool" ]; then
    curl -fsSL --retry 3 -o "$tool" \
      "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$host.AppImage"
    chmod +x "$tool"
  fi
fi

file="$out/lumen-text-${version}-${arch}.AppImage"
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$arch" "$tool" --no-appstream "$appdir" "$file" >&2
echo "Built $file"
