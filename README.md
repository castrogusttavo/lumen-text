# lumen-text

A code editor for Linux written 100% in Rust, designed to use little RAM and to offer a good
TypeScript workflow. Its look is based on the TextMate grammars and VS Code themes used by
[Shiki](https://shiki.style) (65 themes, 242 languages), rendered on the CPU (no GPU driver).

## Numbers (measured)

| | RAM (PSS) |
|---|---|
| Editor with a window open, 1 file | ~12–16 MB |
| 100k-line file (8 MB), tokenized | ~33 MB |
| `tsgo` on a real project, **lean mode** (default; only open files + imports) | ~40–220 MB |
| `tsgo` on a real project, full tsconfig (7k files) | ~780 MB |

The status bar shows `RAM <editor> + tsgo <server>` live.

Learnings that changed the design: `GOMEMLIMIT` barely reduces tsgo's RAM (the live heap
*is* the loaded program); auto-import in completion costs ~3 s and hundreds of MB, so it is opt-in
(the `Ctrl+.` quick fixes already offer "add import"); loading system fonts cost 25 MB,
so the font database holds a single font.

## Install and run

```sh
./install.sh                     # builds and installs into ~/.local
npm i --prefix ~/.local/share/lumen-text/tsgo @typescript/native-preview   # TS support (optional)
lumen-text project/ src/app.ts:42:7
```

Look: light theme (GitHub Light Default) and a minimalist interface; switch themes from the palette ("Aparência: Escolher tema…").

With no arguments it reopens the tabs from the project's last session. In development: `cargo run --release -- <files/folders>`.

Configuration lives in `~/.config/lumen-text/config.json` (theme, font size, format on save, auto-import).
Environment variables: `LUMEN_THEME`, `LUMEN_ASSETS`, `LUMEN_TSGO`, `LUMEN_AUTOIMPORT=1`, `LUMEN_DEBUG=1`.

## Shortcuts

| Action | Shortcut |
|---|---|
| Open file (fuzzy) | `Ctrl+P` |
| Command palette | `Ctrl+Shift+P` |
| Find in file / replace | `Ctrl+F` / `Ctrl+H` (`Alt+C` case, `Alt+R` regex, `Ctrl+Alt+Enter` replace all) |
| Find in project | `Ctrl+Shift+F` |
| Symbol in file / in project | `Ctrl+Shift+O` / `Ctrl+T` |
| Go to line | `Ctrl+G` |
| Definition / references / rename | `F12` (or `Ctrl+click`) / `Shift+F12` / `F2` |
| Completion / signature help | `Ctrl+Space` / `Ctrl+Shift+Space` |
| Hover | mouse resting, or `Ctrl+K I` |
| Quick actions (quick fix) | `Ctrl+.` |
| Next problem | `F8` / `Shift+F8` |
| Format (project's Biome/Prettier, else tsgo) | `Shift+Alt+F` |
| Save / close tab / quit | `Ctrl+S` / `Ctrl+W` / `Ctrl+Q` |
| Tabs | `Ctrl+Tab`, `Ctrl+Shift+Tab`, `Alt+1…9` |
| Sidebar / focus explorer | `Ctrl+B` / `Ctrl+Shift+E` (in the explorer: `A` new file, `Shift+A` folder, `F2` rename, `Delete` delete) |
| Bottom panel (output / terminal) | `Ctrl+J` |
| Integrated terminal: open / toggle focus | ``Ctrl+` `` |
| Tests: at cursor / file / rerun task / stop | `Ctrl+K T` / `Ctrl+K F` / `Ctrl+K R` / `Ctrl+K X` |
| Zoom | `Ctrl+=`, `Ctrl+-`, `Ctrl+0` |
| Navigate back / forward | `Alt+←` / `Alt+→` |
| Snippet: expand / next field | `Tab` after the prefix (`fn`, `clg`, `it`, `desc`, `us`, `rfc`…) / `Tab` |
| Next occurrence / all occurrences | `Ctrl+D` / `Ctrl+Shift+L` |
| Cursor above/below, cursor at click | `Ctrl+Alt+↑/↓`, `Alt+clique` |
| Comment, move line, duplicate, delete line | `Ctrl+/`, `Alt+↑/↓`, `Ctrl+Shift+D`, `Ctrl+Shift+K` |
| Undo / redo | `Ctrl+Z` / `Ctrl+Shift+Z` |

Lint: with a `biome.json` in the project, the editor uses `biome lsp-proxy` for diagnostics and fixes (Ctrl+.).

More palette commands: git (changed files, stage, commit, discard), `package.json`/`cargo` tasks,
choose theme, organize imports, format on save, toggle auto-import, reload from disk.

## Architecture

```
src/
  buffer.rs      rope (ropey), grouped undo, atomic save
  editor.rs      cursor, selection, editing, multi-cursor (pure logic, tested)
  textmate.rs    port of the vscode-textmate core on top of onig
  highlight.rs   per-line states, incremental tokenization with a per-frame budget
  theme.rs       VS Code themes (text and UI colors)
  ui.rs / doc.rs drawing primitives; document (render, animated scroll and cursor)
  app*.rs        window, tabs, sidebar, overlays, LSP, output panel, git, files
  lsp.rs         JSON-RPC client (reader/writer threads), lazy tsgo
  tasks.rs       processes, ANSI, file:line:col links
  terminal.rs    PTY + VT emulator (vte), keys -> bytes
  snippets.rs    snippets with tab stops
  git.rs, format.rs, search.rs, config.rs, fuzzy.rs, field.rs, tree.rs, clipboard.rs
  script.rs      headless mode for tests and screenshots
```

## Tests

`cargo test --release` (77 tests, including an end-to-end test against the real tsgo, if installed).

Headless mode: runs the same keyboard/render code without a window and writes PNGs.

```sh
lumen-text --size 900x500 --snap /tmp/out \
  --keys "ctrl+p;type:app;enter;wait:800;f12;wait:500;snap:after" project/ project/src/a.ts
```

Tokens: `ctrl+shift+x` (combos), `type:text`, `wait:ms`, `snap:name`, `hover:x,y`, `click:x,y`, `sh:command`, `full`.

## Known limitations

- TextMate tokenization runs at ~9k lines/s: opening huge files and jumping to the end leaves that stretch uncolored for a few seconds (next step: tokenize in a thread/chunks).
- No TextMate `injections` (code embedded in Markdown/HTML only gets the outer grammar).
- Single font, no bold/italic; glyphs missing from the font show as a square.
- No word wrap, folding, minimap or split panes; the terminal has no scrollback or alternate screen (vim/less).
- Clipboard is Wayland-only (`wl-clipboard`).
- tsgo lean mode: errors and references only see open files and their imports (use the command "LSP: Alternar modo enxuto / projeto completo" to see the whole project, at the cost of more RAM).
