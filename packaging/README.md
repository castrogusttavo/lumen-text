# Packaging

All formats install the binary as `lumen-text` next to a `share/lumen-text/assets` directory,
because `config::assets_dir()` looks at `<exe dir>/../share/lumen-text/assets`
(overridable with `LUMEN_ASSETS`).

- `arch/PKGBUILD`: Arch package built from the GitHub release tarball with
  `cargo build --release --locked`. Installs `/usr/bin/lumen-text`,
  `/usr/share/lumen-text/assets`, the `.desktop` file and the README. Run `updpkgsums` after
  tagging. Build: `cd packaging/arch && makepkg -si`.
- `flatpak/io.github.lumen_text.yml`: flatpak-builder manifest (Freedesktop 24.08 + rust-stable
  extension; Wayland, X11 fallback, home access). For offline (Flathub) builds generate
  `cargo-sources.json` with flatpak-cargo-generator (see comments in the file). Build from the
  repo root: `flatpak-builder --user --install --force-clean build-dir packaging/flatpak/io.github.lumen_text.yml`.
  No icon is shipped yet.
- `appimage/build-appimage.sh`: builds the release binary, assembles an AppDir (binary, assets,
  `.desktop`, generated placeholder icon, `AppRun`) and calls `appimagetool` if available;
  otherwise prints how to download it. Output in `packaging/appimage/out/`.
