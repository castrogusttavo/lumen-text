---
title: Debugging
description: Breakpoints and stepping for TypeScript, JavaScript, Rust and C through DAP.
section: Using Lumen
order: 7
---

Lumen speaks the Debug Adapter Protocol. TypeScript and JavaScript use `js-debug`; Rust and C use `gdb` through its DAP mode.

| Action | Shortcut |
|---|---|
| Start / continue | `F5` |
| Stop | `Shift+F5` |
| Toggle breakpoint | `F9` (or click the gutter) |
| Step over / into / out | `F10` / `F11` / `Shift+F11` |

The debug panel shows the stack, variables and an output tab. *Debug: Remove all breakpoints* clears them. The bottom bar has a debug button, which you can hide in the settings.
