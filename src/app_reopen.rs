//! "Reopen closed tab" (`Ctrl+Shift+T`): a bounded stack of the tabs closed most recently, with
//! the cursor, selection, scroll and the split pane each was shown in. Untitled buffers have
//! nothing on disk to come back to and are not remembered; a file deleted since is skipped.

use crate::{app::App, buffer::Sel};
use std::path::PathBuf;

/// Most closed tabs remembered.
pub const MAX_CLOSED: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct ClosedTab {
    pub path: PathBuf,
    pub sel: Sel,
    /// First visible line (fractional) and horizontal scroll in pixels.
    pub top: f64,
    pub left: f64,
    /// Split pane that showed it, as numbered once it was closed (`None`: not split, or not shown).
    pub pane: Option<usize>,
    pub pinned: bool,
}

/// Remember `t` as the most recently closed tab: one entry per file (closing it again moves it
/// to the top), at most [`MAX_CLOSED`] (the oldest are forgotten).
pub fn push_closed(stack: &mut Vec<ClosedTab>, t: ClosedTab) {
    stack.retain(|c| c.path != t.path);
    stack.push(t);
    if stack.len() > MAX_CLOSED {
        let extra = stack.len() - MAX_CLOSED;
        stack.drain(..extra);
    }
}

impl App {
    /// What reopening document `idx` needs, taken before it is closed. The pane is filled in by
    /// [`remember_closed`](Self::remember_closed) once the split has adjusted.
    pub fn closed_tab_entry(&self, idx: usize) -> Option<(ClosedTab, bool)> {
        let d = self.docs.get(idx)?;
        let path = d.path()?.to_path_buf();
        let t = ClosedTab { path, sel: d.ed.sel, top: d.target, left: d.left_target, pane: None, pinned: d.pinned };
        // Only the focused pane can survive its document being closed (other panes on it close).
        Some((t, self.split.is_some() && idx == self.active))
    }

    /// `close_tab` removed the document described by `entry` (see `closed_tab_entry`).
    pub fn remember_closed(&mut self, entry: Option<(ClosedTab, bool)>) {
        let Some((mut t, focused)) = entry else { return };
        if focused {
            t.pane = self.split.as_ref().map(|sp| sp.focus);
        }
        push_closed(&mut self.closed_tabs, t);
    }

    /// Reopen the most recently closed tab where it was: same pane (if it still exists), cursor,
    /// selection and scroll. A file that is open again already is just shown.
    pub fn reopen_closed_tab(&mut self) {
        while let Some(t) = self.closed_tabs.pop() {
            if !t.path.is_file() {
                continue; // deleted or moved since
            }
            if let Some(i) = self.docs.iter().position(|d| d.path() == Some(t.path.as_path())) {
                self.activate(i);
                return;
            }
            if let Some(p) = t.pane
                && self.split.as_ref().is_some_and(|sp| p < sp.panes.len() && p != sp.focus)
            {
                self.split_focus_idx(p);
            }
            let before = self.docs.len();
            let Some(i) = self.open_path(&t.path, None) else { return };
            if i == before {
                let d = &mut self.docs[i];
                let n = d.buf.len_chars();
                d.ed.sel = Sel { anchor: t.sel.anchor.min(n), head: t.sel.head.min(n) };
                d.ed.extra.clear();
                d.top = t.top;
                d.target = t.top;
                d.left = t.left;
                d.left_target = t.left;
                d.reveal = false;
                d.pinned = t.pinned;
            }
            let name = self.docs[i].name.clone();
            self.say(format!("Reopened {name}"));
            self.request_redraw();
            return;
        }
        self.say("No closed tab to reopen");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp, untitled};

    fn entry(p: &str) -> ClosedTab {
        ClosedTab { path: p.into(), sel: Sel::caret(0), top: 0.0, left: 0.0, pane: None, pinned: false }
    }

    #[test]
    fn the_stack_is_bounded_and_keeps_one_entry_per_file() {
        let mut s = Vec::new();
        for i in 0..25 {
            push_closed(&mut s, entry(&format!("/p/{i}")));
        }
        assert_eq!(s.len(), MAX_CLOSED);
        assert_eq!(s[0].path, PathBuf::from("/p/5"), "the oldest are forgotten");
        assert_eq!(s.last().unwrap().path, PathBuf::from("/p/24"));
        push_closed(&mut s, entry("/p/10"));
        assert_eq!(s.len(), MAX_CLOSED);
        assert_eq!(s.last().unwrap().path, PathBuf::from("/p/10"), "closing again moves it to the top");
        assert_eq!(s.iter().filter(|c| c.path == PathBuf::from("/p/10")).count(), 1);
    }

    fn lines(n: usize) -> String {
        (0..n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn reopening_restores_the_cursor_selection_and_scroll() {
        let _g = lock();
        let dir = tmp("reopen");
        let mut app = test_app(&dir);
        let a = dir.join("a.txt");
        std::fs::write(&a, lines(200)).unwrap();
        let i = app.open_path(&a, None).unwrap();
        {
            let d = &mut app.docs[i];
            let (s, e) = (d.buf.pos(120, 2), d.buf.pos(121, 4));
            d.ed.sel = Sel { anchor: s, head: e };
            d.top = 100.0;
            d.target = 100.0;
            d.pinned = true;
        }
        let sel = app.docs[i].ed.sel;
        app.close_tab(i, true);
        assert!(app.docs.iter().all(|d| d.path() != Some(a.as_path())));
        // Untitled buffers are not remembered.
        let u = untitled(&mut app, "scratch");
        app.close_tab(u, true);
        assert_eq!(app.closed_tabs.len(), 1);

        app.run_cmd(crate::app_overlay::Cmd::ReopenClosedTab);
        let d = &app.docs[app.active];
        assert_eq!(d.path(), Some(a.as_path()), "the closed file is back and active");
        assert_eq!(d.ed.sel, sel, "selection restored");
        assert_eq!((d.top, d.target), (100.0, 100.0), "scroll restored");
        assert!(d.pinned);
        assert!(app.closed_tabs.is_empty());
        // Nothing left: a message, no panic.
        app.reopen_closed_tab();
        assert!(app.status.as_ref().is_some_and(|(m, _)| m.contains("No closed tab")));

        // A file deleted since is skipped in favor of the next one.
        let b = dir.join("b.txt");
        std::fs::write(&b, "b\n").unwrap();
        let ib = app.open_path(&b, None).unwrap();
        app.close_tab(ib, true);
        let ia = app.docs.iter().position(|d| d.path() == Some(a.as_path())).unwrap();
        app.close_tab(ia, true);
        std::fs::remove_file(&a).unwrap();
        app.reopen_closed_tab();
        assert_eq!(app.docs[app.active].path(), Some(b.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tab_comes_back_in_the_pane_it_was_closed_in() {
        let _g = lock();
        let dir = tmp("reopenpane");
        let mut app = test_app(&dir);
        app.layout.card = crate::ui::Rect::new(0, 0, 1400, 800);
        let (a, b, c) = (dir.join("a.txt"), dir.join("b.txt"), dir.join("c.txt"));
        for p in [&a, &b, &c] {
            std::fs::write(p, lines(10)).unwrap();
        }
        let ia = app.open_path(&a, None).unwrap();
        app.open_path(&b, None).unwrap();
        app.activate(ia);
        app.split_right(); // pane 1 (focused) on a, pane 0 on a too
        let ib = app.docs.iter().position(|d| d.path() == Some(b.as_path())).unwrap();
        app.activate(ib); // focused pane 1 shows b
        app.split_focus_idx(0);
        app.open_path(&c, None).unwrap(); // pane 0 shows c
        assert_eq!(app.split.as_ref().map(|s| s.panes.len()), Some(2));
        // Close b from pane 1: focus it, close, then move the focus back to pane 0.
        app.split_focus_idx(1);
        let ib = app.docs.iter().position(|d| d.path() == Some(b.as_path())).unwrap();
        app.close_tab(ib, true);
        assert_eq!(app.closed_tabs.last().and_then(|t| t.pane), Some(1));
        app.split_focus_idx(0);
        assert_eq!(app.docs[app.active].path(), Some(c.as_path()), "pane 0 still shows c");

        app.reopen_closed_tab();
        let sp = app.split.as_ref().expect("still split");
        assert_eq!(sp.focus, 1, "back in pane 1");
        assert_eq!(app.docs[app.active].path(), Some(b.as_path()));
        assert_eq!(app.docs[sp.doc_of(0, app.active)].path(), Some(c.as_path()), "pane 0 untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
