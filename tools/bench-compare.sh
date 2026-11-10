#!/usr/bin/env bash
# Real, reproducible editor comparison for the landing page.
#
# Measures Lumen, VS Code, Zed, Sublime Text and openidea on THIS machine and merges the results
# into web/src/data/benchmarks.json (schema kept; see web/README.md "Methodology").
#
#   tools/bench-compare.sh                      # everything, 5 runs per cell
#   tools/bench-compare.sh --runs 3             # fewer runs
#   tools/bench-compare.sh --only zed,lumen     # only these editors (comma list); others keep their cells
#   tools/bench-compare.sh --skip gui           # skip the GUI metrics (startup, RAM)   [gui|lsp|cli|disk]
#   tools/bench-compare.sh --no-merge           # measure only, leave benchmarks.json alone
#   tools/bench-compare.sh --merge-only         # just merge the results already in the cache
#
# Needs: a running Hyprland session (hyprctl) and grim for the GUI part (real windows open briefly
# on the current workspace, one editor at a time), node, npm, python3, curl, tar, pacman (Zed/Sublime sizes).
# Everything lives in ~/.cache/lumen-bench (sample project, VS Code tarball, TypeScript, per-editor
# profiles via XDG_*; your real editor configs are not touched). Nothing is written to /tmp.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(dirname "$here")"
export LUMEN_BENCH_DIR="${LUMEN_BENCH_DIR:-$HOME/.cache/lumen-bench}"
B="$LUMEN_BENCH_DIR"
runs=5; only="lumen,vscode,zed,sublime,openidea"; skip=","; merge=1; merge_only=0

while [ $# -gt 0 ]; do
  case "$1" in
    --runs) runs="$2"; shift 2;;
    --only) only="$2"; shift 2;;
    --skip) skip="$skip$2,"; shift 2;;
    --no-merge) merge=0; shift;;
    --merge-only) merge_only=1; shift;;
    -h|--help) sed -n '2,19p' "$0"; exit 0;;
    *) echo "unknown option $1" >&2; exit 2;;
  esac
done
has() { case ",$only," in *",$1,"*) return 0;; esac; return 1; }
skipped() { case "$skip" in *",$1,"*) return 0;; esac; return 1; }

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing dependency: $1" >&2; exit 1; }; }
for c in node npm python3 curl tar; do need "$c"; done
if [ "$merge_only" = 0 ]; then
  skipped gui || { need hyprctl; need grim; [ -n "${WAYLAND_DISPLAY:-}" ] || { echo "needs a Wayland (Hyprland) session" >&2; exit 1; }; }
  need lumen-text
fi

mkdir -p "$B/out" "$B/profiles"
log() { printf '\n== %s\n' "$*" >&2; }
free_gate() { while [ "$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)" -lt 1500 ]; do echo "waiting for >=1.5 GB available RAM" >&2; sleep 5; done; }

if [ "$merge_only" = 0 ]; then
  log "sample workspace"
  [ -f "$B/sample/big/big.ts" ] || node "$here/bench/gen-sample.mjs" "$B/sample"

  if has vscode && [ ! -x "$B/vscode/VSCode-linux-x64/code" ]; then
    log "downloading VS Code (official stable linux-x64 tarball) into $B/vscode"
    mkdir -p "$B/vscode-dl" "$B/vscode"
    curl -fL -o "$B/vscode-dl/vscode.tar.gz" https://update.code.visualstudio.com/latest/linux-x64/stable
    tar xzf "$B/vscode-dl/vscode.tar.gz" -C "$B/vscode"
  fi
  if ! skipped cli || ! skipped lsp; then
    [ -x "$B/ts/node_modules/.bin/tsc" ]  || (mkdir -p "$B/ts"  && cd "$B/ts"  && npm init -y >/dev/null && npm i typescript@7.0.2 >/dev/null 2>&1)
    [ -x "$B/ts6/node_modules/.bin/tsc" ] || (mkdir -p "$B/ts6" && cd "$B/ts6" && npm init -y >/dev/null && npm i typescript@6.0.3 >/dev/null 2>&1)
  fi

  if ! skipped gui; then
    for e in lumen vscode zed sublime openidea; do
      has "$e" || continue
      free_gate
      log "$e: warm-up launch (first-run initialisation, language-server downloads; discarded)"
      w=8; [ "$e" = zed ] && w=80
      python3 "$here/bench/measure.py" --editor "$e" --scenario warm --wait "$w" --out "$B/out/warm-$e.json" >&2 || true
      log "$e: idle scenario ($runs runs)"
      python3 "$here/bench/measure.py" --editor "$e" --scenario idle --runs "$runs" --out "$B/out/$e-idle.json" >&2
      log "$e: 100k-line file scenario ($runs runs)"
      python3 "$here/bench/measure.py" --editor "$e" --scenario big --runs "$runs" --out "$B/out/$e-big.json" >&2
    done
  fi

  if ! skipped lsp; then
    has lumen  && { log "tsgo (Lumen narrow mirror + full project)"; free_gate
                   node "$here/bench/lsp-bench.mjs" --mode tsgo-lumen --runs "$runs" --out "$B/out/lsp-tsgo-lumen.json" >&2
                   node "$here/bench/lsp-bench.mjs" --mode tsgo-full  --runs "$runs" --out "$B/out/lsp-tsgo-full.json" >&2; }
    has vscode && { log "tsserver bundled in VS Code"; free_gate
                   node "$here/bench/lsp-bench.mjs" --mode tsserver-vscode --runs "$runs" --out "$B/out/lsp-tsserver-vscode.json" >&2; }
    has zed    && { log "vtsls (Zed)"; free_gate
                   node "$here/bench/lsp-bench.mjs" --mode vtsls-zed --runs "$runs" --out "$B/out/lsp-vtsls-zed.json" >&2; }
  fi

  skipped cli  || { log "tsc vs tsgo, full check of the sample project"; python3 "$here/bench/cli-check.py" "$B" "$runs" "$B/out/cli.json" >&2; }
  skipped disk || { log "disk footprint"; python3 "$here/bench/disk.py" "$B" "$B/out/disk.json" >&2; }
fi

if [ "$merge" = 1 ]; then
  log "merging into web/src/data/benchmarks.json"
  python3 "$here/bench/merge.py" "$B" "$repo/web/src/data/benchmarks.json" "$here/bench/results"
fi
