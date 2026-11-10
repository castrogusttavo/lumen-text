---
title: Interface tour
description: The parts of the Lumen window, from the project switcher to the status bar.
section: Start
order: 2
visual: tour
---

Lumen is one window with one process. Nothing in it is a web view: every pixel is drawn on the CPU.

## The parts

1. **Project and branch.** The project switcher (command *Project: Switch project…*) and the branch switcher (*Git: Switch branch…*).
2. **Files.** The explorer. Toggle it with `Ctrl+B`, focus it with `Ctrl+Shift+E`. While it has focus, `A` creates a file, `Shift+A` a folder, `F2` renames and `Delete` deletes. *View: Toggle Files / Outline* switches the sidebar to the symbol outline. The explorer hides what git ignores (`.gitignore`, your global ignore file and `.git/info/exclude`); turn *Hide git-ignored files* off, or run *Explorer: Toggle hiding git-ignored files*, to show those entries dimmed.
3. **Tabs.** `Ctrl+Tab` and `Ctrl+Shift+Tab` cycle, `Alt+1` to `Alt+9` jump to a tab, `Ctrl+W` closes.
4. **Breadcrumbs.** The file path and the symbol under the cursor (switch off in Settings, *Breadcrumbs*).
5. **The editor.** Line numbers, gutter marks for git changes and diagnostics, folding chevrons, indent guides and sticky scroll.
6. **Minimap.** A thumbnail of the file at the right edge (`minimap` in the settings, style *subtle* or *normal*).
7. **Status bar.** Problems count, language server state, cursor position, indentation, language, terminal, debug and Git buttons, and the memory readout (`RAM <editor> + tsgo <server>`). Items can be hidden and reordered in the settings.

## Bottom panel

`Ctrl+J` toggles the output panel (task and test output with ANSI colors). `` Ctrl+` `` opens the integrated terminal.

:::note
The tool tips are the reference for everything not covered here: rest the pointer on any control to see what it does and its shortcut.
:::
