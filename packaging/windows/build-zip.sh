#!/usr/bin/env bash
# Build the portable Windows archive: lumen-text-<version>-windows-<arch>.zip (+ .sha256).
#   build-zip.sh <lumen-text.exe> <version> <arch> <outdir>      arch: x86_64
# Layout: lumen-text.exe next to assets/ (config::assets_dir() looks at <exe dir>/assets).
# Runs in Git Bash on a Windows runner.
set -euo pipefail
[ $# -eq 4 ] || { echo "usage: $0 <exe> <version> <arch> <outdir>" >&2; exit 2; }
exe="$1"; version="$2"; arch="$3"; out="$4"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
name="lumen-text-${version}-windows-${arch}"

mkdir -p "$out"; out="$(cd "$out" && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
d="$work/$name"
mkdir -p "$d"
cp "$exe" "$d/lumen-text.exe"
cp -r "$root/assets" "$d/assets"
# Licensed HugeIcons Pro masks live inside the binary only, never as loose files.
rm -rf "$d/assets/icons/hugeicons" "$d/assets/icons/hugeicons_bulk"
cp "$root/LICENSE" "$root/README.md" "$d/"
rm -f "$out/$name.zip"
# Windows 10+ ships bsdtar, which writes zip archives.
(cd "$work" && tar -a -cf "$out/$name.zip" "$name")
(cd "$out" && sha256sum "$name.zip" > "$name.zip.sha256")
echo "Built $out/$name.zip"
