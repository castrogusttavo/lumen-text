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
for png in "$here"/share/icons/hicolor/*x*/apps/lumen-text.png; do
  [ -f "$png" ] || continue
  sz="$(basename "$(dirname "$(dirname "$png")")")"
  install -Dm644 "$png" "$prefix/share/icons/hicolor/$sz/apps/lumen-text.png"
done
if command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -f "$prefix/share/icons/hicolor/index.theme" ]; then
  gtk-update-icon-cache -f -t "$prefix/share/icons/hicolor" >/dev/null 2>&1 || true
fi
install -Dm644 "$here/share/applications/lumen-text.desktop" "$prefix/share/applications/lumen-text.desktop"
# Absolute Exec= so the launcher works even when $prefix/bin is not in the desktop's PATH.
sed -i "s|^Exec=.*|Exec=$prefix/bin/lumen-text %F|" "$prefix/share/applications/lumen-text.desktop"

# lumen-text needs at least one system font (it exits at start-up without any) and the
# libxkbcommon shared library; a tarball cannot declare dependencies, so check here.
if command -v fc-list >/dev/null 2>&1; then
  [ -n "$(fc-list 2>/dev/null | head -n 1)" ] || echo "warning: no fonts found - install one (e.g. fonts-dejavu-core / dejavu-sans-mono-fonts / ttf-dejavu)" >&2
else
  echo "warning: fontconfig not found - install fontconfig and a font (e.g. fonts-dejavu-core, dejavu-sans-mono-fonts, ttf-dejavu)" >&2
fi
if command -v ldconfig >/dev/null 2>&1 && ! ldconfig -p 2>/dev/null | grep -q 'libxkbcommon\.so'; then
  echo "warning: libxkbcommon not found - install libxkbcommon0 / libxkbcommon" >&2
fi

echo "installed to $prefix (make sure $prefix/bin is in PATH)"
echo "TypeScript support: npm i --prefix \"$prefix/share/lumen-text/tsgo\" @typescript/native-preview"
