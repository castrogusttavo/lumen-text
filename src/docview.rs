//! Glue between a [`Doc`] and `rowmap`: word wrap + code folding state, the fold commands,
//! visual-row coordinates and vertical caret movement.
//!
//! The view is *inactive* (no `RowMap` kept, rows == lines) unless word wrap is on or something
//! is folded, so plain editing pays nothing for it. All scroll units (`Doc::top`, `target`) are
//! visual rows; with an inactive view a row is a line.

use crate::{
    buffer::Buffer,
    doc::Doc,
    editor::{Editor, col_from_visual},
    rowmap::{FoldSet, RowMap, col_at, fold_regions_by_indent, locate, merge_regions, wrap_breaks},
};

/// Extra gutter width reserved for the fold chevron.
pub const FOLD_W: i32 = 12;

pub struct ViewMap {
    /// User setting (Config.word_wrap).
    pub wrap: bool,
    /// Effective wrap width in columns (0 = no wrapping).
    pub cols: usize,
    pub tab_w: usize,
    pub folds: FoldSet,
    /// Foldable regions of the text at `avail_ver` (computed on demand).
    pub avail: FoldSet,
    pub avail_ver: i32,
    /// Ranges from the language server (`textDocument/foldingRange`) and the text version they
    /// describe; merged over the indentation regions while that version is current.
    pub lsp_regions: Vec<(usize, usize)>,
    pub lsp_ver: i32,
    wrap_rows: Vec<u32>,
    wrap_valid: bool,
    pub map: RowMap,
    lines: usize,
}

impl Default for ViewMap {
    fn default() -> Self {
        Self {
            wrap: false,
            cols: 0,
            tab_w: 4,
            folds: FoldSet::new(),
            avail: FoldSet::new(),
            avail_ver: -1,
            lsp_regions: Vec::new(),
            lsp_ver: -1,
            wrap_rows: Vec::new(),
            wrap_valid: false,
            map: RowMap::new(0),
            lines: 0,
        }
    }
}

impl ViewMap {
    pub fn active(&self) -> bool {
        self.cols > 0 || !self.folds.is_empty()
    }

    /// Char indices where each continuation row of `text` starts (empty without wrapping).
    pub fn breaks(&self, text: &str) -> Vec<usize> {
        if self.cols == 0 { Vec::new() } else { wrap_breaks(text, self.cols, self.tab_w) }
    }

    /// Forget everything derived from the text (it was replaced wholesale).
    pub fn reset(&mut self) {
        self.map = RowMap::new(0);
        self.wrap_rows = Vec::new();
        self.wrap_valid = false;
        self.lines = 0;
    }

    /// Exact number of visual rows `text` needs.
    pub fn rows_for(&self, text: &str) -> usize {
        if self.cols == 0 || (text.len() <= self.cols && !text.contains('\t')) {
            1
        } else {
            wrap_breaks(text, self.cols, self.tab_w).len() + 1
        }
    }
}

/// Visual column of char column `col` of `text`, counting from char `from` (row start).
pub fn visual_col_from(text: &str, from: usize, col: usize, tab_w: usize) -> usize {
    let mut v = 0;
    for ch in text.chars().skip(from).take(col.saturating_sub(from)) {
        v += if ch == '\t' { tab_w - v % tab_w } else { 1 };
    }
    v
}

/// Char range `(start, end)` of row `sub` of a line with `nchars` chars and wrap `breaks`.
pub fn row_range(breaks: &[usize], sub: usize, nchars: usize) -> (usize, usize) {
    let sub = sub.min(breaks.len());
    let c0 = if sub == 0 { 0 } else { breaks[sub - 1] };
    let c1 = breaks.get(sub).copied().unwrap_or(nchars);
    (c0, c1)
}

fn indent_width(s: &str, tab_w: usize) -> Option<usize> {
    let mut v = 0;
    for ch in s.chars() {
        match ch {
            ' ' => v += 1,
            '\t' => v += tab_w - v % tab_w,
            c if c.is_whitespace() => v += 1,
            _ => return Some(v),
        }
    }
    None
}

/// Move the caret of `e` by `delta` visual rows (word wrap and folds aware).
pub fn vertical_rows(e: &mut Editor, buf: &mut Buffer, extend: bool, delta: isize, view: &ViewMap) {
    let tab_w = view.tab_w;
    let (l, col) = buf.line_col(e.head());
    let text = buf.line(l);
    let breaks = view.breaks(&text);
    let (sub, _) = locate(&breaks, col);
    let c0 = if sub == 0 { 0 } else { breaks[sub - 1] };
    let vcol = e.want_col().unwrap_or_else(|| visual_col_from(&text, c0, col, tab_w));
    let target = view.map.row_of(l) as isize + sub as isize + delta;
    if target < 0 {
        return e.vertical_to(buf, extend, 0, None);
    }
    if target as usize >= view.map.total_rows() {
        let end = buf.len_chars();
        return e.vertical_to(buf, extend, end, None);
    }
    let (tl, tsub) = view.map.line_at_row(target as usize);
    let ttext = buf.line(tl);
    let tb = view.breaks(&ttext);
    let (r0, r1) = row_range(&tb, tsub, ttext.chars().count());
    let rowtext: String = ttext.chars().skip(r0).take(r1 - r0).collect();
    let mut c = r0 + col_from_visual(&rowtext, vcol, tab_w);
    if tsub < tb.len() {
        // Not the last row of its line: stay before the wrap point.
        c = c.min(r1.saturating_sub(1).max(r0));
    }
    let p = buf.pos(tl, c);
    e.vertical_to(buf, extend, p, Some(vcol));
}

impl Doc {
    /// The view map describes the current text (cheap consistency check).
    pub fn vm_live(&self) -> bool {
        self.view.active() && self.view.map.line_count() == self.buf.line_count()
    }

    pub fn total_rows(&self) -> usize {
        if self.vm_live() { self.view.map.total_rows() } else { self.buf.line_count() }
    }

    /// First visual row of `line`.
    pub fn row_of_line(&self, line: usize) -> usize {
        if self.vm_live() { self.view.map.row_of(line) } else { line }
    }

    /// (line, row within the line) shown at the top of the viewport.
    pub fn top_pos(&self) -> (usize, usize) {
        let row = self.top.max(0.0) as usize;
        if self.vm_live() { self.view.map.line_at_row(row) } else { (row.min(self.buf.line_count().saturating_sub(1)), 0) }
    }

    /// Scroll so that (line, sub) is at the top again, after the map changed.
    fn set_top_to(&mut self, (line, sub): (usize, usize)) {
        let line = self.nearest_visible(line);
        let row = if self.vm_live() { self.view.map.row_of(line) + sub.min(self.view.map.rows_in(line).saturating_sub(1)) } else { line };
        if row != self.top.max(0.0) as usize {
            self.top = row as f64;
            self.target = self.top;
            self.animating = false;
        }
        let mt = self.max_top();
        self.target = self.target.clamp(0.0, mt);
        self.top = self.top.clamp(0.0, mt);
    }

    /// `line`, or the fold header that hides it.
    pub fn nearest_visible(&self, line: usize) -> usize {
        let line = line.min(self.buf.line_count().saturating_sub(1));
        match self.view.folds.hidden_ranges().iter().find(|r| r.0 <= line && line <= r.1) {
            Some(r) => r.0.saturating_sub(1),
            None => line,
        }
    }

    /// (global visual row, visual column inside that row) of char index `c`.
    pub fn caret_geom(&self, c: usize) -> (usize, usize) {
        let (line, col) = self.buf.line_col(c);
        let text = self.buf.line(line);
        if !self.vm_live() {
            return (line, self.inlay.hints.vcol(line, &text, col, self.ed.tab_w, false));
        }
        if self.view.cols == 0 {
            // Folds only: a row is a whole line (inlay hints apply).
            return (self.view.map.row_of(line), self.inlay.hints.vcol(line, &text, col, self.ed.tab_w, false));
        }
        let breaks = self.view.breaks(&text);
        let (sub, _) = locate(&breaks, col);
        let sub = sub.min(self.view.map.rows_in(line).saturating_sub(1));
        let c0 = if sub == 0 { 0 } else { breaks[sub - 1] };
        (self.view.map.row_of(line) + sub, visual_col_from(&text, c0, col, self.ed.tab_w))
    }

    /// First and last logical line touched by the viewport (for LSP requests keyed by line).
    pub fn visible_line_span(&self) -> (usize, usize) {
        let n = self.buf.line_count();
        let first = self.top_pos().0.min(n - 1);
        let bottom = (self.top + self.visible_lines()) as usize + 1;
        let last = if self.vm_live() { self.view.map.line_at_row(bottom).0 } else { bottom };
        (first, last.min(n - 1))
    }

    // ── keeping the map in step ──

    pub fn set_word_wrap(&mut self, on: bool) {
        if self.view.wrap != on {
            self.view.wrap = on;
            if on {
                self.left = 0.0;
            }
        }
    }

    /// Per-frame: adopt the wrap width (`cols_avail`, 0 = no room) and rebuild when anything that
    /// affects the row layout changed.
    pub fn view_prepare(&mut self, cols_avail: usize) {
        let want = if self.view.wrap { cols_avail } else { 0 };
        let tab = self.ed.tab_w;
        let n = self.buf.line_count();
        let changed = want != self.view.cols || tab != self.view.tab_w || !self.view.wrap_valid && want > 0;
        let broken = self.view.active() && self.view.map.line_count() != n;
        if !changed && !broken {
            return;
        }
        let anchor = if broken { (self.top.max(0.0) as usize, 0) } else { self.top_pos() };
        if changed {
            self.view.wrap_valid = false;
        }
        self.view.cols = want;
        self.view.tab_w = tab;
        self.view_rebuild();
        self.set_top_to(anchor);
    }

    /// Recompute everything (exact wrap rows when wrapping, then the map).
    fn view_rebuild(&mut self) {
        let n = self.buf.line_count();
        self.view.lines = n;
        if !self.view.active() {
            self.view.map = RowMap::new(0);
            self.view.wrap_rows = Vec::new();
            self.view.wrap_valid = false;
            return;
        }
        if self.view.cols > 0 && (!self.view.wrap_valid || self.view.wrap_rows.len() != n) {
            let mut rows = Vec::with_capacity(n);
            for i in 0..n {
                rows.push(if self.view.folds.is_hidden(i) { 1 } else { self.view.rows_for(&self.buf.line(i)) as u32 });
            }
            self.view.wrap_rows = rows;
            self.view.wrap_valid = true;
        } else if self.view.cols == 0 {
            self.view.wrap_rows = Vec::new();
            self.view.wrap_valid = false;
        }
        self.view_remap();
    }

    /// Derive the `RowMap` from the cached wrap rows and the folds.
    fn view_remap(&mut self) {
        let n = self.buf.line_count();
        self.view.lines = n;
        if !self.view.active() {
            self.view.map = RowMap::new(0);
            return;
        }
        let wrap = self.view.cols > 0 && self.view.wrap_rows.len() == n;
        let rows: Vec<usize> = (0..n)
            .map(|i| if self.view.folds.is_hidden(i) { 0 } else if wrap { self.view.wrap_rows[i] as usize } else { 1 })
            .collect();
        let mut m = RowMap::new(n);
        m.splice_lines(0, n, &rows);
        self.view.map = m;
    }

    /// Call after the folds changed; `anchor` is `top_pos()` taken BEFORE the change.
    fn view_refold(&mut self, anchor: (usize, usize)) {
        self.view_remap();
        self.set_top_to(anchor);
    }

    /// Text changed: first changed line `l`. Keeps folds, wrap rows and the map consistent.
    pub fn view_on_edit(&mut self, l: usize) {
        let n = self.buf.line_count();
        if !self.view.active() {
            self.view.lines = n;
            return;
        }
        let old = self.view.lines;
        let delta = n as isize - old as isize;
        let was_folds = !self.view.folds.is_empty();
        if delta != 0 {
            self.view.folds.adjust_for_edit(l, delta);
            let wr = &mut self.view.wrap_rows;
            if wr.len() == old {
                let at = (l + 1).min(wr.len());
                if delta > 0 {
                    wr.splice(at..at, std::iter::repeat_n(1u32, delta as usize));
                } else {
                    let end = (at + (-delta) as usize).min(wr.len());
                    wr.drain(at..end);
                }
            } else {
                self.view.wrap_valid = false;
            }
        }
        // Exact rows for the edited line(s).
        if self.view.cols > 0 && self.view.wrap_valid && self.view.wrap_rows.len() == n {
            let hi = (l + delta.max(0) as usize).min(n - 1);
            for i in l.min(n - 1)..=hi {
                if !self.view.folds.is_hidden(i) {
                    self.view.wrap_rows[i] = self.view.rows_for(&self.buf.line(i)) as u32;
                }
            }
        } else if self.view.cols > 0 {
            self.view.wrap_valid = false;
        }
        if !self.view.wrap_valid && self.view.cols > 0 {
            self.view_rebuild();
        } else if delta != 0 || was_folds != !self.view.folds.is_empty() {
            self.view_remap();
        } else if self.view.map.line_count() == n && l < n {
            let r = if self.view.cols > 0 { self.view.wrap_rows[l] as usize } else { 1 };
            if !self.view.folds.is_hidden(l) {
                self.view.map.update_line(l, r);
            }
        }
        self.view.lines = n;
    }

    /// Before scrolling to the caret: unfold what hides it and make its row count exact.
    pub fn view_reveal_caret(&mut self) {
        let line = self.caret_line();
        if self.view.folds.is_hidden(line) {
            let anchor = self.top_pos();
            self.view.folds.reveal(line);
            self.view_refold(anchor);
        }
        self.view_fix_line(line);
    }

    /// Make the row counts of `line` exact (e.g. a line that was hidden when the map was built).
    /// Returns true when something changed.
    pub fn view_fix_line(&mut self, line: usize) -> bool {
        if self.view.cols == 0 || !self.vm_live() || line >= self.buf.line_count() || self.view.folds.is_hidden(line) {
            return false;
        }
        let n = self.view.rows_for(&self.buf.line(line)) as u32;
        if self.view.wrap_rows.get(line).is_some_and(|&v| v != n) {
            self.view.wrap_rows[line] = n;
            self.view.map.update_line(line, n as usize);
            return true;
        }
        false
    }

    /// Verify the wrap rows of the lines about to be drawn; keeps the top line anchored.
    pub fn view_sync_visible(&mut self, rows_wanted: usize) {
        if self.view.cols == 0 || !self.vm_live() {
            return;
        }
        for _ in 0..3 {
            let anchor = self.top_pos();
            let first = self.top.max(0.0) as usize;
            let mut changed = false;
            let mut prev = usize::MAX;
            for (line, _) in self.view.map.visible_lines(first, rows_wanted) {
                if line != prev {
                    prev = line;
                    changed |= self.view_fix_line(line);
                }
            }
            if !changed {
                break;
            }
            self.set_top_to(anchor);
        }
    }

    // ── folding ──

    /// Foldable regions of the current text (indentation based), cached per text version.
    pub fn fold_regions(&mut self) -> Vec<(usize, usize)> {
        if self.view.avail_ver != self.version {
            let tab = self.ed.tab_w;
            let n = self.buf.line_count();
            let buf = &self.buf;
            let mut r = fold_regions_by_indent(&mut |i| Some(buf.line(i)), n, tab);
            if self.view.lsp_ver == self.version {
                r = merge_regions(&r, &self.view.lsp_regions);
            }
            self.view.avail.fold_all(&r);
            self.view.avail_ver = self.version;
        }
        self.view.avail.regions().to_vec()
    }

    /// The language server answered `foldingRange` for the current text.
    pub fn set_lsp_folds(&mut self, regions: Vec<(usize, usize)>) {
        let n = self.buf.line_count();
        self.view.lsp_regions = regions.into_iter().filter(|r| r.1 < n).collect();
        self.view.lsp_ver = self.version;
        self.view.avail_ver = -1; // recompute the merged list on demand
    }

    /// Cheap per-line test for the gutter chevron: is `line` the header of an indented block?
    pub fn is_foldable(&self, line: usize) -> bool {
        if self.view.lsp_ver == self.version && self.view.lsp_regions.binary_search_by_key(&line, |r| r.0).is_ok() {
            return true;
        }
        let tab = self.ed.tab_w;
        let Some(own) = indent_width(&self.buf.line(line), tab) else { return false };
        let n = self.buf.line_count();
        let mut l = line + 1;
        while l < n && l < line + 200 {
            if let Some(w) = indent_width(&self.buf.line(l), tab) {
                return w > own;
            }
            l += 1;
        }
        false
    }

    fn caret_line(&self) -> usize {
        self.buf.line_col(self.ed.head()).0
    }

    /// If the caret ended up inside a fold, move it to the end of the header line.
    fn caret_out_of_folds(&mut self) {
        let line = self.caret_line();
        if self.view.folds.is_hidden(line) {
            let h = self.nearest_visible(line);
            let p = self.buf.pos(h, usize::MAX);
            self.ed.click(&mut self.buf, p, false);
        }
    }

    /// Fold the innermost unfolded region around the caret. Returns whether something changed.
    pub fn fold_at_cursor(&mut self) -> bool {
        let regions = self.fold_regions();
        let line = self.caret_line();
        let best = regions
            .iter()
            .filter(|r| r.0 <= line && line <= r.1 && self.view.folds.is_fold_start(r.0).is_none())
            .max_by_key(|r| r.0)
            .copied();
        let Some((s, e)) = best else { return false };
        let anchor = self.top_pos();
        self.view.folds.fold(s, e);
        self.caret_out_of_folds();
        self.view_refold(anchor);
        self.reveal = true;
        true
    }

    /// Unfold the fold at the caret line (or the innermost one around it).
    pub fn unfold_at_cursor(&mut self) -> bool {
        let line = self.caret_line();
        let start = self
            .view
            .folds
            .regions()
            .iter()
            .filter(|r| r.0 <= line && line <= r.1)
            .map(|r| r.0)
            .max();
        let Some(s) = start else { return false };
        let anchor = self.top_pos();
        self.view.folds.unfold(s);
        self.view_refold(anchor);
        true
    }

    pub fn fold_all(&mut self) {
        let regions = self.fold_regions();
        let anchor = self.top_pos();
        self.view.folds.fold_all(&regions);
        self.caret_out_of_folds();
        self.view_refold(anchor);
        self.reveal = true;
    }

    pub fn unfold_all(&mut self) {
        if self.view.folds.is_empty() {
            return;
        }
        let anchor = self.top_pos();
        self.view.folds.unfold_all();
        self.view_refold(anchor);
    }

    /// Toggle the fold that starts at `line` (chevron / "…" click).
    pub fn toggle_fold(&mut self, line: usize) -> bool {
        let regions = self.fold_regions();
        let anchor = self.top_pos();
        let changed = self.view.folds.toggle_at(line, &regions);
        if changed {
            self.caret_out_of_folds();
            self.view_refold(anchor);
        }
        changed
    }

    /// Mouse press at window (x, y): handles the gutter chevron and the folded "…" marker.
    pub fn click_fold(&mut self, x: f64, y: f64) -> bool {
        if let Some(line) = self.fold_pills.iter().find(|(r, _)| r.contains(x, y)).map(|(_, l)| *l) {
            return self.toggle_fold(line);
        }
        let gx = (self.rect.x + self.gutter_w()) as f64;
        if x < gx - FOLD_W as f64 - 2.0 || x >= gx || y < self.rect.y as f64 || y >= self.rect.bottom() as f64 {
            return false;
        }
        let lh = crate::ui::line_h() as f64;
        let row = ((y - self.rect.y as f64 + self.top * lh) / lh).floor().max(0.0) as usize;
        let (line, sub) = if self.vm_live() { self.view.map.line_at_row(row) } else { (row, 0) };
        if sub != 0 || line >= self.buf.line_count() {
            return false;
        }
        if self.view.folds.is_fold_start(line).is_none() && !self.is_foldable(line) {
            return false;
        }
        self.toggle_fold(line)
    }

    // ── movement ──

    /// Up/Down/PageUp/PageDown by `delta` visual rows, for every cursor.
    pub fn move_vertical(&mut self, extend: bool, delta: isize) {
        if !self.vm_live() {
            return self.ed.move_multi(&mut self.buf, |e, b| e.vertical(b, extend, delta));
        }
        let view = &self.view;
        self.ed.move_multi(&mut self.buf, |e, b| vertical_rows(e, b, extend, delta, view));
    }

    /// End key with word wrap: first to the end of the visual row, then of the line.
    pub fn row_end_target(&self, c: usize) -> Option<usize> {
        if self.view.cols == 0 {
            return None;
        }
        let (line, col) = self.buf.line_col(c);
        let text = self.buf.line(line);
        let breaks = self.view.breaks(&text);
        let (sub, _) = locate(&breaks, col);
        if sub >= breaks.len() {
            return None;
        }
        let end = col_at(&breaks, sub + 1, 0).saturating_sub(1);
        (end != col).then(|| self.buf.pos(line, end))
    }

    /// Home key with word wrap: start of the visual row when the caret is past it.
    pub fn row_home_target(&self, c: usize) -> Option<usize> {
        if self.view.cols == 0 {
            return None;
        }
        let (line, col) = self.buf.line_col(c);
        let text = self.buf.line(line);
        let breaks = self.view.breaks(&text);
        let (sub, _) = locate(&breaks, col);
        if sub == 0 {
            return None;
        }
        let start = col_at(&breaks, sub, 0);
        (start != col).then(|| self.buf.pos(line, start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;

    fn doc(text: &str) -> Doc {
        let mut d = Doc::new(Buffer::from_str(text), None, None, "//", 8.0);
        d.rect = crate::ui::Rect::new(0, 0, 800, 400);
        d
    }

    fn click(d: &mut Doc, l: usize, c: usize) {
        let p = d.buf.pos(l, c);
        d.ed.click(&mut d.buf, p, false);
    }

    fn code() -> String {
        // 0 fn a() {
        // 1     if x {
        // 2         y();
        // 3     }
        // 4     z();
        // 5 }
        // 6 fn b() {
        // 7     w();
        // 8 }
        "fn a() {\n    if x {\n        y();\n    }\n    z();\n}\nfn b() {\n    w();\n}\n".to_string()
    }

    #[test]
    fn inactive_view_is_identity() {
        let mut d = doc(&code());
        d.view_prepare(80);
        assert!(!d.view.active());
        assert!(!d.vm_live());
        assert_eq!(d.total_rows(), d.buf.line_count());
        assert_eq!(d.row_of_line(5), 5);
        assert_eq!(d.caret_geom(d.buf.pos(2, 3)), (2, 3));
    }

    #[test]
    fn fold_hides_lines_and_moves_rows() {
        let mut d = doc(&code());
        click(&mut d, 0, 2);
        assert!(d.fold_at_cursor());
        d.view_prepare(80);
        assert!(d.vm_live());
        assert_eq!(d.view.folds.regions(), &[(0, 4)]);
        // lines 1..=4 hidden: rows = 10 lines - 4
        assert_eq!(d.total_rows(), d.buf.line_count() - 4);
        assert_eq!(d.row_of_line(5), 1);
        assert_eq!(d.row_of_line(6), 2);
        assert_eq!(d.caret_geom(d.buf.pos(6, 1)), (2, 1));
        assert!(d.unfold_at_cursor());
        assert!(!d.view.active());
        assert_eq!(d.total_rows(), d.buf.line_count());
    }

    #[test]
    fn fold_picks_innermost_and_moves_caret_out() {
        let mut d = doc(&code());
        click(&mut d, 2, 4); // inside `if x {` block
        assert!(d.fold_at_cursor());
        assert_eq!(d.view.folds.regions(), &[(1, 2)]);
        assert_eq!(d.caret_line(), 1, "caret moves to the header");
        // second fold folds the enclosing function
        assert!(d.fold_at_cursor());
        assert_eq!(d.view.folds.regions(), &[(0, 4), (1, 2)]);
        assert_eq!(d.caret_line(), 0);
        d.unfold_all();
        assert!(d.view.folds.is_empty());
    }

    #[test]
    fn click_to_hidden_line_reveals_via_apply_reveal() {
        let mut d = doc(&code());
        click(&mut d, 0, 0);
        d.fold_at_cursor();
        d.view_prepare(80);
        let p = d.buf.pos(2, 3);
        d.ed.click(&mut d.buf, p, false);
        d.apply_reveal();
        assert!(d.view.folds.is_empty(), "caret inside a fold unfolds it");
    }

    #[test]
    fn vertical_skips_folded_lines() {
        let mut d = doc(&code());
        click(&mut d, 0, 0);
        d.fold_at_cursor();
        d.view_prepare(80);
        d.move_vertical(false, 1);
        assert_eq!(d.caret_line(), 5, "down from the folded header lands after the fold");
        d.move_vertical(false, -1);
        assert_eq!(d.caret_line(), 0);
    }

    #[test]
    fn wrap_rows_and_geometry() {
        let long = "word ".repeat(30); // 150 chars
        let mut d = doc(&format!("short\n{long}\nend\n"));
        d.set_word_wrap(true);
        d.view_prepare(40);
        assert!(d.vm_live());
        let rows1 = d.view.map.rows_in(1);
        assert!(rows1 >= 4, "150 chars at 40 cols wrap to >=4 rows, got {rows1}");
        assert_eq!(d.view.map.rows_in(0), 1);
        assert_eq!(d.row_of_line(2), 1 + rows1);
        // caret deep in the long line sits on a later row, inside the width
        let c = d.buf.pos(1, 100);
        let (row, vcol) = d.caret_geom(c);
        assert!(row > 1 && row < 1 + rows1);
        assert!(vcol < 40);
        // turning wrap off restores the identity
        d.set_word_wrap(false);
        d.view_prepare(40);
        assert!(!d.view.active());
        assert_eq!(d.total_rows(), d.buf.line_count());
    }

    #[test]
    fn pos_at_inverts_caret_geom_with_wrap() {
        let long = "abc ".repeat(40);
        let mut d = doc(&format!("{long}\nnext\n"));
        d.set_word_wrap(true);
        d.cell_w = 8.0;
        d.view_prepare(30);
        let lh = crate::ui::line_h() as f64;
        for col in [0usize, 5, 29, 31, 60, 100, 150] {
            let c = d.buf.pos(0, col);
            let (row, vcol) = d.caret_geom(c);
            let x = (d.rect.x + d.gutter_w()) as f64 + vcol as f64 * 8.0;
            let y = d.rect.y as f64 + row as f64 * lh + lh / 2.0;
            assert_eq!(d.pos_at(x, y), c, "col {col}");
        }
    }

    #[test]
    fn wrap_vertical_moves_by_visual_row() {
        let long = "abcdefgh".repeat(12); // 96 chars, no spaces -> hard breaks at 30
        let mut d = doc(&format!("{long}\nnext\n"));
        d.set_word_wrap(true);
        d.view_prepare(30);
        click(&mut d, 0, 5);
        d.move_vertical(false, 1);
        assert_eq!(d.buf.line_col(d.ed.head()), (0, 35));
        d.move_vertical(false, 1);
        assert_eq!(d.buf.line_col(d.ed.head()), (0, 65));
        d.move_vertical(false, -1);
        assert_eq!(d.buf.line_col(d.ed.head()), (0, 35));
        // last wrapped row -> next line
        d.move_vertical(false, 3);
        assert_eq!(d.caret_line(), 1);
    }

    #[test]
    fn home_end_follow_visual_rows() {
        let long = "abcdefgh".repeat(10); // 80 chars, breaks at 30, 60
        let mut d = doc(&format!("{long}\n"));
        d.set_word_wrap(true);
        d.view_prepare(30);
        let mid = d.buf.pos(0, 35);
        assert_eq!(d.row_home_target(mid), Some(d.buf.pos(0, 30)));
        assert_eq!(d.row_end_target(mid), Some(d.buf.pos(0, 59)));
        let last = d.buf.pos(0, 70);
        assert_eq!(d.row_end_target(last), None);
    }

    #[test]
    fn edits_keep_map_in_step() {
        let mut d = doc(&code());
        click(&mut d, 6, 0);
        d.fold_at_cursor(); // fold fn b (6..7)
        d.view_prepare(80);
        assert_eq!(d.view.folds.regions(), &[(6, 7)]);
        // insert two lines near the top: fold must shift
        click(&mut d, 0, 0);
        d.ed.paste(&mut d.buf, "// a\n// b\n");
        assert!(d.sync_edits());
        assert_eq!(d.view.folds.regions(), &[(8, 9)]);
        assert!(d.vm_live());
        assert_eq!(d.total_rows(), d.buf.line_count() - 1);
    }

    #[test]
    fn wrap_edit_updates_row_counts() {
        let mut d = doc("a\nb\nc\n");
        d.set_word_wrap(true);
        d.view_prepare(10);
        assert_eq!(d.total_rows(), d.buf.line_count());
        click(&mut d, 1, 1);
        d.ed.paste(&mut d.buf, &"x".repeat(25));
        assert!(d.sync_edits());
        assert_eq!(d.view.map.rows_in(1), 3);
        d.ed.paste(&mut d.buf, "\nnew");
        assert!(d.sync_edits());
        assert!(d.vm_live());
        assert_eq!(d.view.map.line_count(), d.buf.line_count());
    }

    #[test]
    fn foldable_test_and_regions() {
        let mut d = doc(&code());
        assert!(d.is_foldable(0));
        assert!(d.is_foldable(1));
        assert!(!d.is_foldable(2));
        assert!(!d.is_foldable(5));
        assert!(d.is_foldable(6));
        let r = d.fold_regions();
        assert!(r.contains(&(0, 4)) && r.contains(&(1, 2)) && r.contains(&(6, 7)));
    }

    #[test]
    fn lsp_regions_merge_and_expire() {
        let mut d = doc("a\nb\n// x\n// y\nc\n");
        assert!(!d.is_foldable(2));
        d.set_lsp_folds(vec![(2, 3)]); // a comment block the indentation cannot see
        assert!(d.is_foldable(2));
        assert!(d.fold_regions().contains(&(2, 3)));
        click(&mut d, 2, 0);
        assert!(d.fold_at_cursor());
        assert_eq!(d.view.folds.regions(), &[(2, 3)]);
        // after an edit the old ranges no longer describe the text
        d.version += 1;
        assert!(!d.is_foldable(2) || d.view.lsp_ver == d.version);
        assert!(!d.fold_regions().contains(&(2, 3)));
    }

    #[test]
    fn toggle_fold_via_click() {
        let mut d = doc(&code());
        d.cell_w = 8.0;
        d.view_prepare(80);
        let lh = crate::ui::line_h() as f64;
        let x = (d.rect.x + d.gutter_w()) as f64 - 4.0;
        // click on the chevron of line 6
        assert!(d.click_fold(x, 6.0 * lh + lh / 2.0));
        assert_eq!(d.view.folds.regions(), &[(6, 7)]);
        // line 2 is not foldable
        assert!(!d.click_fold(x, 2.0 * lh + lh / 2.0));
        // click again to unfold (header is still on row 6)
        assert!(d.click_fold(x, 6.0 * lh + lh / 2.0));
        assert!(d.view.folds.is_empty());
    }
}
