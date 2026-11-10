//! Explorer interaction on top of `tree.rs`: multi-selection (Ctrl+click toggles, Shift+click
//! selects a range, Shift+arrows extend it), `Ctrl+C` / `Ctrl+X` / `Ctrl+V` with the internal file
//! clipboard (it holds several paths), actions on the whole selection, and drag-and-drop of files
//! and folders onto folders of the tree (Ctrl while dropping copies). Moves follow the paste rules:
//! a taken name becomes "name (copy)", and open tabs follow the files that moved.

use crate::{
    app::{App, Focus, ROW_H, SidebarMode, TAB_H},
    app_ctxmenu::paste_one,
    tree::{self, Drag},
    ui::{Canvas, Rect},
};
use std::path::{Path, PathBuf};

/// Pixels the pointer must travel with the button down before a press becomes a drag.
const DRAG_START: f64 = 5.0;
/// Distance from the top / bottom edge of the list where dragging scrolls it.
const EDGE: f64 = 22.0;

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// "3 itens" / "a.ts".
pub fn describe(paths: &[PathBuf]) -> String {
    match paths {
        [one] => name_of(one),
        many => format!("{} items", many.len()),
    }
}

impl App {
    fn files_sidebar(&self) -> bool {
        self.sidebar_open && self.sidebar_mode == SidebarMode::Files
    }

    /// Open a file / toggle a folder: what a plain click on a row does.
    fn activate_row(&mut self, i: usize) {
        let Some(row) = self.tree.rows.get(i).cloned() else { return };
        if row.is_dir {
            self.tree.toggle(i);
        } else {
            self.open_path(&row.path, None);
            self.focus = Focus::Editor;
        }
    }

    /// Left press on tree row `i` (the sidebar already took the focus).
    pub fn tree_press(&mut self, i: usize, x: f64, y: f64) {
        let (ctrl, shift) = (self.mods.control_key(), self.mods.shift_key());
        self.tree.drag = None;
        if shift {
            return self.tree.select_range(i);
        }
        if ctrl {
            return self.tree.toggle_select(i);
        }
        let Some(row) = self.tree.rows.get(i).cloned() else { return };
        let sel = self.tree.selection();
        let mk = |paths: Vec<PathBuf>, click_row, toggled| Drag {
            origin: (x, y),
            paths,
            active: false,
            click_row,
            toggled,
            over: None,
            copy: false,
        };
        if sel.len() > 1 && self.tree.is_selected(&row.path) {
            // Keep the selection: this press may start dragging all of it. Released without
            // moving, it is a click and collapses the selection to this row.
            self.tree.drag = Some(mk(sel, Some(i), None));
            return;
        }
        self.tree.select_only(i);
        if row.is_dir {
            // A folder toggles on press (put back if the press turns into a drag).
            self.activate_row(i);
            self.tree.drag = Some(mk(vec![row.path.clone()], None, Some(row.path.clone())));
        } else {
            // A file opens on release, and only if the pointer never dragged it.
            self.tree.drag = Some(mk(vec![row.path.clone()], Some(i), None));
        }
    }

    /// Folder a drop at (x, y) would land in: the folder under the pointer, the folder holding the
    /// file under it, or the project root over the empty area.
    fn drop_dir_at(&self, x: f64, y: f64) -> Option<PathBuf> {
        let sb = self.layout.sidebar;
        if !self.files_sidebar() || !sb.contains(x, y) || y < (sb.y + TAB_H) as f64 {
            return None;
        }
        let rel = y - (sb.y + TAB_H) as f64 + self.tree.scroll;
        let row = (rel >= 0.0).then(|| (rel / ROW_H as f64) as usize).and_then(|k| self.tree.rows.get(k));
        Some(match row {
            Some(r) if r.is_dir => r.path.clone(),
            Some(r) => r.path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()),
            None => self.root.clone(),
        })
    }

    /// Pointer moved with the button down. True when the tree drag consumed the move.
    pub fn tree_drag_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        let Some(d) = self.tree.drag.as_mut() else { return false };
        if !d.active {
            if (x - d.origin.0).hypot(y - d.origin.1) < DRAG_START {
                return false;
            }
            d.active = true;
            // The press toggled a folder open / closed; undo that, the user is dragging it.
            if let Some(p) = d.toggled.take()
                && let Some(k) = self.tree.rows.iter().position(|r| r.path == p)
            {
                self.tree.toggle(k);
            }
        }
        let paths = self.tree.drag.as_ref().map(|d| d.paths.clone()).unwrap_or_default();
        let over = self.drop_dir_at(x, y).filter(|dir| tree::drop_valid(&paths, dir));
        let copy = self.mods.control_key();
        if let Some(d) = self.tree.drag.as_mut() {
            d.over = over;
            d.copy = copy;
        }
        // Scroll while the pointer rests near the top or bottom edge.
        let sb = self.layout.sidebar;
        if self.files_sidebar() && sb.contains(x, y) {
            let (top, bottom) = ((sb.y + TAB_H) as f64, sb.bottom() as f64);
            let max = (self.tree.rows.len() as f64 * ROW_H as f64 - (sb.h - TAB_H) as f64).max(0.0);
            if y < top + EDGE {
                self.tree.scroll = (self.tree.scroll - 12.0).clamp(0.0, max);
            } else if y > bottom - EDGE {
                self.tree.scroll = (self.tree.scroll + 12.0).clamp(0.0, max);
            }
        }
        true
    }

    /// Button released: finish a drag (drop), or turn a press that never moved into a click (a file
    /// opens here; so does a press on a multi-selection, which collapses to that row).
    pub fn tree_drag_release(&mut self) -> bool {
        let Some(d) = self.tree.drag.take() else { return false };
        if !d.active {
            if let Some(i) = d.click_row.filter(|&i| self.tree.rows.get(i).is_some_and(|r| d.paths.contains(&r.path))) {
                self.tree.select_only(i);
                self.activate_row(i);
                self.request_redraw();
                return true;
            }
            return false;
        }
        if let Some(dir) = d.over {
            self.transfer(&d.paths, &dir, !d.copy);
        }
        self.request_redraw();
        true
    }

    /// Move (`cut`) or copy `paths` into `dir`; returns the new paths. Open tabs follow moved files,
    /// the tree reveals and selects the results.
    pub fn transfer(&mut self, paths: &[PathBuf], dir: &Path, cut: bool) -> Vec<PathBuf> {
        let (mut done, mut err): (Vec<PathBuf>, Option<String>) = (Vec::new(), None);
        for from in paths {
            match paste_one(from, dir, cut) {
                Ok(Some(to)) => {
                    if cut {
                        self.repoint_docs(from, &to);
                    }
                    done.push(to);
                }
                Ok(None) => {}
                Err(e) => err = Some(e),
            }
        }
        if let Some(first) = done.first().cloned() {
            self.refresh_tree();
            self.tree.reveal(&first);
            self.watch_dirs();
            self.tree.select_paths(&done);
        }
        match (err, done.len()) {
            (Some(e), _) => self.say(format!("error while {}: {e}", if cut { "moving" } else { "pasting" })),
            (None, 0) => self.say("already in this folder"),
            (None, n) => self.say(format!(
                "{} {}",
                if n == 1 { "1 item".to_string() } else { format!("{n} items") },
                if cut { "moved" } else { "copied" }
            )),
        }
        done
    }

    /// Esc in the explorer: drop a drag in progress, then a pending cut, then the multi-selection.
    /// True when there was something to cancel.
    pub fn explorer_escape(&mut self) -> bool {
        if self.tree.drag.take().is_some_and(|d| d.active) {
            self.request_redraw();
            return true;
        }
        if self.cancel_cut() {
            return true;
        }
        self.tree.clear_selection()
    }

    /// Ctrl+C / Ctrl+X / Ctrl+V / Ctrl+A while the explorer has the focus. True when handled.
    pub fn explorer_ctrl_key(&mut self, key: &str) -> bool {
        match key.to_lowercase().as_str() {
            k @ ("c" | "x") => {
                let paths = self.tree.selection();
                if paths.is_empty() {
                    return true;
                }
                let cut = k == "x";
                self.say(format!("{}: {}", if cut { "cut" } else { "copied" }, describe(&paths)));
                self.set_file_clip(paths, cut);
            }
            "v" => {
                let dir = self.explorer_target_dir();
                self.paste_files(&dir);
            }
            "a" => self.tree.select_all(),
            _ => return false,
        }
        true
    }

    /// Folder Ctrl+V pastes into: the folder under the cursor row, or the folder of the file.
    fn explorer_target_dir(&self) -> PathBuf {
        match self.tree.rows.get(self.tree.sel) {
            Some(r) if r.is_dir => r.path.clone(),
            Some(r) => r.path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()),
            None => self.root.clone(),
        }
    }

    /// Delete / trash every path of a selection (one refresh, one message).
    pub fn remove_paths(&mut self, paths: &[PathBuf], trash: bool) {
        let (mut ok, mut err) = (0, None);
        for p in paths {
            let r = if trash {
                crate::app_ctxmenu::trash_path(p)
            } else if p.is_dir() {
                std::fs::remove_dir_all(p).map_err(|e| e.to_string())
            } else {
                std::fs::remove_file(p).map_err(|e| e.to_string())
            };
            match r {
                Ok(()) => {
                    self.close_docs_under(p);
                    ok += 1;
                }
                Err(e) => err = Some(format!("{}: {e}", name_of(p))),
            }
        }
        self.tree.selected.clear();
        self.refresh_tree();
        self.watch_dirs();
        let verb = if trash { "moved to trash" } else { "deleted" };
        match err {
            Some(e) => self.say(format!("error: {e} ({ok} {verb})")),
            None if ok == 1 => self.say(format!("{verb}: {}", describe(paths))),
            None => self.say(format!("{ok} items {verb}")),
        }
    }

    // ───────────── drawing ─────────────

    /// Drop highlight on the target folder and the floating label under the pointer.
    pub fn draw_tree_drag(&mut self, cv: &mut Canvas) {
        let Some(d) = self.tree.drag.clone().filter(|d| d.active) else { return };
        let c = self.theme.chrome;
        let sb = self.layout.sidebar;
        let body = Rect::new(sb.x, sb.y + TAB_H, sb.w - 1, sb.h - TAB_H);
        if let Some(dir) = &d.over {
            match self.tree.rows.iter().position(|r| &r.path == dir) {
                Some(k) => {
                    let y = body.y - self.tree.scroll as i32 + k as i32 * ROW_H;
                    let r = Rect::new(sb.x + 8, y + 1, sb.w - 17, ROW_H - 2);
                    cv.fill_round_clip(r, 7, c.accent.with_alpha(0x2a), body);
                    if r.y >= body.y && r.bottom() <= body.bottom() {
                        cv.ring(r, 7, c.accent.with_alpha(0xc0));
                    }
                }
                None => cv.ring(body.inset(2), 8, c.accent.with_alpha(0xc0)),
            }
        }
        let verb = if d.copy { "Copy" } else { "Move" };
        let label = match &d.over {
            Some(dir) => {
                let to = if *dir == self.root { "root".to_string() } else { name_of(dir) };
                format!("{verb} {} → {to}", describe(&d.paths))
            }
            None => describe(&d.paths),
        };
        let w = self.tx.ui_width(&label, false) + 24;
        let (sw, sh) = (self.size.0 as i32, self.size.1 as i32);
        let x = (self.mouse.0 as i32 + 14).min(sw - w - 6).max(6);
        let y = (self.mouse.1 as i32 + 16).min(sh - 34).max(6);
        let r = Rect::new(x, y, w, 26);
        cv.shadow(r, 8);
        cv.card(r, 8, c.widget_bg, if d.over.is_some() { c.accent } else { c.border });
        let fg = if d.over.is_some() { self.theme.foreground } else { c.tab_inactive_fg };
        self.tx.draw_ui(cv, r.x + 12, r.y + 4, &label, fg, r, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_names_one_or_counts_many() {
        assert_eq!(describe(&[PathBuf::from("/p/a.ts")]), "a.ts");
        assert_eq!(describe(&[PathBuf::from("/p/a.ts"), PathBuf::from("/p/b.ts")]), "2 items");
    }
}
