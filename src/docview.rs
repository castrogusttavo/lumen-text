//! Glue between a [`Doc`] and `rowmap`: word wrap + code folding state, the fold commands,
//! visual-row coordinates and vertical caret movement.
//!
//! The view is *inactive* (no `RowMap` kept, rows == lines) unless word wrap is on or something
//! is folded, so plain editing pays nothing for it. All scroll units (`Doc::top`, `target`) are
//! visual rows; with an inactive view a row is a line.

use crate::{
    buffer::{Buffer, EditRec, LineOp},
    doc::Doc,
    editor::{Editor, col_from_visual},
    rowmap::{FoldSet, RowMap, col_at, fold_regions_by_indent, locate, merge_regions, wrap_breaks},
};

/// Extra gutter width reserved for the fold chevron.
pub const FOLD_W: i32 = 12;

#[derive(Clone)]
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

    /// Line count the map and wrap rows were last made for.
    pub fn tracked_lines(&self) -> usize {
        self.lines
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

impl ViewMap {
    /// Recompute everything (exact wrap rows when wrapping, then the map).
    pub fn rebuild(&mut self, buf: &Buffer) {
        let n = buf.line_count();
        self.lines = n;
        if !self.active() {
            self.map = RowMap::new(0);
            self.wrap_rows = Vec::new();
            self.wrap_valid = false;
            return;
        }
        if self.cols > 0 && (!self.wrap_valid || self.wrap_rows.len() != n) {
            let mut rows = Vec::with_capacity(n);
            for i in 0..n {
                rows.push(if self.folds.is_hidden(i) { 1 } else { self.rows_for(&buf.line(i)) as u32 });
            }
            self.wrap_rows = rows;
            self.wrap_valid = true;
        } else if self.cols == 0 {
            self.wrap_rows = Vec::new();
            self.wrap_valid = false;
        }
        self.remap(n);
    }

    /// Derive the `RowMap` from the cached wrap rows and the folds (`n` = line count of the text).
    pub fn remap(&mut self, n: usize) {
        self.lines = n;
        if !self.active() {
            self.map = RowMap::new(0);
            return;
        }
        let wrap = self.cols > 0 && self.wrap_rows.len() == n;
        let rows: Vec<usize> = (0..n)
            .map(|i| if self.folds.is_hidden(i) { 0 } else if wrap { self.wrap_rows[i] as usize } else { 1 })
            .collect();
        let mut m = RowMap::new(n);
        m.splice_lines(0, n, &rows);
        self.map = m;
    }

    /// Follow one line-level operation of an edit in the cached wrap rows.
    fn apply_line_op(&mut self, op: LineOp) {
        let consistent = self.wrap_rows.len() == self.lines;
        match op {
            LineOp::Remove { after, n } => {
                let at = ((after + 1).max(0) as usize).min(self.lines);
                let k = n.min(self.lines - at);
                if consistent {
                    self.wrap_rows.drain(at..at + k);
                }
                self.lines -= k;
            }
            LineOp::Insert { after, n } => {
                let at = ((after + 1).max(0) as usize).min(self.lines);
                if consistent {
                    self.wrap_rows.splice(at..at, std::iter::repeat_n(1u32, n));
                }
                self.lines += n;
            }
            LineOp::Swap { first, a_lines, b_lines } => {
                let end = first + a_lines + b_lines;
                if consistent && end <= self.wrap_rows.len() {
                    self.wrap_rows[first..end].rotate_left(a_lines);
                } else if end > self.lines {
                    self.wrap_valid = false;
                }
            }
        }
        if !consistent && self.cols > 0 {
            self.wrap_valid = false;
        }
    }

    /// The text changed by `recs` (oldest first; `first` = lowest line touched): move the folds,
    /// the wrap rows and the map along, so a region keeps hiding the same lines whatever happened
    /// above it (also when the line count stayed the same, e.g. a line moved up or down).
    pub fn on_edit(&mut self, recs: &[EditRec], first: usize, buf: &Buffer) {
        let n = buf.line_count();
        if !self.active() {
            self.lines = n;
            return;
        }
        let was_folds = !self.folds.is_empty();
        let mut structural = false;
        let mut lo = first.min(n - 1);
        let mut hi = lo;
        let mut ins_total = 0;
        if recs.is_empty() {
            // Nothing recorded: fall back to the line-count difference.
            let delta = n as isize - self.lines as isize;
            let op = if delta > 0 { Some(LineOp::Insert { after: first as isize, n: delta as usize }) } else if delta < 0 { Some(LineOp::Remove { after: first as isize, n: (-delta) as usize }) } else { None };
            if let Some(op) = op {
                structural = true;
                self.folds.apply_op(op);
                self.apply_line_op(op);
            }
        }
        for r in recs {
            lo = lo.min(r.line);
            hi = hi.max(r.line + r.ins_lines);
            ins_total += r.ins_lines;
            for op in r.line_ops() {
                structural = true;
                self.folds.apply_op(op);
                self.apply_line_op(op);
            }
        }
        if recs.len() > 1 {
            hi += ins_total;
        }
        if self.lines != n {
            // The records do not add up (text replaced behind our back): start over.
            self.wrap_valid = false;
            self.folds.clip(n);
            structural = true;
        }
        let lo = lo.min(n - 1);
        let hi = hi.min(n - 1);
        // Exact rows for the edited line(s).
        if self.cols > 0 && self.wrap_valid && self.wrap_rows.len() == n {
            for i in lo..=hi {
                if !self.folds.is_hidden(i) {
                    self.wrap_rows[i] = self.rows_for(&buf.line(i)) as u32;
                }
            }
        } else if self.cols > 0 {
            self.wrap_valid = false;
        }
        if !self.wrap_valid && self.cols > 0 {
            self.rebuild(buf);
        } else if structural || was_folds != !self.folds.is_empty() {
            self.remap(n);
        } else if self.cols > 0 && self.map.line_count() == n {
            for i in lo..=hi {
                if !self.folds.is_hidden(i) {
                    let r = self.wrap_rows[i] as usize;
                    self.map.update_line(i, r);
                }
            }
        }
        self.lines = n;
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
    #[allow(dead_code)]
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
        let row = self.view.map.row_of(line) + sub;
        let hs = self.inlay.hints.hints_on(line);
        if hs.is_empty() {
            return (row, visual_col_from(&text, c0, col, self.ed.tab_w));
        }
        // Inlay hints on a wrapped row: slice them to the row and measure its text alone.
        let len = text.chars().count();
        let (r0, r1) = row_range(&breaks, sub, len);
        let rh = crate::inlay::row_hints(hs, len, r0, r1, sub >= breaks.len());
        let rowtext: String = text.chars().skip(r0).take(r1 - r0).collect();
        (row, crate::inlay::vcol_in(&rh.hints, &rowtext, col - c0, self.ed.tab_w, false))
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
        self.view.rebuild(&self.buf);
    }

    /// Derive the `RowMap` from the cached wrap rows and the folds.
    fn view_remap(&mut self) {
        let n = self.buf.line_count();
        self.view.remap(n);
    }

    /// Call after the folds changed; `anchor` is `top_pos()` taken BEFORE the change.
    fn view_refold(&mut self, anchor: (usize, usize)) {
        self.view_remap();
        self.set_top_to(anchor);
    }

    /// Text changed: first changed line `l`. Keeps folds, wrap rows and the map consistent.
    pub fn view_on_edit(&mut self, l: usize) -> Vec<EditRec> {
        let recs = self.buf.take_edits();
        self.view.on_edit(&recs, l, &self.buf);
        recs
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

    // ── line commands that respect folds ──

    /// `l0..=l1` widened so that no folded region is cut in two (a folded header takes its hidden
    /// lines with it).
    pub fn fold_extend(&self, l0: usize, l1: usize) -> (usize, usize) {
        let n = self.buf.line_count();
        (l0, self.fold_end_from(l0, l1).min(n.saturating_sub(1)))
    }

    /// Last line of `l0..=l1` once every region starting inside it is included.
    fn fold_end_from(&self, l0: usize, mut end: usize) -> usize {
        loop {
            let mut grew = false;
            for &(s, e) in self.view.folds.regions() {
                if s >= l0 && s <= end && e > end {
                    end = e;
                    grew = true;
                }
            }
            if !grew {
                return end;
            }
        }
    }

    /// The block of whole lines that Alt+Up / Alt+Down swaps with the selected block `x`: the
    /// next visible line (with its hidden body when it is a fold header).
    fn move_partner(&self, x: (usize, usize), down: bool) -> Option<(usize, usize)> {
        let n = self.buf.line_count();
        if down {
            let y0 = x.1 + 1;
            if y0 >= n {
                return None;
            }
            Some((y0, self.fold_end_from(y0, y0).min(n - 1)))
        } else {
            let y1 = x.0.checked_sub(1)?;
            let y0 = match self.view.folds.hidden_ranges().iter().find(|r| r.0 <= y1 && y1 <= r.1) {
                Some(r) => r.0 - 1,
                None => y1,
            };
            Some((y0, y1))
        }
    }

    /// Alt+Up / Alt+Down: move the selected lines (a folded header moves with its body) past the
    /// next visible line.
    pub fn move_lines_cmd(&mut self, down: bool) {
        let (l0, l1) = self.ed.selected_line_range(&self.buf);
        let x = self.fold_extend(l0, l1);
        if let Some(y) = self.move_partner(x, down) {
            self.ed.move_block(&mut self.buf, down, x, y);
        }
    }

    /// Ctrl+Shift+D: copy the selection, or the caret's line (with its hidden body when folded).
    pub fn duplicate_cmd(&mut self) {
        if !self.ed.sel.is_empty() {
            return self.ed.duplicate(&mut self.buf);
        }
        let (l0, l1) = self.ed.selected_line_range(&self.buf);
        let (l0, l1) = self.fold_extend(l0, l1);
        self.ed.duplicate_lines(&mut self.buf, l0, l1);
    }

    /// Ctrl+Shift+K: delete the selected lines (a folded header goes with its hidden body).
    pub fn delete_line_cmd(&mut self) {
        let (l0, l1) = self.ed.selected_line_range(&self.buf);
        let (l0, l1) = self.fold_extend(l0, l1);
        self.ed.delete_lines(&mut self.buf, l0, l1);
    }

    /// Ctrl+C: the selection(s), or with nothing selected the caret's line (a folded header copies
    /// its hidden lines too, so the clipboard holds what the cut would remove).
    pub fn copy_cmd(&self) -> String {
        if self.ed.sel.is_empty() && self.ed.extra.is_empty() {
            let (l0, l1) = self.fold_extend_caret();
            return Editor::lines_text(&self.buf, l0, l1);
        }
        self.ed.copy_text_multi(&self.buf)
    }

    /// Ctrl+X with a single cursor: the selection, or the caret's line with its hidden fold body.
    pub fn cut_cmd(&mut self) -> String {
        if self.ed.sel.is_empty() {
            let (l0, l1) = self.fold_extend_caret();
            return self.ed.cut_lines(&mut self.buf, l0, l1);
        }
        self.ed.cut(&mut self.buf)
    }

    /// The caret's line, widened over the lines its fold hides.
    fn fold_extend_caret(&self) -> (usize, usize) {
        let l = self.caret_line();
        self.fold_extend(l, l)
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

    /// Home for every cursor: the start of the visual row when wrapped, else the line's start /
    /// first non-blank.
    pub fn home_cmd(&mut self, extend: bool) {
        let view = &self.view;
        self.ed.move_multi(&mut self.buf, |e, b| match row_home_in(view, b, e.head()) {
            Some(p) => e.click(b, p, extend),
            None => e.home(b, extend),
        });
    }

    /// End for every cursor: the end of the visual row when wrapped, else of the line.
    pub fn end_cmd(&mut self, extend: bool) {
        let view = &self.view;
        self.ed.move_multi(&mut self.buf, |e, b| match row_end_in(view, b, e.head()) {
            Some(p) => e.click(b, p, extend),
            None => e.end(b, extend),
        });
    }
}

/// End key with word wrap: the end of the visual row (None on the last row: then the line's end).
fn row_end_in(view: &ViewMap, buf: &Buffer, c: usize) -> Option<usize> {
    if view.cols == 0 {
        return None;
    }
    let (line, col) = buf.line_col(c);
    let text = buf.line(line);
    let breaks = view.breaks(&text);
    let (sub, _) = locate(&breaks, col);
    if sub >= breaks.len() {
        return None;
    }
    let end = col_at(&breaks, sub + 1, 0).saturating_sub(1);
    (end != col).then(|| buf.pos(line, end))
}

/// Home key with word wrap: the start of the visual row when the caret is past it.
fn row_home_in(view: &ViewMap, buf: &Buffer, c: usize) -> Option<usize> {
    if view.cols == 0 {
        return None;
    }
    let (line, col) = buf.line_col(c);
    let text = buf.line(line);
    let breaks = view.breaks(&text);
    let (sub, _) = locate(&breaks, col);
    if sub == 0 {
        return None;
    }
    let start = col_at(&breaks, sub, 0);
    (start != col).then(|| buf.pos(line, start))
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

    fn hint(line: usize, col: usize, label: &str, pad_left: bool, pad_right: bool) -> crate::inlay::Hint {
        crate::inlay::Hint { line, col, label: label.into(), kind: crate::inlay::HintKind::Type, pad_left, pad_right, links: Vec::new(), edits: Vec::new() }
    }

    #[test]
    fn inlay_hints_on_wrapped_rows_keep_caret_and_clicks_exact() {
        let long = "abcdefgh".repeat(12); // 96 chars, hard breaks at 30, 60, 90
        let mut d = doc(&format!("{long}\nnext\n"));
        d.set_word_wrap(true);
        d.cell_w = 8.0;
        d.view_prepare(30);
        let hs = vec![hint(0, 35, ": number", true, false), hint(0, 70, "x:", false, true), hint(0, 96, ": T", false, false)];
        d.inlay.apply(0, 1, hs, d.buf.line_count());
        let lh = crate::ui::line_h() as f64;
        let base = (d.rect.x + d.gutter_w()) as f64;
        for col in [0usize, 5, 29, 30, 34, 35, 36, 59, 60, 69, 70, 71, 89, 90, 96] {
            let c = d.buf.pos(0, col);
            let (row, vcol) = d.caret_geom(c);
            let (x, y) = (base + vcol as f64 * 8.0, d.rect.y as f64 + row as f64 * lh + lh / 2.0);
            assert_eq!(d.pos_at(x, y), c, "col {col}");
        }
        // the caret after the hint's column sits past the hint's cells (1 pad + 7 label cells)
        let (_, before) = d.caret_geom(d.buf.pos(0, 35));
        let (_, after) = d.caret_geom(d.buf.pos(0, 36));
        assert_eq!((before, after), (5, 5 + 1 + 1 + 7));
        // a pointer over the hint's label reports it (and the label char); over text it does not
        let row = 1; // second row of line 0
        let y = d.rect.y as f64 + row as f64 * lh + lh / 2.0;
        let over_label = base + (5.0 + 1.0 + 3.0) * 8.0;
        let (p, hit) = d.hit_at(over_label, y);
        assert_eq!(p, d.buf.pos(0, 35));
        let hit = hit.expect("over the hint");
        assert_eq!((hit.line, hit.idx), (0, 0));
        assert!(hit.off.is_some());
        let pad = d.hit_at(base + 5.0 * 8.0 + 2.0, y).1.expect("padding is part of the hint");
        assert_eq!(pad.off, None);
        assert!(d.hit_at(base + 2.0 * 8.0, y).1.is_none());
        // the third row's hint "x:" (col 70) is hit with its own index
        let y2 = d.rect.y as f64 + 2.0 * lh + lh / 2.0;
        assert_eq!(d.hit_at(base + (10.0 + 0.5) * 8.0, y2).1.map(|h| h.idx), Some(1));
        // the hint at the very end of the last row
        let y3 = d.rect.y as f64 + 3.0 * lh + lh / 2.0;
        assert_eq!(d.hit_at(base + (6.0 + 0.5) * 8.0, y3).1.map(|h| h.idx), Some(2));
    }

    #[test]
    fn double_click_hint_edit_is_one_undo_step() {
        let mut d = doc("const x = 5;\n");
        d.inlay.apply(0, 0, vec![hint(0, 7, ": number", true, false)], d.buf.line_count());
        let hit = crate::doc::HintHit { line: 0, idx: 0, off: Some(2) };
        assert!(d.apply_hint_edits(hit));
        assert_eq!(d.buf.line(0), "const x: number = 5;");
        d.ed.undo(&mut d.buf);
        assert_eq!(d.buf.line(0), "const x = 5;");
        // a parameter-name hint has nothing to insert
        let mut p = hint(0, 7, "name:", false, true);
        p.kind = crate::inlay::HintKind::Parameter;
        d.inlay.apply(0, 0, vec![p], d.buf.line_count());
        assert!(!d.apply_hint_edits(hit));
        assert_eq!(d.buf.line(0), "const x = 5;");
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
        assert_eq!(row_home_in(&d.view, &d.buf, mid), Some(d.buf.pos(0, 30)));
        assert_eq!(row_end_in(&d.view, &d.buf, mid), Some(d.buf.pos(0, 59)));
        let last = d.buf.pos(0, 70);
        assert_eq!(row_end_in(&d.view, &d.buf, last), None);
    }

    #[test]
    fn home_and_end_use_the_visual_row_for_every_cursor() {
        let long = "abcdefgh".repeat(10); // 80 chars, breaks at 30, 60
        let mut d = doc(&format!("{long}\n{long}\n"));
        d.set_word_wrap(true);
        d.view_prepare(30);
        // primary on line 0 / row 2 (col 70), second cursor on line 1 / row 1 (col 35)
        click(&mut d, 0, 70);
        let p = d.buf.pos(1, 35);
        d.ed.add_cursor_at(p);
        d.end_cmd(false);
        let cols: Vec<(usize, usize)> = d.ed.all_sels().iter().map(|s| d.buf.line_col(s.head)).collect();
        assert!(cols.contains(&(0, 80)) && cols.contains(&(1, 59)), "{cols:?}: the row end, the line end on the last row");
        d.home_cmd(false);
        let cols: Vec<(usize, usize)> = d.ed.all_sels().iter().map(|s| d.buf.line_col(s.head)).collect();
        assert!(cols.contains(&(0, 60)) && cols.contains(&(1, 30)), "{cols:?}: each cursor to the start of its own row");
        // selecting with shift keeps every anchor
        d.end_cmd(true);
        assert!(d.ed.all_sels().iter().all(|s| s.anchor != s.head));
    }

    #[test]
    fn cut_and_copy_without_selection_take_the_folded_body_along() {
        let mut d = doc(&code());
        click(&mut d, 0, 2);
        assert!(d.fold_at_cursor()); // fn a: lines 0..5
        d.view_prepare(80);
        click(&mut d, 0, 2);
        let whole = "fn a() {\n    if x {\n        y();\n    }\n    z();\n"; // the fold hides lines 1..=4
        assert_eq!(d.copy_cmd(), whole, "copy has the hidden lines too");
        assert_eq!(d.buf.line_count(), 10, "copy changes nothing");
        let t = d.cut_cmd();
        assert_eq!(t, whole, "the clipboard text is complete");
        assert_eq!(d.buf.to_string(), "}\nfn b() {\n    w();\n}\n");
        d.sync_edits();
        d.view_prepare(80);
        assert!(d.view.folds.is_empty(), "the fold went with its lines");
        // pasting it back restores the text
        click(&mut d, 0, 0);
        d.ed.paste(&mut d.buf, &t);
        assert_eq!(d.buf.to_string(), code());
        // a header that is not folded still cuts one line
        let mut d = doc(&code());
        click(&mut d, 6, 0);
        assert_eq!(d.cut_cmd(), "fn b() {\n");
        // the last line (no line break after it) still ends the clipboard text with one
        let mut d = doc("a\nb");
        click(&mut d, 1, 0);
        assert_eq!(d.copy_cmd(), "b\n");
        // with a selection nothing changes: the selection is copied
        let mut d = doc(&code());
        d.ed.select_range(&mut d.buf, 0, 2);
        assert_eq!(d.copy_cmd(), "fn");
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

    // ── folds follow line commands and edits that keep the line count ──

    fn folded(d: &mut Doc, line: usize) {
        click(d, line, 0);
        assert!(d.fold_at_cursor());
        d.view_prepare(80);
    }

    #[test]
    fn moving_a_line_past_a_folded_block_takes_the_fold_along() {
        // 0 fn a() {  1 if x {  2 y();  3 }  4 z();  5 }  6 fn b() {  7 w();  8 }
        let mut d = doc(&code());
        folded(&mut d, 6);
        let n = d.buf.line_count();
        click(&mut d, 5, 0);
        d.move_lines_cmd(true); // `}` (line 5) goes below fn b and its hidden body
        d.sync_edits();
        assert_eq!(d.buf.line_count(), n, "same number of lines");
        assert_eq!(d.buf.line(5), "fn b() {");
        assert_eq!(d.buf.line(7), "}");
        assert_eq!(d.view.folds.regions(), &[(5, 6)], "the fold moved up with its header");
        assert!(d.vm_live());
        assert_eq!(d.total_rows(), n - 1);
        assert_eq!(d.row_of_line(7), 6);
        assert_eq!(d.caret_line(), 7, "the moved line keeps the caret");
        // undo puts everything back, fold included
        d.ed.undo(&mut d.buf);
        d.sync_edits();
        assert_eq!(d.view.folds.regions(), &[(6, 7)]);
        assert_eq!(d.buf.line(6), "fn b() {");
    }

    #[test]
    fn moving_a_folded_header_moves_its_hidden_lines_too() {
        let mut d = doc(&code());
        folded(&mut d, 6);
        click(&mut d, 6, 0);
        d.move_lines_cmd(false); // up past `}`
        d.sync_edits();
        assert_eq!(d.buf.line(5), "fn b() {");
        assert_eq!(d.buf.line(6), "    w();", "the hidden body went along");
        assert_eq!(d.buf.line(7), "}");
        assert_eq!(d.view.folds.regions(), &[(5, 6)]);
        assert_eq!(d.caret_line(), 5);
        d.move_lines_cmd(true);
        d.sync_edits();
        assert_eq!(d.view.folds.regions(), &[(6, 7)]);
        assert_eq!(d.caret_line(), 6);
        // down past the last real line
        d.move_lines_cmd(true);
        d.sync_edits();
        assert_eq!(d.view.folds.regions(), &[(7, 8)]);
    }

    #[test]
    fn a_line_moved_across_a_fold_header_keeps_other_folds() {
        let mut d = doc(&code());
        click(&mut d, 2, 4);
        d.fold_at_cursor(); // fold `if x {` (1..2)
        folded(&mut d, 6); // and fn b (6..7)
        assert_eq!(d.view.folds.regions(), &[(1, 2), (6, 7)]);
        click(&mut d, 0, 0);
        d.move_lines_cmd(true); // `fn a() {` swaps with the folded block (1..=2)
        d.sync_edits();
        assert_eq!(d.buf.line(0), "    if x {");
        assert_eq!(d.buf.line(2), "fn a() {");
        assert_eq!(d.view.folds.regions(), &[(0, 1), (6, 7)]);
    }

    #[test]
    fn duplicating_and_deleting_a_folded_header_handles_the_whole_block() {
        let mut d = doc(&code());
        folded(&mut d, 6);
        let n = d.buf.line_count();
        click(&mut d, 6, 3);
        d.duplicate_cmd();
        d.sync_edits();
        assert_eq!(d.buf.line_count(), n + 2, "the block is the header and its hidden line");
        assert_eq!((d.buf.line(8), d.buf.line(9)), ("fn b() {".to_string(), "    w();".to_string()));
        assert_eq!(d.view.folds.regions(), &[(6, 7)], "the original stays folded");
        assert_eq!(d.caret_line(), 8, "the caret is on the copy");
        // deleting the folded header removes its body with it, and no fold is left behind
        click(&mut d, 6, 0);
        d.delete_line_cmd();
        d.sync_edits();
        assert_eq!(d.buf.line_count(), n);
        assert_eq!(d.buf.line(6), "fn b() {", "the copy is what is left");
        assert!(d.view.folds.is_empty());
    }

    #[test]
    fn newline_before_a_folded_header_pushes_the_fold_down() {
        let mut d = doc(&code());
        folded(&mut d, 6);
        click(&mut d, 6, 0);
        d.ed.newline(&mut d.buf);
        d.sync_edits();
        assert_eq!(d.view.folds.regions(), &[(7, 8)]);
        assert_eq!(d.buf.line(7), "fn b() {");
    }

    #[test]
    fn edits_that_keep_the_line_count_leave_folds_alone_unless_they_touch_the_header() {
        let mut d = doc(&code());
        folded(&mut d, 6); // fn b (6..7)
        // replace lines 3..=4 by two other lines: same count, the fold below stays
        let (s, e) = (d.buf.line_start(3), d.buf.pos(4, usize::MAX));
        let before = d.ed.sel;
        d.buf.replace(s, e, "fn q() {\n    r();", crate::buffer::EditKind::Other, before);
        d.sync_edits();
        assert_eq!(d.view.folds.regions(), &[(6, 7)]);
        // a body line inserted at the start of the hidden line grows the region; it stays folded
        let p = d.buf.line_start(7);
        d.buf.replace(p, p, "    q();\n", crate::buffer::EditKind::Other, before);
        d.sync_edits();
        assert_eq!(d.view.folds.regions(), &[(6, 8)]);
        // replacing the header together with the line above (equal count) drops that fold
        let (s, e) = (d.buf.line_start(5), d.buf.pos(6, usize::MAX));
        d.buf.replace(s, e, "}\nfn z() {", crate::buffer::EditKind::Other, before);
        d.sync_edits();
        assert!(d.view.folds.is_empty(), "the header line was rewritten");
    }

    #[test]
    fn wrap_rows_and_map_stay_exact_across_line_moves() {
        let long = "word ".repeat(30);
        let mut d = doc(&format!("a\n{long}\nb\nc {long}\nd\n"));
        d.set_word_wrap(true);
        d.view_prepare(40);
        click(&mut d, 1, 0);
        d.move_lines_cmd(true); // the long line swaps with `b`
        d.sync_edits();
        click(&mut d, 3, 0);
        d.move_lines_cmd(false);
        d.sync_edits();
        let mut fresh = d.view.clone();
        fresh.wrap_valid = false;
        fresh.rebuild(&d.buf);
        for l in 0..d.buf.line_count() {
            assert_eq!(d.view.map.rows_in(l), fresh.map.rows_in(l), "line {l}");
            assert_eq!(d.view.map.row_of(l), fresh.map.row_of(l));
        }
        assert_eq!(d.total_rows(), fresh.map.total_rows());
    }

    #[test]
    fn minimap_is_drawn_by_visual_rows_and_the_viewport_is_exact() {
        use crate::minimap::{drag_to_top_line, layout_for, viewport_rect};
        let long = "word ".repeat(30);
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("fn f{i}() {{\n    {long}\n}}\n"));
        }
        let mut d = doc(&text);
        d.rect = crate::ui::Rect::new(0, 0, 800, 400);
        d.set_word_wrap(true);
        d.view_prepare(40);
        let wrapped_rows = d.total_rows();
        assert!(wrapped_rows > d.buf.line_count(), "wrapping adds rows");
        for i in 0..100 {
            click(&mut d, i * 3, 0);
            d.fold_at_cursor();
        }
        d.view_prepare(40);
        let total = d.total_rows();
        assert!(total < wrapped_rows, "folds shrink the row count");
        let l = layout_for(crate::ui::Rect::new(0, 0, 800, 400), 64, total);
        let vis = d.visible_lines();
        // scrolled to the end: the marker ends exactly where the content ends
        d.scroll_direct(1e9);
        let vp = viewport_rect(&l, total, d.top, vis);
        let content_end = l.rect.y + (total as f64 * l.line_px as f64).round() as i32;
        assert_eq!(vp.bottom(), content_end.min(l.rect.bottom()));
        // scrolled to the start: it starts at the top
        d.scroll_direct(-1e9);
        let vp0 = viewport_rect(&l, total, d.top, vis);
        assert_eq!(vp0.y, l.rect.y);
        // its height is the visible share of the rows (rounded to the pixel)
        let want = vis * l.line_px as f64;
        assert!((vp0.h as f64 - want).abs() <= 1.5, "{} vs {want}", vp0.h);
        // clicking the middle of the strip centers the viewport on the middle row
        d.minimap_layout = Some(l);
        d.minimap_scroll_to(l.rect.y as f64 + l.rect.h as f64 / 2.0);
        let mid = (total as f64 / 2.0 - vis / 2.0).round();
        assert!((d.top - mid).abs() <= 2.0, "top {} vs {mid}", d.top);
        assert_eq!(drag_to_top_line(&l, l.rect.y as f64 + 1.0, total, vis as usize), 0);
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
