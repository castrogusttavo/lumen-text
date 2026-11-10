#!/usr/bin/env bash
# Optional: generate the HugeIcons Pro icon masks used by the platform UI.
#
# HugeIcons Pro is a commercial icon set. Its license does not allow redistribution, so the
# generated masks (assets/icons/hugeicons and assets/icons/hugeicons_bulk) are NOT part of this
# repository. Without them lumen-text falls back to the bundled `ui/` icons automatically.
# If you own a HugeIcons Pro license, run this once to get the original look:
#
#   HUGEICONS_TOKEN=<your npm token> tools/setup-hugeicons.sh
#
# Requirements: node + npm, python3, rsvg-convert (librsvg). CI runs this with the maintainer's key
# (GitHub secret HUGEICONS_TOKEN); the masks are then embedded into the release binary by build.rs.
set -euo pipefail

if [[ -z "${HUGEICONS_TOKEN:-}" ]]; then
  echo "error: set HUGEICONS_TOKEN to your HugeIcons Pro npm token" >&2
  exit 1
fi

for tool in node npm python3 rsvg-convert; do
  command -v "$tool" >/dev/null || { echo "error: $tool is required" >&2; exit 1; }
done

# Never echo the token (GitHub Actions masks it in logs; harmless elsewhere).
echo "::add-mask::${HUGEICONS_TOKEN}"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cd "$work"
cat > .npmrc <<EOF
@hugeicons-pro:registry=https://npm.hugeicons.com/
//npm.hugeicons.com/:_authToken=${HUGEICONS_TOKEN}
EOF
echo '{"name":"hugeicons-fetch","private":true}' > package.json
npm install --loglevel=error --no-audit --no-fund @hugeicons-pro/core-solid-rounded @hugeicons-pro/core-bulk-rounded

gen() { # <package> <output dir name>
  local pkg="$1" out="$2"
  node "$root/tools/hugeicons-to-svg.mjs" "$work/node_modules/@hugeicons-pro/$pkg/dist/esm" "$work/svg-$out"
  rm -rf "$root/assets/icons/$out"
  python3 "$root/tools/gen-icons.py" "$work/svg-$out" "$root/assets/icons/$out"
}
gen core-solid-rounded hugeicons
gen core-bulk-rounded hugeicons_bulk
echo "done: assets/icons/hugeicons and assets/icons/hugeicons_bulk generated (git-ignored)"
