#!/usr/bin/env bash
# Build lumen-text-bin-<version>-1-x86_64.pkg.tar.zst inside an archlinux container with
# makepkg (as an unprivileged user), from the portable tarball built by CI.
#   build-arch.sh <tarball.tar.gz> <version> <outdir>          (host; needs docker)
#   build-arch.sh --inner <tarball> <version> <outdir>         (inside the container, as root)
# Only x86_64: the official archlinux image is x86_64-only.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ "${1:-}" != "--inner" ]; then
  [ $# -eq 3 ] || { echo "usage: $0 <tarball> <version> <outdir>" >&2; exit 2; }
  tarball="$(realpath "$1")"; version="$2"; out="$(mkdir -p "$3" && cd "$3" && pwd)"
  root="$(cd "$here/../.." && pwd)"
  docker run --rm -e "HOME=/root" \
    -v "$root/packaging/arch:/src:ro" -v "$tarball:/in/$(basename "$tarball"):ro" -v "$out:/out" \
    archlinux:latest bash /src/build-arch.sh --inner "/in/$(basename "$tarball")" "$version" /out
  exit 0
fi

shift
tarball="$1"; version="$2"; out="$3"
case "$(uname -m)" in x86_64) ;; *) echo "arch packages are only built for x86_64" >&2; exit 1 ;; esac

pacman -Syu --noconfirm --needed base-devel >/dev/null
id builder >/dev/null 2>&1 || useradd -m builder
b=/home/builder/pkg
rm -rf "$b"; mkdir -p "$b"
cp "$here/lumen-text-bin/PKGBUILD" "$b/PKGBUILD"
# makepkg uses a source file that already sits next to the PKGBUILD instead of downloading it.
cp "$tarball" "$b/lumen-text-$version-x86_64.tar.gz"
sum="$(sha256sum "$tarball" | cut -d' ' -f1)"
sed -i -e "s/^pkgver=.*/pkgver=$version/" \
       -e "s/^sha256sums_x86_64=.*/sha256sums_x86_64=('$sum')/" \
       -e "/^sha256sums_aarch64=/d" -e "/^source_aarch64=/d" "$b/PKGBUILD"
chown -R builder: "$b"
# --nodeps: dependencies are only metadata for a prebuilt binary.
su builder -c "cd $b && makepkg --nodeps --noconfirm --nocheck -f"
cp "$b"/*.pkg.tar.zst "$out/"
chmod 644 "$out"/*.pkg.tar.zst
