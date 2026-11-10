# lumen-text — roadmap

Legend: ✅ done, wired up and tested · 🔌 module done and tested, still to be wired into the UI · 🔧 in progress now · ⬜ not done yet.
Tests: `cargo test --release` (≈500 tests). Last updated: 2026-11-09 (evening). Roadmap features are complete; what remains is the visual design (coming from your mockups).

## Summary
(the table counts the items listed in each section below; ➖ = deliberately dropped, not counted)

| Area | ✅ | 🔌 | 🔧 | ⬜ |
|---|---:|---:|---:|---:|
| Core editor | 14 | 0 | 0 | 0 |
| Navigation and project | 4 | 0 | 0 | 0 |
| TypeScript / LSP | 4 | 0 | 0 | 0 |
| Assisted editing | 1 | 0 | 0 | 0 |
| Testing, running and debugging | 2 | 0 | 0 | 0 |
| Git | 3 | 0 | 0 | 0 |
| UX and platform | 10 | 0 | 0 | 1 |
| Performance | 3 | 0 | 0 | 0 |

## 0. Core editor
- ✅ Rope, opens 100k lines in ~21 ms, atomic save, CRLF; Wayland/X11 window on CPU; scrolling with a short exponential glide (see "Mouse input and scrolling")
- ✅ TextMate highlighting (65 themes) **on a background thread** (the UI spends ~0.05 ms; immediate color on long jumps)
- ✅ Full editing: auto-indent, bracket pairs, JSX tags, multi-cursor, move/duplicate line, comment
- ✅ **Tree undo** (branches; command "Ver ramos do histórico")
- ✅ Tabs, quick open, restored session, reload when the file changes on disk
- ✅ Wayland and X11 clipboard, primary selection (middle click), IME (input-method text)
- ✅ **Terminal**: PTY + VT emulator, **5,000-line scrollback**, **alternate screen** (vim/less), application cursor, bracketed paste
- ✅ Zoom, horizontal scroll, huge lines, indent guides, bracket pair matching, color swatches, sticky scroll
- ✅ **Word wrap + folding**: `rowmap.rs` wired in through `docview.rs` (word wrapping, indentation folds, clickable gutter chevrons and "…", Ctrl+Shift+[ / ], commands Fold/Unfold region/all and Toggle word wrap; Up/Down/Home/End by visual line; 13 tests). Without wrapping/folds the map does not even exist (zero cost); with wrapping, 120k lines open in ~180 ms. Indentation folds + LSP `textDocument/foldingRange` (imports, comments, regions). Limitations: inlay hints are hidden on wrapped lines, the minimap still follows logical lines
- ✅ **Minimap** (O(height) sampling, click/drag, toggled in settings; validated on real files)
- ✅ **Theme bold/italic/underline** in code + on-demand glyph fallback (+2 MB). Fixed: `github-dark-default` and `vesper` did not load (a new test loads every theme)
- ✅ **Fractional hi-DPI scaling** (1.25 / 1.5 / 1.75 / 2.0): `window.scale_factor()` is read on open and on `ScaleFactorChanged`; the whole layout stays in *logical* pixels (constants `TAB_H`, `ROW_H`, `SIDEBAR_W`… untouched) and `Canvas`/`TextCtx` convert to physical pixels when drawing (`ui::scale()`, `ui::sc()`): rectangles round at the edges (neighbors fit together), 1 px lines become physical hairlines, corners/rings/cards are antialiased at physical resolution, shadows, icons sampled directly at physical resolution, UI and code text rasterized at `size × scale` (code zoom multiplies on top; `cell_w` measured at physical size ÷ scale), minimap. Mouse and wheel (`PixelDelta`) are divided by the scale on input; `App::size` is logical; the softbuffer buffer has the physical size. Override: `ui_scale` in the modal (Appearance; 0 = automatic) and env `LUMEN_SCALE`; headless `--scale 1.5` (`--size` is logical, the PNG comes out `size × scale`). Scale 1.0 validated **byte-for-byte identical** (3 screenshots vs. the previous binary). RAM: the physical frame costs `w×h×4` B (1.5×: 1920×1200 ≈ 9 MB; 4K ≈ 33 MB) and is drawn straight into the softbuffer buffer (no extra copy; on Wayland wl_shm keeps up to 2 buffers) and paint CPU grows with area (2.0 = 4× pixels). Limitations: with fractional code zoom the physical line height rounds per line (±1 px unevenness); check marks and the dotted wavy underline are drawn in logical blocks (slightly jagged at non-integer scales); not tested on a real Wayland compositor (validated by headless screenshots)
- ✅ **Zed-style mouse input and scrolling** (`scroller.rs`, `doc_scroll.rs`, `app_input.rs`; ~35 new tests):
  - **`Scroller`** (shared helper, simulated time in tests): target + position that converges exponentially (tau 40 ms, no overshoot, ~12 frames per notch), **acceleration** on fast spins (each notch following within <140 ms in the same direction adds +30% to the step, capped at 3.4x), reversing direction discards the pending travel without a jump, touchpad with exact pixels and **inertia** when the fingers lift (wheel `Ended` phase; decays with tau 320 ms), 80 ms glide (ease-out) for jumps. One notch = 3 lines.
  - **Editor**: `Doc::wheel` (vertical in lines/pixels, horizontal in `Doc.left`/`left_target`); PageUp/PageDown and Ctrl+Home/End glide ~80 ms; dragging the bar/minimap and typing (cursor always visible) stay instant. **Horizontal scroll** with the horizontal wheel, touchpad X delta and **Shift+wheel**, limited to the widest line near the window (cached per version; without word wrap) and a **draggable horizontal bar** (`ui::HBar`, pair of `VBar`, 3 tests) at the bottom of the editor. Also works in the split panel (each panel animates its own).
  - **Sidebar**: same vertical logic (3-line step) and **horizontal scrolling** (`tree.hscroll`, content and icons shifted, thin bar at the bottom when a name exceeds the width); output panel with the same vertical dynamics.
  - **Drag selection** (`DragSel`, 10 tests): dragging after a single click extends (even after an existing selection), **double click + drag = by words**, **triple click/gutter + drag = by lines** (anchor = initial word/line, flips when passing it), **Shift+click** extends and continues with drag, a click **inside** a selection only collapses on release (dragging from there starts a new selection), **Alt+drag = block selection** (multi-cursor, one selection per line). Vertical and horizontal **autoscroll** with the pointer outside the text, speed proportional to distance (60 px/s + 10x the distance), continuous even with the mouse still (every frame). The primary selection is only published if not empty.
  - Headless: tokens `wheel:dx,dy[,x,y]` (notches, positive = down/right), `pwheel:` (pixels), `wheel-end`, `mods:shift+alt`, `down:x,y`, `move:x,y`, `up`, `frames:N`, `still` (renders until settled and prints the frame count) and `state`. Measured: 1 notch settles in ~13 frames (~210 ms), PageDown/Ctrl+End in 5 frames (80 ms).
  - Limitations: palette, menus and completion scroll by whole lines (no glide; the touchpad pixel delta there still truncates); Git panel, diff and Markdown reader keep the old (instant) scrolling; the maximum width considers ±800 lines around the window (a giant line far away only comes into range when scrolling there); inlay hints do not count toward the width; inertia depends on the compositor sending `TouchPhase::Ended` (Wayland yes; X11 has no phase = no inertia); Alt+drag may be captured by the window manager
- ✅ **Split panels** (`app_split.rs`, 7 tests): palette → "Dividir editor à direita" (`Ctrl+\`) opens a second, already focused panel with the same document; clicking another tab swaps the focused panel's document (two documents side by side). `Ctrl+1`/`Ctrl+2` or "Focar outro painel" switch focus; clicking a panel focuses it; "Fechar painel dividido" keeps the focused panel's document. Each panel has its own scroll and cursor (even for the same file in both); 1 px draggable divider; the mouse wheel scrolls the panel under the pointer without changing focus; the focused panel's tab is underlined in the accent color and the other panel's in gray; the unfocused panel has no current-line band and a hidden cursor. Search, completion, hover and LSP follow the focused panel (`App::active` = the focused panel's document). State in `App::split: Option<Box<Split>>` (zero cost when closed), not persisted in the session. The Markdown reader and the diff take the whole area: opening either closes the split (and splitting closes the reader). Limitations: the same file in both panels shares folds and word wrap (with wrapping on and different widths the line map is rebuilt every frame and scrolling may jump a bit); a single split level (2 panels, right only); no resize cursor over the divider

## 1. Navigation and project
- ✅ Sidebar (tree, new/rename/delete), quick open, find in file/project, symbols, problems, back/forward
- ✅ Clickable breadcrumbs, side outline ("Estrutura"), sticky scroll
- ✅ **inotify watcher** (`watcher.rs`, 19 tests; `App::watch_dirs`/`pump_watcher`): watches the root, the tree's open folders and the folders of open files; reloads the file and refreshes the tree immediately (the periodic 1.5 s check stays as a safety net)
- ✅ **Context menus (right click)** (`app_ctxmenu.rs`, 7 tests): rounded floating card with shadow, HugeIcons/Zed icons, shortcut on the right, thin separators and hover/selection via a low-opacity tint (text always legible); arrows + Enter, Esc or a click outside close it (`Overlay::Ctx`). **Folder**: New file, New folder, Cut, Copy, Paste (only with something in the internal clipboard), Duplicate, Rename, Move to trash, Delete; **file**: Cut, Copy, Duplicate, Rename, Trash, Delete, Copy path, Open to the side (with split); **tab**: Close, Close others, Close all, Pin/Unpin, Copy path, Open/Close Markdown preview (`.md`); empty tree area: New file/folder/Paste at the root. Cut/copy/paste are internal (paste copies/moves, unique name `name (cópia).ext`; renaming/moving an open file updates the tab; deleting closes the tab). Trash: `gio trash`, with a freedesktop fallback (`~/.local/share/Trash` + `.trashinfo`). Delete asks for a Yes/No confirmation (focus on No). **Pinned tab**: appears on the left (display order only, document indexes do not change), shows the pin icon in place of the ×, survives "Close all/others" and persists in the session; clicking the pin unpins it. Headless: token `rclick:x,y` in `--keys`. Limitations: one item at a time in the internal clipboard; no Ctrl+C/X/V in the explorer (the copy shortcut remains the editor's); cut items are not dimmed in the tree; Alt+digit follows display order (pinned first); "Close others/all" keeps tabs with unsaved changes

## 2. TypeScript via LSP
- ✅ tsgo on demand in **lean mode** (≈800 → ≈220 MB), project version or bundled
- ✅ Diagnostics, hover, completion, definition, references, rename, format, organize imports, quick fixes, signature help
- ✅ **Rich hover** (`app_hover.rs` + `hover_md.rs`, ~30 tests): VS Code/Zed-style popup. Real Markdown (```ts blocks colored by the TextMate tokenizer with the current theme, `code`, **bold**, lists, quotes, separators, links, bare URLs become links); JSDoc in sections (`@param` with highlighted name + signature type + description, `Retorna`, `Lança`, `Exemplo`, `Veja também`, `@deprecated` as an "Obsoleto" banner, other tags); wrapping by width, maximum height with scrolling (mouse wheel) and a thin draggable bar (`ui::VBar`); never leaves the editor area (above or below the line, stable side while it grows); the mouse can **enter the popup** (scroll, click links/actions, drag to select lines, `Ctrl+C` copies) without flicker: the popup only closes ~280 ms after the pointer leaves both the symbol and the popup, or with `Esc`/a key/a click outside. Cache keyed by (document, version, symbol span): going back to the same symbol is instant with no new request. **Content**: for `interface`/`type`/`enum`/`class` tsgo only sends "interface User"; the app requests `textDocument/definition` and cuts the declaration out of the file (open buffer or disk, own lexer that ignores strings/comments/templates): full interface and type (a long union becomes one line per member), enum with implicit values (`Admin, // = 0`), class with only the public surface (signatures without body/initializer, constructor, `/** summary */`, "+N private members"), up to 40 lines with "… mais N linhas — ver definição"; function: signature + parameters + return + overloads (consecutive `export function f(...)`); variable/const: short literal value; a variable/property/parameter whose type is a project interface/class shows the expanded "Tipo X" (`typeDefinition`, ignores `lib.*.d.ts`); a function parameter with a named type in the same file (component props) likewise; import/`require`: module, resolved file (clickable) and `package@version` in `node_modules`; a literal (string/number/boolean) shows the type of the property/JSX attribute it belongs to (`key: "x"`, `prop="x"`). **Diagnostics** above the type (colored bar, code `TS2322` with a link to typescript.tv, action "Ações rápidas (Ctrl+.)"). **Footer** with discreet links: Go to definition, Go to type definition (variable/property/parameter), References, Copy type (the expanded declaration, when there is one). Nothing blocks the UI: the extra requests (`Pending::HoverExtra`) arrive later and the popup recomposes in place keeping its scroll. Headless: `hover:x,y;wait:ms;snap:name`, `wheel:x,y,n`, `down:`/`move:`/`up`, `esc`. Limitations: Markdown italic only appears lighter (the UI font has no italic); text selection is per line (not per character); declaration cutting is lexical (no parser: `<T extends { a: 1 }>` in class/interface and ambiguous regex can fool it); method overloads only when the signatures are consecutive; expanded type only if the declaration is in a file outside `lib.*.d.ts`; without tsgo (a file with no server) only diagnostics appear
- ✅ Expand selection, call hierarchy, **semantic tokens**, **code lens** (optional)
- ✅ **Inlay hints** (optional, `inlay_hints` in `config.json`/modal, palette "Alternar inlay hints"): `textDocument/inlayHint` only for the visible lines ±20 (180 ms debounce when scrolling, 500 ms after editing; edits shift/discard the line's hints immediately), stored in `Doc.inlay`; drawn in gray/italic with a soft band, in virtual columns (cursor, selection, clicks, brackets, diagnostics and sticky scroll are correct). Off = zero cost. Validated by screenshot with tsgo (variable/return/parameter types, argument names, enum)

## 3. Assisted editing
- ✅ Project formatters (Biome/Prettier), inline lint (Biome LSP + ESLint CLI), snippets, **Emmet**, auto-closing tags

## 4. Testing, running and debugging
- ✅ Output panel (ANSI, links), tasks, Vitest/Jest with **✓/✗ marks in the gutter**, integrated terminal
- ✅ **Debugging**: F5/F9/F10/F11/Shift+F5, breakpoints in the gutter, highlighted stopped line, "Depurar" tab (stack, variables, output). **TypeScript/JS via js-debug** (validated: stop, stack, variables, step) and **Rust/C via gdb DAP** (cargo build + gdb; full flow validated in the UI: breakpoint, stop, stack, variables, step)

## 5. Git
- ✅ Gutter, branch, badges, stage/unstage/commit/discard, inline blame
- ✅ **Stage/revert hunk**, **side-by-side diff** (with intraline highlight), **file history** (opens the commit's diff), **merge conflicts** (colored regions + "Aceitar atual / entrada / ambos"). Logic: `gitx.rs` (43 tests with real git); side-by-side diff and conflicts validated by screenshot
- ✅ **Git panel** (`app_gitpanel.rs`, 8 tests with real git), docked on the right; opens from the git button in the bottom bar, `Ctrl+Shift+G` or the palette ("Git: Abrir painel"). **Alterações** tab: Staged / Mudanças / Não rastreados, checkbox = stage/unstage (also per section), a click opens the side-by-side diff (HEAD vs. file), discard with 2-click confirmation, multiline message box and a **Commit** button that really commits (`Ctrl+Enter`; the option "include all if nothing is staged" runs `git add -A`), error/success shown in the panel and the bar. **Histórico** tab: paginated `git log` (60 at a time, loads on scroll), short hash · author · relative date; clicking a commit expands its files and opens the diff. Refreshes by itself when the index/HEAD/tree change. Keyboard: arrows, Enter, Space (stage), Delete (discard), Tab. Validated by headless screenshot in a temporary repository. Limitations: no amend, no push/pull, no per-hunk staging in the panel (it already exists as a command), fixed width (not resizable), files > 3 MB do not open a diff

## 6. UX and platform
- ✅ Palette, JSON config, CLI `file:line:col`, **single instance**, **shortcuts in `keybindings.json`**, **system light/dark theme**
- ✅ Packaging (PKGBUILD, Flatpak, AppImage, `install.sh`) and CI with regression benchmarks
- ✅ **Icons**: Zed set (tree, tabs, status) and **HugeIcons Pro solid-rounded** (rest of the platform; bulk for large icons) as masks tinted by the theme; file mapping identical to the "Zed (Default)" theme
- ✅ Icons wired up (tree, tabs, status) and **Ctrl +/- only on the code font** (validated)
- ✅ **Settings modal** (`Ctrl+,`) and **shortcuts modal** (palette → "Atalhos de teclado"; records a combo, detects conflicts, restores defaults); saves to `config.json`/`keybindings.json` and applies live (minimap, indent guides, sticky scroll, font, theme…)
- ✅ **Themes**: picker with **live preview** (arrows apply, Esc restores), imports installed Zed themes (min-theme → `~/.config/lumen-text/themes/zed-*.json`), custom themes in the same folder and the command "Criar tema personalizado" (JSON template)
- ✅ **Side-by-side Markdown reader** (palette → "Markdown: Abrir leitor ao lado" or `Ctrl+Shift+V` in a `.md`): editor on the left and live preview on the right, scroll synced both ways (by source line), the mouse wheel over the preview scrolls the preview, link clicks (http/mailto → `xdg-open`, `#anchor` scrolls, a relative path opens the tab already with the reader); state in `Doc::md` (`Option`, zero cost when closed). Logic in `app_md.rs` (7 tests) on top of `mdview.rs` (40 tests); validated by screenshot
- ✅ **Default shortcuts as a single table**: `keymap::DEFAULTS` (normalized combo → `Cmd` id), validated by a test against `COMMANDS` (ids exist, no duplicate combos, palette text agrees). `Keymap::lookup` consults `keybindings.json` first (`none` disables) and the defaults afterwards; `handle_key` dispatches every `Cmd` through it and `App::shortcut`/`named_shortcut` keep only what is not a `Cmd` (Alt+digit, Ctrl+K prefix, Ctrl+Space, panels). The modal shows the real default and, on reassignment, the old default becomes `none` in `keybindings.json` (`keymap::bind_in`). Validated headless (Ctrl+B remapped/disabled via a temporary XDG_CONFIG_HOME + screenshots). Limitations: chords (`Ctrl+K R`) and explorer keys stay in code with no default row; one default per command (Shift variants of `+ _ { }` are resolved in `lookup`); Ctrl+Shift+V outside `.md` still pastes
- ⬜ **Open folder…** inside the app (today the root only comes from the command line; a new instance hands files to the existing window without changing the root): palette + root switch with LSP/watcher/tree/session
- ✅ **Top and bottom bars** (`app_topbar.rs`, 8 tests): the top shows the project name (dropdown: find project, 3 recents, "Abrir pasta…" with a folder browser) and the current branch (dropdown with search and all local and remote branches: `name | author, há N dias, last commit…`; Enter checks out); switching folders closes tabs and restarts tree, watcher, LSP and session. Footer: sidebar, find in project, diagnostics | terminal, debug, git, RAM
- ✅ **UI customization** (`config.rs`, `app_tooltip.rs`, `app_topbar.rs`, `app_settings.rs`; ~13 tests): **bottom bar with every item removable** (`status_items` in `config.json`, id → bool map, absent = visible; ids in `config::STATUS_ITEMS`: `sidebar`, `search`, `problems`, `terminal`, `debug`, `git`, `ram`, `position`, `indent`, `language`). The sidebar toggle button (left corner) is **mandatory**: `Config::status_visible("sidebar")` is always true and neither the file nor the modal can hide it; the rest reflows by itself when an item disappears (the transient message gets the space). Modal: **Barra inferior** section with one toggle per item (`status_<id>`) + **memory format** (`ram_display` = `long` "RAM 13 MB + tsgo 65 MB" | `short` = only the HugeIcons `hugeicons/cpu` chip icon with a discreet green/yellow/red dot depending on the total, no numbers); everything applies live in `apply_setting`. `breadcrumbs` (default on; off, the row disappears and `compute_layout` gives the space back to the editor/panel) and `git_blame` (default on; gates the blame text on the line; palette → "Git: Alternar git blame"; Git section of the modal). **Generic tooltips**: `App::tooltip: Option<Tooltip>` only stores "target under the pointer since when" (`TipKey::Top(MenuKind)` / `TipKey::Status(StatusBtn)`); after **450 ms** a rounded card with shadow appears (above the button in the bottom bar, below in the top one, always inside the window, flips sides if it does not fit) with the name and the real shortcut (reads `keybindings.json`, e.g. "Terminal  Ctrl+`", "Alternar sidebar  Ctrl+B"); git: "N arquivos alterados · Painel Git"; problems: error/warning counts; **RAM (even in short mode) shows the full editor + servers values**; position/indentation/language also describe themselves. Closes when leaving the target, clicking, typing or the pointer leaving the window; from one button to its neighbor the tooltip appears with no new delay. To give another group of buttons a tooltip: a new variant in `TipKey`, one line in `App::tip_target` and the text in `App::tip_text`. Headless: `hover:x,y` (now draws a frame first, so it works right at the start) + a 700 ms wait. Validated by light and dark screenshots. Limitations: tooltips only on the bottom bar and the top bar (tabs, sidebar and panels come later); no fade animation; position/indentation/language are not clickable (tooltip only); ids are fixed (no custom item order)
- ✅ **Visual v1 (minimalist, Zed/Apple style)**: flat chrome derived from the theme (background + low-opacity tint, hairline borders), pill tabs, sidebar with a git status dot instead of colored letters, light minimap, top bar and bottom bar, **mouse cursor per item** (arrow, hand, text, resize)
- ✅ **Thin draggable scrollbars** (editor and sidebar; discreet at rest, highlighted when the mouse approaches; clicking the track jumps) and **editor polish**: airier line height, wider gutter, discreet line numbers, subtle current-line band, premium Ctrl+P (radius-16 card, soft shadow)
- ✅ Tabs with **horizontal scrolling** (wheel/touchpad), active tab always visible, faded edges; project/branch dropdowns with mouse hover; `Ctrl+A` selects all in every field; menu tabs indicated by background only; highlight text color with guaranteed contrast (`accent_text`)
- ⬜ Rest of the interface (to refine with your feedback) (coming from your mockups; HugeIcons icons already available)

## 7. Performance and memory
- ✅ Live RAM (editor + servers), single font (+2 MB with variants and fallbacks), malloc_trim
- ✅ 100k-line file with tsgo: editor stable at ~24 MB (huge LSP responses — outline and semantic tokens — are skipped above 20k lines; they used to cause a ~900 MB spike)
- ✅ Benchmarks: open 100k lines 21 ms · 1,000 keystrokes 3.9 ms · tokenizer 4–6k lines/s in the background · 18.8 MB with 100k lines
- ➖ Parallel tokenization in chunks: dropped for now. Each line's state depends on the previous one (it would require speculative tokenization) and the background thread already covers 100k lines in a few seconds without blocking the UI

## Accessibility and scrolling (audit)

### WCAG 2.x AA in all themes
- ✅ **Minimum contrast 4.5:1 for all text and 3:1 for the highlight fill**, in the 65 bundled themes (light and dark) and the ones imported from Zed (they go through the same `Theme::load`). The fix lives in `theme.rs` (`Theme::load_with(path, fix)`; `fix=false` shows the theme's original colors) and preserves hue: `Rgba::readable_on` pushes the color toward black/white in 5% steps only until it passes; colors that already pass do not change.
  - **Chrome**: `foreground`, `tab_active_fg`, `tab_inactive_fg`, `status_fg`, `sidebar_fg` (now opaque: translucency is baked in beforehand) and `accent_text` pass on *every* surface (editor, sidebar, active tab, widget, input, selected row, hovered row, current-line band); `error`/`warning`/`info`/`git_*` likewise (the git gutter gets a bit lighter/darker where the theme was weak).
  - **Editor**: line numbers (discreet, but ≥ 4.5) and the active number; `foreground` over selection, search matches and diff backgrounds (additions/removals, and changed word) — these translucent backgrounds are weakened (lower alpha) only when the text would not pass (`Chrome::diff_*`, `find_match*`, `Theme::selection`).
  - **Syntax tokens**: any token color below 4.5:1 on the background (comments, `carriage-return`, `markup.ignored`…) is corrected when the theme loads.
  - **Primary button**: new `Chrome::on_accent` (white or near-black, whichever reads better over `accent`; a mid-tone accent is pushed until one of the two passes); `accent` itself guarantees 3:1 over the editor.
  - **Output/terminal text**: the ANSI palette (made for dark backgrounds) goes through `tasks::legible` when drawn, against the cell's real background (yellow/white on a light theme no longer vanish).
  - **Translucent text removed**: placeholders, hints and Git panel details, completion details, code lens title, blame; `line_number` is no longer used as a "gray text" color (completion/hover/output use `tab_inactive_fg`).
  - **Matched letters in autocomplete** (Ctrl+P / Ctrl+Shift+P): `accent_text` color (≥ 4.6:1 on the selected row and on hover) **plus underline**; in themes whose accent has no hue (min-light, high contrast) it uses the text color + underline, never a gray weaker than the rest.
  - **Icons**: the current branch's icon in the menu no longer turns accent (the state is background + bold + ✓); bottom bar buttons use `accent_text`.
- Tests: `theme::tests::every_text_pair_meets_wcag_aa_in_every_bundled_theme` (all text/background pairs above, in all bundled themes; fails with the list of pairs) and `the_fix_keeps_already_readable_colors`. Report for the table below: `cargo test --release accessibility_report -- --ignored --nocapture`.
- Limitations: syntax tokens over the *selection* and the search band are not checked individually (only `foreground`); disabled text (Commit button with no message) is exempt under WCAG and stays translucent; translucent texts in `settingsui.rs` (`dim.with_alpha` hints, "—") were left out because the modal's drawing is being edited in parallel; the table's "before" uses `fix=false` (theme colors as written, with `accent_text` = raw `focusBorder`).

**Worst pair per theme, before → after the fix** (contrast ratio; minimum 4.5, 3.0 for the highlight):

| theme | worst pair (before) | before | after |
|---|---|---:|---:|
| andromeeda | accent_text on list_active_bg | 2.31 | 4.87 |
| aurora-x | token `markup.raw.block.fenced.markdown` | 1.02 | 5.25 |
| ayu-dark | line_number on line_highlight | 2.25 | 5.33 |
| ayu-light | token `punctuation.definition.tag.end` | 1.50 | 5.42 |
| ayu-mirage | line_number on line_highlight | 2.26 | 5.67 |
| catppuccin-frappe | line_number on line_highlight | 2.20 | 5.45 |
| catppuccin-latte | line_number on line_highlight | 1.86 | 5.31 |
| catppuccin-macchiato | line_number on line_highlight | 2.38 | 5.77 |
| catppuccin-mocha | line_number on line_highlight | 2.52 | 5.78 |
| dark-plus | token `header` | 1.04 | 5.30 |
| dracula | git_deleted on list_active_bg | 1.71 | 4.55 |
| dracula-soft | git_deleted on list_active_bg | 1.73 | 4.60 |
| everforest-dark | accent_text on background | 1.00 | 6.12 |
| everforest-light | accent_text on background | 1.00 | 5.39 |
| github-dark | token `carriage-return` | 1.00 | 5.51 |
| github-dark-default | token `markup.ignored` | 1.09 | 5.69 |
| github-dark-dimmed | token `markup.ignored` | 1.18 | 5.25 |
| github-dark-high-contrast | foreground on selection | 1.11 | 4.58 |
| github-light | token `carriage-return` | 1.04 | 4.88 |
| github-light-default | token `carriage-return` | 1.06 | 4.96 |
| github-light-high-contrast | foreground on selection | 1.00 | 4.84 |
| gruvbox-dark-hard | accent_text on list_active_bg | 1.05 | 4.99 |
| gruvbox-dark-medium | accent_text on list_hover_bg | 1.02 | 5.00 |
| gruvbox-dark-soft | accent_text on line_highlight | 1.01 | 5.81 |
| gruvbox-light-hard | accent_text on list_active_bg | 1.07 | 4.71 |
| gruvbox-light-medium | accent_text on list_active_bg | 1.04 | 5.02 |
| gruvbox-light-soft | accent_text on tab_active_bg | 1.01 | 4.88 |
| horizon | accent_text on background | 1.02 | 6.28 |
| horizon-bright | foreground on diff added (word) | 1.08 | 4.53 |
| houston | token `comment markup.link` | 2.48 | 5.10 |
| kanagawa-dragon | accent_text on list_active_bg | 1.09 | 5.02 |
| kanagawa-lotus | accent_text on list_active_bg | 1.08 | 4.64 |
| kanagawa-wave | accent_text on list_hover_bg | 1.01 | 4.99 |
| laserwave | line_number on line_highlight | 3.00 | 6.03 |
| light-plus | warning on list_active_bg | 1.88 | 4.70 |
| material-theme | accent_text on background | 1.00 | 6.71 |
| material-theme-darker | accent_text on background | 1.00 | 6.85 |
| material-theme-lighter | accent_text on background | 1.00 | 5.34 |
| material-theme-ocean | accent_text on background | 1.00 | 6.13 |
| material-theme-palenight | accent_text on background | 1.00 | 5.95 |
| min-dark | accent_text on list_active_bg | 1.26 | 4.98 |
| min-light | accent_text on list_active_bg | 1.29 | 4.82 |
| monokai | git_modified on list_active_bg | 2.33 | 4.85 |
| night-owl | accent_text on list_active_bg | 1.02 | 4.72 |
| night-owl-light | token `object.comma` | 1.03 | 5.55 |
| nord | accent_text on tab_active_bg | 1.02 | 5.34 |
| one-dark-pro | accent_text on list_active_bg | 1.13 | 4.75 |
| one-light | warning on list_active_bg | 1.90 | 4.74 |
| plastic | line_number on line_highlight | 2.05 | 5.67 |
| poimandres | accent_text on background | 1.00 | 5.83 |
| red | token `constant` | 2.83 | 5.39 |
| rose-pine | accent_text on list_active_bg | 1.20 | 4.92 |
| rose-pine-dawn | accent_text on input_bg | 1.09 | 5.27 |
| rose-pine-moon | accent_text on list_active_bg | 1.17 | 4.78 |
| slack-dark | token `header` | 1.01 | 5.90 |
| slack-ochin | warning on list_active_bg | 1.88 | 4.70 |
| snazzy-light | token `constant.character.escape` | 1.71 | 5.16 |
| solarized-dark | line_number on line_highlight | 1.69 | 5.16 |
| solarized-light | line_number on line_highlight | 1.60 | 4.79 |
| synthwave-84 | accent_text on background | 1.05 | 6.49 |
| tokyo-night | accent_text on list_active_bg | 1.14 | 4.63 |
| vesper | token `markup.raw.block.fenced.markdown` | 1.03 | 5.16 |
| vitesse-black | accent_text on background | 1.00 | 5.69 |
| vitesse-dark | accent_text on background | 1.00 | 6.10 |
| vitesse-light | accent_text on background | 1.00 | 5.49 |

### Scrolling: wheel + thin draggable bar
- ✅ New `scrollreg.rs` (4 tests): each panel that can overflow registers a *region* when drawing (`App::region_bar`: wheel area + `ui::VBar` bar track); `regions_wheel` / `regions_press` / `regions_drag` handle wheel, track click and drag, and `set_region` hands the position back to the owner. With a modal open only the modal's regions respond. Hooks in `app.rs`/`app_input.rs`: one per event (wheel, click, move, release), `regions.clear()` per frame and an arrow cursor over the bar.
- ✅ **Debug** (Stack / Variables / Output, each column scrolls on its own; Output follows the end; resets when the session starts and when stopping at a new breakpoint), **Output**, **Terminal** (scrollback: the bar counts from oldest to live; there is no bar on the alternate screen), **Git** (Changes and History), **Ctrl+P / Ctrl+Shift+P / references / problems / symbols / themes** (any `Picker` with more than 10 results; fixed the card that shrank when scrolling to the end), **project/branch/folder menus** (same height fix), **completion** (wheel + bar; same), **Markdown reader** (dragging the bar moves the editor along) and **side-by-side diff** (scrolling now stops at the last line instead of scrolling into the void; draggable bar).
- Already had wheel and bar: editor, sidebar/outline, settings and shortcuts modals (bar visual only).
- Headless: new tokens `wheel:x,y,n` and `drag:x1,y1,x2,y2` in `--keys`.
- Limitations / pending: the settings/shortcuts modal bar is not draggable (drawing is being edited in parallel; `Modal` already has `wheel`); hover (`app_hover.rs`) and context menus do not scroll (context only overflows in windows under ~340 px tall; hover is with the other agent); the split panel has no bar in the unfocused panel; no visual test of the Debug tab (needs live js-debug/gdb — the logic is the same as the other columns); uses simple pixel deltas (no `Scroller` inertia).

## Later (requested by the user, outside the current focus)
- (nothing pending)

## Possible follow-ups (easiest to hardest)
Validated by the user in the real window: mouse cursor, fractional hi-DPI, tab scrolling by wheel, bar dragging. Double clicks: an old bug, no longer occurs.

**Level 1 — small (hours)**
- ⬜ Sticky line: limit to 2 headers / snap to the line height (today it cuts the partial line)
- ⬜ Hover on rows of the Git panel and the Debug panel
- ⬜ Shortcuts: chords (`Ctrl+K …`), `Ctrl+Y` and explorer keys in the single table; `Ctrl+F5` and `Ctrl+Shift+W/Q/G/J/H` with variable Shift
- ⬜ Context menus: dim cut items; "Close others/all" should ask before discarding unsaved tabs
- ⬜ Tooltips on tabs, sidebar and panels too; fade
- ⬜ Bottom bar position/indentation/language clickable (go to line, change indentation, change language)
- ⬜ Git panel: resizable width, staged+modified on a single row, scroll to the expanded commit's files, do not swallow `Ctrl+<key>`
- ⬜ Lighter minimap or hidden by default
- ⬜ Markdown reader: persist in the session and resizable split
- ⬜ Switching project with an unsaved tab: ask instead of refusing
- ⬜ Reorder bottom bar items

**Level 2 — medium (one agent each)**
- ⬜ Git panel: amend, push/pull, per-hunk staging
- ⬜ Explorer: copy multiple items, `Ctrl+C/X/V`
- ⬜ Panels: more than two, split below, the other panel's cursor follows edits, independent folds/wrap per panel
- ⬜ Folds: reposition when moving/duplicating/deleting lines; minimap by visual lines
- ⬜ Themes: respect the theme's sidebar/tabs when they pass contrast
- ⬜ Markdown: incremental parse, real images, footnotes, math, highlighting in code blocks

**Level 3 — hard**
- ⬜ Smaller inlay hints (separate metric), clickable, on wrapped lines
- ⬜ Outline/semantic colors in files >20k lines (partial LSP response)
- ➖ Parallel tokenization (dropped), real italic without a font file (depends on the font)

## How to try it
| Feature | How |
|---|---|
| Debug TS | open a `.ts`, `F9` on a line, `F5` |
| Terminal with history | ``Ctrl+` ``, `Shift+PageUp` or the mouse wheel; run `vim`/`less` |
| Git panel | `Ctrl+Shift+G` or the git button in the bottom bar |
| Diff / history / hunk | `Ctrl+Shift+P` → "Git: Ver diff…", "Git: Histórico do arquivo", "Git: Adicionar trecho…" |
| Merge conflict | open a file with `<<<<<<<`: click "Aceitar …" |
| Split panels | `Ctrl+\` splits (same file; click another tab to swap the panel's document), `Ctrl+1` / `Ctrl+2` focus, drag the divider; palette → "Fechar painel dividido" |
| Markdown reader | open a `.md`, `Ctrl+Shift+V` (or palette → "Markdown: Abrir leitor ao lado"); click the links; again to close |
| Undo branches | `Ctrl+Z`, edit, palette → "Ver ramos do histórico" |
| Custom shortcuts | `~/.config/lumen-text/keybindings.json` |
