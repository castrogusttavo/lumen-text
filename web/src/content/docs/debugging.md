---
title: Debugging
description: Breakpoints, stepping and watches for TypeScript, JavaScript, Rust, C, Go and GHP pages through DAP.
section: Using Lumen
order: 7
---

Lumen speaks the Debug Adapter Protocol. TypeScript and JavaScript use `js-debug`; Rust and C use `gdb` through its DAP mode; Go (`package main`) uses Delve (`dlv`); `.ghp` pages use `ghp build --debug` plus Delve and stop in the page itself ([GHP pages](/docs/ghp)).

| Action | Shortcut |
|---|---|
| Start / continue | `F5` |
| Stop | `Shift+F5` |
| Toggle breakpoint | `F9` (or click the gutter) |
| Step over / into / out | `F10` / `F11` / `Shift+F11` |

The debug panel has a toolbar (continue, step over / into / out, pause `F6`, restart, stop and the launch configuration chip) over the Call Stack, Variables, Breakpoints, Watch and the Debug Console. *Debug: Remove all breakpoints* clears them. The bottom bar has a debug button, which you can hide in the settings.

## Breakpoints

Click the gutter or press `F9`. Right-click the gutter for a conditional breakpoint or a logpoint. Breakpoints follow your edits the way bookmarks do: a line inserted or deleted above one moves it (with its condition), and a running session receives the new lines at once. They are remembered per project.

## Watch

Add an expression with `+` in the Watch list. A value with fields (a struct, slice, map or object) has a chevron: open it to see its children, fetched when you open them; what you opened stays open while you step. Expressions are evaluated in the frame selected in the Call Stack.

## Building before a session

Configurations that need a build step run it in the background: the toolbar shows *Building…*, the window stays responsive and *Stop* cancels. A failed build prints its output in the Debug Console. (GHP pages build with `ghp build --debug`; Rust builds with `cargo build` as a task.)

## Overriding a launch: `.lumen/launch.json`

Go and GHP launches can be overridden per project with `.lumen/launch.json`: `program`, `args`, `env`, `cwd`, and `adapter`, for the configuration named by `config` (`delve` or `ghp`). The format is described in [GHP pages](/docs/ghp#overriding-the-launch-lumenlaunchjson). The file runs what it names when you press `F5`: keep it only in projects you trust.
