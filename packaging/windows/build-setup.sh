#!/usr/bin/env bash
# Build the Windows installer: lumen-text-<version>-windows-<arch>-setup.exe (+ .sha256), with Inno Setup.
#   build-setup.sh <lumen-text.exe> <version> <arch> <outdir>      arch: x86_64
# Stages the same files as the zip (stage.sh), then compiles lumen-text.iss. ISCC is taken from $ISCC,
# PATH or the default Inno Setup 6 install directories. Runs in Git Bash on a Windows runner.
set -euo pipefail
[ $# -eq 4 ] || { echo "usage: $0 <exe> <version> <arch> <outdir>" >&2; exit 2; }
exe="$1"; version="$2"; arch="$3"; out="$4"
[ "$arch" = x86_64 ] || { echo "build-setup.sh: only x86_64 is supported" >&2; exit 2; }
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
name="lumen-text-${version}-windows-${arch}-setup"

iscc="${ISCC:-}"
if [ -z "$iscc" ]; then
  iscc="$(command -v ISCC.exe || command -v iscc || true)"
fi
if [ -z "$iscc" ]; then
  for c in "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" "/c/Program Files/Inno Setup 6/ISCC.exe" "$LOCALAPPDATA/Programs/Inno Setup 6/ISCC.exe"; do
    [ -f "$c" ] && { iscc="$c"; break; }
  done
fi
[ -n "$iscc" ] || { echo "build-setup.sh: Inno Setup (ISCC.exe) not found; install it (choco install innosetup) or set ISCC" >&2; exit 1; }

win() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }

mkdir -p "$out"; out="$(cd "$out" && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
"$here/stage.sh" "$exe" "$work/stage"
py=python; "$py" -c '' 2>/dev/null || py=python3
"$py" "$here/make-ico.py" "$work/lumen-text.ico"

# Windows file versions are four numbers: pad CalVer (YYYY.M.D.N) with zeros if needed.
IFS=. read -r a b c d <<<"$version"
numeric="${a:-0}.${b:-0}.${c:-0}.${d:-0}"

rm -f "$out/$name.exe"
# MSYS would rewrite the /D... switches as paths.
MSYS2_ARG_CONV_EXCL='*' "$iscc" /Q \
  "/DStageDir=$(win "$work/stage")" \
  "/DIconFile=$(win "$work/lumen-text.ico")" \
  "/DAppVersion=$version" \
  "/DVersionNumeric=$numeric" \
  "/DOutDir=$(win "$out")" \
  "/DOutName=$name" \
  "$(win "$here/lumen-text.iss")"
[ -f "$out/$name.exe" ] || { echo "build-setup.sh: $name.exe was not produced" >&2; exit 1; }
(cd "$out" && sha256sum "$name.exe" > "$name.exe.sha256")
echo "Built $out/$name.exe"
