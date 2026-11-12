---
title: Getting started
description: Install Lumen on Linux, Windows or macOS, open a project and learn the first three shortcuts.
section: Start
order: 1
---

Lumen is a lean, CPU-rendered code editor for Linux, Windows and macOS, written in Rust, with a first-class TypeScript workflow. Pre-built packages are attached to every [GitHub release](https://github.com/castrogusttavo/lumen-text/releases). The [download page](/download) always links the newest one.

## Platforms

| Platform | Package | Notes |
|---|---|---|
| Linux x86_64 and aarch64 | `.deb`, `.rpm`, Arch package, AppImage, tarball | Wayland and X11; self-updating AppImage and user install |
| Windows x86_64 | `lumen-text-<v>-windows-x86_64-setup.exe`, `lumen-text-<v>-windows-x86_64.zip` | Installer (per user, self-updating) or portable zip; not code-signed |
| macOS Apple Silicon | `lumen-text-<v>-macos-aarch64.dmg` | Not notarized, see below |
| macOS Intel | `lumen-text-<v>-macos-x86_64.dmg` | Not notarized, see below |

The Windows and macOS builds are new: they are built and unit-tested by CI, but have had much less real-world use than Linux. On macOS and for the Windows portable zip the update check announces new versions with a link to the release page; you install them by hand. The Windows installer updates itself like the Linux user install.

### Windows

Run `lumen-text-<v>-windows-x86_64-setup.exe`. It installs for your user only (`%LOCALAPPDATA%\Programs\Lumen`, no administrator rights; the dialog can switch to all users), adds a Start Menu shortcut and an entry in *Installed apps*, and offers optional tasks (add Lumen to your `PATH` and the Explorer context menus *Open with Lumen* / *Open folder in Lumen* are ticked by default; a desktop icon and *Open with* entries for common file types are not). Later versions are downloaded, verified (minisign) and installed by the app itself: when you choose *Restart now*, Lumen closes, the installer upgrades it in place, and Lumen starts again with your tabs. Uninstalling keeps your settings unless you answer *Yes* to the question about deleting them (settings live in `%APPDATA%\lumen-text`, caches in `%LOCALAPPDATA%\lumen-text`).

Silent install, for scripts and deployment tools: `lumen-text-<v>-windows-x86_64-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /CURRENTUSER`. Useful switches: `/DIR="C:\Tools\Lumen"`, `/TASKS="addtopath,contextmenu"` (also `desktopicon`, `fileassoc`; `/TASKS=` for none), `/ALLUSERS` (needs administrator rights), `/LOG="setup.log"`. Silent uninstall: `"%LOCALAPPDATA%\Programs\Lumen\unins000.exe" /VERYSILENT`.

The installer and the executable are not code-signed, so Windows SmartScreen may show "Windows protected your PC": choose *More info*, then *Run anyway*. The portable zip works as before: unzip anywhere and run `lumen-text.exe` (keep `assets/` next to it); it does not update itself. The integrated terminal uses PowerShell through ConPTY (set `LUMEN_SHELL` to use another shell).

### macOS

Open the `.dmg` and drag *Lumen Text* to Applications. The app is ad-hoc signed but not notarized (that needs a paid Apple Developer ID), so Gatekeeper blocks the first launch ("cannot be opened because the developer cannot be verified"). To open it once: right-click the app, choose *Open*, then *Open* again; or run `xattr -dr com.apple.quarantine "/Applications/Lumen Text.app"`. On macOS `Cmd` plays the role of `Ctrl` in every shortcut (`Cmd+P`, `Cmd+S`, ...). Settings and session data live in `~/Library/Application Support/lumen-text`.

## Install on Linux

Versions use CalVer `YYYY.M.D.N` (year, month, day of the release, then a counter for that day), for example `2026.11.10.1`. Replace `<v>` with the version you downloaded.

| Distribution | Command |
|---|---|
| Debian, Ubuntu, Mint, Pop!_OS | `sudo apt install ./lumen-text_<v>-1_amd64.deb` |
| Fedora, RHEL, Rocky, Alma | `sudo dnf install ./lumen-text-<v>-1.x86_64.rpm` |
| openSUSE | `sudo zypper install ./lumen-text-<v>-1.x86_64.rpm` |
| Arch, Manjaro, EndeavourOS | `sudo pacman -U lumen-text-bin-<v>-1-x86_64.pkg.tar.zst` |
| Any distribution | `chmod +x lumen-text-<v>-x86_64.AppImage && ./lumen-text-<v>-x86_64.AppImage` |
| Portable | `tar xzf lumen-text-<v>-linux-x86_64.tar.gz && ./lumen-text-<v>-linux-x86_64/bin/lumen-text` |

Release binaries need glibc 2.35 or newer (they are built on Ubuntu 22.04). The binary links only glibc; libxkbcommon, Wayland and X11 are loaded at run time.

:::tip
Prefer a user install that keeps itself up to date? Run `lumen-text --install-user` from the portable tarball or any running Lumen. It installs into `~/.local/share/lumen-text/<version>/`, adds a launcher entry and an icon, and needs no root.
:::

Verify what you downloaded with `sha256sum -c SHA256SUMS --ignore-missing`; the AppImage and the tarball also carry a minisign signature, see [Auto-update and signed releases](/docs/updates).

## TypeScript support

TypeScript is served by `tsgo`, the native TypeScript compiler. It is optional; install the language server once:

```sh
npm i --prefix ~/.local/share/lumen-text/tsgo @typescript/native-preview
```

A project that already has `@typescript/native-preview` installed is used first. See [Language tooling](/docs/typescript) and [Language servers](/docs/language-servers).

## Open a project

```sh
lumen-text                          # reopen the tabs of the project's last session
lumen-text project/                 # open a folder
lumen-text project/ src/app.ts:42:7 # open a file at line:column
```

A second `lumen-text` invocation hands its files to the already running window.

## The first three shortcuts

- `Ctrl+P` opens a file (fuzzy).
- `Ctrl+Shift+P` opens the command palette.
- `Ctrl+,` opens the settings.

Everything else is in the palette, in the [shortcuts reference](/docs/shortcuts) and in the tool tips.
