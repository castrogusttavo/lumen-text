---
title: Never lose your work
description: Local history, hot exit, crash recovery, persistent undo, file encodings and saving files you do not own.
section: Using Lumen
order: 2.6
---

Lumen keeps several independent safety nets, all on by default and all stored under your state folder (`$XDG_STATE_HOME/lumen-text/`), never inside your project.

## Hot exit and crash recovery

Quitting with unsaved changes does not ask you to save. Every unsaved tab, untitled ones included, is stored and comes back as an unsaved tab the next time you open the project. While you type, dirty buffers are also backed up a few seconds after your last edit (at most every ten seconds during continuous typing), so a crash or a kill loses almost nothing. After a crash a notice says what was restored.

If the file changed on disk in the meantime, your unsaved text is kept and the tab is marked as conflicting. Undo then shows the version from disk. A backup is deleted when you save, close without saving, undo back to the saved text, or when it turns out identical to the file.

Turn it off with `hot_exit: false`: quitting asks to save again and no backups are written. With hot exit on, *Help: Restart to update* no longer asks you to save first.

## Local history

Independent of git. A snapshot is stored on every save and right before a buffer is replaced because another program rewrote the file (a formatter, `git checkout`, an AI agent). Identical snapshots are skipped, files over 5 MB are not recorded, and each file keeps at most `local_history_max_entries` (default 50) within a 200 MB total budget.

Open **File: Local history** from the palette, the tab menu or the explorer menu. Choose a snapshot to see a side-by-side diff against the current buffer, then press `r` or *Restore this version*. Restoring first snapshots the current text and then replaces the buffer as one undoable edit. **File: Local history (deleted files)** lists files whose path no longer exists; restoring one writes it back to disk.

## Persistent undo

The undo tree of a file is saved when you close it or quit and restored when you reopen it, but only if the file's text still matches what the history was recorded against. Limits: 2 MB per file (side branches go first, then the oldest steps), 48 MB overall, entries unused for 60 days are dropped. Switch it off with `persistent_undo: false`.

## File encodings

Lumen detects a UTF-8 BOM, UTF-16 LE/BE (with a BOM, or from the NUL pattern without one), valid UTF-8, and otherwise falls back to Windows-1252. The file is saved back in the same encoding. Valid UTF-8 streams straight into the buffer, so big files stay fast.

The bottom bar shows the encoding. Click it for **Reopen with encoding…** and **Save with encoding…**: UTF-8 (with or without BOM), UTF-16, Windows-1252, ISO-8859-1/15/2, Windows-1250/1251/1253/1254, KOI8-R, Shift_JIS, EUC-JP, GBK, Big5 and EUC-KR. A save that cannot represent a character fails with the line number and leaves the file untouched. Binary files are refused with a clear message.

## Saving files you do not own

When a save is refused for permissions (`/etc/hosts`), a card offers **Save as administrator**. On Linux the text goes to a private temp file and `pkexec` copies it over the target in place, keeping its owner and mode and refusing symlinks. On macOS the system password prompt is used. A missing `pkexec` or a cancelled prompt gives a clear message.

## Reopen closed tabs

`Ctrl+Shift+T` reopens the most recently closed tab: the last 20 are kept, each with its selection and scroll and back in the split pane it came from.

## Asking before killing a job

Closing a terminal, quitting or restarting asks first when a terminal's shell is running a command (`npm run dev`), a task or Build & Run process is alive, or an AI agent session is working. An idle shell closes without a question.
