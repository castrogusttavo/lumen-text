#!/usr/bin/env bash
# Build lumen-text-<version>-1.<arch>.rpm with rpmbuild (works on Fedora and on Debian/Ubuntu
# with the `rpm` package installed).
#   build-rpm.sh <binary> <version> <arch> <outdir>      arch: x86_64 | aarch64
set -euo pipefail
[ $# -eq 4 ] || { echo "usage: $0 <binary> <version> <arch> <outdir>" >&2; exit 2; }
bin="$(realpath "$1")"; version="$2"; arch="$3"; out="$4"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

case "$arch" in x86_64|aarch64) ;; *) echo "unsupported arch: $arch" >&2; exit 2 ;; esac
[[ "$version" =~ ^[0-9]+(\.[0-9]+)*$ ]] || { echo "bad version: $version" >&2; exit 2; }

mkdir -p "$out"; out="$(cd "$out" && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
"$here/../stage.sh" "$bin" "$work/stage" /usr

rpmbuild -bb "$here/lumen-text.spec" \
  --target "${arch}-linux" \
  --define "_topdir $work/top" \
  --define "_rpmdir $out" \
  --define "_rpmfilename %%{NAME}-%%{VERSION}-%%{RELEASE}.%%{ARCH}.rpm" \
  --define "lumen_version $version" \
  --define "lumen_stage $work/stage" \
  --define "changelog_date $(LC_ALL=C date -u +'%a %b %d %Y')" >&2
echo "Built $out/lumen-text-${version}-1.${arch}.rpm"
