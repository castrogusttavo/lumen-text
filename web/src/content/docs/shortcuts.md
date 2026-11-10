---
title: Keyboard shortcuts
description: Every default shortcut, generated from the editor's key table, and how to rebind them.
section: Using Lumen
order: 2
generated: shortcuts
---

The shortcut modal (palette: *Preferences: Keyboard shortcuts*) lets you search, rebind and spot conflicts. The table below is generated from `DEFAULTS` in `src/keymap.rs`.

## Rebinding

User bindings live in `~/.config/lumen-text/keybindings.json` and map a key combination to a command id. A value of `"none"` disables a built-in shortcut.

```json
{
  "ctrl+shift+t": "TestFile",
  "f5": "RepeatTask",
  "ctrl+w": "none"
}
```

A combination is `ctrl+shift+p` (modifiers in any case, key lowercase), a chord is two strokes separated by a space (`ctrl+k r`), and `explorer:a` only applies while the file explorer has focus.

## Defaults
