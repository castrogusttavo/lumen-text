---
title: FAQ and troubleshooting
description: Common questions about platforms, TypeScript, memory, updates and fonts.
section: Project
order: 2
---

## Which platforms are supported?

Linux on Wayland and X11 (x86_64 and aarch64), Windows 10 or newer (x86_64) and macOS 11 or newer (Apple Silicon and Intel). Windows and macOS are recent ports that CI builds and tests but that have seen less real-world use; they do not self-update yet (the update card links to the release page).

## Windows says "Windows protected your PC"

The Windows build is not code-signed, so SmartScreen warns on first launch. Choose *More info*, then *Run anyway*.

## macOS says the app cannot be opened

The app is not notarized. Right-click *Lumen Text* in Applications, choose *Open*, and confirm; or run `xattr -dr com.apple.quarantine "/Applications/Lumen Text.app"` once.

## TypeScript features are missing

Install the language server once with `npm i --prefix ~/.local/share/lumen-text/tsgo @typescript/native-preview`, or install `@typescript/native-preview` in the project. The status bar shows the server state (`tsgo ready`). The setting *tsgo source* chooses between the project's server and the bundled one.

## Memory is higher than expected

The status bar shows `RAM <editor> + tsgo <server>`. The editor stays small; the language server dominates. On a low-RAM machine you can turn on the opt-in *Lean mode* (it only loads the open files and their imports, at the cost of cross-file errors and references) and leave *Code lens* and *Auto import* off. Very large tsconfigs of thousands of files cost the most.

## The clipboard does not work on Wayland

Lumen supports the Wayland clipboard through `wl-clipboard` and the X11 clipboard natively. Install `wl-clipboard` if copy and paste fail.

## How do I turn updates off?

Set *Updates* to `off` in the settings, or `"updates": "off"` in `config.json`. See [Auto-update and signed releases](/docs/updates).

## Where is my configuration?

`~/.config/lumen-text/config.json` and `~/.config/lumen-text/keybindings.json`; sessions are stored under the state directory. Both files are plain JSON.
