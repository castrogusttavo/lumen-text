#!/usr/bin/env bash
# Build lumen-text_<version>-1_<debarch>.deb with plain dpkg-deb.
#   build-deb.sh <binary> <version> <arch> <outdir>      arch: x86_64 | aarch64
# <version> is the CalVer release version (YYYY.M.D.N), used as the upstream Debian version.
set -euo pipefail
[ $# -eq 4 ] || { echo "usage: $0 <binary> <version> <arch> <outdir>" >&2; exit 2; }
bin="$(realpath "$1")"; version="$2"; arch="$3"; out="$4"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"

case "$arch" in
  x86_64) debarch=amd64 ;;
  aarch64) debarch=arm64 ;;
  *) echo "unsupported arch: $arch" >&2; exit 2 ;;
esac
[[ "$version" =~ ^[0-9]+(\.[0-9]+)*$ ]] || { echo "bad version: $version" >&2; exit 2; }
debver="${version}-1"

mkdir -p "$out"; out="$(cd "$out" && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
pkg="$work/pkg"
"$here/../stage.sh" "$bin" "$pkg" /usr

# The binary links only libc/libgcc; the windowing libraries are dlopen()ed at run time
# (libxkbcommon always, Wayland or X11 depending on the session), hence Depends vs Recommends.
install -d "$pkg/DEBIAN"
size_kb="$(du -sk --exclude=DEBIAN "$pkg" | cut -f1)"
cat > "$pkg/DEBIAN/control" <<CTRL
Package: lumen-text
Version: ${debver}
Architecture: ${debarch}
Maintainer: Gustavo Leandro <castrogusttavo@users.noreply.github.com>
Installed-Size: ${size_kb}
Section: editors
Priority: optional
Homepage: https://github.com/castrogusttavo/lumen-text
Depends: libc6 (>= 2.35), libgcc-s1, libxkbcommon0, fontconfig, fonts-dejavu-core | fonts-liberation | fonts-noto-core
Recommends: libwayland-client0, libwayland-cursor0, libxkbcommon-x11-0, libx11-6, libx11-xcb1, libxcursor1, libxi6
Suggests: wl-clipboard, xclip
Description: Lightweight code editor with TextMate highlighting and a TypeScript workflow
 lumen-text is a fast, memory-frugal code editor written in Rust. It renders
 with the CPU, highlights with TextMate grammars and talks to the TypeScript
 native language server (tsgo) for diagnostics and completion.
CTRL

# Debian policy: copyright file and a (minimal) changelog.
cat > "$pkg/usr/share/doc/lumen-text/copyright" <<CPR
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: lumen-text
Source: https://github.com/castrogusttavo/lumen-text

Files: *
Copyright: 2026 Gustavo Leandro (castrogusttavo) and lumen-text contributors
License: MIT

Files: usr/share/lumen-text/assets/*
Copyright: see usr/share/lumen-text/assets/licenses/NOTICE.md
License: various
 Grammars, themes and icons bundled under /usr/share/lumen-text/assets keep
 their upstream licenses; the texts are in /usr/share/lumen-text/assets/licenses/.

License: MIT
$(sed -e 's/^$/./' -e 's/^/ /' "$root/LICENSE" | tail -n +3)
CPR
rm -f "$pkg/usr/share/doc/lumen-text/LICENSE"
{
  echo "lumen-text (${debver}) unstable; urgency=medium"
  echo
  echo "  * Automated release ${version}. See https://github.com/castrogusttavo/lumen-text/releases/tag/v${version}"
  echo
  echo " -- Gustavo Leandro <castrogusttavo@users.noreply.github.com>  $(LC_ALL=C date -u -R)"
} | gzip -9n > "$pkg/usr/share/doc/lumen-text/changelog.Debian.gz"
chmod 644 "$pkg/usr/share/doc/lumen-text/"*

(cd "$pkg" && find usr -type f -print0 | sort -z | xargs -0 md5sum > DEBIAN/md5sums)
chmod 755 "$pkg/DEBIAN"; chmod 644 "$pkg/DEBIAN/control" "$pkg/DEBIAN/md5sums"

file="$out/lumen-text_${debver}_${debarch}.deb"
dpkg-deb --root-owner-group -Zxz --build "$pkg" "$file" >/dev/null
echo "Built $file"
