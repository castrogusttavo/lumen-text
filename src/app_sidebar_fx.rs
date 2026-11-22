//! Motion, filter and badge state of the explorer sidebar. Everything that glides or fades lives
//! in [`TreeFx`] (hover / cursor highlight, indent guides, chevrons, the filter ring) and is
//! advanced once per frame by [`App::sidebar_fx_frame`], which reports whether anything still
//! moves so the window sleeps when the tree is at rest. Drawing is in `app_sidebar_draw`.
//!
//! The filter (`/` while the explorer has the focus, or the search button in the header) does not
//! rebuild the tree: rows that match stay bright, the others dim, Up / Down jump between matches
//! and Enter opens the one under the cursor. That keeps every row index stable for the mouse code.

use crate::{
    anim::{Anim, Hot, dur},
    app::{App, Focus, KeyIn, ROW_H, SidebarMode, TAB_H},
    field::TextField,
    textarea::KeyAct,
    tree::Row,
    ui::Rect,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use winit::{
    keyboard::{Key, NamedKey},
    window::CursorIcon,
};

/// Buttons of the empty-folder state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyAct {
    NewFile,
    NewFolder,
}

/// Hit rectangles of the sidebar header and the empty state, rebuilt on every draw.
#[derive(Default)]
pub struct Hits {
    pub search: Option<Rect>,
    pub field: Option<Rect>,
    pub clear: Option<Rect>,
    pub empty: Vec<(Rect, EmptyAct)>,
}

pub struct TreeFx {
    /// Hover highlight: y of its row in list coordinates (`row * ROW_H`), and how visible it is.
    pub hover_y: Anim,
    pub hover_a: Hot,
    /// Keyboard / selection highlight, gliding from row to row.
    pub cursor_y: Anim,
    pub cursor_a: Hot,
    /// Indent guides, visible while the pointer is over the tree.
    pub guides: Hot,
    /// Open (1) / closed (0) amount of the folders seen so far, for the chevron cross-fade.
    pub chev: HashMap<PathBuf, Anim>,
    /// Filter field text, and whether the field is shown.
    pub filter: TextField,
    pub filter_open: bool,
    /// Accent ring of the field while it has the keys.
    pub ring: Hot,
    pub search_hot: Hot,
    pub clear_hot: Hot,
    /// Entrance of the empty state.
    pub empty_t: Anim,
    /// Per row: 2 = matches the filter, 1 = holds a match, 0 = neither (empty = no filter).
    pub flags: Vec<u8>,
    flags_sig: (String, u64),
    /// Strongest git status below each folder (the dot of a collapsed folder).
    pub dir_badge: HashMap<PathBuf, char>,
    badge_sig: (usize, u64),
    pub hits: Hits,
}

impl Default for TreeFx {
    fn default() -> Self {
        Self {
            hover_y: Anim::new(0.0, dur::BASE),
            hover_a: Hot::default(),
            cursor_y: Anim::new(0.0, dur::BASE),
            cursor_a: Hot::default(),
            guides: Hot(Anim::new(0.0, dur::SLOW)),
            chev: HashMap::new(),
            filter: TextField::default(),
            filter_open: false,
            ring: Hot::default(),
            search_hot: Hot::default(),
            clear_hot: Hot::default(),
            empty_t: Anim::new(0.0, dur::SLOW),
            flags: Vec::new(),
            flags_sig: (String::new(), 0),
            dir_badge: HashMap::new(),
            badge_sig: (0, 0),
            hits: Hits::default(),
        }
    }
}

/// Which rows the (lowercase) query `q` lights up: 2 for a name that contains it, 1 for a folder
/// above such a row.
pub fn match_flags(rows: &[Row], q: &str) -> Vec<u8> {
    let mut flags = vec![0u8; rows.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (k, r) in rows.iter().enumerate() {
        stack.truncate(r.depth);
        if r.name.to_lowercase().contains(q) {
            flags[k] = 2;
            for &a in &stack {
                flags[a] = flags[a].max(1);
            }
        }
        stack.push(k);
    }
    flags
}

/// A cheap fingerprint of the visible rows (expanding, renaming or reloading changes it).
pub fn rows_sig(rows: &[Row]) -> u64 {
    rows.iter().fold(rows.len() as u64, |a, r| {
        let first = r.name.bytes().next().map_or(0, u64::from);
        a.wrapping_mul(31).wrapping_add(r.name.len() as u64 + 7 * r.depth as u64 + 13 * first)
    })
}

/// The next row (in `step` direction, wrapping) that matches, starting at `from`.
pub fn next_match(flags: &[u8], from: usize, step: isize, include_self: bool) -> Option<usize> {
    let n = flags.len();
    if n == 0 {
        return None;
    }
    let from = from.min(n - 1) as isize;
    let start = usize::from(!include_self);
    (start..start + n)
        .map(|i| (from + step.signum() * i as isize).rem_euclid(n as isize) as usize)
        .find(|&k| flags[k] == 2)
}

/// Which status wins when a folder holds several: changed over deleted over new.
pub fn badge_rank(c: char) -> u8 {
    match c {
        'M' | 'R' => 3,
        'D' => 2,
        _ => 1,
    }
}

/// The strongest status below every folder that holds a changed path.
pub fn fold_badges(status: &HashMap<PathBuf, char>, root: &Path) -> HashMap<PathBuf, char> {
    let mut out: HashMap<PathBuf, char> = HashMap::new();
    for (p, &c) in status {
        for d in p.ancestors().skip(1).take_while(|d| *d != root) {
            let e = out.entry(d.to_path_buf()).or_insert(c);
            if badge_rank(c) > badge_rank(*e) {
                *e = c;
            }
        }
    }
    out
}

impl App {
    /// Advance the explorer's animations; true while something still moves.
    pub fn sidebar_fx_frame(&mut self, dt: f64) -> bool {
        let files = self.sidebar_mode == SidebarMode::Files;
        let side = self.layout.sidebar;
        let top = side.y + TAB_H;
        let (mx, my) = self.mouse;
        let free = self.overlay.is_none() && !self.tree.drag.as_ref().is_some_and(|d| d.active);
        let in_list = free && side.contains(mx, my) && my >= top as f64 && mx < (side.right() - 15) as f64;
        let n = if files { self.tree.rows.len() } else { self.docs.get(self.active).map_or(0, |d| d.symbols.len()) };
        let rel = my - top as f64 + self.tree.scroll;
        let hover = (in_list && rel >= 0.0).then(|| (rel / ROW_H as f64) as usize).filter(|&k| k < n);
        let focused = self.focus == Focus::Sidebar;
        let single = self.tree.selected.is_empty();
        let first = (self.tree.scroll / ROW_H as f64).floor().max(0.0) as usize;
        let count = ((side.h - TAB_H) / ROW_H + 2).max(0) as usize;
        let over = |r: Option<Rect>| free && r.is_some_and(|r| r.contains(mx, my));
        let (search_over, clear_over) = (over(self.tree.fx.hits.search), over(self.tree.fx.hits.clear));

        let t = &mut self.tree;
        let fx = &mut t.fx;
        match hover {
            Some(k) => {
                let y = (k as i32 * ROW_H) as f32;
                if fx.hover_a.v() < 0.02 {
                    fx.hover_y.snap(y);
                } else {
                    fx.hover_y.to(y);
                }
                fx.hover_a.set(true);
            }
            None => fx.hover_a.set(false),
        }
        fx.cursor_a.set(files && focused && single && !t.rows.is_empty());
        let cy = (t.sel as i32 * ROW_H) as f32;
        if fx.cursor_a.v() < 0.02 {
            fx.cursor_y.snap(cy);
        } else {
            fx.cursor_y.to(cy);
        }
        fx.guides.set(in_list);
        fx.ring.set(fx.filter_open && focused);
        fx.search_hot.set(search_over);
        fx.clear_hot.set(clear_over);
        let show_empty = files && t.rows.is_empty();
        fx.empty_t.to(if show_empty { 1.0 } else { 0.0 });
        if !show_empty {
            fx.empty_t.snap(0.0);
        }
        if files {
            if fx.chev.len() > 512 {
                fx.chev.clear();
            }
            for row in t.rows.iter().skip(first).take(count).filter(|r| r.is_dir) {
                let want = if row.expanded { 1.0 } else { 0.0 };
                match fx.chev.get_mut(&row.path) {
                    Some(a) => a.to(want),
                    None => {
                        fx.chev.insert(row.path.clone(), Anim::new(want, dur::BASE));
                    }
                }
            }
        }
        let mut busy = false;
        for a in fx.chev.values_mut() {
            busy |= a.step(dt);
        }
        busy |= fx.hover_y.step(dt);
        busy |= fx.hover_a.step(dt);
        busy |= fx.cursor_y.step(dt);
        busy |= fx.cursor_a.step(dt);
        busy |= fx.guides.step(dt);
        busy |= fx.ring.step(dt);
        busy |= fx.search_hot.step(dt);
        busy |= fx.clear_hot.step(dt);
        busy |= fx.empty_t.step(dt);
        self.refresh_filter_flags();
        self.refresh_dir_badges();
        busy
    }

    /// Re-run the filter over the rows when the text or the rows changed.
    fn refresh_filter_flags(&mut self) {
        let t = &mut self.tree;
        if t.fx.filter.text.is_empty() {
            t.fx.flags.clear();
            t.fx.flags_sig = (String::new(), 0);
            return;
        }
        let sig = (t.fx.filter.text.to_lowercase(), rows_sig(&t.rows));
        if sig != t.fx.flags_sig {
            t.fx.flags = match_flags(&t.rows, &sig.0);
            t.fx.flags_sig = sig;
        }
    }

    /// Re-fold the git status into per-folder badges when the status changed.
    fn refresh_dir_badges(&mut self) {
        let sum = self.git_status.values().fold(0u64, |a, &c| a.wrapping_add(u64::from(c as u32) * 31 + 7));
        let sig = (self.git_status.len(), sum);
        if sig != self.tree.fx.badge_sig {
            self.tree.fx.dir_badge = fold_badges(&self.git_status, &self.root);
            self.tree.fx.badge_sig = sig;
        }
    }

    /// Show the filter field and give it the keys.
    pub fn sidebar_filter_open(&mut self) {
        self.tree.fx.filter_open = true;
        self.focus = Focus::Sidebar;
        self.request_redraw();
    }

    /// Hide the field and forget the text.
    pub fn sidebar_filter_close(&mut self) {
        self.tree.fx.filter_open = false;
        self.tree.fx.filter = TextField::default();
        self.request_redraw();
    }

    /// Keep the cursor row inside the viewport (same rule as the arrow keys).
    fn sidebar_scroll_to_sel(&mut self) {
        let sel_y = self.tree.sel as f64 * ROW_H as f64;
        let view = (self.layout.sidebar.h - TAB_H).max(ROW_H) as f64;
        if sel_y < self.tree.scroll {
            self.tree.scroll = sel_y;
        } else if sel_y + ROW_H as f64 > self.tree.scroll + view {
            self.tree.scroll = sel_y + ROW_H as f64 - view;
        }
    }

    fn filter_jump(&mut self, step: isize, include_self: bool) {
        if let Some(k) = next_match(&self.tree.fx.flags, self.tree.sel, step, include_self) {
            self.tree.select_only(k);
            self.sidebar_scroll_to_sel();
        }
    }

    /// Keys while the explorer has the focus: `/` opens the filter; with it open the field edits
    /// itself, Up / Down jump between matches, Enter opens, Esc closes. True when taken.
    pub fn sidebar_filter_key(&mut self, ev: &KeyIn) -> bool {
        if self.sidebar_mode != SidebarMode::Files {
            return false;
        }
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        if !self.tree.fx.filter_open {
            if let Key::Character(c) = &ev.key
                && c.as_str() == "/"
                && !ctrl
                && !alt
            {
                self.sidebar_filter_open();
                return true;
            }
            return false;
        }
        match &ev.key {
            Key::Named(NamedKey::Escape) => {
                self.sidebar_filter_close();
                return true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.refresh_filter_flags();
                self.filter_jump(1, false);
                return true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.refresh_filter_flags();
                self.filter_jump(-1, false);
                return true;
            }
            Key::Named(NamedKey::Enter) => {
                let i = self.tree.sel;
                let file = self.tree.rows.get(i).is_some_and(|r| !r.is_dir);
                self.activate_row(i);
                if file {
                    self.sidebar_filter_close();
                }
                return true;
            }
            _ => {}
        }
        match self.tree.fx.filter.key(&ev.key, ctrl, shift, alt) {
            KeyAct::Handled => {}
            KeyAct::Copy(t) => {
                self.clip.set(&t);
                return true;
            }
            KeyAct::Paste => {
                let t = self.clip.get();
                self.tree.fx.filter.insert(&t.replace('\n', " "));
            }
            KeyAct::Ignored => {
                let Some(t) = ev.text.as_deref().filter(|t| !ctrl && !alt && !t.chars().any(char::is_control)) else {
                    return false;
                };
                self.tree.fx.filter.insert(t);
            }
        }
        self.refresh_filter_flags();
        self.filter_jump(1, true);
        true
    }

    /// Left press in the sidebar: the header controls and the empty-state buttons. True when taken.
    pub fn sidebar_click(&mut self, x: f64, y: f64) -> bool {
        if self.sidebar_mode != SidebarMode::Files {
            return false;
        }
        let (clear, field, search, empty) = {
            let h = &self.tree.fx.hits;
            (
                h.clear.is_some_and(|r| r.contains(x, y)),
                h.field.is_some_and(|r| r.contains(x, y)),
                h.search.is_some_and(|r| r.contains(x, y)),
                h.empty.iter().find(|(r, _)| r.contains(x, y)).map(|(_, a)| *a),
            )
        };
        if clear {
            if self.tree.fx.filter.text.is_empty() {
                self.sidebar_filter_close();
            } else {
                self.tree.fx.filter = TextField::default();
            }
        } else if field {
            self.tree.fx.filter_open = true;
        } else if search {
            self.sidebar_filter_open();
        } else if let Some(a) = empty {
            self.prompt_new_file(a == EmptyAct::NewFolder);
        } else {
            return false;
        }
        true
    }

    /// Pointer over a header control (hand) or the filter field (I-beam).
    pub fn sidebar_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        if !self.sidebar_open || self.sidebar_mode != SidebarMode::Files {
            return None;
        }
        let h = &self.tree.fx.hits;
        let on = |r: Option<Rect>| r.is_some_and(|r| r.contains(x, y));
        if on(h.clear) || on(h.search) || h.empty.iter().any(|(r, _)| r.contains(x, y)) {
            Some(CursorIcon::Pointer)
        } else if on(h.field) {
            Some(CursorIcon::Text)
        } else {
            None
        }
    }

    /// Tooltip target: the filter button of the header.
    pub fn sidebar_filter_tip_at(&self, x: f64, y: f64) -> Option<Rect> {
        if !self.sidebar_open || self.sidebar_mode != SidebarMode::Files || !self.overlay.is_none() {
            return None;
        }
        self.tree.fx.hits.search.filter(|r| r.contains(x, y))
    }

    pub fn sidebar_filter_tip(&self) -> crate::app_tooltip::TipText {
        crate::app_tooltip::TipText {
            title: "Filter files".into(),
            keys: Some("/".into()),
            detail: Some("Dims what does not match; Up / Down jump between matches, Enter opens, Esc clears".into()),
            more: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, depth: usize, is_dir: bool) -> Row {
        Row { path: PathBuf::from(name), name: name.into(), depth, is_dir, expanded: is_dir, ignored: false }
    }

    #[test]
    fn filter_marks_matches_and_their_folders() {
        let rows = vec![
            row("src", 0, true),
            row("main.rs", 1, false),
            row("lib", 1, true),
            row("a.rs", 2, false),
            row("docs", 0, true),
        ];
        assert_eq!(match_flags(&rows, "main"), vec![1, 2, 0, 0, 0]);
        assert_eq!(match_flags(&rows, "a.rs"), vec![1, 0, 1, 2, 0]);
        assert_eq!(match_flags(&rows, "zzz"), vec![0; 5]);
        assert_eq!(match_flags(&rows, "DOC"), vec![0; 5], "the query is expected in lowercase");
        assert_eq!(match_flags(&rows, "doc")[4], 2);
    }

    #[test]
    fn next_match_wraps_and_skips() {
        let flags = [0, 2, 0, 2, 0];
        assert_eq!(next_match(&flags, 0, 1, false), Some(1));
        assert_eq!(next_match(&flags, 1, 1, false), Some(3));
        assert_eq!(next_match(&flags, 3, 1, false), Some(1), "wraps around");
        assert_eq!(next_match(&flags, 1, -1, false), Some(3), "backwards wraps too");
        assert_eq!(next_match(&flags, 1, 1, true), Some(1), "the current row counts when asked");
        assert_eq!(next_match(&[0, 0], 0, 1, true), None);
        assert_eq!(next_match(&[], 0, 1, true), None);
    }

    #[test]
    fn folders_carry_the_strongest_status() {
        let mut st = HashMap::new();
        st.insert(PathBuf::from("/p/src/a.rs"), 'A');
        st.insert(PathBuf::from("/p/src/ui/b.rs"), 'M');
        st.insert(PathBuf::from("/p/docs/c.md"), 'D');
        let m = fold_badges(&st, Path::new("/p"));
        assert_eq!(m.get(Path::new("/p/src")), Some(&'M'));
        assert_eq!(m.get(Path::new("/p/src/ui")), Some(&'M'));
        assert_eq!(m.get(Path::new("/p/docs")), Some(&'D'));
        assert!(!m.contains_key(Path::new("/p")), "the root itself carries no dot");
    }

    #[test]
    fn row_fingerprint_changes_with_the_rows() {
        let a = vec![row("a", 0, false), row("b", 0, false)];
        let b = vec![row("a", 0, false), row("c", 0, false)];
        assert_ne!(rows_sig(&a), rows_sig(&b));
        assert_eq!(rows_sig(&a), rows_sig(&a));
    }
}
