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
