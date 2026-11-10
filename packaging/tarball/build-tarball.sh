#!/usr/bin/env bash
# Build the portable archive: lumen-text-<version>-linux-<arch>.tar.gz (+ .sha256).
#   build-tarball.sh <binary> <version> <arch> <outdir>      arch: x86_64 | aarch64
# Runs in place (bin/lumen-text finds ../share/lumen-text/assets) or via ./install.sh.
set -euo pipefail
[ $# -eq 4 ] || { echo "usage: $0 <binary> <version> <arch> <outdir>" >&2; exit 2; }
bin="$(realpath "$1")"; version="$2"; arch="$3"; out="$4"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
name="lumen-text-${version}-linux-${arch}"

mkdir -p "$out"; out="$(cd "$out" && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
"$here/../stage.sh" "$bin" "$work/$name" ""
install -m755 "$here/install.sh" "$work/$name/install.sh"
install -m644 "$here/../../LICENSE" "$here/../../README.md" "$work/$name/"
tar -C "$work" --owner=0 --group=0 --numeric-owner -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "Built $out/$name.tar.gz"
