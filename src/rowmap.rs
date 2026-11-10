#![allow(dead_code)]
//! Line -> visual-row mapping for word wrap and code folding (pure logic).
//!
//! * [`RowMap`] keeps cumulative visual-row counts per logical line.
//! * [`wrap_breaks`] / [`locate`] / [`col_at`] describe how one line wraps.
//! * [`FoldSet`] holds folded regions; [`fold_regions_by_indent`] and
//!   [`fold_regions_from_lsp`] compute candidate regions.
//!
//! Wrapping rule: a row holds at most `cols` columns (every char is 1 column
//! except tab, which advances to the next multiple of `tab_w` measured from the
//! START OF THE ROW). When the next char would overflow, break at the last
//! whitespace boundary (just after a whitespace char) if that boundary lies in
//! the last 25% of the row (its width >= 75% of `cols`), else hard break.
//! `RowMap::rebuild` only knows the total width, so it estimates rows as
//! `ceil(width/cols)`; use `update_line` with `wrap_breaks(..).len() + 1` for
//! exact counts.

use crate::buffer::LineOp;
use std::collections::BTreeMap;

/// Char indices where each continuation row starts (empty = no wrap needed).
/// `cols == 0` disables wrapping.
pub fn wrap_breaks(text: &str, cols: usize, tab_w: usize) -> Vec<usize> {
    let mut out = Vec::new();
    if cols == 0 {
        return out;
    }
    let tab_w = tab_w.max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut s = 0; // row start
    let mut i = 0;
    let mut w = 0;
    let mut ws: Option<(usize, usize)> = None; // (break index, width at it)
    while i < chars.len() {
        let ch = chars[i];
        let cw = if ch == '\t' { tab_w - w % tab_w } else { 1 };
        if w + cw > cols && i > s {
            let brk = match ws {
                Some((b, bw)) if bw * 4 >= cols * 3 => b,
                _ => i,
            };
            out.push(brk);
            s = brk;
            i = brk;
            w = 0;
            ws = None;
            continue;
        }
        w += cw;
        i += 1;
        if ch.is_whitespace() {
            ws = Some((i, w));
        }
    }
    out
}

/// (row within the line, char column within that row) for char column `col`.
pub fn locate(breaks: &[usize], col: usize) -> (usize, usize) {
    let row = breaks.partition_point(|&b| b <= col);
    let start = if row == 0 { 0 } else { breaks[row - 1] };
    (row, col - start)
}

/// Inverse of [`locate`]: line char column of (`row_in_line`, `col_in_row`).
/// Rows past the last are clamped to the last row.
pub fn col_at(breaks: &[usize], row_in_line: usize, col_in_row: usize) -> usize {
    let row = row_in_line.min(breaks.len());
    let start = if row == 0 { 0 } else { breaks[row - 1] };
    start + col_in_row
}

/// Set of folded regions `(start, end)`: lines `start+1..=end` are hidden,
/// `start` stays visible with a fold marker.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct FoldSet {
    regions: Vec<(usize, usize)>, // sorted by start, unique starts
    hidden: Vec<(usize, usize)>,  // merged inclusive hidden ranges (outer wins)
}

impl FoldSet {
    pub fn new() -> Self {
        Self::default()
    }

    fn recompute(&mut self) {
        self.regions.sort_unstable();
        self.regions.dedup_by(|b, a| {
            if a.0 == b.0 {
                a.1 = a.1.max(b.1);
                true
            } else {
                false
            }
        });
        self.hidden.clear();
        for &(s, e) in &self.regions {
            let (lo, hi) = (s + 1, e);
            if lo > hi {
                continue;
            }
            match self.hidden.last_mut() {
                Some(last) if lo <= last.1 + 1 => last.1 = last.1.max(hi),
                _ => self.hidden.push((lo, hi)),
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn regions(&self) -> &[(usize, usize)] {
        &self.regions
    }

    /// Merged inclusive ranges of hidden lines, sorted (for fast sweeps).
    pub fn hidden_ranges(&self) -> &[(usize, usize)] {
        &self.hidden
    }

    /// Fold `start..=end` (hides start+1..=end). Ignored when `end <= start`.
    /// A second fold with the same start replaces the first.
    pub fn fold(&mut self, start: usize, end: usize) {
        if end <= start {
            return;
        }
        self.regions.retain(|r| r.0 != start);
        self.regions.push((start, end));
        self.recompute();
    }

    /// Unfold the region starting at `line`. Inner folds stay folded.
    pub fn unfold(&mut self, line: usize) -> bool {
        let n = self.regions.len();
        self.regions.retain(|r| r.0 != line);
        let changed = self.regions.len() != n;
        if changed {
            self.recompute();
        }
        changed
    }

    /// Unfold every region that hides `line` (used to reveal the caret).
    pub fn reveal(&mut self, line: usize) -> bool {
        let n = self.regions.len();
        self.regions.retain(|r| !(r.0 < line && line <= r.1));
        let changed = self.regions.len() != n;
        if changed {
            self.recompute();
        }
        changed
    }

    pub fn unfold_all(&mut self) {
        self.regions.clear();
        self.hidden.clear();
    }

    /// Replace the set with `regions` (single-line ranges dropped).
    pub fn fold_all(&mut self, regions: &[(usize, usize)]) {
        self.regions = regions.iter().copied().filter(|r| r.1 > r.0).collect();
        self.recompute();
    }

    /// Toggle the fold that starts at `line`: unfold if folded, otherwise fold
    /// the widest region of `available` starting there. Returns whether the
    /// state changed.
    pub fn toggle_at(&mut self, line: usize, available: &[(usize, usize)]) -> bool {
        if self.unfold(line) {
            return true;
        }
        match available.iter().filter(|r| r.0 == line && r.1 > r.0).map(|r| r.1).max() {
            Some(end) => {
                self.fold(line, end);
                true
            }
            None => false,
        }
    }

    pub fn is_hidden(&self, line: usize) -> bool {
        let i = self.hidden.partition_point(|r| r.1 < line);
        self.hidden.get(i).is_some_and(|r| r.0 <= line)
    }

    /// The folded region starting at `line` (also when `line` itself is hidden
    /// by an outer fold; check `is_hidden` first if that matters).
    pub fn is_fold_start(&self, line: usize) -> Option<(usize, usize)> {
        self.regions.binary_search_by_key(&line, |r| r.0).ok().map(|i| self.regions[i])
    }

    /// Keep regions consistent after an edit on `first_changed_line` that
    /// inserted (`delta > 0`) or removed (`delta < 0`) lines immediately after
    /// it. Regions starting at or before that line keep their start (a region
    /// whose body contains the edit grows/shrinks); regions after it shift;
    /// regions that start inside removed lines, or collapse to one line, are
    /// dropped.
    pub fn adjust_for_edit(&mut self, first_changed_line: usize, delta_lines: isize) {
        self.shift_after(first_changed_line as isize, delta_lines);
    }

    /// Like `adjust_for_edit` with `after` = the last line that keeps its place (-1: before line 0).
    pub fn shift_after(&mut self, after: isize, delta_lines: isize) {
        if delta_lines == 0 {
            return;
        }
        let f = after;
        let mut out = Vec::with_capacity(self.regions.len());
        for &(s, e) in &self.regions {
            let (si, ei) = (s as isize, e as isize);
            if si > f {
                if delta_lines < 0 && si <= f - delta_lines {
                    continue;
                }
                out.push(((si + delta_lines) as usize, (ei + delta_lines) as usize));
            } else if ei > f {
                let ne = if delta_lines < 0 && ei <= f - delta_lines { f } else { ei + delta_lines };
                if ne <= si {
                    continue;
                }
                out.push((s, ne as usize));
            } else {
                out.push((s, e));
            }
        }
        self.regions = out;
        self.recompute();
    }

    /// Two adjacent blocks of whole lines exchanged places: lines `first..first+a_lines` and the
    /// `b_lines` after them. Folds inside a block travel with it, folds around both stay, a fold
    /// that straddles a block edge is dropped.
    pub fn swap_blocks(&mut self, first: usize, a_lines: usize, b_lines: usize) {
        let (a0, a1, b1) = (first, first + a_lines, first + a_lines + b_lines);
        let mut out = Vec::with_capacity(self.regions.len());
        for &(s, e) in &self.regions {
            if e < a0 || s >= b1 {
                out.push((s, e));
            } else if s >= a0 && e < a1 {
                out.push((s + b_lines, e + b_lines));
            } else if s >= a1 && e < b1 {
                out.push((s - a_lines, e - a_lines));
            } else if s < a0 && e >= b1 {
                out.push((s, e));
            }
        }
        self.regions = out;
        self.recompute();
    }

    /// Apply the line-level effect of one text edit.
    pub fn apply_op(&mut self, op: LineOp) {
        match op {
            LineOp::Remove { after, n } => self.shift_after(after, -(n as isize)),
            LineOp::Insert { after, n } => self.shift_after(after, n as isize),
            LineOp::Swap { first, a_lines, b_lines } => self.swap_blocks(first, a_lines, b_lines),
        }
    }

    /// Drop regions that reach past the last of `lines` lines.
    pub fn clip(&mut self, lines: usize) {
        let n = self.regions.len();
        self.regions.retain(|r| r.1 < lines);
        if self.regions.len() != n {
            self.recompute();
        }
    }
}

/// Cumulative visual-row table.
#[derive(Default, Clone, Debug)]
pub struct RowMap {
    /// `rows_before[i]` = first visual row of line i; len = lines + 1.
    rows_before: Vec<u32>,
}

impl RowMap {
    pub fn new(line_count: usize) -> Self {
        RowMap { rows_before: (0..=line_count as u32).collect() }
    }

    pub fn line_count(&self) -> usize {
        self.rows_before.len() - 1
    }

    /// Recompute everything. `widths(i)` = visual width in columns of line i
    /// (tabs expanded); not called when `cols == 0` or the line is hidden.
    pub fn rebuild(&mut self, widths: &mut dyn FnMut(usize) -> usize, cols: usize, folds: &FoldSet) {
        let n = self.line_count();
        let hidden = folds.hidden_ranges();
        let mut h = 0;
        let mut acc: u32 = 0;
        for i in 0..n {
            self.rows_before[i] = acc;
            while h < hidden.len() && hidden[h].1 < i {
                h += 1;
            }
            if h < hidden.len() && hidden[h].0 <= i {
                continue;
            }
            acc += if cols == 0 { 1 } else { widths(i).div_ceil(cols).max(1) as u32 };
        }
        self.rows_before[n] = acc;
    }

    /// Set the row count of one line (0 = hidden) and shift everything after.
    pub fn update_line(&mut self, line: usize, new_rows: usize) {
        if line >= self.line_count() {
            return;
        }
        let old = self.rows_before[line + 1] - self.rows_before[line];
        let new = new_rows as u32;
        if old == new {
            return;
        }
        for v in &mut self.rows_before[line + 1..] {
            *v = *v - old + new;
        }
    }

    /// Replace `removed` lines at `at` by lines whose row counts are `inserted`.
    pub fn splice_lines(&mut self, at: usize, removed: usize, inserted: &[usize]) {
        let n = self.line_count();
        let at = at.min(n);
        let end = (at + removed).min(n);
        let mut rows: Vec<u32> = (0..n).map(|i| self.rows_before[i + 1] - self.rows_before[i]).collect();
        rows.splice(at..end, inserted.iter().map(|&r| r as u32));
        self.rows_before.clear();
        let mut acc = 0;
        self.rows_before.push(0);
        for r in rows {
            acc += r;
            self.rows_before.push(acc);
        }
    }

    /// First visual row of `line` (`total_rows()` for `line >= line_count`).
    pub fn row_of(&self, line: usize) -> usize {
        self.rows_before[line.min(self.line_count())] as usize
    }

    /// Visual rows used by `line` (0 when hidden).
    pub fn rows_in(&self, line: usize) -> usize {
        if line >= self.line_count() {
            return 0;
        }
        (self.rows_before[line + 1] - self.rows_before[line]) as usize
    }

    pub fn total_rows(&self) -> usize {
        *self.rows_before.last().unwrap() as usize
    }

    /// (line, row within line) showing visual `row`; clamped to the last row.
    pub fn line_at_row(&self, row: usize) -> (usize, usize) {
        let total = self.total_rows();
        if total == 0 {
            return (0, 0);
        }
        let row = row.min(total - 1) as u32;
        let idx = self.rows_before.partition_point(|&v| v <= row);
        let line = idx - 1;
        (line, (row - self.rows_before[line]) as usize)
    }

    /// One `(line, row_within_line)` per visual row starting at `first_row`
    /// (fewer when the document ends).
    pub fn visible_lines(&self, first_row: usize, rows_wanted: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::with_capacity(rows_wanted);
        if first_row >= self.total_rows() {
            return out;
        }
        let (mut line, mut sub) = self.line_at_row(first_row);
        while out.len() < rows_wanted && line < self.line_count() {
            let r = self.rows_in(line);
            while sub < r && out.len() < rows_wanted {
                out.push((line, sub));
                sub += 1;
            }
            line += 1;
            sub = 0;
        }
        out
    }
}

fn indent_of(s: &str, tab_w: usize) -> Option<usize> {
    let mut v = 0;
    for ch in s.chars() {
        match ch {
            ' ' => v += 1,
            '\t' => v += tab_w - v % tab_w,
            c if c.is_whitespace() => v += 1,
            _ => return Some(v),
        }
    }
    None // blank
}

/// Indentation-based regions `(header, last_line)`.
///
/// A header is a non-blank line followed by deeper-indented non-blank lines.
/// The region ends at the last non-blank line before the first line whose
/// indent is <= the header's (trailing blank lines excluded). Choice for
/// closers: a closing-bracket-only line (`}`, `)`, `];`...) with the SAME
/// indent as the header is not part of the block, so the region ends one line
/// before it and the closer stays visible under the fold marker. A closer
/// indented deeper than the header is simply part of the block. Minimum size
/// is 2 lines (end > start). Linear time; the closure is called once per line.
pub fn fold_regions_by_indent(
    lines: &mut dyn FnMut(usize) -> Option<String>,
    line_count: usize,
    tab_w: usize,
) -> Vec<(usize, usize)> {
    let tab_w = tab_w.max(1);
    let mut ends: Vec<usize> = vec![0; line_count];
    let mut have = vec![false; line_count];
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (line, indent)
    let mut prev: Option<usize> = None; // last non-blank line
    let close = |stack: &mut Vec<(usize, usize)>,
                 d: usize,
                 prev: Option<usize>,
                 ends: &mut Vec<usize>,
                 have: &mut Vec<bool>| {
        while let Some(&(h, hi)) = stack.last() {
            if hi < d {
                break;
            }
            stack.pop();
            if let Some(p) = prev
                && p > h
            {
                ends[h] = p;
                have[h] = true;
            }
        }
    };
    for i in 0..line_count {
        let Some(text) = lines(i) else { continue };
        let Some(d) = indent_of(&text, tab_w) else { continue };
        close(&mut stack, d, prev, &mut ends, &mut have);
        stack.push((i, d));
        prev = Some(i);
    }
    close(&mut stack, 0, prev, &mut ends, &mut have);
    (0..line_count).filter(|&i| have[i]).map(|i| (i, ends[i])).collect()
}

/// Parse a `textDocument/foldingRange` result (`[{startLine,endLine,..}]`).
/// Single-line and malformed ranges are dropped; sorted by start; for equal
/// starts the widest wins.
pub fn fold_regions_from_lsp(v: &serde_json::Value) -> Vec<(usize, usize)> {
    let mut m: BTreeMap<usize, usize> = BTreeMap::new();
    if let Some(arr) = v.as_array() {
        for r in arr {
            let (Some(s), Some(e)) = (r["startLine"].as_u64(), r["endLine"].as_u64()) else { continue };
            if e <= s {
                continue;
            }
            let (s, e) = (s as usize, e as usize);
            let slot = m.entry(s).or_insert(e);
            *slot = (*slot).max(e);
        }
    }
    m.into_iter().collect()
}

/// Merge region lists, deduplicating by start line. `a` = indent-based,
/// `b` = LSP; on equal starts `b` wins. Result sorted by start.
pub fn merge_regions(a: &[(usize, usize)], b: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut m: BTreeMap<usize, usize> = BTreeMap::new();
    for &(s, e) in a {
        m.insert(s, e);
    }
    for &(s, e) in b {
        m.insert(s, e);
    }
    m.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::visual_col;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    fn rand_str(r: &mut Rng, max: usize) -> String {
        let alpha = ['a', 'b', ' ', ' ', '\t', 'x', '-', 'é'];
        (0..r.below(max + 1)).map(|_| alpha[r.below(alpha.len())]).collect()
    }

    fn rm(widths: &[usize], cols: usize, f: &FoldSet) -> RowMap {
        let mut m = RowMap::new(widths.len());
        m.rebuild(&mut |i| widths[i], cols, f);
        m
    }

    #[test]
    fn wrap_none_when_fits_or_zero_cols() {
        assert!(wrap_breaks("hello", 5, 4).is_empty());
        assert!(wrap_breaks("hello world", 0, 4).is_empty());
        assert!(wrap_breaks("", 10, 4).is_empty());
    }

    #[test]
    fn wrap_exact_fit_and_one_over() {
        assert!(wrap_breaks("abcdefghij", 10, 4).is_empty());
        assert_eq!(wrap_breaks("abcdefghijk", 10, 4), vec![10]);
    }

    #[test]
    fn wrap_prefers_word_boundary_in_last_quarter() {
        // cols 10: "aaaaaaa bb" -> boundary after the space (width 8 >= 7.5)
        assert_eq!(wrap_breaks("aaaaaaa bbbb", 10, 4), vec![8]);
    }

    #[test]
    fn wrap_ignores_early_boundary() {
        // space at width 3 (< 75%): hard break
        assert_eq!(wrap_breaks("aa bbbbbbbbbbbb", 10, 4), vec![10]);
    }

    #[test]
    fn wrap_long_unbroken_token() {
        let s = "x".repeat(25);
        assert_eq!(wrap_breaks(&s, 10, 4), vec![10, 20]);
    }

    #[test]
    fn wrap_tabs_expand() {
        // tab = 4 cols: "\t\t\t" is 12 cols
        assert_eq!(wrap_breaks("\t\t\t", 8, 4), vec![2]);
        // tab wider than the row still progresses
        assert_eq!(wrap_breaks("\t\ta", 2, 8), vec![1, 2]);
    }

    #[test]
    fn wrap_total_width_matches_visual_col_without_breaks() {
        let s = "a\tbc\td";
        let w = visual_col(s, s.chars().count(), 4);
        assert!(wrap_breaks(s, w, 4).is_empty());
        assert_eq!(wrap_breaks(s, w - 1, 4).len(), 1);
    }

    #[test]
    fn wrap_rows_never_exceed_cols() {
        let mut r = Rng(7);
        for _ in 0..300 {
            let s = rand_str(&mut r, 80);
            let cols = 1 + r.below(20);
            let b = wrap_breaks(&s, cols, 4);
            let chars: Vec<char> = s.chars().collect();
            let mut starts = vec![0];
            starts.extend(b.iter().copied());
            starts.push(chars.len());
            for w in starts.windows(2) {
                assert!(w[0] < w[1] || chars.is_empty());
                let seg: String = chars[w[0]..w[1]].iter().collect();
                let vw = visual_col(&seg, seg.chars().count(), 4);
                // a single wide tab may exceed cols only when alone
                assert!(vw <= cols || seg.chars().count() == 1, "{seg:?} cols {cols}");
            }
        }
    }

    #[test]
    fn locate_basic() {
        let b = [8, 16];
        assert_eq!(locate(&b, 0), (0, 0));
        assert_eq!(locate(&b, 7), (0, 7));
        assert_eq!(locate(&b, 8), (1, 0));
        assert_eq!(locate(&b, 20), (2, 4));
        assert_eq!(locate(&[], 5), (0, 5));
    }

    #[test]
    fn col_at_basic_and_clamp() {
        let b = [8, 16];
        assert_eq!(col_at(&b, 1, 3), 11);
        assert_eq!(col_at(&b, 9, 2), 18);
        assert_eq!(col_at(&[], 0, 4), 4);
    }

    #[test]
    fn locate_col_at_roundtrip_random() {
        let mut r = Rng(42);
        for _ in 0..200 {
            let s = rand_str(&mut r, 100);
            let cols = 1 + r.below(15);
            let b = wrap_breaks(&s, cols, 4);
            for col in 0..=s.chars().count() {
                let (row, c) = locate(&b, col);
                assert_eq!(col_at(&b, row, c), col);
            }
        }
    }

    #[test]
    fn rowmap_no_wrap_is_identity() {
        let m = rm(&[5, 500, 0], 0, &FoldSet::new());
        assert_eq!(m.total_rows(), 3);
        assert_eq!((m.row_of(0), m.row_of(1), m.row_of(2)), (0, 1, 2));
        assert_eq!(m.rows_in(1), 1);
    }

    #[test]
    fn rowmap_wrap_prefix_sums() {
        let m = rm(&[0, 10, 11, 25, 5], 10, &FoldSet::new());
        let rows: Vec<_> = (0..5).map(|i| m.rows_in(i)).collect();
        assert_eq!(rows, vec![1, 1, 2, 3, 1]);
        assert_eq!(m.row_of(3), 4);
        assert_eq!(m.total_rows(), 8);
    }

    #[test]
    fn rowmap_hidden_lines_zero_rows() {
        let mut f = FoldSet::new();
        f.fold(1, 3);
        let m = rm(&[1; 6], 0, &f);
        assert_eq!(m.rows_in(2), 0);
        assert_eq!(m.rows_in(3), 0);
        assert_eq!(m.rows_in(1), 1);
        assert_eq!(m.total_rows(), 4);
        assert_eq!(m.row_of(4), 2);
    }

    #[test]
    fn line_at_row_skips_hidden() {
        let mut f = FoldSet::new();
        f.fold(0, 2);
        let m = rm(&[1; 5], 0, &f);
        assert_eq!(m.line_at_row(0), (0, 0));
        assert_eq!(m.line_at_row(1), (3, 0));
        assert_eq!(m.line_at_row(2), (4, 0));
    }

    #[test]
    fn line_at_row_clamps() {
        let m = rm(&[1, 1, 1], 0, &FoldSet::new());
        assert_eq!(m.line_at_row(99), (2, 0));
        let e = RowMap::new(0);
        assert_eq!(e.line_at_row(5), (0, 0));
        assert_eq!(e.total_rows(), 0);
    }

    #[test]
    fn row_of_line_at_row_inverse() {
        let mut r = Rng(3);
        let widths: Vec<usize> = (0..200).map(|_| r.below(60)).collect();
        let mut f = FoldSet::new();
        f.fold(10, 30);
        f.fold(20, 25);
        f.fold(100, 120);
        let m = rm(&widths, 12, &f);
        for row in 0..m.total_rows() {
            let (l, sub) = m.line_at_row(row);
            assert!(m.rows_in(l) > sub);
            assert_eq!(m.row_of(l) + sub, row);
        }
        for l in 0..200 {
            if m.rows_in(l) > 0 {
                assert_eq!(m.line_at_row(m.row_of(l)), (l, 0));
            }
        }
    }

    #[test]
    fn visible_lines_slices() {
        let m = rm(&[5, 25, 5], 10, &FoldSet::new());
        assert_eq!(m.visible_lines(0, 3), vec![(0, 0), (1, 0), (1, 1)]);
        assert_eq!(m.visible_lines(2, 3), vec![(1, 1), (1, 2), (2, 0)]);
        assert_eq!(m.visible_lines(4, 10), vec![(2, 0)]);
        assert!(m.visible_lines(5, 4).is_empty());
        assert!(m.visible_lines(0, 0).is_empty());
    }

    #[test]
    fn visible_lines_with_fold() {
        let mut f = FoldSet::new();
        f.fold(1, 2);
        let m = rm(&[1; 5], 0, &f);
        let v: Vec<usize> = m.visible_lines(0, 10).iter().map(|x| x.0).collect();
        assert_eq!(v, vec![0, 1, 3, 4]);
    }

    #[test]
    fn update_line_matches_rebuild() {
        let mut r = Rng(11);
        let mut widths: Vec<usize> = (0..50).map(|_| r.below(40)).collect();
        let mut m = rm(&widths, 8, &FoldSet::new());
        for _ in 0..200 {
            let l = r.below(50);
            widths[l] = r.below(40);
            m.update_line(l, widths[l].div_ceil(8).max(1));
            assert_eq!(m.rows_before, rm(&widths, 8, &FoldSet::new()).rows_before);
        }
    }

    #[test]
    fn splice_matches_rebuild_property() {
        let mut r = Rng(99);
        let mut widths: Vec<usize> = (0..30).map(|_| r.below(30)).collect();
        let mut m = rm(&widths, 7, &FoldSet::new());
        for _ in 0..300 {
            let at = r.below(widths.len() + 1);
            let removed = r.below((widths.len() - at).min(4) + 1);
            let ins: Vec<usize> = (0..r.below(4)).map(|_| r.below(30)).collect();
            widths.splice(at..at + removed, ins.iter().copied());
            let rows: Vec<usize> = ins.iter().map(|w| w.div_ceil(7).max(1)).collect();
            m.splice_lines(at, removed, &rows);
            assert_eq!(m.rows_before, rm(&widths, 7, &FoldSet::new()).rows_before);
        }
    }

    #[test]
    fn rebuild_100k_lines_is_fast() {
        let widths: Vec<usize> = (0..100_000).map(|i| i % 200).collect();
        let mut f = FoldSet::new();
        for k in 0..100 {
            f.fold(k * 900, k * 900 + 300);
        }
        let mut m = RowMap::new(100_000);
        let t = std::time::Instant::now();
        m.rebuild(&mut |i| widths[i], 80, &f);
        let el = t.elapsed();
        eprintln!("rebuild 100k: {el:?}");
        assert!(el.as_millis() < 50, "{el:?}");
        assert!(m.total_rows() > 0);
    }

    #[test]
    fn fold_basic_and_hidden() {
        let mut f = FoldSet::new();
        f.fold(2, 5);
        assert!(!f.is_hidden(2));
        assert!(f.is_hidden(3) && f.is_hidden(5));
        assert!(!f.is_hidden(6));
        assert_eq!(f.is_fold_start(2), Some((2, 5)));
        assert_eq!(f.is_fold_start(3), None);
        f.fold(4, 4);
        assert_eq!(f.regions().len(), 1);
    }

    #[test]
    fn fold_toggle() {
        let avail = [(1, 4), (2, 3)];
        let mut f = FoldSet::new();
        assert!(f.toggle_at(1, &avail));
        assert!(f.is_hidden(2));
        assert!(f.toggle_at(1, &avail));
        assert!(!f.is_hidden(2));
        assert!(!f.toggle_at(0, &avail));
    }

    #[test]
    fn fold_nested_outer_wins_unfold_outer_keeps_inner() {
        let mut f = FoldSet::new();
        f.fold(0, 10);
        f.fold(3, 6);
        assert_eq!(f.hidden_ranges(), &[(1, 10)]);
        f.unfold(0);
        assert!(!f.is_hidden(1));
        assert!(f.is_hidden(4) && f.is_hidden(6));
        assert!(!f.is_hidden(7));
    }

    #[test]
    fn fold_overlapping_merge() {
        let mut f = FoldSet::new();
        f.fold(1, 5);
        f.fold(3, 8);
        assert_eq!(f.hidden_ranges(), &[(2, 8)]);
    }

    #[test]
    fn fold_unfold_all_fold_all_reveal() {
        let mut f = FoldSet::new();
        f.fold_all(&[(0, 3), (5, 5), (6, 9)]);
        assert_eq!(f.regions(), &[(0, 3), (6, 9)]);
        assert!(f.reveal(8));
        assert!(!f.is_hidden(8) && f.is_hidden(2));
        f.unfold_all();
        assert!(f.is_empty() && !f.is_hidden(2));
    }

    #[test]
    fn adjust_insert_above_shifts() {
        let mut f = FoldSet::new();
        f.fold(10, 20);
        f.adjust_for_edit(3, 2);
        assert_eq!(f.regions(), &[(12, 22)]);
    }

    #[test]
    fn adjust_insert_inside_grows_and_below_untouched() {
        let mut f = FoldSet::new();
        f.fold(10, 20);
        f.adjust_for_edit(15, 3);
        assert_eq!(f.regions(), &[(10, 23)]);
        f.adjust_for_edit(30, 3);
        assert_eq!(f.regions(), &[(10, 23)]);
    }

    #[test]
    fn adjust_remove_cases() {
        let mut f = FoldSet::new();
        f.fold_all(&[(10, 20), (30, 40)]);
        f.adjust_for_edit(5, -3); // removes 6..=8
        assert_eq!(f.regions(), &[(7, 17), (27, 37)]);
        f.adjust_for_edit(26, -5); // removes 27..=31 -> second starts inside
        assert_eq!(f.regions(), &[(7, 17)]);
        f.adjust_for_edit(8, -9);
        assert_eq!(f.regions(), &[(7, 8)]);
        f.adjust_for_edit(7, -1);
        assert!(f.is_empty());
    }

    #[test]
    fn shift_after_handles_the_line_before_the_first() {
        let mut f = FoldSet::new();
        f.fold_all(&[(0, 3), (5, 8)]);
        f.shift_after(-1, 2); // two lines inserted before line 0
        assert_eq!(f.regions(), &[(2, 5), (7, 10)]);
        f.shift_after(-1, -2); // and removed again
        assert_eq!(f.regions(), &[(0, 3), (5, 8)]);
        f.shift_after(-1, -1); // the first line removed: the first fold starts inside it
        assert_eq!(f.regions(), &[(4, 7)]);
    }

    #[test]
    fn swap_blocks_moves_folds_with_their_lines() {
        let mut f = FoldSet::new();
        f.fold_all(&[(1, 2), (4, 6), (9, 12)]);
        // lines 1..=2 (2 lines) and 3..=6 (4 lines) change places
        f.swap_blocks(1, 2, 4);
        assert_eq!(f.regions(), &[(2, 4), (5, 6), (9, 12)]);
        // a fold around both blocks stays, one that straddles the edge between them is dropped
        let mut f = FoldSet::new();
        f.fold_all(&[(0, 8), (2, 5), (7, 9)]);
        f.swap_blocks(1, 2, 4);
        assert_eq!(f.regions(), &[(0, 8), (7, 9)]);
        // swapping back restores the original positions
        let mut f = FoldSet::new();
        f.fold_all(&[(1, 2), (4, 6)]);
        f.swap_blocks(1, 2, 4);
        f.swap_blocks(1, 4, 2);
        assert_eq!(f.regions(), &[(1, 2), (4, 6)]);
    }

    #[test]
    fn adjust_keeps_fold_start_line() {
        let mut f = FoldSet::new();
        f.fold(4, 8);
        f.adjust_for_edit(4, 1);
        assert_eq!(f.regions(), &[(4, 9)]);
    }

    fn run_indent(src: &str) -> Vec<(usize, usize)> {
        let v: Vec<&str> = src.lines().collect();
        fold_regions_by_indent(&mut |i| v.get(i).map(|s| s.to_string()), v.len(), 4)
    }

    #[test]
    fn indent_typescript_snippet() {
        let src = "\
import x from 'y'

export class Foo {
  private a = 1

  method(b: number) {
    if (b) {
      return 1
    }
    return 2
  }
}

function g() {
  return 0
}
";
        assert_eq!(run_indent(src), vec![(2, 10), (5, 9), (6, 7), (13, 14)]);
    }

    #[test]
    fn indent_python_style() {
        let src = "def f(x):\n    if x:\n        return 1\n    else:\n        return 2\n\n\nclass A:\n    pass\n\n\n";
        assert_eq!(run_indent(src), vec![(0, 4), (1, 2), (3, 4), (7, 8)]);
    }

    #[test]
    fn indent_min_size_and_blank_inside() {
        assert_eq!(run_indent("a\nb\nc"), vec![]);
        assert_eq!(run_indent("a\n  b\n\n  c\nd"), vec![(0, 3)]);
        assert_eq!(run_indent("a\n\n\nb"), vec![]);
    }

    #[test]
    fn indent_tabs_and_deeper_closer() {
        assert_eq!(run_indent("a {\n\tb\n}"), vec![(0, 1)]);
        // closer deeper than header belongs to the block
        assert_eq!(run_indent("a {\n    b\n    }\nc"), vec![(0, 2)]);
    }

    #[test]
    fn indent_linear_on_large_input() {
        let mut v = Vec::new();
        for i in 0..50_000 {
            v.push(format!("{}x{}", " ".repeat(i % 5 * 2), i));
        }
        let t = std::time::Instant::now();
        let r = fold_regions_by_indent(&mut |i| v.get(i).cloned(), v.len(), 4);
        assert!(!r.is_empty());
        assert!(t.elapsed().as_millis() < 200);
    }

    #[test]
    fn lsp_parse_and_drop_single_line() {
        let j: serde_json::Value = serde_json::from_str(
            r#"[{"startLine":5,"endLine":9},{"startLine":2,"endLine":2},
               {"startLine":0,"endLine":3,"kind":"imports"},{"startLine":5,"endLine":12},
               {"startLine":"x"},{"endLine":4}]"#,
        )
        .unwrap();
        assert_eq!(fold_regions_from_lsp(&j), vec![(0, 3), (5, 12)]);
        assert!(fold_regions_from_lsp(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn merge_prefers_lsp() {
        let a = [(1, 5), (8, 9)];
        let b = [(1, 6), (3, 4)];
        assert_eq!(merge_regions(&a, &b), vec![(1, 6), (3, 4), (8, 9)]);
    }
}
