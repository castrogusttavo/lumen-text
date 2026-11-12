#!/usr/bin/env bash
# Build the macOS app: "Lumen Text.app" (ad-hoc signed) inside lumen-text-<version>-macos-<arch>.dmg (+ .sha256).
#   build-app.sh <binary> <version> <arch> <outdir>      arch: aarch64 | x86_64
# The app is NOT notarized (that needs a paid Apple Developer ID): on first launch macOS asks the user
# to confirm it, see docs (getting-started: "Opening the app on macOS").
set -euo pipefail
[ $# -eq 4 ] || { echo "usage: $0 <binary> <version> <arch> <outdir>" >&2; exit 2; }
bin="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; version="$2"; arch="$3"; out="$4"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
[[ "$version" =~ ^[0-9]+(\.[0-9]+)*$ ]] || { echo "bad version: $version" >&2; exit 2; }

mkdir -p "$out"; out="$(cd "$out" && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
app="$work/Lumen Text.app"
install -d "$app/Contents/MacOS" "$app/Contents/Resources"
install -m755 "$bin" "$app/Contents/MacOS/lumen-text"
# The GHP language server next to the editor binary (GHP_LSP_BIN=path/to/ghp-lsp).
[ -n "${GHP_LSP_BIN:-}" ] && install -m755 "$GHP_LSP_BIN" "$app/Contents/MacOS/ghp-lsp"
cp -R "$root/assets" "$app/Contents/Resources/assets"
# Licensed HugeIcons Pro masks live inside the binary only, never as loose files.
rm -rf "$app/Contents/Resources/assets/icons/hugeicons" "$app/Contents/Resources/assets/icons/hugeicons_bulk"
cp "$root/LICENSE" "$app/Contents/Resources/LICENSE"

# AppIcon.icns from the brand icon.
iconset="$work/AppIcon.iconset"; mkdir "$iconset"
src="$root/assets/brand/icon.png"
for s in 16 32 128 256 512; do
  sips -z $s $s "$src" --out "$iconset/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) "$src" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Lumen Text</string>
  <key>CFBundleDisplayName</key><string>Lumen Text</string>
  <key>CFBundleIdentifier</key><string>io.github.lumen-text</string>
  <key>CFBundleExecutable</key><string>lumen-text</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Text document</string>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array><string>public.text</string><string>public.source-code</string><string>public.data</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST

# Ad-hoc signature: keeps the bundle valid on Apple Silicon (unsigned arm64 code does not run).
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"

name="lumen-text-${version}-macos-${arch}"
dmgdir="$work/dmg"; mkdir "$dmgdir"
cp -R "$app" "$dmgdir/"
ln -s /Applications "$dmgdir/Applications"
rm -f "$out/$name.dmg"
# hdiutil sporadically fails with "Resource busy" on CI runners (Spotlight/diskarbitrationd): retry.
for attempt in 1 2 3 4 5; do
  hdiutil create -volname "Lumen Text" -srcfolder "$dmgdir" -fs HFS+ -format UDZO -ov "$out/$name.dmg" >/dev/null && break
  [ "$attempt" = 5 ] && { echo "hdiutil create failed after $attempt attempts" >&2; exit 1; }
  echo "hdiutil busy, retrying ($attempt)..." >&2
  sleep $((attempt * 5))
done
(cd "$out" && shasum -a 256 "$name.dmg" > "$name.dmg.sha256")
echo "Built $out/$name.dmg"
