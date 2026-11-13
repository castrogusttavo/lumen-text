---
title: AI agents and terminals
description: Run Claude Code, Gemini CLI or Codex CLI side by side, each in its own worktree, with notifications, several terminals and the kitty keyboard protocol.
section: Using Lumen
order: 4.6
---

Lumen never handles API keys or talks to a model itself. The **AI panel** runs the agent CLI you already use (Claude Code, Gemini CLI or Codex CLI) in a terminal at the right of the editor, in the project folder, with your environment and your own login.

## Sessions

`Ctrl+Shift+A` shows the panel (it shares the slot with the Git panel, with a `Git | AI` switch on top). Each session is a tab with the agent icon, a name and a status dot: *Starting*, *Working*, *Waiting for you*, *Done* or *Exited*. `+` starts another session and asks which agent; double-click renames, middle-click or `×` closes (it asks first while the agent is working). Each session has its own terminal, status and working folder.

The bottom bar badge sums up the sessions you have not looked at ("2 waiting · 1 done"), and `Ctrl+Alt+A` (*AI: Next Session Waiting for You*) jumps to the next one, even while an agent has the keyboard.

For Claude Code, status comes from its hooks, routed to the right session, which is more reliable than reading the output. Gemini CLI and Codex CLI sessions use an output heuristic.

## Worktrees

Run agents on separate branches without them stepping on each other. *Git: Worktrees…* (or the branch menu) lists worktrees with branch, folder and dirty state; *Git: New worktree…* creates one on a new, existing or remote branch (default folder `<repo>/../<repo>-worktrees/<branch>`, see `git_worktree_dir`); open it here or in a new window; remove it (it asks again before forcing a dirty one) or prune. **AI: New session in worktree…** starts an agent with that folder as its working directory.

## Notifications and pasting

When a session waits for you or finishes while Lumen is in the background, a system notification appears once per turn (`notify-send` or D-Bus on Linux, `osascript` on macOS; setting `ai_notifications`). There is none on Windows yet.

`Ctrl+V` sends the clipboard as one bracketed paste, however long. An image on the clipboard is left to the agent (Claude Code reads it itself); files copied in a file manager paste as shell-quoted paths. While the panel has the focus every key goes to the agent; `Ctrl+Shift+P`, `Ctrl+Q` and `Ctrl+Shift+A` stay with Lumen, and clicking the editor or pressing `Ctrl+1` brings the keyboard back.

## Several terminals

The bottom panel's Terminal tab now holds several terminals, each with its own shell, scrollback and state. The tab strip has `+` to add one (``Ctrl+Shift+` ``), `×` to kill it and a split button that shows two side by side; double-click a tab to rename it, otherwise the title follows the program's OSC title or the foreground process. Killing a terminal, or quitting, asks first when its shell is running a job.

## Kitty keyboard protocol

The terminal emulator (also used by the AI panel) implements the kitty keyboard protocol's progressive enhancement flags 1, 4, 8 and 16 with a stack per screen, so TUIs such as Claude Code, Codex, Neovim and Helix get unambiguous `Esc`, `Ctrl+…` and modified `Enter`, `Tab` and `Backspace`. Programs that do not ask keep the classic encoding. Key release events (flag 2) are not supported.

## Explain and Fix with AI

Send a problem to the agent without copying anything. The prompt holds the message, the source and code, `file:line` and a few lines of code around it, and is pasted into the session shown (a new one starts when none runs, and the prompt waits until it is ready). Lumen never submits it: read it, edit it, press Enter. With no agent CLI installed the install command is copied and shown instead.

- **Editor**: put the caret on an underlined problem and use *AI: Explain Problem at Cursor* / *Fix Problem at Cursor*, the same entries in the editor's right-click menu, in the quick actions menu (`Ctrl+.`) and in the hover card's footer.
- **Problems list**: `Alt+E` explains and `Alt+X` fixes the selected row.
- **Tests panel / Build panel**: `E` explains and `X` fixes the selected failure (or the first one); also *AI: Explain Failed Test*, *AI: Fix Build Error*...
- **Terminal**: select output and run *AI: Send Terminal Selection to Agent*.
- *AI: Send File to Agent* mentions the active file and *AI: Send Problems Summary to Agent* lists the problems of the open files and the last build.

Settings `ai_prompt_explain` and `ai_prompt_fix` replace the built-in templates (placeholders `{message} {severity} {location} {file} {line} {source} {code} {lang} {frame} {related}`); `ai_prompt_context` sets how many lines of code go above and below the problem (default 5).
