#!/bin/sh
# Build and install lumen-text for the current user (no root needed).
set -e
cd "$(dirname "$0")"
cargo build --release
install -Dm755 target/release/lumen-text "$HOME/.local/bin/lumen-text"
mkdir -p "$HOME/.local/share/lumen-text"
rm -rf "$HOME/.local/share/lumen-text/assets"
cp -r assets "$HOME/.local/share/lumen-text/assets"
install -Dm644 packaging/icons/lumen-text.svg "$HOME/.local/share/icons/hicolor/scalable/apps/lumen-text.svg"
install -Dm644 lumen-text.desktop "$HOME/.local/share/applications/lumen-text.desktop"
echo "installed: ~/.local/bin/lumen-text (make sure ~/.local/bin is in PATH)"
echo "TypeScript support: npm i --prefix ~/.local/share/lumen-text/tsgo @typescript/native-preview"
