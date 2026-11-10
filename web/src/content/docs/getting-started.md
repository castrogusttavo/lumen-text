---
title: Getting started
description: Install Lumen on Linux, open a project and learn the first three shortcuts.
section: Start
order: 1
---

Lumen is a lean, CPU-rendered code editor for Linux, written in Rust, with a first-class TypeScript workflow. Pre-built x86_64 packages are attached to every [GitHub release](https://github.com/castrogusttavo/lumen-text/releases). The [download page](/download) always links the newest one.

## Install

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

A project that already has `@typescript/native-preview` installed is used first. See [Language tooling](/docs/typescript).

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
