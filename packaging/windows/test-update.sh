#!/usr/bin/env bash
# End-to-end test of the in-app updater on Windows, against a local HTTP server and locally signed
# fake releases (the real release key is not involved):
#   installs the real lumen-text.exe with the setup (version V), serves a fake V+1 setup with a
#   signed latest.json, runs `lumen-text.exe --update` (the same path as "Update now" + "Restart"
#   minus the window) and asserts that the installed copy became V+1 with the settings kept.
#
#   test-update.sh <setup for V> <lumen-text.exe stamped V> <V>        (Git Bash on a Windows runner)
#
# Needs GHP_LSP_BIN (for build-setup.sh) and Inno Setup (ISCC) like build-setup.sh.
set -uo pipefail
[ $# -eq 3 ] || { echo "usage: $0 <setup.exe> <lumen-text.exe> <version>" >&2; exit 2; }
SETUP="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
EXE="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
V="$3"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IFS=. read -r a b c d <<<"$V"
V2="$a.$b.$c.$((d + 1))"
V3="$a.$b.$c.$((d + 2))"
PORT=18766
APPKEY='HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{B7C1E0D4-5A3F-4C8E-9E21-6F0A7D3C41B2}_is1'
# Native programs that take /SWITCHES must not have them rewritten as paths (but curl/unzip need real paths).
nc() { MSYS2_ARG_CONV_EXCL='*' "$@"; }

PY=python; "$PY" -c '' 2>/dev/null || PY=python3
W="$(mktemp -d)"; mkdir -p "$W/srv" "$W/inst"
win() { cygpath -w "$1"; }
FAILS=0
pass() { echo "  ok    $1"; }
fail() { echo "  FAIL  $1"; FAILS=$((FAILS + 1)); }
check() { local m="$1"; shift; if "$@" >/dev/null 2>&1; then pass "$m"; else fail "$m"; fi; }
dispver() { nc reg.exe query "$APPKEY" /v DisplayVersion 2>/dev/null | tr -d '\r' | awk '/DisplayVersion/{print $NF}'; }
is_ver() { [ "$(dispver)" = "$1" ]; }
wait_for() { local n="$1"; shift; for _ in $(seq "$n"); do "$@" && return 0; sleep 1; done; return 1; }
cleanup() {
  kill "${SERVER:-0}" 2>/dev/null
  [ -f "$W/inst/unins000.exe" ] && nc "$W/inst/unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART >/dev/null 2>&1
  sleep 3; rm -rf "$APPDATA/lumen-text/ci-marker"
}
trap cleanup EXIT

# minisign for Windows, pinned and checksummed
curl -fsSL --retry 3 -o "$W/ms.zip" https://github.com/jedisct1/minisign/releases/download/0.12/minisign-0.12-win64.zip
echo "37b600344e20c19314b2e82813db2bfdcc408b77b876f7727889dbd46d539479  $W/ms.zip" | sha256sum -c - || exit 1
unzip -q "$W/ms.zip" -d "$W/ms"
MS="$W/ms/minisign-win64/x86_64/minisign.exe"
"$MS" -G -W -f -p "$W/good.pub" -s "$W/good.key" >/dev/null
"$MS" -G -W -f -p "$W/evil.pub" -s "$W/evil.key" >/dev/null

make_release() { # make_release <version> <key name>: setup for that version, signed, plus latest.json
  local v="$1" key="$2" f="lumen-text-$1-windows-x86_64-setup.exe"
  rm -f "$W/srv/lumen-text-"*
  "$here/build-setup.sh" "$EXE" "$v" x86_64 "$W/srv" >/dev/null || return 1
  "$MS" -S -s "$W/$key.key" -m "$W/srv/$f" -x "$W/srv/$f.minisig" -t "lumen-text $v $f" >/dev/null
  printf '{"version":"%s","notes":"## What changed\\n- fake release for the e2e test","pub_date":"2099-01-01T00:00:00Z","platforms":{"windows-x86_64-setup":{"url":"http://127.0.0.1:%s/%s","signature":"%s"}}}\n' \
    "$v" "$PORT" "$f" "$(base64 -w0 "$W/srv/$f.minisig")" > "$W/srv/latest.json"
}

(cd "$W/srv" && exec "$PY" -m http.server "$PORT" --bind 127.0.0.1 >"$W/server.log" 2>&1) &
SERVER=$!
wait_for 30 curl -s -o /dev/null "http://127.0.0.1:$PORT/" || { echo "local server did not start" >&2; exit 1; }

echo "== install $V (the real exe) with the setup, silently"
nc "$SETUP" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER "/DIR=$(win "$W/inst")" /TASKS= >/dev/null 2>&1
wait_for 60 test -f "$W/inst/unins000.exe" || { fail "setup did not install"; exit 1; }
check "DisplayVersion is $V" test "$(dispver)" = "$V"
mkdir -p "$APPDATA/lumen-text"; echo keep > "$APPDATA/lumen-text/ci-marker"

export LUMEN_UPDATE_URL="http://127.0.0.1:$PORT/latest.json"
export LUMEN_UPDATE_PUBKEY="$(win "$W/good.pub")"
unset HOME XDG_CACHE_HOME XDG_DATA_HOME XDG_CONFIG_HOME XDG_STATE_HOME APPIMAGE
LUMEN="$W/inst/lumen-text.exe"
check "installed exe runs and prints $V" test "$("$LUMEN" --version | tr -d '\r')" = "lumen-text $V"

echo "== a release signed with another key is rejected, nothing changes"
make_release "$V2" evil || exit 1
out="$("$LUMEN" --check-update 2>&1)"; code=$?
echo "$out"
check "check: exit 10, announces $V2, sees an installer install" test "$code" = 10 -a -n "$(grep "Update available: $V2" <<<"$out")" -a -n "$(grep 'Windows installer' <<<"$out")"
out="$("$LUMEN" --update 2>&1)"; code=$?
echo "$out"
check "update: exit 1 with a signature error" test "$code" = 1 -a -n "$(grep -i signature <<<"$out")"
sleep 3
check "installed version unchanged ($V)" test "$(dispver)" = "$V"

echo "== a release signed with the right key is installed in place"
make_release "$V2" good || exit 1
out="$("$LUMEN" --update 2>&1)"; code=$?
echo "$out"
check "update: exit 0 and the installer is started" test "$code" = 0 -a -n "$(grep 'Installing' <<<"$out")"
check "installed version became $V2" wait_for 180 is_ver "$V2"
# the uninstall entry flips before the last files are written; wait for the installer to finish
setup_gone() { ! nc tasklist /FI "IMAGENAME eq lumen-text-$V2-windows-x86_64-setup.exe" 2>/dev/null | grep -qi "windows-x86_64-setup"; }
wait_for 60 setup_gone
check "installed exe still runs" test -n "$("$LUMEN" --version | tr -d '\r')"
check "install dir unchanged (in-place upgrade)" test -f "$W/inst/unins000.exe" -a -f "$W/inst/ghp-lsp.exe" -a -f "$W/inst/assets/language-manifest.json"
check "settings survived" test -f "$APPDATA/lumen-text/ci-marker"
check "one uninstall entry only" test "$(nc reg.exe query 'HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall' | tr -d '\r' | grep -c 'B7C1E0D4-5A3F-4C8E-9E21-6F0A7D3C41B2')" = 1

echo "== a replayed older signed setup is rejected"
cp "$W/srv/lumen-text-$V2-windows-x86_64-setup.exe" "$W/old.exe"; cp "$W/srv/lumen-text-$V2-windows-x86_64-setup.exe.minisig" "$W/old.minisig"
make_release "$V3" good || exit 1
cp "$W/old.exe" "$W/srv/lumen-text-$V3-windows-x86_64-setup.exe"; cp "$W/old.minisig" "$W/srv/lumen-text-$V3-windows-x86_64-setup.exe.minisig"
sed -i "s|\"signature\":\"[^\"]*\"|\"signature\":\"$(base64 -w0 "$W/old.minisig")\"|" "$W/srv/latest.json"
out="$("$LUMEN" --update 2>&1)"; code=$?
echo "$out"
check "replay: exit 1, another file or version" test "$code" = 1 -a -n "$(grep -i 'another file or version' <<<"$out")"
check "version still $V2" test "$(dispver)" = "$V2"

echo "$FAILS failure(s)"
[ "$FAILS" = 0 ]
