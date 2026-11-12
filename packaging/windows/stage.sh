#!/usr/bin/env bash
# Stage the Windows file layout (shared by the zip and the installer, so the file list lives in one place).
#   stage.sh <lumen-text.exe> <destdir>
# destdir gets: lumen-text.exe, ghp-lsp.exe (when GHP_LSP_BIN is set), assets/, LICENSE, README.md.
# config::assets_dir() looks at <exe dir>/assets. Runs in Git Bash on a Windows runner.
set -euo pipefail
[ $# -eq 2 ] || { echo "usage: $0 <exe> <destdir>" >&2; exit 2; }
exe="$1"; d="$2"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
[ -f "$exe" ] || { echo "stage.sh: exe not found: $exe" >&2; exit 1; }
[ -f "$root/assets/language-manifest.json" ] || { echo "stage.sh: assets/ is incomplete" >&2; exit 1; }
mkdir -p "$d"
cp "$exe" "$d/lumen-text.exe"
# The GHP language server next to the exe (GHP_LSP_BIN=path/to/ghp-lsp.exe).
[ -n "${GHP_LSP_BIN:-}" ] && cp "$GHP_LSP_BIN" "$d/ghp-lsp.exe"
cp -r "$root/assets" "$d/assets"
# Licensed HugeIcons Pro masks live inside the binary only, never as loose files.
rm -rf "$d/assets/icons/hugeicons" "$d/assets/icons/hugeicons_bulk"
cp "$root/LICENSE" "$root/README.md" "$d/"
exit 0
