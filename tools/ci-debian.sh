#!/usr/bin/env bash
# Run a command in a Debian container with some Debian packages installed. CI uses it for the
# few tools the Ubuntu runner image lacks (rpmbuild, rsvg-convert), so no job depends on the
# Ubuntu archive being reachable: Debian's mirrors (deb.debian.org) are independent of Canonical's.
#   tools/ci-debian.sh <image> "<debian packages>" <command> [args...]
# The working directory is mounted at the same path. Files the command writes under dist/ and
# assets/icons/ are handed back to the calling user. HUGEICONS_TOKEN and VERSION pass through.
set -euo pipefail
[ $# -ge 3 ] || { echo "usage: $0 <image> \"<packages>\" <command> [args...]" >&2; exit 2; }
image="$1"
pkgs="$2"
shift 2
ws="$(pwd)"
docker run --rm -v "$ws:$ws" -w "$ws" \
  -e HUGEICONS_TOKEN -e VERSION -e GHP_LSP_BIN -e PKGS="$pkgs" -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
  "$image" bash -c '
    set -euo pipefail
    export DEBIAN_FRONTEND=noninteractive
    apt-get -o Acquire::Retries=3 update -qq >/dev/null
    # shellcheck disable=SC2086 # the package list is meant to split
    apt-get -o Acquire::Retries=3 install -y -qq --no-install-recommends $PKGS >/dev/null
    status=0
    "$@" || status=$?
    for p in dist assets/icons; do
      [ -e "$p" ] && chown -R "$HOST_UID:$HOST_GID" "$p"
    done
    exit "$status"
  ' bash "$@"
