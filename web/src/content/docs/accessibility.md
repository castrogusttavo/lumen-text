---
title: Screen readers
description: How Lumen talks to Orca, NVDA, Narrator and VoiceOver, what they can read, and the setting that turns it on or off.
section: Customize
order: 2.5
---

Lumen draws every pixel itself, so a screen reader cannot see widgets. Instead it exposes a small accessibility tree through [AccessKit](https://accesskit.dev), the native bridge to each platform's API: AT-SPI on Linux (Orca), UI Automation on Windows (NVDA, Narrator, JAWS) and NSAccessibility on macOS (VoiceOver). There is no web view and no JavaScript engine in it.

## It costs nothing until you use it

The tree is built **only while an assistive technology is connected**. Without one, the editor never creates a node, and a normal frame does no accessibility work. When a screen reader connects, Lumen builds the tree once and then sends a new one only after something on screen changed. On Linux the window also opens a connection to the accessibility bus at start; that is the whole idle cost.

## What a screen reader sees

- **Window and tabs.** The window is named after the project, the tabs form a tab list (the open tab is selected, unsaved tabs are read as "modified").
- **The editor.** A multi-line text input with one line of text per run, for the visible lines and a few around them, plus the caret and the selection. The screen reader reads the line you moved to and the characters you pass over by itself.
- **The explorer.** A tree with levels, expanded and collapsed folders and the selected row.
- **Palette and pickers.** Quick open, the command palette, the Problems list and the other pickers are a dialog with a text input and a list; the highlighted result has the focus. Prompts (rename, new file) and the settings window are dialogs too.
- **Find bar.** The query, the replacement and a summary such as "match 2 of 4".
- **Terminal.** The visible rows of the focused terminal and the cursor position.
- **Live regions.** The status message at the bottom ("saved", "no matches") is spoken when it appears, and a second region adds what the screen reader cannot know on its own.

## What Lumen announces

- Moving to another line: "Line 12 of 340", followed by the problem under the caret when there is one ("error: expected `;`"). A problem that appears or changes on the current line is announced too.
- Mode changes: the keyboard moving to the editor, the file explorer, the terminal, the find bar or a dialog (named after the dialog), and macro recording starting or stopping.
- Switching to another file.

## The setting

*Screen reader* in the settings (`accessibility` in `config.json`):

| Value | Behaviour |
| --- | --- |
| `auto` (default) | Connect to the platform and build the tree only while a screen reader is attached. |
| `on` | The same, but build the tree from the start instead of waiting to be asked. Use it if your screen reader attaches without telling the application. |
| `off` | Never connect. Nothing is created. |

The adapter has to exist before the window is shown, so a change takes effect the next time Lumen starts.

## Limits

- The screen reader can read and follow the caret, but cannot yet move the focus, click or scroll through the accessibility API; use the keyboard.
- Only the visible lines (and a margin) are in the tree, the 200 explorer rows around the cursor and the 50 list results around the selection.
- The sidebar's outline view, the Markdown reader, diff and merge views, and the Git and debug panels are not described yet.
- Screen reader support is built for all three systems but has been tested mostly through the tree builder's unit tests; real-world reports are welcome.
