---
title: Git panel
description: Stage files and hunks, commit, pull, push and review diffs without leaving the editor.
section: Using Lumen
shot: git
order: 4
visual: git
---

`Ctrl+Shift+G` opens the Git panel at the right of the editor.

## Changes

The **Changes** tab lists *Staged* and *Unstaged* files with a checkbox, the file icon, the name, the folder and a status letter (`M` modified). *Add all* stages everything unstaged and *Remove all* unstages everything staged. Type a message in the box and press `Ctrl+Enter` to commit; *Amend last commit* rewrites the previous commit. With *Include everything if nothing is staged* checked, a commit with nothing staged takes all changes.

**History** lists the commits of the branch.

## Sync

*Pull* and *Push* sit next to the branch name. Right-click *Pull* to choose how it integrates remote commits: merge, rebase or fast-forward only (the setting *Pull strategy*). A force push uses `--force-with-lease` behind a confirmation.

## In the editor

- Gutter marks show added, modified and deleted lines.
- *Git blame on line* shows the author and date of the last commit on the cursor line.
- Status dots in the explorer mark changed files.
- Palette commands: *Git: Stage hunk*, *Git: Revert hunk*, *Git: View file diff (side by side)*, *Git: File history*, *Git: Switch branch…*.
- Merge conflicts: *Git: Conflict — accept current / incoming / both*.

The side-by-side diff highlights changes inside the line, and `n` / `p` pick a hunk while you are in a diff (`s` stages it).

## Stashes

The **Stashes** section of the Changes tab has a *Stash…* button (or *Stash N selected…* when you Ctrl/Shift+click files), asks for a message, and asks whether to include untracked files when any are involved. Right-click a changed file for *Stash File…*. Each stash expands to its files and shows the diff in the normal diff view, with *Apply*, *Pop* and *Drop* (drop asks first). A stash that conflicts is kept and its conflicting file opens with the usual conflict blocks. Palette: *Git: Stash…*, *Git: Stash Including Untracked…*, *Git: Stash Pop*, *Git: Stashes…*.

## Commit graph

**History** is a graph: coloured lanes for every branch with merge and fork curves, branch / tag / HEAD labels, author, relative date and subject. It loads a page at a time off the UI thread and draws only the visible rows, so it stays smooth on repositories with 100,000 commits. Right-click a commit to check it out, create a branch or tag there, cherry-pick, revert (not for merge commits), copy the hash or undo the last commit.

## Compare, whitespace and single lines

- **Compare with any ref**: *Git: Compare with branch or commit…* lists branches, remote branches, tags and recent commits, or takes any typed revision (`HEAD~3`, `main...` for "changes since the branches diverged"). *Compare two refs…* and *Compare file with…* work the same. Changed files open side by side; `]` / `[` step through them and `f` brings the list back.
- **Ignore whitespace**: the diff header toggle, the `w` key or `diff_ignore_whitespace` hides changes in spaces, tabs and line endings (`git diff -w`). Staging a hunk still stages the real lines.
- **Stage single lines**: drag rows in the diff or use `Shift+↓` / `Shift+↑`, then `s` stages or unstages them and `r` reverts them in the working file. CRLF files and a missing final newline come out exact. `Esc` drops the selection.

## Merge editor

*Git: Open merge editor* on a conflicted file (or the conflict icon on its row in the Git panel) shows **Incoming** and **Current** side by side over the editable **Result**. Each conflict has *Accept incoming* and *Accept current* checkboxes (both keeps both, in the order checked); typing resolves by hand. The header counts "2 of 5 conflicts resolved", *Previous* / *Next* move between conflicts, and *Mark resolved* saves and stages the file once no marker is left. It works for merge, rebase and cherry-pick conflicts.

## Always up to date

The repository's `.git` is watched: a `git checkout`, `commit` or `pull` typed in a terminal refreshes the branch, gutter marks and status. Regaining window focus checks the open files and the git state, and *File: Refresh explorer and git status* (and a button in the explorer header) does it on demand. On NFS, SMB, FUSE/sshfs, 9p, FAT/exFAT, or when the inotify watch limit is reached, Lumen polls only the open files and expanded folders every `watcher_poll_ms` (2000 by default) and the bottom bar shows "Polling (NFS)".

## Worktrees

See [AI agents and terminals](/docs/ai-agents) for creating, opening, removing and pruning worktrees, and for starting an agent inside one.
