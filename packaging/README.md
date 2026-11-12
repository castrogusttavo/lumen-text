# Packaging

Every push to `main` that passes the tests produces a GitHub Release (CalVer `YYYY.M.D.N`, tag
`vYYYY.M.D.N`) with the files below. They are built by `.github/workflows/ci.yml`
(`version` -> `build-linux` / `build-windows` / `build-macos` -> one `release` job); the scripts in this directory do the
actual work and can be run locally with any release binary.

| File | Format | Script |
| --- | --- | --- |
| `lumen-text-<v>-linux-<arch>.tar.gz` + `.sha256` | portable archive | `tarball/build-tarball.sh` |
| `lumen-text_<v>-1_<amd64\|arm64>.deb` | Debian, Ubuntu, Mint, ... | `deb/build-deb.sh` |
| `lumen-text-<v>-1.<x86_64\|aarch64>.rpm` | Fedora, RHEL family, openSUSE | `rpm/build-rpm.sh` + `rpm/lumen-text.spec` |
| `lumen-text-bin-<v>-1-x86_64.pkg.tar.zst` | Arch Linux, Manjaro, ... | `arch/build-arch.sh` + `arch/lumen-text-bin/PKGBUILD` |
| `lumen-text-<v>-<arch>.AppImage` | any distribution | `appimage/build-appimage.sh` |
| `lumen-text-<v>-windows-x86_64-setup.exe` + `.sha256` | Windows installer (Inno Setup, per user, self-updating, not code-signed) | `windows/build-setup.sh` + `windows/lumen-text.iss` |
| `lumen-text-<v>-windows-x86_64.zip` + `.sha256` | portable Windows archive (exe + assets, not code-signed) | `windows/build-zip.sh` |
| `lumen-text-<v>-macos-<aarch64\|x86_64>.dmg` + `.sha256` | `Lumen Text.app`, ad-hoc signed, not notarized | `macos/build-app.sh` |
| `SHA256SUMS` | checksums of all of the above | (release job) |

`<v>` is the CalVer version, e.g. `2026.11.10.1`. aarch64 Linux files are only produced when the
repository variable `ENABLE_ARM_BUILDS` is `true` (GitHub-hosted arm runners are not available to
private repositories). `.github/workflows/platforms.yml` builds and tests every OS on the `platforms` branch.

## Updates and signing

CI signs the AppImage, the tarballs, the Windows zip and installer and the macOS dmgs (`<file>.minisig`, minisign) and publishes `latest.json` with
all package URLs; the app's updater reads it. The user layout it maintains (and `lumen-text
--install-user` creates) is `~/.local/share/lumen-text/<version>/{bin,share}` plus a `current`
symlink, i.e. exactly the tarball contents, so the layout contract below holds there too. See
CONTRIBUTING.md (Releases) for keys, secrets and rotation.

## Windows installer

`windows/build-setup.sh <exe> <v> x86_64 <outdir>` stages the files with `windows/stage.sh` (the same list the zip uses),
turns `icons/png` into an `.ico` (`windows/make-ico.py`) and compiles `windows/lumen-text.iss` with Inno Setup (pinned
in `.github/actions/setup-innosetup`; `choco install innosetup` locally, or set `ISCC`). The script hands the staged
directory to the `.iss` as `/DStageDir=...`, so adding a file to `stage.sh` ships it in both formats.

- Per user by default (`PrivilegesRequired=lowest`: `%LOCALAPPDATA%\Programs\Lumen`, no admin); the dialog or `/ALLUSERS`
  installs for all users. Fixed `AppId` (`{B7C1E0D4-5A3F-4C8E-9E21-6F0A7D3C41B2}`), so a newer setup upgrades in place; `assets/`
  is replaced wholesale. A running Lumen is closed through Restart Manager; the PC is never restarted.
- Tasks: desktop icon (off), user `PATH` (on, `HKCU\Environment`, removed on uninstall), *Open with Lumen* / *Open folder in
  Lumen* context menus (on), *Open with* entries for common file types (off, never takes over a default program).
- Settings (`%APPDATA%\lumen-text`) and caches (`%LOCALAPPDATA%\lumen-text`) live outside the install directory. The
  uninstaller keeps them; interactively it asks whether to delete them (default No); silently it never does.
- Silent: `setup.exe /VERYSILENT /SUPPRESSMSGBOXES /CURRENTUSER` plus optionally `/DIR=`, `/TASKS=`, `/LOG=`, `/RELAUNCH=1`
  (start Lumen when done). Uninstall: `<dir>\unins000.exe /VERYSILENT`.
- Not code-signed: SmartScreen warns on first run.

**Self-update.** `src/update.rs` recognises an installer copy (`InstallKind::Setup`: `lumen-text.exe` next to `unins000.exe`
under the user's profile) and uses the `windows-x86_64-setup` entry of `latest.json`: download, minisign check (same key
and trusted comment as every other file), then on *Restart now* (or `lumen-text --update`) the app starts the verified setup
silently (`/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER /CLOSEAPPLICATIONS /DIR=<install dir> /RELAUNCH=1`) and exits;
the installer replaces the files in place, keeps the settings and starts Lumen again. The portable zip and an all-users
install (updating it needs elevation) only announce new versions with a link to the release page.

**Tests.** `windows/test-installer.ps1` (silent install, assertions on files, uninstall entry, shortcuts, `PATH`, context
menus, `--version`, upgrade over the previous install, silent uninstall, settings kept) and `windows/test-update.sh` (install
V, serve a signed V+1 setup from a local server, `lumen-text --update`, assert the installed version became V+1; a wrong
key and a replayed old file are rejected). Both run on `windows-latest` in `packaging.yml` (pull requests) and `ci.yml`
(release).

## Layout contract

`config::assets_dir()` looks for `language-manifest.json` in `$LUMEN_ASSETS`, then
`<exe dir>/../share/lumen-text/assets`, then `~/.local/share/lumen-text/assets`. So in **every**
format the binary and the assets keep the same relative position (`stage.sh` creates it):

```
<prefix>/bin/lumen-text
<prefix>/share/lumen-text/assets/
<prefix>/share/applications/lumen-text.desktop
<prefix>/share/icons/hicolor/scalable/apps/lumen-text.svg
<prefix>/share/doc/lumen-text/{README.md,LICENSE}
```

The prefix is `/usr` for deb/rpm/Arch/AppImage and empty for the tarball (so it also runs in
place: `./bin/lumen-text`).

The binary links only glibc and libgcc. The windowing libraries (libxkbcommon, Wayland, X11) are
loaded with `dlopen` at run time, so the packages depend on `libxkbcommon` and only *recommend*
the rest.
The editor also needs at least one system font (it panics with `no default font found` otherwise), so
the deb/rpm/Arch packages depend on fontconfig plus a font; with the AppImage and the tarball you must
have one installed (e.g. `fonts-dejavu-core`, `dejavu-sans-mono-fonts`, `ttf-dejavu`). Release binaries are built on Ubuntu 22.04 and need glibc 2.35 or newer.

## Installing

Replace `<v>` with the version of the release you downloaded. Verify downloads with
`sha256sum -c SHA256SUMS --ignore-missing`.

### Debian, Ubuntu, Linux Mint, Pop!_OS (.deb)

```sh
sudo apt install ./lumen-text_<v>-1_amd64.deb     # pulls in the dependencies
sudo apt remove lumen-text
```

### Fedora, RHEL, Rocky, Alma (.rpm)

```sh
sudo dnf install ./lumen-text-<v>-1.x86_64.rpm
sudo dnf remove lumen-text
```

On openSUSE use `sudo zypper install ./lumen-text-<v>-1.x86_64.rpm`.

### Arch Linux, Manjaro, EndeavourOS (.pkg.tar.zst)

```sh
sudo pacman -U lumen-text-bin-<v>-1-x86_64.pkg.tar.zst
sudo pacman -R lumen-text-bin
```

### AppImage (any distribution)

```sh
chmod +x lumen-text-<v>-x86_64.AppImage
./lumen-text-<v>-x86_64.AppImage
```

If FUSE is missing, run it with `APPIMAGE_EXTRACT_AND_RUN=1` or
`--appimage-extract-and-run`. It does not integrate itself into the desktop menu; use a tool such
as AppImageLauncher if you want that.

### Portable archive (.tar.gz)

```sh
tar xzf lumen-text-<v>-linux-x86_64.tar.gz
cd lumen-text-<v>-linux-x86_64
./bin/lumen-text            # run in place, or:
./install.sh                # install into ~/.local (`sudo PREFIX=/usr/local ./install.sh` for all users)
```

### From source

`../install.sh` (repository root) builds with cargo and installs into `~/.local`.

## Source packages and the AUR

- `arch/PKGBUILD` (`lumen-text`): builds the tagged source tree with cargo.
- `arch/lumen-text-bin/PKGBUILD` (`lumen-text-bin`): repackages the release tarball, no
  compilation. CI builds the Arch package from exactly this file, substituting `pkgver` and the
  checksum and using the tarball it just built (makepkg picks up a source file that already sits
  next to the PKGBUILD).

To publish to the AUR after a release: set `pkgver`, run `updpkgsums` and
`makepkg --printsrcinfo > .SRCINFO`, then push to the AUR git repository. Arch uses `pkgver`
with dots, so the CalVer `2026.11.10.1` is valid as is.

## Building locally

```sh
cargo build --release        # set LUMEN_VERSION=2026.11.10.1 to stamp the version
bin=target/release/lumen-text; v=2026.11.10.1
packaging/tarball/build-tarball.sh $bin $v x86_64 out
packaging/deb/build-deb.sh         $bin $v x86_64 out   # needs dpkg-deb
packaging/rpm/build-rpm.sh         $bin $v x86_64 out   # needs rpmbuild
packaging/appimage/build-appimage.sh $bin $v x86_64 out # downloads appimagetool
packaging/arch/build-arch.sh out/lumen-text-$v-linux-x86_64.tar.gz $v out   # needs docker
```

## Flatpak

`flatpak/io.github.lumen_text.yml` is a flatpak-builder manifest (Freedesktop 24.08 + rust-stable
extension; Wayland, X11 fallback, home access). It is not part of the automated release. For
offline (Flathub) builds generate `cargo-sources.json` with flatpak-cargo-generator (see comments
in the file). Build from the repo root:
`flatpak-builder --user --install --force-clean build-dir packaging/flatpak/io.github.lumen_text.yml`.

## Adding another OS later

Add a sibling job next to `build-linux` in the workflow that uploads its files as an artifact
named `dist-<something>`, and add it to the `release` job's `needs`. The release job downloads every
`dist-*` artifact into one directory and publishes it all in a single GitHub Release.
