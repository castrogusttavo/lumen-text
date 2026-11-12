#!/usr/bin/env bash
# Installs the language servers the end-to-end LSP test (`src/lsp_matrix.rs`) exercises, into
# Lumen's own folder (~/.local/share/lumen-text/lsp, executables linked in lsp/bin), plus the
# bundled tsgo. Used by CI (job "Language servers") and handy on a dev machine. Linux only.
#
# Needs on PATH: curl, tar, xz, unzip, node + npm, python3 (with venv), Java 25+ (jdtls), dotnet 10,
# go, ruby + gem, php, rustup (or rust-analyzer is downloaded), and clangd from the system.
#
#   tools/install-lsp-servers.sh            # everything
#   tools/install-lsp-servers.sh npm zig    # only some parts (names: see the `want` calls below)
#
# Re-running is cheap: a part that is already installed is skipped (delete its folder to update).
set -euo pipefail

# ── pinned versions (bump deliberately; the matrix test is the check) ──
NPM_PACKAGES=(
  "vscode-langservers-extracted@4.10.0"   # json, css/scss/less, html
  "yaml-language-server@2.0.0"
  "bash-language-server@5.8.1"
  "dockerfile-language-server-nodejs@0.15.0"
  "basedpyright@1.40.2"
  "intelephense@1.18.5"
  "svelte-language-server@0.18.6"
  "@astrojs/language-server@2.17.3"
  "@vue/language-server@3.3.14"
  "@tailwindcss/language-server@0.16.0"
  "typescript@6.0.3"
)
PYRIGHT=1.1.414
TSGO=7.0.0-dev.20260707.2
RUFF=0.17.4
PYLSP=1.15.0
LUA_LS=3.19.2
MARKSMAN=2026-02-08
TAPLO=0.10.0
PHPACTOR=2026.10.07.0
ZIG=0.15.2
ZLS=0.15.1
CSHARP_LS=0.28.0
RUBY_LSP=0.26.11
GOPLS=v0.24.0
KOTLIN_LSP=263.7084.0
RUST_ANALYZER=2026-11-09   # only when rustup is not available
# Spell checkers (companions, used when `spell_check` is on): release archives, checked against these sums.
HARPER=3.0.0
HARPER_SHA256_x86_64=17fd8dfea277ee9d1fc536a1baf596c6acb7172ea79f77dcd8ee2400f55a88a3
HARPER_SHA256_aarch64=374a7a267a3c68b1ccfa1e2d489a19fc96cd48390406f9b40cb0da1a8677e3ed
TYPOS_LSP=0.1.57
TYPOS_LSP_SHA256_x86_64=00da356302c54cb6daeaaa7ee883a0d5a6557c9418466fcbd8d2f25fcea51779
TYPOS_LSP_SHA256_aarch64=63e74e98e09333c94ca3454907d89c5b78ff1a8b5d5940a9136f14cea4eba84e

DATA="${XDG_DATA_HOME:-$HOME/.local/share}/lumen-text"
# lsp/bin is looked up under the home folder (src/lspreg.rs `user_tool_dirs`), whatever XDG says.
DIR="$HOME/.local/share/lumen-text/lsp"
BIN="$DIR/bin"
mkdir -p "$BIN"

case "$(uname -m)" in
  x86_64) A=x86_64 NODE_A=x64 GO_A=amd64 ;;
  aarch64 | arm64) A=aarch64 NODE_A=arm64 GO_A=arm64 ;;
  *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

ONLY=("$@")
want() { # want <part>: true when no parts were named or <part> is one of them
  [ ${#ONLY[@]} -eq 0 ] && return 0
  local p; for p in "${ONLY[@]}"; do [ "$p" = "$1" ] && return 0; done
  return 1
}
step() { printf '\n── %s\n' "$*"; }
link() { ln -sf "$1" "$BIN/$2"; }  # link <target relative to bin/ or absolute> <name>
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
fetch() { curl -fsSL --retry 4 --retry-delay 3 "$1" -o "$2"; }
verify() { echo "$2  $1" | sha256sum -c --quiet - || { echo "checksum mismatch: $1" >&2; exit 1; }; }  # verify <file> <sha256>

if want npm; then
  step "npm servers"
  (cd "$DIR" && { [ -f package.json ] || echo '{"private":true}' > package.json; } \
    && npm install --no-audit --no-fund --loglevel=error "${NPM_PACKAGES[@]}")
  for b in vscode-json-language-server vscode-css-language-server vscode-html-language-server \
    yaml-language-server bash-language-server docker-langserver basedpyright-langserver intelephense \
    svelteserver astro-ls vue-language-server tailwindcss-language-server; do
    link "../node_modules/.bin/$b" "$b"
  done
  # pyright gets its own prefix: basedpyright ships a `pyright-langserver` too.
  mkdir -p "$DIR/pyright"
  (cd "$DIR/pyright" && { [ -f package.json ] || echo '{"private":true}' > package.json; } \
    && npm install --no-audit --no-fund --loglevel=error "pyright@$PYRIGHT")
  link ../pyright/node_modules/.bin/pyright-langserver pyright-langserver
fi

if want tsgo; then
  step "tsgo $TSGO"
  mkdir -p "$DATA/tsgo"
  (cd "$DATA/tsgo" && { [ -f package.json ] || echo '{"private":true}' > package.json; } \
    && npm install --no-audit --no-fund --loglevel=error "@typescript/native-preview@$TSGO")
fi

if want python; then
  step "ruff $RUFF, python-lsp-server $PYLSP"
  [ -x "$DIR/venv/bin/python" ] || python3 -m venv "$DIR/venv"
  "$DIR/venv/bin/pip" install -q --disable-pip-version-check "ruff==$RUFF" "python-lsp-server==$PYLSP"
  link ../venv/bin/ruff ruff
  link ../venv/bin/pylsp pylsp
fi

if want rust; then
  step "rust-analyzer"
  if command -v rustup >/dev/null; then
    rustup component add rust-analyzer rust-src
  elif [ ! -x "$BIN/rust-analyzer" ]; then
    fetch "https://github.com/rust-lang/rust-analyzer/releases/download/$RUST_ANALYZER/rust-analyzer-$A-unknown-linux-gnu.gz" "$BIN/rust-analyzer.gz"
    gunzip -f "$BIN/rust-analyzer.gz" && chmod +x "$BIN/rust-analyzer"
  fi
fi

if want go; then
  step "gopls $GOPLS"
  [ -x "$BIN/gopls" ] || GOBIN="$BIN" GOFLAGS=-mod=mod go install "golang.org/x/tools/gopls@$GOPLS"
fi

if want taplo && [ ! -x "$BIN/taplo" ]; then
  step "taplo $TAPLO"
  fetch "https://github.com/tamasfe/taplo/releases/download/$TAPLO/taplo-linux-$A.gz" "$BIN/taplo.gz"
  gunzip -f "$BIN/taplo.gz" && chmod +x "$BIN/taplo"
fi

if want marksman && [ ! -x "$BIN/marksman" ]; then
  step "marksman $MARKSMAN"
  m=x64; [ "$A" = aarch64 ] && m=arm64
  fetch "https://github.com/artempyanykh/marksman/releases/download/$MARKSMAN/marksman-linux-$m" "$BIN/marksman"
  chmod +x "$BIN/marksman"
fi

if want lua && [ ! -x "$DIR/lua-ls/bin/lua-language-server" ]; then
  step "lua-language-server $LUA_LS"
  mkdir -p "$DIR/lua-ls"
  fetch "https://github.com/LuaLS/lua-language-server/releases/download/$LUA_LS/lua-language-server-$LUA_LS-linux-$NODE_A.tar.gz" "$TMP/lua-ls.tgz"
  tar -xzf "$TMP/lua-ls.tgz" -C "$DIR/lua-ls" && rm "$TMP/lua-ls.tgz"
fi
want lua && link ../lua-ls/bin/lua-language-server lua-language-server

if want zig; then
  if [ ! -x "$DIR/zig/zig" ]; then
    step "zig $ZIG"
    fetch "https://ziglang.org/download/$ZIG/zig-$A-linux-$ZIG.tar.xz" "$TMP/zig.txz"
    mkdir -p "$DIR/zig" && tar -xJf "$TMP/zig.txz" -C "$DIR/zig" --strip-components=1 && rm "$TMP/zig.txz"
  fi
  link ../zig/zig zig
  if [ ! -x "$BIN/zls" ]; then
    step "zls $ZLS"
    fetch "https://github.com/zigtools/zls/releases/download/$ZLS/zls-$A-linux.tar.xz" "$TMP/zls.txz"
    tar -xJf "$TMP/zls.txz" -C "$BIN" zls && rm "$TMP/zls.txz"
  fi
fi

if want php && [ ! -x "$BIN/phpactor" ]; then
  step "phpactor $PHPACTOR"
  fetch "https://github.com/phpactor/phpactor/releases/download/$PHPACTOR/phpactor.phar" "$BIN/phpactor"
  chmod +x "$BIN/phpactor"
fi

if want java && [ ! -x "$DIR/jdtls/bin/jdtls" ]; then
  step "jdtls (latest snapshot)"
  fetch https://download.eclipse.org/jdtls/snapshots/jdt-language-server-latest.tar.gz "$TMP/jdtls.tgz"
  mkdir -p "$DIR/jdtls" && tar -xzf "$TMP/jdtls.tgz" -C "$DIR/jdtls" && rm "$TMP/jdtls.tgz"
fi
want java && link ../jdtls/bin/jdtls jdtls

if want csharp && [ ! -x "$DIR/dotnet-tools/csharp-ls" ]; then
  step "csharp-ls $CSHARP_LS"
  dotnet tool install csharp-ls --version "$CSHARP_LS" --tool-path "$DIR/dotnet-tools"
fi
want csharp && link ../dotnet-tools/csharp-ls csharp-ls

if want kotlin && [ ! -x "$DIR/kotlin-lsp/kotlin-lsp.sh" ]; then
  # JetBrains' server; its license terms: `kotlin-lsp --show-eula`.
  step "kotlin-lsp $KOTLIN_LSP"
  k=""; [ "$A" = aarch64 ] && k="-aarch64"
  fetch "https://download.jetbrains.com/language-server/kotlin-server/$KOTLIN_LSP/kotlin-server-$KOTLIN_LSP$k.tar.gz" "$TMP/kotlin-lsp.tgz"
  # The archive wraps everything in kotlin-server-<version>/.
  mkdir -p "$DIR/kotlin-lsp" && tar -xzf "$TMP/kotlin-lsp.tgz" -C "$DIR/kotlin-lsp" --strip-components=1
  chmod +x "$DIR/kotlin-lsp/kotlin-lsp.sh"
fi
want kotlin && link ../kotlin-lsp/kotlin-lsp.sh kotlin-lsp

if want ruby; then
  step "ruby-lsp $RUBY_LSP"
  # Distro Rubies (Arch, Fedora) can't write to the system gem folder: install for the user.
  user=(); [ -w "$(gem env gemdir)" ] || user=(--user-install)
  gem list -i ruby-lsp -v "$RUBY_LSP" >/dev/null || gem install "${user[@]}" --no-document ruby-lsp -v "$RUBY_LSP"
  for d in "$(ruby -e 'print Gem.bindir')" "$(ruby -e 'print Gem.user_dir')/bin"; do
    if [ -x "$d/ruby-lsp" ]; then link "$d/ruby-lsp" ruby-lsp; break; fi
  done
fi

if want spell; then
  if [ ! -x "$BIN/harper-ls" ]; then
    step "harper-ls $HARPER"
    fetch "https://github.com/Automattic/harper/releases/download/v$HARPER/harper-ls-$A-unknown-linux-gnu.tar.gz" "$TMP/harper.tgz"
    sum="HARPER_SHA256_$A"; verify "$TMP/harper.tgz" "${!sum}"
    tar -xzf "$TMP/harper.tgz" -C "$BIN" harper-ls && rm "$TMP/harper.tgz"
  fi
  if [ ! -x "$BIN/typos-lsp" ]; then
    step "typos-lsp $TYPOS_LSP"
    fetch "https://github.com/tekumara/typos-lsp/releases/download/v$TYPOS_LSP/typos-lsp-v$TYPOS_LSP-$A-unknown-linux-gnu.tar.gz" "$TMP/typos-lsp.tgz"
    sum="TYPOS_LSP_SHA256_$A"; verify "$TMP/typos-lsp.tgz" "${!sum}"
    tar -xzf "$TMP/typos-lsp.tgz" -C "$BIN" typos-lsp && rm "$TMP/typos-lsp.tgz"
  fi
fi

if want clangd && ! command -v clangd >/dev/null; then
  echo "note: clangd is not installed (C/C++); install it with the system package manager" >&2
fi

step "installed in $BIN"
ls "$BIN"
