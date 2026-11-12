#!/usr/bin/env bash
# Build the portable Windows archive: lumen-text-<version>-windows-<arch>.zip (+ .sha256).
#   build-zip.sh <lumen-text.exe> <version> <arch> <outdir>      arch: x86_64
# Layout: lumen-text.exe next to assets/ (config::assets_dir() looks at <exe dir>/assets).
# Runs in Git Bash on a Windows runner.
set -euo pipefail
[ $# -eq 4 ] || { echo "usage: $0 <exe> <version> <arch> <outdir>" >&2; exit 2; }
exe="$1"; version="$2"; arch="$3"; out="$4"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
name="lumen-text-${version}-windows-${arch}"

mkdir -p "$out"; out="$(cd "$out" && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
d="$work/$name"
"$here/stage.sh" "$exe" "$d"   # the file list is shared with the installer (build-setup.sh)
rm -f "$out/$name.zip"
# Windows 10+ ships bsdtar (System32\tar.exe), which writes zip archives; Git Bash's own GNU tar does not.
if command -v cygpath >/dev/null 2>&1; then
  bsdtar="$(cygpath -u "${SYSTEMROOT:-C:\\Windows}")/System32/tar.exe"
  zip_out="$(cygpath -m "$out/$name.zip")"
else
  bsdtar=tar; zip_out="$out/$name.zip"
fi
(cd "$work" && "$bsdtar" -a -cf "$zip_out" "$name")
(cd "$out" && sha256sum "$name.zip" > "$name.zip.sha256")
echo "Built $out/$name.zip"
