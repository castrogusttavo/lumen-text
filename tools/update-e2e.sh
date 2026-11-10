#!/usr/bin/env bash
# End-to-end test of the auto-updater against a local HTTP server and locally signed fake releases.
#
#   tools/update-e2e.sh [path/to/lumen-text]        (default: target/release/lumen-text)
#
# Needs python3, curl, tar and either `minisign` or docker (a tiny ubuntu image with minisign is
# built once as `lumen-e2e-minisign`). Everything happens under ~/.cache/lumen-e2e (never /tmp);
# the keys are throwaway ones generated for the run, the real release key is not involved.
# The binary under test is only run with a fake HOME / XDG dirs and LUMEN_UPDATE_URL pointing at
# 127.0.0.1, so nothing touches your installation or the network.
set -uo pipefail

BIN="$(realpath "${1:-target/release/lumen-text}")"
ROOT="${LUMEN_E2E_DIR:-$HOME/.cache/lumen-e2e}"
W="$ROOT/work"
PORT="${LUMEN_E2E_PORT:-18765}"
rm -rf "$W"; mkdir -p "$W/srv" "$W/home" "$W/proj" "$W/snap"
echo hello > "$W/proj/a.txt"
FAILS=0

pass() { echo "  ok    $1"; }
fail() { echo "  FAIL  $1"; FAILS=$((FAILS + 1)); }
check() { # check <description> <command...>
  local d="$1"; shift
  if "$@" >/dev/null 2>&1; then pass "$d"; else fail "$d"; fi
}

# ── signing ──
mk_keys() { # mk_keys <name>
  if command -v minisign >/dev/null; then
    minisign -G -W -f -p "$W/$1.pub" -s "$W/$1.key" >/dev/null
  else
    docker run --rm -u "$(id -u):$(id -g)" -v "$W:/w" lumen-e2e-minisign minisign -G -W -f -p "/w/$1.pub" -s "/w/$1.key" >/dev/null
  fi
}
sign() { # sign <file under $W/srv> <key name> <trusted comment>
  if command -v minisign >/dev/null; then
    minisign -S -s "$W/$2.key" -m "$W/srv/$1" -x "$W/srv/$1.minisig" -t "$3" >/dev/null
  else
    docker run --rm -u "$(id -u):$(id -g)" -v "$W:/w" lumen-e2e-minisign minisign -S -s "/w/$2.key" -m "/w/srv/$1" -x "/w/srv/$1.minisig" -t "$3" >/dev/null
  fi
}
if ! command -v minisign >/dev/null; then
  docker image inspect lumen-e2e-minisign >/dev/null 2>&1 || docker build -q -t lumen-e2e-minisign - >/dev/null <<'EOF'
FROM ubuntu:24.04
RUN apt-get update -qq && apt-get install -y -qq minisign && rm -rf /var/lib/apt/lists/*
EOF
fi
mk_keys good; mk_keys evil
PUB="$W/good.pub"

# ── fake releases ──
MARKER="$W/restarted.txt"
make_release() { # make_release <version> [key] : AppImage + tarball + latest.json, signed
  local v="$1" key="${2:-good}" t="$W/pkg/lumen-text-$1-linux-x86_64"
  rm -rf "$W/pkg"; mkdir -p "$t/bin" "$t/share/lumen-text/assets" "$W/srv"
  printf '#!/bin/sh\nif [ "$1" = "--version" ]; then echo "lumen-text %s"; else echo "$@" > "%s"; fi\n' "$v" "$MARKER" > "$t/bin/lumen-text"
  chmod 755 "$t/bin/lumen-text"
  echo '{}' > "$t/share/lumen-text/assets/language-manifest.json"
  tar -C "$W/pkg" -czf "$W/srv/lumen-text-$v-linux-x86_64.tar.gz" "lumen-text-$v-linux-x86_64"
  printf '#!/bin/sh\necho "lumen-text %s (fake appimage)"\n' "$v" > "$W/srv/lumen-text-$v-x86_64.AppImage"
  for f in "lumen-text-$v-linux-x86_64.tar.gz" "lumen-text-$v-x86_64.AppImage"; do sign "$f" "$key" "lumen-text $v $f"; done
  python3 - "$v" "$W/srv" <<'PY'
import base64, json, sys
v, d = sys.argv[1], sys.argv[2]
def plat(name, signed=True):
    p = {"url": f"http://127.0.0.1:{__import__('os').environ.get('PORT', '18765')}/{name}", "signature": ""}
    if signed:
        p["signature"] = base64.b64encode(open(f"{d}/{name}.minisig", "rb").read()).decode()
    return p
json.dump({
    "version": v,
    "notes": "## What's Changed\n* Fake release for the e2e test\n* Second line of notes",
    "pub_date": "2026-11-10T00:00:00Z",
    "platforms": {
        "linux-x86_64-appimage": plat(f"lumen-text-{v}-x86_64.AppImage"),
        "linux-x86_64-tarball": plat(f"lumen-text-{v}-linux-x86_64.tar.gz"),
    },
}, open(f"{d}/latest.json", "w"), indent=2)
PY
}
export PORT
(cd "$W/srv" && exec python3 -m http.server "$PORT" --bind 127.0.0.1 >"$W/server.log" 2>&1) &
SERVER=$!
trap 'kill $SERVER 2>/dev/null; wait $SERVER 2>/dev/null' EXIT
for _ in $(seq 50); do curl -s -o /dev/null "http://127.0.0.1:$PORT/" && break; sleep 0.1; done

export LUMEN_UPDATE_URL="http://127.0.0.1:$PORT/latest.json"
export LUMEN_UPDATE_PUBKEY="$PUB"
export XDG_CACHE_HOME="$W/cache" XDG_CONFIG_HOME="$W/cfg" XDG_STATE_HOME="$W/state"
export XDG_DATA_HOME="$W/home/.local/share" HOME="$W/home"
unset APPIMAGE

V1=2999.1.1.1; V2=2999.1.1.2; V3=2999.1.1.3

echo "== check: up to date / available"
make_release 2000.1.1.1
out="$("$BIN" --check-update 2>&1)"; code=$?
check "older release: exit 0 and 'Up to date'" test "$code" = 0 -a -n "$(grep 'Up to date' <<<"$out")"
make_release $V1
out="$("$BIN" --check-update 2>&1)"; code=$?
check "newer release: exit 10 and names the version" test "$code" = 10 -a -n "$(grep "Update available: $V1" <<<"$out")"
check "no leftovers in the cache after a check" test ! -e "$XDG_CACHE_HOME/lumen-text/update/latest.json.part"

echo "== AppImage: verify, replace, keep .old"
mkdir -p "$W/apps"; printf 'old appimage\n' > "$W/apps/Lumen.AppImage"; chmod 755 "$W/apps/Lumen.AppImage"
out="$(APPIMAGE="$W/apps/Lumen.AppImage" "$BIN" --update 2>&1)"; code=$?
check "update exits 0" test "$code" = 0
check "AppImage is the new file" cmp -s "$W/apps/Lumen.AppImage" "$W/srv/lumen-text-$V1-x86_64.AppImage"
check "previous AppImage kept as .old" grep -q 'old appimage' "$W/apps/Lumen.AppImage.old"
check "AppImage is executable" test -x "$W/apps/Lumen.AppImage"
check "no .new left behind" test ! -e "$W/apps/Lumen.AppImage.new"
check "cache cleaned after install" test -z "$(ls -A "$XDG_CACHE_HOME/lumen-text/update" 2>/dev/null)"

echo "== bad files are rejected, installed file untouched"
printf 'old appimage\n' > "$W/apps/Lumen.AppImage"
cp "$W/srv/lumen-text-$V1-x86_64.AppImage" "$W/good.AppImage"
echo tamper >> "$W/srv/lumen-text-$V1-x86_64.AppImage"
out="$(APPIMAGE="$W/apps/Lumen.AppImage" "$BIN" --update 2>&1)"; code=$?
check "tampered download: exit 1" test "$code" = 1
check "tampered download: signature error is reported" grep -qi 'signature' <<<"$out"
check "tampered download: installed file untouched" grep -q 'old appimage' "$W/apps/Lumen.AppImage"
check "tampered download: nothing left in the cache" test -z "$(find "$XDG_CACHE_HOME/lumen-text/update" -type f 2>/dev/null)"
make_release $V1 evil
out="$(APPIMAGE="$W/apps/Lumen.AppImage" "$BIN" --update 2>&1)"; code=$?
check "signed by another key: rejected" test "$code" = 1 -a -n "$(grep -i 'signature' <<<"$out")"
make_release $V1
cp "$W/srv/lumen-text-$V1-x86_64.AppImage" "$W/v1.AppImage"; cp "$W/srv/lumen-text-$V1-x86_64.AppImage.minisig" "$W/v1.minisig"
make_release $V2
# Replay: the manifest announces V2 but serves the (validly signed) V1 file and signature.
cp "$W/v1.AppImage" "$W/srv/lumen-text-$V2-x86_64.AppImage"; cp "$W/v1.minisig" "$W/srv/lumen-text-$V2-x86_64.AppImage.minisig"
python3 - "$W/srv" "$V2" <<'PY'
import base64, json, sys
d, v = sys.argv[1:3]
m = json.load(open(f"{d}/latest.json"))
m["platforms"]["linux-x86_64-appimage"]["signature"] = base64.b64encode(open(f"{d}/lumen-text-{v}-x86_64.AppImage.minisig", "rb").read()).decode()
json.dump(m, open(f"{d}/latest.json", "w"))
PY
out="$(APPIMAGE="$W/apps/Lumen.AppImage" "$BIN" --update 2>&1)"; code=$?
check "replayed old signed file: rejected" test "$code" = 1 -a -n "$(grep -i 'another file or version' <<<"$out")"
check "replay: installed file untouched" grep -q 'old appimage' "$W/apps/Lumen.AppImage"

echo "== installs that cannot update themselves only notify"
make_release $V1
out="$("$BIN" --update 2>&1)"; code=$?
check "unmanaged install: exit 10 and explains how to get updates" test "$code" = 10 -a -n "$(grep -- '--install-user' <<<"$out")"

echo "== --install-user"
out="$("$BIN" --install-user 2>&1)"; code=$?
VER="$("$BIN" --version | cut -d' ' -f2)"
D="$W/home/.local/share"
check "install-user exits 0" test "$code" = 0
check "version dir has bin and assets" test -x "$D/lumen-text/$VER/bin/lumen-text" -a -f "$D/lumen-text/$VER/share/lumen-text/assets/language-manifest.json"
check "current points at the version" test "$(readlink "$D/lumen-text/current")" = "$VER"
check "~/.local/bin/lumen-text is a working symlink" test -L "$W/home/.local/bin/lumen-text" -a -x "$W/home/.local/bin/lumen-text"
check "installed command runs" test "$("$W/home/.local/bin/lumen-text" --version)" = "lumen-text $VER"
check "desktop entry has an absolute Exec at the symlink" grep -qx "Exec=$W/home/.local/bin/lumen-text %F" "$D/applications/lumen-text.desktop"
check "icon is installed under hicolor" test -s "$D/icons/hicolor/scalable/apps/lumen-text.svg"
if command -v desktop-file-validate >/dev/null; then check "desktop entry validates" desktop-file-validate "$D/applications/lumen-text.desktop"; fi
out="$("$W/home/.local/bin/lumen-text" --check-update 2>&1)"
check "installed copy recognises the user layout" grep -q 'user install' <<<"$out"

echo "== user layout: update, flip current, keep two versions"
INST="$D/lumen-text/$VER/bin/lumen-text"
"$INST" --update >/dev/null 2>&1; code=$?
check "update to $V1 exits 0" test "$code" = 0
check "current -> $V1" test "$(readlink "$D/lumen-text/current")" = "$V1"
check "command now runs the new version" test "$("$W/home/.local/bin/lumen-text" --version)" = "lumen-text $V1"
check "previous version dir kept" test -d "$D/lumen-text/$VER"
make_release $V2; "$INST" --update >/dev/null 2>&1
make_release $V3; "$INST" --update >/dev/null 2>&1
check "current -> $V3" test "$(readlink "$D/lumen-text/current")" = "$V3"
check "old versions pruned (the running one and the previous stay)" test ! -d "$D/lumen-text/$V1" -a -d "$D/lumen-text/$V2" -a -d "$D/lumen-text/$VER"

echo "== GUI (headless): auto download, card, restart"
rm -rf "$D/lumen-text" "$W/home/.local/bin" "$W/cache"; unset APPIMAGE
"$BIN" --install-user >/dev/null 2>&1
INST="$D/lumen-text/$VER/bin/lumen-text"
make_release $V1
export LUMEN_UPDATE_DELAY_MS=100
"$INST" --size 1000x700 --snap "$W/snap" --keys "wait:3500;snap:auto-ready" "$W/proj" >"$W/gui1.log" 2>&1
check "auto mode installed the new version in the background" test "$(readlink "$D/lumen-text/current")" = "$V1"
check "snapshot written" test -s "$W/snap/auto-ready.png"
mkdir -p "$XDG_CONFIG_HOME/lumen-text"; echo '{"updates": "notify"}' > "$XDG_CONFIG_HOME/lumen-text/config.json"
make_release $V2
"$INST" --size 1000x700 --snap "$W/snap" --keys "wait:3000;snap:notify" "$W/proj" >"$W/gui2.log" 2>&1
check "notify mode downloads nothing" test "$(readlink "$D/lumen-text/current")" = "$V1"
echo '{"updates": "off"}' > "$XDG_CONFIG_HOME/lumen-text/config.json"
"$INST" --size 1000x700 --snap "$W/snap" --keys "wait:2000;snap:off-quiet;ctrl+shift+p;type:verificar atualiz;enter;wait:2500;snap:off-manual" "$W/proj" >"$W/gui3.log" 2>&1
check "off mode: manual check still works (card snapshot)" test -s "$W/snap/off-manual.png"
check "off mode: nothing installed" test "$(readlink "$D/lumen-text/current")" = "$V1"
echo '{}' > "$XDG_CONFIG_HOME/lumen-text/config.json"
rm -f "$MARKER"
"$INST" --size 1000x700 --snap "$W/snap" --keys "wait:3500;snap:before-restart;ctrl+shift+p;type:reiniciar para atual;enter;wait:3000;snap:never" "$W/proj" >"$W/gui4.log" 2>&1
for _ in $(seq 40); do [ -s "$MARKER" ] && break; sleep 0.1; done
check "restart started the new executable with the project" grep -q "$W/proj" "$MARKER"
check "restart quit the old process before its last snapshot" test ! -e "$W/snap/never.png"

echo
if [ "$FAILS" = 0 ]; then echo "update e2e: all passed"; else echo "update e2e: $FAILS FAILED"; fi
exit "$FAILS"
