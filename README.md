<p align="center">
  <img src="assets/brand/icon.svg" width="96" alt="Lumen logo">
</p>

# lumen-text

**A lean, CPU-rendered code editor for Linux, Windows and macOS, written 100% in Rust, with a first-class TypeScript workflow.**

[![CI](https://github.com/castrogusttavo/lumen-text/actions/workflows/ci.yml/badge.svg)](https://github.com/castrogusttavo/lumen-text/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/castrogusttavo/lumen-text)](https://github.com/castrogusttavo/lumen-text/releases)

![lumen-text, light theme, TypeScript project](docs/screenshots/light-editor.png)

## Why

Modern editors are great and heavy. lumen-text is an experiment in how far you can get with a small
memory budget (the editor itself stays around **20 MB**; the TypeScript server adds **~65 MB for a small project and several hundred MB for a very large one**, see the table below)
and no GPU driver:

- Everything is drawn on the CPU (`softbuffer` + `tiny-skia`), so it runs anywhere a Wayland or X11 window does.
- Highlighting uses the same TextMate grammars and VS Code themes as [Shiki](https://shiki.style)
  (65 themes, 260 grammar files), through a port of the `vscode-textmate` core on top of `onig`.
- TypeScript is served by [`tsgo`](https://github.com/microsoft/typescript-go) (the native TypeScript
  compiler) started on demand with full project awareness (like VS Code or Zed); an opt-in *lean mode* only loads open files and their imports for low-RAM machines.
- Any language server works: a built-in table (rust-analyzer, pyright, gopls, clangd, jdtls (Java), csharp-ls (C#),
  kotlin-lsp, lua-language-server, taplo, the vscode JSON/CSS/HTML servers, yaml, bash, marksman, zls, ruby-lsp,
  phpactor, Dockerfile, sourcekit-lsp, dart, elixir-ls, haskell, metals, svelte, astro, vue, Tailwind, deno...)
  starts a server **only when a matching file is opened and the program is installed**; add your own in `config.json`.
- One window, one process per editor, one font in memory, no web view, no Electron.

## Features

**Editor**
- Rope buffer (`ropey`), atomic save, CRLF, tree-shaped undo history, opens a 100k-line file in ~21 ms
- Multi-cursor, auto-indent, bracket pairs and matching, JSX/HTML tag closing, Emmet, snippets with tab stops
- Word wrap, folding, sticky scroll, indent guides, minimap, breadcrumbs, color swatches
- Find/replace (case, regex), project search, symbols, go to line, back/forward navigation
- Tabs, quick open (fuzzy), session restore, live reload when files change on disk (inotify)
- Fonts: separate **interface** and **buffer** (code, terminal, diffs) fonts with a searchable picker of the installed families, weight, size, line height and ligatures, all live
- Display options: line numbers, bookmarks (`Ctrl+F2`), breakpoint dots, scrollbars (show / auto-hide / never) and a master switch for the language servers
- Smooth Zed-style scrolling, drag selection by word/line, Wayland and X11 clipboard, IME, fractional hi-DPI scaling

**TypeScript and LSP**
- Diagnostics, rich Markdown hover, completion, signature help, definition, references, rename,
  organize imports, quick fixes (`Ctrl+.`), expand selection, call hierarchy
- Semantic tokens, optional code lens and inlay hints, optional auto-import
- Full-project mode (default; also reacts to files changed, created or deleted on disk) versus an opt-in lean mode; uses the project's own `tsgo` or a bundled one
- Formatting with the project's Biome or Prettier (falls back to `tsgo`); lint through `biome lsp-proxy` and ESLint
- **Language servers beyond TypeScript** (see the [Language servers](web/src/content/docs/language-servers.md) page): a data-driven
  registry (`src/lsp_servers.json`) plus `language_servers` in `config.json`; one process per (server, workspace root), started lazily,
  stopped after an idle grace period, restarted with backoff when it crashes; several servers per file (a primary plus companions such
  as Tailwind or ruff) with merged diagnostics, hover, completion and code actions; status, switches and a bounded server log
  (palette: *LSP: Restart language servers*, *LSP: Show server log*)
- Vitest/Jest runs with pass/fail marks in the gutter, `package.json` and `cargo` tasks with an ANSI output panel

**Git**
- Gutter marks, branch switcher, status dots in the explorer, inline blame
- Git panel (`Ctrl+Shift+G`): stage/unstage files and single hunks, commit, amend, fetch/pull (merge, rebase or ff-only)/push (force with lease behind a confirmation), history
- Side-by-side diff with intraline highlight, file history, merge-conflict resolution (accept current / incoming / both)

**Debugging**
- Breakpoints in the gutter, step over/into/out, stack, variables and output tab
- TypeScript/JavaScript through js-debug; Rust and C through `gdb` DAP

**Terminal**
- Integrated PTY terminal with a VT emulator (`vte`), 5,000-line scrollback, alternate screen (`vim`, `less`), bracketed paste

**Themes**
- 65 bundled VS Code themes with live preview (arrows apply, `Esc` restores), follow-the-system light/dark mode
- Import installed Zed themes, write your own custom theme (a template is generated for you)
- Text/background pairs checked against WCAG AA in every theme

**Markdown preview**
- Side-by-side reader (`Ctrl+Shift+V`) with live updates, scroll sync in both directions, highlighted code blocks, clickable links

**Split panes**
- Up to six panes (a row of four, or a grid up to 3x2), moveable tabs, per-pane focus shortcuts, saved with the session; the Markdown reader and the diff open inside the focused pane

**Customization**
- Settings modal (`Ctrl+,`) and shortcut editor with conflict detection
- `config.json`, `keybindings.json`, user themes folder, removable and reorderable bottom-bar items

## Screenshots

| | |
|---|---|
| ![Light theme with the Git panel](docs/screenshots/light-git.png) | ![Command palette / quick open](docs/screenshots/light-palette.png) |
| Git panel (light) | Quick open (light) |
| ![Dark theme](docs/screenshots/dark-editor.png) | ![Two split panes](docs/screenshots/dark-split.png) |
| Dracula theme | Split panes |

![Markdown reader](docs/screenshots/dark-markdown.png)

Regenerate the screenshots with the [headless mode](#headless-mode) on any small project, using a throwaway config so your own settings are not touched:

```sh
mkdir -p ~/.cache/lumen-shots/cfg/lumen-text ~/.cache/lumen-shots/state
echo '{"theme":"github-light-default"}' > ~/.cache/lumen-shots/cfg/lumen-text/config.json
XDG_CONFIG_HOME=~/.cache/lumen-shots/cfg XDG_STATE_HOME=~/.cache/lumen-shots/state \
  lumen-text --size 1100x700 --snap docs/screenshots \
  --keys "wait:1500;snap:light-editor;ctrl+shift+g;wait:800;snap:light-git" project/ project/src/todo.ts
```

The headless PNG writer stores uncompressed data; recompress the files (for example with `zlib` level 9 or `optipng`) before committing so each stays well under 250 KB.

## Measured numbers

Resident memory is PSS (proportional set size) as reported by the status bar (`RAM <editor> + tsgo <server>`; with three or more servers `+ N servers <sum>`, listed one by one in the tooltip).

| Scenario | Result |
|---|---|
| Editor with a window open, 1 file | ~12-16 MB |
| 100k-line file (8 MB), tokenized | ~33 MB (about 19-24 MB without the LSP) |
| `tsgo`, small project (30 files, 14 open), full project (default) | ~65-75 MB (editor ~20 MB) |
| `tsgo` on a real project, **lean mode** (opt-in: open files + imports) | ~40-220 MB |
| `tsgo` on a real project, full tsconfig (7k files; the default mode) | ~780 MB |
| Open a 100k-line file | ~21 ms |
| 1,000 keystrokes | ~3.9 ms total |
| Background tokenizer | 4-9k lines/s, off the UI thread |

Learnings that shaped the design: `GOMEMLIMIT` barely reduces `tsgo`'s RAM (the live heap *is* the
loaded program); auto-import in completion costs ~3 s and hundreds of MB, so it is opt-in (the
`Ctrl+.` quick fixes already offer "add import"); loading system fonts cost 25 MB, so the font
database holds a single font plus lazily loaded fallbacks.

## Install

Pre-built packages for Linux (x86_64 and aarch64), Windows (portable zip) and macOS (`.dmg`, Apple Silicon and Intel) are attached to every
[GitHub Release](https://github.com/castrogusttavo/lumen-text/releases). Versions use **CalVer
`YYYY.M.D.N`** (year, month, day of the release, then a counter for that day), for example
`2026.11.10.1`; tags are `vYYYY.M.D.N`. Every push to `main` that passes CI publishes a release.
Replace `<v>` below with the version you downloaded and verify with
`sha256sum -c SHA256SUMS --ignore-missing`.

| Distribution | Command |
|---|---|
| Debian, Ubuntu, Mint, Pop!_OS | `sudo apt install ./lumen-text_<v>-1_amd64.deb` |
| Fedora, RHEL, Rocky, Alma | `sudo dnf install ./lumen-text-<v>-1.x86_64.rpm` (openSUSE: `sudo zypper install ./...rpm`) |
| Arch, Manjaro, EndeavourOS | `sudo pacman -U lumen-text-bin-<v>-1-x86_64.pkg.tar.zst` (a source `PKGBUILD` and an AUR-style `lumen-text-bin` live in `packaging/arch/`) |
| Any distribution | `chmod +x lumen-text-<v>-x86_64.AppImage && ./lumen-text-<v>-x86_64.AppImage` |
| Portable | `tar xzf lumen-text-<v>-linux-x86_64.tar.gz && ./lumen-text-<v>-linux-x86_64/bin/lumen-text` (or run its `install.sh`) |

Release binaries need glibc 2.35 or newer (built on Ubuntu 22.04). The binary links only glibc;
libxkbcommon, Wayland and X11 are loaded at run time. Details: [packaging/README.md](packaging/README.md).

**Windows:** unzip `lumen-text-<v>-windows-x86_64.zip` and run `lumen-text.exe`. It is not code-signed, so SmartScreen
asks for confirmation (*More info*, *Run anyway*).
**macOS:** open `lumen-text-<v>-macos-<aarch64|x86_64>.dmg` and drag the app to Applications. It is ad-hoc signed but
not notarized: right-click, *Open* the first time, or `xattr -dr com.apple.quarantine "/Applications/Lumen Text.app"`.
`Cmd` acts as `Ctrl` for every shortcut.

**TypeScript support** is optional; install the language server once:

```sh
npm i --prefix ~/.local/share/lumen-text/tsgo @typescript/native-preview
```

(a project that already has `@typescript/native-preview` installed is used first).

**Build from source** (Rust stable, edition 2024):

```sh
sudo apt-get install build-essential pkg-config libxkbcommon-dev libwayland-dev \
  libfontconfig1-dev libclang-dev libonig-dev     # Debian/Ubuntu names
./install.sh                                      # cargo build --release + install into ~/.local
```

## Updates

Lumen updates itself where it safely can. About 20 seconds after startup and then every 4 hours
(also after a failed attempt, with a growing delay) it fetches
`https://github.com/castrogusttavo/lumen-text/releases/latest/download/latest.json`, a small file
published with every release. Nothing else is sent; there is no telemetry.

| Setting `updates` | Behaviour |
|---|---|
| `"auto"` (default) | Checks, downloads and installs the new version in the background, then shows a small card: **Lumen X is ready - Restart to update** (*Restart now* / *Later* / *Release notes*). A subtle download icon stays in the bottom bar until you restart. Each version is announced once. |
| `"notify"` | Only announces that a new version exists (card + bottom bar icon). The card's *Atualizar agora* button installs it on demand. |
| `"off"` | Never checks by itself. The command palette entries below still work. |

Command palette: **Help: Check for updates** (always ends in a card: up to date, available or the error), **Help: Restart to update**, **Help: About Lumen** (version, install kind). Restart starts the new executable with your project and restores the session; with unsaved files it asks you to save first.

What "install" means depends on how you installed Lumen:

| Install | Update |
|---|---|
| **AppImage** | The new AppImage replaces the file you run, atomically; the previous one stays next to it as `<name>.AppImage.old`. |
| **User install** (`lumen-text --install-user`, layout `~/.local/share/lumen-text/<version>/` + `current` symlink + `~/.local/bin/lumen-text`) | The release tarball is unpacked into a new version directory and `current` is switched atomically. The previous version is kept, older ones are pruned (2 stay). |
| **deb / rpm / pacman** (`/usr/bin`) | Never touched. You get the notification with the command for your distribution (read from `/etc/os-release`), e.g. `sudo dnf install <url>`. |
| Tarball run in place, `/usr/local`, local builds | Notification only. Local builds (version without a build number, e.g. `2026.11.9`) never check on their own. |

Every file is verified before it is used: it must carry a [minisign](https://jedisct1.github.io/minisign/)
signature made with the release key whose public half is embedded in the binary
(`src/update_key.pub`), and the signature must name exactly that version and file. Downloads use
the system `curl` (or `wget`) into `~/.cache/lumen-text/update/` with a time and size limit, on a
background thread.

`lumen-text --install-user` copies the running editor into the user layout above and creates the
launcher entry (`~/.local/share/applications/lumen-text.desktop`, absolute `Exec`) and icon
(`~/.local/share/icons/hicolor/`), so it works from the application menu without a terminal and then
keeps itself updated. Other flags: `lumen-text --check-update` (exit 0 up to date, 10 update
available, 1 error) and `lumen-text --update` (check, then install when this install can).
For testing, `LUMEN_UPDATE_URL` points the check at another `latest.json` (plain http allowed) and
`LUMEN_UPDATE_PUBKEY` at another public key; see `tools/update-e2e.sh`.

## Quick start

```sh
lumen-text                          # reopen the tabs of the project's last session
lumen-text project/                 # open a folder
lumen-text project/ src/app.ts:42:7 # open a file at line:column
```

Press `Ctrl+P` to open files, `Ctrl+Shift+P` for the command palette, `Ctrl+,` for settings.
A second `lumen-text` invocation hands its files to the already running window. The tool tips and
palette are the reference for everything not listed below.

## Keyboard shortcuts

Source of truth: `DEFAULTS` in `src/keymap.rs` (also shown in the palette, "Atalhos de teclado").

| Action | Shortcut |
|---|---|
| Open file (fuzzy) / command palette | `Ctrl+P` / `Ctrl+Shift+P` |
| Find / replace | `Ctrl+F` / `Ctrl+H` (`Alt+C` case, `Alt+R` regex, `Ctrl+Alt+Enter` replace all) |
| Find in project | `Ctrl+Shift+F` |
| Symbol in file / in project | `Ctrl+Shift+O` / `Ctrl+T` |
| Go to line | `Ctrl+G` |
| Definition / references / rename | `F12` (or `Ctrl+click`) / `Shift+F12` / `F2` |
| Completion / signature help | `Ctrl+Space` / `Ctrl+Shift+Space` |
| Hover | mouse resting, or `Ctrl+K I` |
| Quick fix | `Ctrl+.` |
| Next / previous problem | `F8` / `Shift+F8` |
| Format | `Shift+Alt+F` |
| Expand / shrink selection | `Shift+Alt+Right` / `Shift+Alt+Left` |
| Save / close tab / quit | `Ctrl+S` / `Ctrl+W` / `Ctrl+Q` |
| Next / previous tab, jump to tab | `Ctrl+Tab` / `Ctrl+Shift+Tab`, `Alt+1`..`Alt+9` |
| Sidebar / focus explorer | `Ctrl+B` / `Ctrl+Shift+E` |
| Explorer: new file / folder / rename / delete | `A` / `Shift+A` / `F2` / `Delete` |
| Git panel | `Ctrl+Shift+G` (in a diff: `n` / `p` pick a hunk, `s` stage) |
| Bottom panel / terminal | `Ctrl+J` / ``Ctrl+` `` |
| Tests: at cursor / file / rerun / stop | `Ctrl+K T` / `Ctrl+K F` / `Ctrl+K R` / `Ctrl+K X` |
| Tests panel: run all / re-run failed / debug at cursor | `Ctrl+K A` / `Ctrl+K E` / `Ctrl+K D` |
| Debug: start / stop / breakpoint | `F5` / `Shift+F5` / `F9` (or click the gutter) |
| Bookmark: toggle / next / previous | `Ctrl+F2` / `Ctrl+Alt+F2` / `Ctrl+Alt+Shift+F2` |
| Debug: step over / into / out | `F10` / `F11` / `Shift+F11` |
| Zoom in / out / reset | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` |
| Navigate back / forward | `Alt+Left` / `Alt+Right` |
| Next occurrence / all occurrences | `Ctrl+D` / `Ctrl+Shift+L` |
| Add cursor above/below, at click | `Ctrl+Alt+Up/Down`, `Alt+click` |
| Comment / move line / duplicate / delete line | `Ctrl+/`, `Alt+Up/Down`, `Ctrl+Shift+D`, `Ctrl+Shift+K` |
| Fold / unfold | `Ctrl+Shift+[` / `Ctrl+Shift+]` |
| Undo / redo | `Ctrl+Z` / `Ctrl+Shift+Z` (or `Ctrl+Y`) |
| Snippet expand / next field | `Tab` after the prefix (`fn`, `clg`, `it`, `desc`, `us`, `rfc`...) / `Tab` |
| Markdown reader | `Ctrl+Shift+V` |
| Split right / below | `Ctrl+\` / `Ctrl+K \` |
| Focus pane 1..4 | `Ctrl+1`..`Ctrl+4`; `Ctrl+K Left/Right` previous/next pane |
| Move tab to another pane / close pane | `Ctrl+K Shift+Left/Right` / `Ctrl+K Shift+W` |
| Settings | `Ctrl+,` |

Explorer selection: `Ctrl+click`, `Shift+click`, `Ctrl+A`, `Ctrl+C` / `Ctrl+X` / `Ctrl+V`, drag onto a folder (`Ctrl` copies).

## Configuration

All files live in `$XDG_CONFIG_HOME/lumen-text/` (default `~/.config/lumen-text/`). The settings
modal edits `config.json` and applies changes live; unknown or invalid values fall back to defaults.

### `config.json`

| Key | Default | Meaning |
|---|---|---|
| `theme` | none (GitHub Light Default) | Theme name (file stem in `assets/themes` or the user themes folder) |
| `appearance` | `"fixed"` | `"fixed"` keeps `theme`; `"system"` follows the desktop light/dark preference |
| `theme_light` / `theme_dark` | `github-light-default` / `github-dark-default` | Themes used when `appearance` is `"system"` |
| `theme_chrome` | `"derived"` | `"theme"` makes sidebar, tabs and status bar use the theme's own colors where text stays readable (AA) |
| `buffer_font_family` / `ui_font_family` | `""` | Font family of the code (and terminal, diffs, code blocks) / of the interface; `""` = system monospace / sans-serif; a missing family falls back to it |
| `buffer_font_size` / `ui_font_size` | `14` / `13` | 8 to 40 / 10 to 20 (`buffer_font_size` was `font_size`; old files are read as before) |
| `buffer_font_weight` / `ui_font_weight` | `400` / `400` | 100 to 900 (the nearest weight the family has is used) |
| `buffer_line_height` / `ui_line_height` | `1.57` / `1.38` | Multiplier of the font size, 1.0 to 2.5 (a number above 4 means pixels); the UI value spaces multi-line interface text such as hover cards |
| `buffer_font_ligatures` / `ui_font_ligatures` | `false` / `true` | Font ligatures (`liga`, `calt`) in the code / interface |
| `show_line_numbers` | `true` | Off shrinks the gutter to the marks and the fold arrows |
| `show_bookmarks` / `show_breakpoints` | `true` / `true` | Gutter marks; with breakpoints off the gutter click does nothing |
| `scrollbar` | `"show"` | `"show"`, `"auto"` (fade in while scrolling or pointing) or `"never"` |
| `enable_lsp` | `true` | Master switch for every language server |
| `ui_scale` | `0` | `0` = automatic from the window scale factor, else up to 4 |
| `format_on_save` | `false` | Run the formatter on save |
| `updates` | `"auto"` | `"auto"` (check, download and install in the background), `"notify"` (only announce) or `"off"`; see [Updates](#updates) |
| `auto_import` | `false` | Auto-import in completion (costs RAM and time) |
| `lsp_narrow` | `false` | opt-in tsgo lean mode (only open files and their imports; less RAM, incomplete cross-file errors/references) |
| `tsgo_source` | `"auto"` | `"auto"` (project's own, else bundled), `"project"`, `"bundled"` |
| `language_servers` | `{}` | Add, replace or switch off language servers, e.g. `{"rust": {"command": "my-ra"}, "python": {"enabled": false}, "my-server": {"command": "x", "languages": ["foo"]}}`; fields `command`, `args`, `env`, `languages`, `extensions`, `root_markers`, `initialization_options`, `settings`, `enabled`, `role`, `requires`. Mistakes are reported in the status bar, never fatal |
| `code_lens` / `inlay_hints` | `false` | Optional LSP extras (cost server CPU) |
| `inlay_hints_params` / `inlay_hints_types` | `true` | With `inlay_hints` on: show parameter names / inferred types separately. Ctrl+click a type goes to its definition; double click a `: T` hint inserts the annotation |
| `minimap` / `minimap_style` | `true` / `"subtle"` | `"subtle"` or `"normal"` |
| `word_wrap` / `sticky_scroll` / `indent_guides` | `false` / `true` / `true` | Editor view options |
| `breadcrumbs` / `git_blame` | `true` / `true` | Path bar above the editor; inline blame |
| `ram_display` | `"long"` | `"long"` (numbers) or `"short"` (a chip icon) |
| `explorer_hide_gitignored` | `true` | Hide what `.gitignore`, the global ignore file and `.git/info/exclude` ignore; `false` shows those entries dimmed |
| `status_items` | `{}` | Map of bottom-bar item id to `true`/`false` (`search`, `problems`, `terminal`, `debug`, `git`, `ram`, `position`, `indent`, `language`) |
| `status_order` | position, indent, language, terminal, debug, git, ram | Order of the right-hand bottom-bar items |
| `status_left_order` | sidebar, search, problems | Order of the left-hand bottom-bar items |
| `git_pull_strategy` | `""` | `"merge"`, `"rebase"` or `"ff-only"`: how Pull integrates remote commits (empty = ask when the branches diverge) |
| `tab_width` | `4` | Columns a tab character spans (2, 4 or 8); per-file picks live in the session |
| `git_panel_width` / `md_split` | `0` / `0.5` | Panel width in px (0 = automatic); Markdown source share (0.2 to 0.8) |

Environment variables: `LUMEN_THEME` (path to a theme file), `LUMEN_ASSETS` (assets directory),
`LUMEN_TSGO` (path to a `tsgo` binary), `LUMEN_AUTOIMPORT=1`, `LUMEN_DEBUG=1`, `LUMEN_GIT_POLL_MS` (interval of the background ahead/behind refresh, default 60000).
Assets are searched in `$LUMEN_ASSETS`, then `<exe dir>/../share/lumen-text/assets`, then `~/.local/share/lumen-text/assets`.

### `keybindings.json`

Maps a key combination to a command id (the `Cmd` variant names in `src/app_overlay/commands.rs`; `none` disables a built-in shortcut).
Chords use a space, explorer-only bindings use the `explorer:` prefix.

```json
{ "ctrl+shift+t": "TestFile", "f5": "RepeatTask", "ctrl+w": "none", "ctrl+k r": "RepeatTask" }
```

### Themes

Themes are VS Code JSON (`colors` + `tokenColors`). Drop `*.json` files into
`~/.config/lumen-text/themes/` and pick them from the theme list. The palette command
"Criar tema personalizado" writes `custom-<name>.json` there from a template that contains every
`colors` key the editor reads (editor, sidebar, tabs, status bar, widgets, gutter, diagnostics, find
highlights) and about 20 `tokenColors` examples:

```json
{ "name": "My Custom Theme", "type": "dark",
  "colors": { "editor.background": "#1e1e1e", "editor.foreground": "#d4d4d4", "...": "..." },
  "tokenColors": [ { "scope": ["comment"], "settings": { "foreground": "#6a9955", "fontStyle": "italic" } } ] }
```

**Zed themes:** installed Zed themes (`~/.config/zed/themes` and the Zed extensions folder) are
converted automatically to `~/.config/lumen-text/themes/zed-*.json` (Zed `style` keys are mapped to the
VS Code keys; the mapping table is documented at the top of `src/zedtheme.rs`).

## Architecture

```
src/
  buffer.rs, editor.rs     rope (ropey), grouped/tree undo, atomic save; cursors, selections, multi-cursor
  textmate.rs, highlight.rs  vscode-textmate port on onig; background, incremental tokenization
  theme.rs, zedtheme.rs    VS Code themes, WCAG-checked chrome; Zed theme import, custom template
  ui.rs, doc/, docview.rs  drawing primitives (cosmic-text, tiny-skia); document state and render, word wrap/folds
  rowmap.rs, minimap.rs, scroller.rs  visual rows, minimap sampling, exponential-glide scrolling
  app/, app_*.rs           window (winit + softbuffer), tabs, sidebar, overlays, panels, split panes, settings
                           (the biggest ones are directory modules: app/, app_overlay/, app_gitpanel/, app_hover/, settingsui/)
  keymap.rs, config.rs     DEFAULTS table + keybindings.json; config.json and paths
  lsp.rs, inlay.rs, eslint.rs, format.rs  JSON-RPC client (lazy tsgo), inlay hints, lint, formatters
  lspreg.rs, lsp_servers.json, lspsup.rs, lspmerge.rs, lsplog.rs, app_lspx.rs  language-server registry and config, supervision
                           (backoff, dynamic registration, progress), merged answers, server log, lifecycle in the app
  dap.rs, tasks.rs, terminal.rs  debug adapters, processes and ANSI output, PTY + VT emulator
  git.rs, gitx.rs, gitsync.rs  status, blame, hunks, diff, history, conflicts, fetch/pull/push
  mdview/, mdimg.rs, hover_md.rs  Markdown reader and rich hover (pulldown-cmark)
  search.rs, fuzzy.rs, snippets.rs, emmet.rs, watcher.rs (inotify), mem.rs, perf.rs
  script.rs                headless mode for tests and screenshots
assets/   grammars, themes, language-manifest.json, icons, licenses
packaging/  tarball, deb, rpm, Arch, AppImage, Flatpak scripts
```

Dependencies are deliberately few and compiled with minimal features (`image-webp`, `png`, `zune-jpeg`
and `gif` only for Markdown images). The binary is a single executable plus the `assets` directory.

## Performance notes

- Tokenization runs on a background thread with a per-frame budget; the UI thread spends ~0.05 ms on it and long jumps get an immediate color.
- Big files (above 20k lines) get semantic colors for the visible window only (`semanticTokens/range`) and an outline trimmed to its first 3 levels; responses above 4 MB that have no streaming path are dropped.
- A single font is loaded (fallback faces on demand), glibc arenas are limited and memory is trimmed with `malloc_trim`.
- The CI `perf` job runs regression tests (`cargo test --release perf -- --nocapture`); thresholds are generous and the job never blocks a release.

## Headless mode

The same input and render code runs without a window and writes PNGs, which is how the editor is tested and
how the screenshots above were made.

```sh
lumen-text --size 900x500 --snap out/ \
  --keys "ctrl+p;type:app;enter;wait:800;f12;wait:500;snap:after" project/ project/src/a.ts
```

Flags: `--keys "<tokens separated by ;>"`, `--snap <dir>`, `--size WxH` (logical pixels), `--scale <f>`.
The full token list is in [CONTRIBUTING.md](CONTRIBUTING.md#headless-testing).

## Platform status

Linux (Wayland and X11, x86_64 and aarch64) is the primary platform. Windows (x86_64) and macOS (arm64 and x86_64)
build and pass the test suite in CI and ship from the release job, but they are young ports: no real-machine
testing of IME, clipboard corner cases, window behaviour or HiDPI has been done yet, and the updater only announces
new versions there (no in-place update). The clipboard uses `wl-clipboard`/X11 tools on Linux, `pbcopy`/`pbpaste` on
macOS and the Win32 API (text only) on Windows; copying files between programs works on Linux only.

## HugeIcons Pro (optional)

Official release binaries use the commercial HugeIcons Pro icon set, embedded in the executable under the
maintainer's licence (details in [assets/icons/NOTICE.md](assets/icons/NOTICE.md)). The masks are **not** in this
repository (`assets/icons/hugeicons*` are git-ignored) and are never shipped as loose files. Source builds work
without any key and use the bundled Zed icons. If you own a HugeIcons Pro licence and want the same look locally:

```sh
HUGEICONS_TOKEN=<your npm token> tools/setup-hugeicons.sh   # needs node, npm, python3, rsvg-convert
cargo build --release                                       # build.rs embeds the masks it finds
```

## Known limitations

- The interface is English only for now; other languages need a string layer (see the roadmap).
- No TextMate `injections` (code embedded in Markdown/HTML gets only the outer grammar).
- Opening a very large file and jumping to its end leaves that stretch uncolored for a few seconds.
- Full-project mode (default) can use several hundred MB on very large projects; the opt-in lean mode
  (setting `lsp_narrow`, palette "LSP: Toggle lean mode") trades that for errors and references that only see open files and their imports.
- "Open folder" inside the app is limited; the project root normally comes from the command line.

## License and credits

lumen-text is released under the [MIT License](LICENSE). Full list of Rust crates: [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).

- **Grammars and themes:** [Shiki](https://shiki.style)'s [tm-grammars and tm-themes](https://github.com/shikijs/textmate-grammars-themes) (MIT, Pine Wu and Anthony Fu), each grammar and theme keeping its upstream license; notices in `assets/licenses/`.
- **Icons:** masks derived from [Zed](https://github.com/zed-industries/zed)'s icon set, parts from Lucide (ISC); see `assets/icons/ZED_ICONS_LICENSES.txt`. HugeIcons Pro is optional and never redistributed.
- **Fonts:** none are bundled; the editor uses your system monospace font via fontconfig.
- **Language server:** [`tsgo`](https://github.com/microsoft/typescript-go) from Microsoft (Apache-2.0), installed separately.
- Built on `winit`, `softbuffer`, `tiny-skia`, `cosmic-text`, `ropey`, `onig`, `vte`, `pulldown-cmark` and friends.

## Tests panel

`Ctrl+K A` (or the **Tests** tab of the bottom panel) runs the project's tests and lists them as a tree: a live progress bar, pass / fail / skip filters, durations, the failure message with an expected-vs-received box and clickable `file:line` links, and ✓/✗ marks in the gutter. Failing groups open first, `Failed` re-runs only what broke, `Watch` re-runs on save, and `Debug` starts the selected test under gdb (Rust) or js-debug (Vitest, Jest, `node --test`).

Supported runners are detected from the project files: cargo test / cargo nextest, Vitest, Jest, `node --test`, Bun, Deno, `go test`, pytest, PHPUnit / Pest, RSpec, Maven, Gradle, `dotnet test` and ctest. Anything else works through JUnit XML: set `test_command` and `test_report` (the XML file it writes, relative to the project) in `config.json`. Without a report the run still shows in the Output tab.
