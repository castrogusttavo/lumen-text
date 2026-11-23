#!/usr/bin/env bash
# Dev loop: recompiles and relaunches lumen-text every time a source file is saved.
#
#   tools/dev.sh [lumen-text args]     (default: opens this repository)
#   LUMEN_DEV_PROFILE=dev tools/dev.sh # faster builds, slower UI (default: release)
#
# Needs `cargo install cargo-watch`. Changes to .rs files (and Cargo.toml / assets) restart the
# editor, the session restore reopens the tabs. settings.json is reloaded live, no restart needed.
# `--new-window` keeps the dev build from handing off to an already running (installed) instance.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! cargo watch --version >/dev/null 2>&1; then
  echo "cargo-watch not found. Install it once with:  cargo install cargo-watch --locked" >&2
  exit 1
fi

export LUMEN_ASSETS="${LUMEN_ASSETS:-$PWD/assets}"

profile=()
[ "${LUMEN_DEV_PROFILE:-release}" = release ] && profile=(--release)

args=("$@")
[ ${#args[@]} -eq 0 ] && args=(.)

exec cargo watch --why --clear \
  -w src -w assets -w Cargo.toml -w Cargo.lock \
  -x "run ${profile[*]:-} -- --new-window ${args[*]}"
