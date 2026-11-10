---
title: Auto-update and signed releases
description: How Lumen checks, downloads and verifies updates, and what each install type does.
section: Customize
order: 3
---

About 20 seconds after startup and then every 4 hours (also after a failed attempt, with a growing delay) Lumen fetches `releases/latest/download/latest.json` from the GitHub repository, a small file published with every release. Nothing else is sent; there is no telemetry.

## The `updates` setting

| Value | Behaviour |
|---|---|
| `auto` (default) | Checks, downloads and installs in the background, then shows a card: *Lumen X is ready - Restart to update*. |
| `notify` | Only announces that a new version exists. |
| `off` | Never checks by itself. The palette commands still work. |

Palette: *Help: Check for updates*, *Help: Restart to update*, *Help: About Lumen*. From a terminal, `lumen-text --check-update` exits 0 when up to date, 10 when an update is available and 1 on error; `lumen-text --update` checks and installs when this install can.

## What "install" means

| Install | Update |
|---|---|
| AppImage | The new file replaces the one you run, atomically; the old one stays as `<name>.AppImage.old`. |
| User install (`--install-user`) | The tarball is unpacked into a new version directory and the `current` symlink is switched atomically. Two versions are kept. |
| deb, rpm, pacman | Never touched. You get a notice with the command for your distribution. |
| Tarball run in place, local builds | Notification only. Local builds never check on their own. |

## Verification

Every file is verified before use: it must carry a [minisign](https://jedisct1.github.io/minisign/) signature made with the release key whose public half is embedded in the binary, and the signature must name exactly that version and file.

```sh
sha256sum -c SHA256SUMS --ignore-missing
minisign -Vm lumen-text-<v>-x86_64.AppImage -P RWQoQesrU+NXnwocGP2j42WnwJuiGX/qTELkdL0ykq6TA+ASp5mmzyYU
```

Key id `9F57E3532BEB4128`.
