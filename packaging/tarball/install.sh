#!/bin/sh
# Install the prebuilt lumen-text from this archive. No root needed by default.
#   ./install.sh                  -> ~/.local
#   sudo PREFIX=/usr/local ./install.sh   (system-wide)
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
prefix="${PREFIX:-$HOME/.local}"

install -Dm755 "$here/bin/lumen-text" "$prefix/bin/lumen-text"
rm -rf "$prefix/share/lumen-text/assets"
mkdir -p "$prefix/share/lumen-text"
cp -r "$here/share/lumen-text/assets" "$prefix/share/lumen-text/assets"
install -Dm644 "$here/share/icons/hicolor/scalable/apps/lumen-text.svg" \
  "$prefix/share/icons/hicolor/scalable/apps/lumen-text.svg"
install -Dm644 "$here/share/applications/lumen-text.desktop" "$prefix/share/applications/lumen-text.desktop"
# Absolute Exec= so the launcher works even when $prefix/bin is not in the desktop's PATH.
sed -i "s|^Exec=.*|Exec=$prefix/bin/lumen-text %F|" "$prefix/share/applications/lumen-text.desktop"

echo "installed to $prefix (make sure $prefix/bin is in PATH)"
echo "TypeScript support: npm i --prefix \"$prefix/share/lumen-text/tsgo\" @typescript/native-preview"
