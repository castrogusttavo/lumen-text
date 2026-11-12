//! Diff view extras: the "ignore whitespace" toggle and line staging (select rows with the mouse
//! or Shift+arrows, then stage / unstage / revert only those lines; see `gitpatch`).

use crate::{
    app::App,
    gitpatch::{self, Dir},
    gitx::{RowKind, Side},
    ui::line_h,
};

impl App {
    // ───────────── ignore whitespace ─────────────

    /// Bring the open diff in line with the `diff_ignore_whitespace` setting (diffs opened by
    /// code that does not know about it, e.g. the Git panel, are fixed up here).
    pub fn diff_sync_ws(&mut self) {
        let ws = self.config.diff_ignore_whitespace;
        let area_h = self.diff_area().h;
        let Some(dv) = self.diff.as_mut() else { return };
        if dv.ws == ws || !dv.can_ws() {
            return;
        }
        dv.set_ws(ws);
        let max = (dv.rows.len() as f64 - ((area_h - dv.head_h()) / line_h() as i32) as f64).max(0.0);
        dv.scroll = dv.scroll.min(max);
    }

    /// The header toggle / `w`: flip the setting (saved like any other) and redo the diff.
    pub fn diff_toggle_ws(&mut self) {
        if self.diff.as_ref().is_some_and(|d| d.can_ws()) {
            self.toggle_diff_ws_setting();
        }
    }

    /// Palette command: flip `diff_ignore_whitespace` (an open diff follows).
    pub fn toggle_diff_ws_setting(&mut self) {
        let on = !self.config.diff_ignore_whitespace;
        self.apply_setting("diff_ignore_whitespace", &serde_json::json!(on));
        self.diff_sync_ws();
        self.say(if on { "diff: whitespace changes ignored" } else { "diff: whitespace changes shown" });
    }

    // ───────────── row selection ─────────────

    /// The diff row under `y` (rows of the panes, below the header), clamped to the rows.
    fn diff_row_at(&self, y: f64) -> Option<usize> {
        let dv = self.diff.as_ref()?;
        if dv.rows.is_empty() {
            return None;
        }
        let area = self.diff_area();
        let top = (area.y + dv.head_h()) as f64;
        let lh = line_h() as f64;
        let k = (dv.scroll + (y - top) / lh).floor().max(0.0) as usize;
        Some(k.min(dv.rows.len() - 1))
    }

    /// Keep row `k` on screen.
    fn diff_reveal_row(&mut self, k: usize) {
        let area_h = self.diff_area().h;
        let Some(dv) = self.diff.as_mut() else { return };
        let visible = ((area_h - dv.head_h()) / line_h() as i32).max(1) as f64;
        let k = k as f64;
        if k < dv.scroll {
            dv.scroll = k;
        } else if k + 1.0 > dv.scroll + visible {
            dv.scroll = k + 1.0 - visible;
        }
    }

    /// Press in the diff panes: start a row selection there.
    pub fn diff_press(&mut self, x: f64, y: f64) {
        let area = self.diff_area();
        let Some(head_h) = self.diff.as_ref().map(|d| d.head_h()) else { return };
        if !area.contains(x, y) || y < (area.y + head_h) as f64 {
            return;
        }
        let Some(k) = self.diff_row_at(y) else { return };
        if let Some(dv) = self.diff.as_mut() {
            dv.sel = Some((k, k));
            dv.drag = true;
            if let Some(b) = dv.block_of(k) {
                dv.cur = b;
            }
        }
    }

    /// Mouse moved: extend a row selection being dragged (scrolling at the edges). True = handled.
    pub fn diff_drag_move(&mut self, _x: f64, y: f64) -> bool {
        if !self.diff.as_ref().is_some_and(|d| d.drag) {
            return false;
        }
        let Some(k) = self.diff_row_at(y) else { return true };
        if let Some(dv) = self.diff.as_mut()
            && let Some((a, _)) = dv.sel
        {
            dv.sel = Some((a, k));
        }
        self.diff_reveal_row(k);
        true
    }

    pub fn diff_release(&mut self) {
        if let Some(dv) = self.diff.as_mut() {
            dv.drag = false;
        }
    }

    /// Shift+Down / Shift+Up: grow or shrink the selection from its head (it starts on the
    /// current block's first row).
    pub fn diff_extend_sel(&mut self, d: isize) {
        let Some(dv) = self.diff.as_mut() else { return };
        if dv.rows.is_empty() {
            return;
        }
        let last = dv.rows.len() - 1;
        let (a, h) = match dv.sel {
            Some((a, h)) => (a, (h as isize + d).clamp(0, last as isize) as usize),
            None => {
                // Start on the current block when it is on screen, else on the first row shown.
                let top = dv.scroll as usize;
                let start = dv.blocks.get(dv.cur).map(|b| b.0).filter(|&s| s >= top).unwrap_or(top.min(last));
                (start, start)
            }
        };
        dv.sel = Some((a, h));
        if let Some(b) = dv.block_of(h) {
            dv.cur = b;
        }
        self.diff_reveal_row(h);
    }

    // ───────────── stage / unstage / revert lines ─────────────

    /// Stage (not-staged side) or unstage (staged side) the selected lines; with `revert`, drop
    /// them from the working file instead. Without a selection, the current block is used.
    pub fn diff_lines(&mut self, revert: bool) {
        self.diff_sync_ws();
        let Some(dv) = self.diff.as_ref() else { return };
        let Some(src) = dv.src.as_ref() else { return };
        if !src.editable {
            return self
                .say("line by line only works for files that already exist in HEAD and the index; use the checkbox");
        }
        let range = dv.sel_range().or_else(|| dv.blocks.get(dv.cur).map(|&(a, b)| (a, b.saturating_sub(1))));
        let Some((lo, hi)) = range else { return self.say("no changes here") };
        let n = dv.rows.iter().take(hi + 1).skip(lo).filter(|r| r.kind != RowKind::Same).count();
        if n == 0 {
            return self.say("no changed lines selected");
        }
        let (dir, cached) = match (src.side, revert) {
            (Side::Unstaged, false) => (Dir::Apply, true),
            (Side::Staged, false) => (Dir::Undo, true),
            (Side::Unstaged, true) => (Dir::Undo, false),
            (Side::Staged, true) => return self.say("revert works on changes that are not staged: unstage them first"),
        };
        let path = src.top.join(&src.rel);
        let open = self.docs.iter().position(|d| d.path() == Some(path.as_path()));
        if revert && open.is_some_and(|i| self.docs[i].buf.is_dirty()) {
            return self.say("save the file first: revert changes the file on disk");
        }
        let Some(patch) =
            gitpatch::partial_patch(&src.rel, &src.old, &src.new, &dv.rows, |k| (lo..=hi).contains(&k), dir)
        else {
            return self.say("nothing to apply");
        };
        let (top, keep) = (src.top.clone(), dv.cur);
        let res = gitpatch::apply(&top, &patch, cached);
        if let Err(e) = res {
            self.say(format!("git: {e}"));
            // The file or the index probably changed under the diff: show what is there now.
            return self.diff_reload(keep);
        }
        if revert && let Some(i) = open {
            let _ = self.docs[i].reload_from_disk();
        }
        self.diff_reload(keep);
        self.refresh_git_status();
        let lines = if n == 1 { "1 line".to_string() } else { format!("{n} lines") };
        self.say(match (revert, dir) {
            (true, _) => format!("{lines} reverted in the file"),
            (false, Dir::Apply) => format!("{lines} added to stage"),
            (false, Dir::Undo) => format!("{lines} removed from stage"),
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::app_gitpanel::git_run;
    use crate::app_gitx::{DiffBtn, DiffView};
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use crate::gitx::{RowKind, Side};
    use std::path::Path;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    fn repo(d: &Path, base: &str) {
        for a in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "T"],
            &["config", "commit.gpgsign", "false"],
            &["config", "core.autocrlf", "false"],
        ] {
            git_run(d, a).unwrap();
        }
        std::fs::write(d.join("f.txt"), base).unwrap();
        git_run(d, &["add", "."]).unwrap();
        git_run(d, &["commit", "-q", "-m", "base"]).unwrap();
    }

    fn key(app: &mut App, k: Key, mods: ModifiersState) {
        app.mods = mods;
        app.diff_key(&k);
        app.mods = ModifiersState::empty();
    }

    fn ch(app: &mut App, c: &str) {
        key(app, Key::Character(c.into()), ModifiersState::empty());
    }

    /// Draw one frame (lays the window out and records the diff's buttons).
    fn render(app: &mut App) {
        let (w, h) = (1100, 720);
        let mut px = vec![0u32; (w * h) as usize];
        let mut cv = crate::ui::Canvas { px: &mut px, w, h };
        app.render_into(&mut cv, 0.016);
    }

    fn index(d: &Path) -> String {
        git_run(d, &["show", ":f.txt"]).unwrap()
    }

    #[test]
    fn shift_arrows_select_lines_and_s_stages_only_them() {
        let _g = lock();
        let dir = tmp("difflines");
        let mut app = test_app(&dir);
        let d = dir.join("repo");
        std::fs::create_dir_all(&d).unwrap();
        repo(&d, "a\nb\nc\nd\n");
        std::fs::write(d.join("f.txt"), "a\nB\nC\nd\ne\n").unwrap();
        app.diff = DiffView::staging_auto(&d, "f.txt", None, Side::Unstaged, "f.txt");
        let k: Vec<_> = app.diff.as_ref().unwrap().rows.iter().map(|r| r.kind).collect();
        assert_eq!(k, [RowKind::Same, RowKind::Changed, RowKind::Changed, RowKind::Same, RowKind::Added]);

        // Shift+Down starts on the current block (rows 1..3) and grows by one row: select row 1 only
        // after one step back.
        key(&mut app, Key::Named(NamedKey::ArrowDown), ModifiersState::SHIFT);
        assert_eq!(app.diff.as_ref().unwrap().sel, Some((1, 1)));
        key(&mut app, Key::Named(NamedKey::ArrowDown), ModifiersState::SHIFT);
        assert_eq!(app.diff.as_ref().unwrap().sel, Some((1, 2)));
        key(&mut app, Key::Named(NamedKey::ArrowUp), ModifiersState::SHIFT);
        assert_eq!(app.diff.as_ref().unwrap().sel_changes(), 1);
        render(&mut app);
        let btns = app.diff.as_ref().unwrap().btns.clone();
        assert!(btns.iter().any(|(_, b)| *b == DiffBtn::Lines), "a Stage lines button on the selection");
        assert!(btns.iter().any(|(_, b)| *b == DiffBtn::Ws), "the whitespace toggle");
        ch(&mut app, "s");
        assert_eq!(index(&d), "a\nB\nc\nd\n", "only the first changed line is staged");
        let dv = app.diff.as_ref().unwrap();
        assert!(dv.sel.is_none(), "the diff was re-read");
        assert_eq!(dv.src.as_ref().unwrap().side, Side::Unstaged);
        assert_eq!(dv.blocks.len(), 2, "C and e are still not staged");

        // Esc drops a selection before it closes the diff.
        key(&mut app, Key::Named(NamedKey::ArrowDown), ModifiersState::SHIFT);
        key(&mut app, Key::Named(NamedKey::Escape), ModifiersState::empty());
        assert!(app.diff.as_ref().is_some_and(|d| d.sel.is_none()));

        // Flip to the staged side and unstage that line again, by selecting it.
        app.diff_flip();
        assert_eq!(app.diff.as_ref().unwrap().src.as_ref().unwrap().side, Side::Staged);
        let row = app.diff.as_ref().unwrap().rows.iter().position(|r| r.kind == RowKind::Changed).unwrap();
        app.diff.as_mut().unwrap().sel = Some((row, row));
        ch(&mut app, "s");
        assert_eq!(index(&d), "a\nb\nc\nd\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn r_reverts_selected_lines_in_the_file_and_the_open_buffer() {
        let _g = lock();
        let dir = tmp("diffrevert");
        let mut app = test_app(&dir);
        let d = dir.join("repo");
        std::fs::create_dir_all(&d).unwrap();
        repo(&d, "one\ntwo\nthree\n");
        std::fs::write(d.join("f.txt"), "ONE\ntwo\nthree\nfour\n").unwrap();
        app.open_path(&d.join("f.txt"), None);
        app.diff = DiffView::staging_auto(&d, "f.txt", None, Side::Unstaged, "f.txt");
        let added = app.diff.as_ref().unwrap().rows.iter().position(|r| r.kind == RowKind::Added).unwrap();
        app.diff.as_mut().unwrap().sel = Some((added, added));
        ch(&mut app, "r");
        assert_eq!(std::fs::read_to_string(d.join("f.txt")).unwrap(), "ONE\ntwo\nthree\n");
        let doc = app.docs.iter().find(|x| x.path() == Some(d.join("f.txt").as_path())).unwrap();
        assert_eq!(doc.buf.to_string(), "ONE\ntwo\nthree\n", "the open buffer follows");
        assert_eq!(index(&d), "one\ntwo\nthree\n", "the index is untouched");
        // Revert does not apply to the staged side.
        git_run(&d, &["add", "f.txt"]).unwrap();
        app.diff = DiffView::staging_auto(&d, "f.txt", None, Side::Staged, "f.txt");
        app.diff.as_mut().unwrap().sel = Some((0, 0));
        ch(&mut app, "r");
        assert_eq!(index(&d), "ONE\ntwo\nthree\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn whitespace_toggle_recomputes_and_staging_still_applies() {
        let _g = lock();
        let dir = tmp("diffws");
        let mut app = test_app(&dir);
        let d = dir.join("repo");
        std::fs::create_dir_all(&d).unwrap();
        repo(&d, "a\n  b\nc\n");
        std::fs::write(d.join("f.txt"), "a\n    b\nC\n").unwrap();
        app.config.diff_ignore_whitespace = false;
        app.diff = DiffView::staging_auto(&d, "f.txt", None, Side::Unstaged, "f.txt");
        assert_eq!(app.diff.as_ref().unwrap().blocks.len(), 1, "b and c are adjacent: one block");
        assert_eq!(app.diff.as_ref().unwrap().rows.iter().filter(|r| r.kind != RowKind::Same).count(), 2);
        ch(&mut app, "w");
        assert!(app.config.diff_ignore_whitespace);
        let dv = app.diff.as_ref().unwrap();
        assert!(dv.ws);
        assert_eq!(dv.rows.iter().filter(|r| r.kind != RowKind::Same).count(), 1, "the re-indent is hidden");
        assert!(dv.src.as_ref().unwrap().editable);
        // Staging the visible hunk stages C only.
        ch(&mut app, "s");
        assert_eq!(index(&d), "a\n  b\nC\n");
        // A diff opened without the setting (the Git panel's way) is fixed up on the next key.
        app.diff = DiffView::staging_auto(&d, "f.txt", None, Side::Unstaged, "f.txt");
        assert!(!app.diff.as_ref().unwrap().ws);
        app.diff_sync_ws();
        let dv = app.diff.as_ref().unwrap();
        assert!(dv.ws && dv.blocks.is_empty(), "only whitespace left: the view stays, without blocks");
        render(&mut app);
        ch(&mut app, "w");
        assert!(!app.config.diff_ignore_whitespace);
        assert_eq!(app.diff.as_ref().unwrap().blocks.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mouse_drag_selects_rows() {
        let _g = lock();
        let dir = tmp("diffdrag");
        let mut app = test_app(&dir);
        let rows = crate::gitx::side_by_side("a\nb\nc\nd\n", "a\nB\nC\nD\n");
        app.diff = Some(DiffView::plain("t".into(), "l".into(), "r".into(), rows));
        render(&mut app);
        let area = app.diff_area();
        assert!(area.h > 100, "laid out");
        let head = app.diff.as_ref().unwrap().head_h();
        let lh = crate::ui::line_h() as f64;
        let y = |k: usize| (area.y + head) as f64 + lh * k as f64 + lh / 2.0;
        let x = (area.x + 40) as f64;
        app.diff_press(x, y(1));
        assert!(app.diff_drag_move(x, y(3)));
        app.diff_release();
        assert!(!app.diff_drag_move(x, y(0)), "released: no more dragging");
        let dv = app.diff.as_ref().unwrap();
        assert_eq!(dv.sel_range(), Some((1, 3)));
        assert_eq!(dv.sel_changes(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
