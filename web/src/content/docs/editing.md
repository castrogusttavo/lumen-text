---
title: Editing power tools
description: Replace in project, macros, column selection, jump to character, EditorConfig, whitespace, rulers, zen mode, multiple windows and more.
section: Using Lumen
order: 2.5
---

## Replace in project

`Ctrl+Shift+H` opens a replace row under the project search (`Ctrl+H` does the same inside it, `Tab` switches fields). `Alt+C`, `Alt+W` and `Alt+R` toggle case, whole word and regex. The replacement understands `$1`, `${name}`, `$&`, `$$`, `\n` and `\t`.

Every match is its own row with the old text struck through and the new text highlighted. `Enter` replaces the selected match, `Ctrl+Enter` all matches of its file, and `Ctrl+Alt+Enter` asks "Replace 42 matches in 7 files?" before replacing everything. Open files are edited in their buffer as one undo step and stay unsaved; closed files are rewritten atomically, each line keeping its own LF or CRLF, and a BOM is kept. `.gitignore` applies exactly like the search.

## Macros

`Ctrl+Shift+R` starts and stops recording (a REC pill shows in the bottom bar). Keys, shortcuts, palette commands and typed text are recorded, not the mouse. `Ctrl+Alt+R` replays the last macro; *Macro: Replay…* asks for a count. A replay is one undo step. *Macro: Save last as…* writes `macros.json` in the config folder, *Macro: Run saved…* picks one, and `"ctrl+alt+1": "macro:name"` in `keybindings.json` binds it.

## Column selection and jump to character

`Shift+Alt+drag`, middle-button drag or `Ctrl+Shift+Alt+arrows` select a box: one cursor per line, measured in visual columns with tabs expanded, with the cursor at the end of lines shorter than the box. Typing, paste (one clipboard line per cursor when the counts match), delete and copy go through the normal multi-cursor code. With word wrap the box follows buffer lines, not wrapped rows.

`Alt+J` is **Jump to…**: type one or two characters and every visible match gets a one or two letter label over the text. Type the label to move the cursor there (`Shift` extends the selection), `Esc` cancels. The jump is added to back/forward history.

## EditorConfig, whitespace and rulers

`.editorconfig` files are read (walking up to `root = true`): `indent_style`, `indent_size`, `tab_width`, `end_of_line` (for files with no line break yet), `charset`, `trim_trailing_whitespace`, `insert_final_newline` and `max_line_length`, with `*`, `**`, `?`, `[...]`, `{a,b}` and `{1..3}` globs. EditorConfig wins over every settings layer for indent and tab width.

`render_whitespace` is `none`, `boundary`, `trailing` or `all`. `trim_trailing_whitespace` and `insert_final_newline` run on save as one undo step; Markdown is not trimmed unless `.editorconfig` asks. `rulers` draws thin lines at the columns you list (also per language), and `max_line_length` adds one per file.

## Inline diagnostics, highlights and linked editing

`inline_diagnostics` shows the most severe message after the code, faded and cut to the room left: `off`, `current_line` (default) or `all`. When the caret rests on a symbol, its other occurrences light up (read and write in different colours; the same word when no server offers `documentHighlight`). Renaming an HTML or JSX tag renames its pair live as one undo step.

## Layout and look

- **Zen mode** (`Ctrl+K Z`) hides every bar, panel and the minimap and centers the text in `zen_max_width` columns; `Esc Esc` restores the layout exactly. `show_tabs` and `show_status_bar` switch those two on their own.
- **Explorer order** by name (numbers by value), type, date modified or mixed, folders first or not: right-click the explorer header.
- **Font features** (`buffer_font_features`, e.g. `{"zero": true, "ss01": true}`) and `buffer_letter_spacing` apply to the code, terminal and diffs; the font picker previews them.
- **Mouse bindings** in `keybindings.json`: `mouse4`, `mouse5`, `middle`, and `ctrl` / `shift` / `alt` with `click`, `wheelup` or `wheeldown`.
- **Right-to-left text**: Arabic, Hebrew and Persian lines are shown in visual order with joined shaping. Arrow keys walk the logical text and the caret shows a small direction flag; selecting across direction runs highlights separate pieces. `bidi_paragraph` is `auto` or `ltr`.

## Images

PNG, JPEG, GIF, WebP and SVG open as a picture in their tab: checkerboard behind transparency, fit to the pane, `Ctrl+wheel` / `Ctrl+=` / `Ctrl+-` / `Ctrl+0` to zoom, drag to pan, and the bottom bar shows dimensions, file size and zoom. SVG has a *Preview | Source* switch (`Ctrl+Shift+V`).

## Multiple windows

`Ctrl+Shift+N` opens a new window; *File: Open folder in new window…* (or `Ctrl+click` in the project menu) opens a folder in one. Drag a tab out of the window, or choose *Move to New Window*, to carry it over with its cursor, scroll and unsaved text. Each window is its own process, with its own tabs, session and language servers. Starting `lumen-text folder/` raises the window that has that folder open or opens a new one; `--new-window` always opens one. With no arguments, `lumen-text` reopens every window of the last run. Window positions are not restored on Wayland (sizes are).
