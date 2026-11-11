#!/usr/bin/env bash
# CI helper: put the HugeIcons Pro masks in assets/icons/ (see tools/setup-hugeicons.sh), reusing an
# encrypted copy from the Actions cache when there is one.
#
#   tools/ci-hugeicons.sh <cache file> <command that installs rsvg-convert>
#
# Generating the masks needs rsvg-convert, which costs minutes to install on Windows (choco) and
# macOS (brew). The repository is public and the masks may not be redistributed, so the cache only
# ever holds them encrypted with the maintainer's token (fork pull requests can restore a cache
# entry but never see the secret). No token: build with the bundled icons. Any failure: warn and
# build with the bundled icons too, as before.
set -uo pipefail

cache="$1"
install="$2"

if [[ -z "${HUGEICONS_TOKEN:-}" ]]; then
  echo "HUGEICONS_TOKEN not set: building with the bundled icons"
  exit 0
fi
echo "::add-mask::${HUGEICONS_TOKEN}"

icons=assets/icons
# `-md sha256` without -pbkdf2: understood by OpenSSL 3 (Linux, Git for Windows) and LibreSSL (macOS).
# The password is a long random token, so the key derivation is not the weak point.
enc() { openssl enc -aes-256-cbc -md sha256 -salt -pass env:HUGEICONS_TOKEN "$@" 2>/dev/null; }

if [[ -s "$cache" ]]; then
  if enc -d -in "$cache" | tar xzf - -C "$icons"; then
    echo "HugeIcons masks restored from the encrypted cache: $(ls "$icons/hugeicons" | wc -l) icons"
    exit 0
  fi
  echo "::warning::encrypted HugeIcons cache unreadable; generating the masks again"
  rm -rf "$icons/hugeicons" "$icons/hugeicons_bulk" "$cache"
fi

bash -c "$install" >/dev/null 2>&1 || true
if tools/setup-hugeicons.sh >/dev/null 2>&1; then
  echo "HugeIcons masks generated: $(ls "$icons/hugeicons" | wc -l) icons"
  tar czf - -C "$icons" hugeicons hugeicons_bulk | enc -out "$cache" || rm -f "$cache"
else
  echo "::warning::HugeIcons masks could not be generated on ${RUNNER_OS:-this OS}; using the bundled icons"
  rm -rf "$icons/hugeicons" "$icons/hugeicons_bulk" "$cache"
fi
exit 0
