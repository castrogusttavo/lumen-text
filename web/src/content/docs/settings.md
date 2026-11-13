---
title: Settings reference
description: Every setting, its default and what it does, generated from the editor's settings model.
section: Customize
order: 1
visual: settings
generated: settings
---

Open the settings with `Ctrl+,`. Changes apply immediately and are saved in `~/.config/lumen-text/config.json`; the file is plain JSON and the keys below are its names.

```json
{
  "theme": "vitesse-black",
  "format_on_save": true,
  "updates": "notify"
}
```

The tables below are generated from the settings model and the defaults in `src/config.rs`.

## Project settings

A project can override your settings: `.lumen/config.json` at its root (commit it to share with the team) and `.lumen/config.local.json` (personal, add it to `.gitignore`). They take the same keys as `config.json`; the order is **your settings < `.lumen/config.json` < `.lumen/config.local.json`**, and `languages` and `language_servers` are merged key by key. Both files reload live.

```json
{
  "tab_width": 2,
  "format_on_save": true,
  "languages": { "python": { "tab_width": 4 } }
}
```

- The settings window has a **User | Project** switch (`Alt+U` / `Alt+P`) that picks the file it writes. On the project side the rows the project sets are marked, user-only rows are hidden, and the reset icon returns to your own value. *Preferences: Project settings* opens it there; a gear in the status bar shows when project settings are in effect.
- User-only keys (ignored in a project file): `ui_scale`, `scroll_speed`, `updates`, `sidebar_width`, `git_panel_width`, `md_split`, `status_items`, `status_order`, `status_left_order`, `ram_display`, `mem_budget_mb`, the `ui_font_*` keys, `theme_chrome`, `test_watch`.
- Commands from a project file (`build_command`, `run_command`, `test_command`, `language_servers` entries with `command`, `args` or `env`) only apply after a one-time trust prompt, remembered per project until the commands change.

## Per-language settings

`languages` (in your settings or a project file) overrides `tab_width`, `format_on_save`, `word_wrap` and `inlay_hints` for one language, keyed by its id: `{"python": {"tab_width": 4, "format_on_save": true}}`.

## Language servers

The *Language servers* section of the settings window lists every server of the registry with its state and a switch; the `language_servers` object in `config.json` is the full interface (add, replace or switch off servers, set arguments, environment, `initialization_options` and `settings`). It has no row of its own in the tables above because its keys are server names. See [Language servers](/docs/language-servers) for the schema.

## Fonts

Two independent groups: the **interface font** (sidebar, tabs, menus, dialogs, status bar, hover text) and the **buffer font** (the editor and everything monospace: terminal, output, diffs, code blocks). Each has a family, size, weight, line height and ligatures, and they apply the moment you change them.

- **Family.** Click the row in *Settings > Fonts*, or run *Fonts: Buffer font…* / *Fonts: Interface font…* from the palette. The picker lists the installed families, each drawn in its own font; the buffer picker shows monospace families first and has a *Show all fonts* toggle (`Alt+A`). Type a name that is not in the list to use it as it is. A family that is not installed falls back to the system default and a message says so.
- **Weight.** 100 (Thin) to 900 (Black). The nearest weight the family really has is used.
- **Line height.** A multiple of the font size from 1.0 to 2.5. In `config.json` a number above 4 means pixels. The interface value spaces multi-line text such as hover cards.
- **Ligatures.** Joins sequences such as `=>` and `!=` when the font has them. A ligature keeps the width of its characters, and the caret still moves one character at a time.

`font_size` from older versions is read as `buffer_font_size`.

## Editor display

- **Line numbers.** Off shrinks the gutter to the marks and the fold arrows.
- **Bookmarks.** `Ctrl+F2` toggles a bookmark on the caret line, `Ctrl+Alt+F2` and `Ctrl+Alt+Shift+F2` jump to the next and previous one (`F2` stays *Rename*). Bookmarks follow your edits and come back with the session.
- **Breakpoints.** Click the left edge of the gutter to toggle a breakpoint. With the setting off the dots and the click are gone; debugging still stops at the breakpoints you set.
- **Scrollbars.** `show` keeps the thin bars, `auto` fades them in while you scroll or point at them, `never` hides them (the wheel and the keys still scroll).
- **Language servers.** `enable_lsp` off stops every language server and the status bar shows *LSP off*; turning it back on starts them again when a file needs one.
