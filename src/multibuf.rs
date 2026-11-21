//! Multibuffer: one virtual tab made of excerpts of many files (project search results, LSP
//! references). This is the pure model: locations in, excerpts (merged context ranges) and a flat
//! list of rows out, plus the selection and scroll state. Drawing and input are in `app_multibuf`.
//!
//! The tab is read-only navigation: the excerpts are snapshots of the text when the tab was
//! opened (open unsaved buffers included); Enter or a click jumps to the real file.

use crate::{theme::Rgba, ui::Rect};
use std::path::{Path, PathBuf};

/// Lines of context shown above and below every match.
pub const CONTEXT: usize = 2;
/// Longer lines are cut (the tab is for reading matches, not for whole minified files).
pub const MAX_LINE_CHARS: usize = 400;
/// Tabs are shown as spaces up to a multiple of this.
pub const TAB: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Search,
    References,
}

/// A place to show: 0-based line, char column, and the length of the match in chars (0 = unknown,
/// the word at the column is used).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loc {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    pub len: usize,
}

/// A match inside an excerpt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub line: usize,
    /// Char column in the real line (where a jump lands).
    pub col: usize,
    /// Column in the shown line (tabs expanded).
    pub dcol: usize,
    pub len: usize,
}

/// Syntax runs of every shown line: (byte start, byte end, colour, style flags).
pub type Runs = Vec<Vec<(usize, usize, Rgba, u8)>>;

pub struct Excerpt {
    /// First line (0-based) of the excerpt.
    pub start: usize,
    /// The shown text of each line, tabs expanded.
    pub lines: Vec<String>,
    pub marks: Vec<Mark>,
    /// Highlighting, computed when first drawn (keyed by the theme's text colour).
    pub runs: Option<(Rgba, Runs)>,
}

pub struct FileGroup {
    pub path: PathBuf,
    /// The path as shown in the header.
    pub label: String,
    pub excerpts: Vec<Excerpt>,
}

impl FileGroup {
    pub fn match_count(&self) -> usize {
        self.excerpts.iter().map(|e| e.marks.len()).sum()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// Header of file group `0`.
    File(usize),
    /// Break between two excerpts of one file.
    Gap,
    /// Line `i` of excerpt `e` of group `g`.
    Line { g: usize, e: usize, i: usize },
}

// ── pure helpers ──

/// The line ranges (start, end exclusive) to show for matches on `lines`: each match with `ctx`
/// lines around it, clamped to `total` lines, overlapping or touching ranges merged. The result is
/// sorted. Lines past the end are ignored.
pub fn merge_ranges(lines: &[usize], ctx: usize, total: usize) -> Vec<(usize, usize)> {
    let mut v: Vec<usize> = lines.iter().copied().filter(|l| *l < total).collect();
    v.sort_unstable();
    v.dedup();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for l in v {
        let (s, e) = (l.saturating_sub(ctx), (l + ctx + 1).min(total));
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// `line` for display: tabs become spaces (to the next multiple of [`TAB`]), cut at [`MAX_LINE_CHARS`].
pub fn expand_tabs(line: &str) -> String {
    let mut out = String::new();
    let mut w = 0;
    for ch in line.chars().take(MAX_LINE_CHARS) {
        if ch == '\t' {
            let n = TAB - w % TAB;
            for _ in 0..n {
                out.push(' ');
            }
            w += n;
        } else {
            out.push(ch);
            w += 1;
        }
    }
    out
}

/// The shown column of char column `col` of `line` (tabs expanded).
pub fn display_col(line: &str, col: usize) -> usize {
    let mut w = 0;
    for ch in line.chars().take(col) {
        w += if ch == '\t' { TAB - w % TAB } else { 1 };
    }
    w
}

/// Chars of the identifier that starts at char `col` of `line` (at least 1).
pub fn word_len(line: &str, col: usize) -> usize {
    line.chars().skip(col).take_while(|c| c.is_alphanumeric() || *c == '_').count().max(1)
}

/// Groups of excerpts for `locs`: files in path order, each with its matches in line order (a match
/// listed twice shows once) and the context ranges merged. `read` gives the lines of a file (`None`
/// skips it), `label` the header text of a path.
pub fn build(
    locs: &[Loc],
    ctx: usize,
    label: impl Fn(&Path) -> String,
    mut read: impl FnMut(&Path) -> Option<Vec<String>>,
) -> Vec<FileGroup> {
    let mut sorted: Vec<&Loc> = locs.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)).then(a.col.cmp(&b.col)));
    sorted.dedup_by(|a, b| a.path == b.path && a.line == b.line && a.col == b.col);
    let mut groups = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let path = &sorted[i].path;
        let mut j = i;
        while j < sorted.len() && sorted[j].path == *path {
            j += 1;
        }
        let file = &sorted[i..j];
        i = j;
        let Some(text) = read(path) else { continue };
        let on: Vec<usize> = file.iter().map(|l| l.line).collect();
        let mut excerpts = Vec::new();
        for (s, e) in merge_ranges(&on, ctx, text.len()) {
            let marks = file
                .iter()
                .filter(|l| l.line >= s && l.line < e)
                .map(|l| {
                    let raw = &text[l.line];
                    let len = if l.len == 0 { word_len(raw, l.col) } else { l.len };
                    Mark { line: l.line, col: l.col, dcol: display_col(raw, l.col), len }
                })
                .collect();
            let lines = text[s..e].iter().map(|l| expand_tabs(l)).collect();
            excerpts.push(Excerpt { start: s, lines, marks, runs: None });
        }
        if !excerpts.is_empty() {
            groups.push(FileGroup { path: path.clone(), label: label(path), excerpts });
        }
    }
    groups
}

// ── the tab's state ──

pub struct MultiBuf {
    pub kind: Kind,
    pub title: String,
    pub groups: Vec<FileGroup>,
    pub rows: Vec<Row>,
    /// Rows of lines with a match, and the first one of every excerpt (both ascending).
    pub hits: Vec<usize>,
    pub ex_first: Vec<usize>,
    /// The selected row.
    pub sel: usize,
    /// Scroll offset in logical px.
    pub scroll: f64,
    /// The selection must be scrolled into view on the next draw.
    pub follow: bool,
    /// Where it was drawn last frame.
    pub rect: Rect,
    /// The highest line number shown (1-based), for the gutter width.
    pub max_line: usize,
}

impl MultiBuf {
    pub fn new(kind: Kind, title: String, groups: Vec<FileGroup>) -> Self {
        let mut rows = Vec::new();
        let (mut hits, mut ex_first, mut max_line) = (Vec::new(), Vec::new(), 0);
        for (g, grp) in groups.iter().enumerate() {
            rows.push(Row::File(g));
            for (e, ex) in grp.excerpts.iter().enumerate() {
                if e > 0 {
                    rows.push(Row::Gap);
                }
                let first_hit = hits.len();
                for i in 0..ex.lines.len() {
                    if ex.marks.iter().any(|m| m.line == ex.start + i) {
                        hits.push(rows.len());
                    }
                    rows.push(Row::Line { g, e, i });
                }
                if hits.len() > first_hit {
                    ex_first.push(hits[first_hit]);
                }
                max_line = max_line.max(ex.start + ex.lines.len());
            }
        }
        let sel = hits.first().copied().unwrap_or(0);
        MultiBuf {
            kind,
            title,
            groups,
            rows,
            hits,
            ex_first,
            sel,
            scroll: 0.0,
            follow: true,
            rect: Rect::new(0, 0, 0, 0),
            max_line,
        }
    }

    pub fn match_count(&self) -> usize {
        self.groups.iter().map(FileGroup::match_count).sum()
    }

    /// "12 results in 3 files".
    pub fn summary(&self) -> String {
        let (n, f) = (self.match_count(), self.groups.len());
        format!("{n} result{} in {f} file{}", if n == 1 { "" } else { "s" }, if f == 1 { "" } else { "s" })
    }

    /// The absolute line (0-based) a line row shows.
    pub fn line_no(&self, row: usize) -> Option<usize> {
        match self.rows.get(row)? {
            Row::Line { g, e, i } => Some(self.groups[*g].excerpts[*e].start + i),
            _ => None,
        }
    }

    /// The first match on a line row.
    pub fn mark_at(&self, row: usize) -> Option<&Mark> {
        let line = self.line_no(row)?;
        let Row::Line { g, e, .. } = self.rows[row] else { return None };
        self.groups[g].excerpts[e].marks.iter().find(|m| m.line == line)
    }

    /// Where a jump from `row` lands: file, line, char column. A header goes to the file's first
    /// match, a context line to its start, a gap nowhere.
    pub fn loc_at(&self, row: usize) -> Option<(PathBuf, usize, usize)> {
        match *self.rows.get(row)? {
            Row::File(g) => {
                let grp = &self.groups[g];
                let m = grp.excerpts.first()?.marks.first()?;
                Some((grp.path.clone(), m.line, m.col))
            }
            Row::Gap => None,
            Row::Line { g, .. } => {
                let line = self.line_no(row)?;
                let col = self.mark_at(row).map_or(0, |m| m.col);
                Some((self.groups[g].path.clone(), line, col))
            }
        }
    }

    fn select(&mut self, row: usize) {
        self.sel = row.min(self.rows.len().saturating_sub(1));
        self.follow = true;
    }

    /// Select the next (`dir > 0`) or previous line with a match.
    pub fn step_hit(&mut self, dir: i32) {
        let to = if dir > 0 {
            self.hits.iter().copied().find(|r| *r > self.sel)
        } else {
            self.hits.iter().rev().copied().find(|r| *r < self.sel)
        };
        if let Some(r) = to {
            self.select(r);
        }
    }

    /// Select the first match of the next excerpt (`dir > 0`), or of this one / the previous one.
    pub fn step_excerpt(&mut self, dir: i32) {
        let to = if dir > 0 {
            self.ex_first.iter().copied().find(|r| *r > self.sel)
        } else {
            self.ex_first.iter().rev().copied().find(|r| *r < self.sel)
        };
        if let Some(r) = to {
            self.select(r);
        }
    }

    pub fn select_first(&mut self) {
        if let Some(r) = self.hits.first().copied() {
            self.select(r);
        }
    }

    pub fn select_last(&mut self) {
        if let Some(r) = self.hits.last().copied() {
            self.select(r);
        }
    }

    /// Select a row by a click (a gap cannot be selected).
    pub fn click_row(&mut self, row: usize) {
        if matches!(self.rows.get(row), Some(Row::File(_) | Row::Line { .. })) {
            self.select(row);
        }
    }

    pub fn content_h(&self, lh: i32) -> f64 {
        self.rows.len() as f64 * lh as f64
    }

    pub fn clamp_scroll(&mut self, lh: i32) {
        let max = (self.content_h(lh) - self.rect.h as f64).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    pub fn scroll_by(&mut self, px: f64, lh: i32) {
        self.scroll += px;
        self.clamp_scroll(lh);
    }

    /// Scroll just enough for the selected row to be on screen.
    pub fn ensure_visible(&mut self, lh: i32) {
        let top = self.sel as f64 * lh as f64;
        let view = self.rect.h as f64;
        if top < self.scroll {
            self.scroll = top;
        } else if top + lh as f64 > self.scroll + view {
            self.scroll = top + lh as f64 - view;
        }
        self.clamp_scroll(lh);
    }

    /// The row under `y` (px from the top of the tab).
    pub fn row_at(&self, y: f64, lh: i32) -> Option<usize> {
        if y < 0.0 || lh <= 0 {
            return None;
        }
        let r = ((y + self.scroll) / lh as f64) as usize;
        (r < self.rows.len()).then_some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(p: &str, line: usize, col: usize) -> Loc {
        Loc { path: PathBuf::from(p), line, col, len: 0 }
    }

    fn file(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    fn groups(locs: &[Loc], total: usize) -> Vec<FileGroup> {
        build(locs, 2, |p| p.display().to_string(), |_| Some(file(total)))
    }

    #[test]
    fn context_ranges_merge_when_they_overlap_or_touch() {
        // 10 and 14 with 2 lines of context: 8..13 and 12..17 overlap.
        assert_eq!(merge_ranges(&[10, 14], 2, 100), vec![(8, 17)]);
        // 10 and 15: 8..13 and 13..18 touch, no line is missing between them.
        assert_eq!(merge_ranges(&[10, 15], 2, 100), vec![(8, 18)]);
        // 10 and 16: 8..13 and 14..19 leave line 13 out, so two excerpts.
        assert_eq!(merge_ranges(&[10, 16], 2, 100), vec![(8, 13), (14, 19)]);
    }

    #[test]
    fn ranges_are_sorted_clamped_and_deduplicated() {
        assert_eq!(merge_ranges(&[50, 1, 50], 2, 100), vec![(0, 4), (48, 53)]);
        assert_eq!(merge_ranges(&[9], 3, 10), vec![(6, 10)], "clamped to the end of the file");
        assert_eq!(merge_ranges(&[0], 2, 10), vec![(0, 3)], "clamped to the start");
        assert_eq!(merge_ranges(&[12], 2, 10), Vec::<(usize, usize)>::new(), "a line past the end is ignored");
        assert_eq!(merge_ranges(&[], 2, 10), Vec::<(usize, usize)>::new());
        assert_eq!(merge_ranges(&[3], 0, 10), vec![(3, 4)], "no context");
    }

    #[test]
    fn tabs_expand_to_the_next_stop_and_columns_follow() {
        assert_eq!(expand_tabs("\tx"), "    x");
        assert_eq!(expand_tabs("ab\tc"), "ab  c");
        assert_eq!(display_col("\tfoo", 1), 4);
        assert_eq!(display_col("ab\tc", 3), 4);
        assert_eq!(display_col("abc", 99), 3);
        assert_eq!(expand_tabs(&"x".repeat(MAX_LINE_CHARS + 50)).chars().count(), MAX_LINE_CHARS);
    }

    #[test]
    fn the_word_at_a_column_is_the_match_when_its_length_is_unknown() {
        assert_eq!(word_len("let foo_bar = 1;", 4), 7);
        assert_eq!(word_len("a.b", 1), 1, "never zero");
        assert_eq!(word_len("", 0), 1);
    }

    #[test]
    fn files_come_in_path_order_with_their_matches_in_line_order() {
        let locs = [loc("b.rs", 3, 0), loc("a.rs", 40, 2), loc("a.rs", 5, 1), loc("a.rs", 5, 1), loc("b.rs", 3, 6)];
        let g = groups(&locs, 100);
        let names: Vec<&str> = g.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(names, ["a.rs", "b.rs"]);
        assert_eq!(g[0].excerpts.len(), 2);
        assert_eq!(g[0].excerpts[0].start, 3);
        assert_eq!(g[0].excerpts[0].lines.len(), 5);
        assert_eq!(g[0].excerpts[0].lines[2], "line 5");
        assert_eq!(g[0].excerpts[1].start, 38);
        assert_eq!(g[0].match_count(), 2, "the repeated match shows once");
        assert_eq!(g[1].match_count(), 2, "two matches on a line stay two");
        assert_eq!(g[1].excerpts.len(), 1);
    }

    #[test]
    fn unreadable_files_and_lines_past_the_end_are_skipped() {
        let locs = [loc("gone.rs", 1, 0), loc("short.rs", 50, 0), loc("ok.rs", 1, 0)];
        let g = build(
            &locs,
            2,
            |p| p.display().to_string(),
            |p| if p.ends_with("gone.rs") { None } else { Some(file(10)) },
        );
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].label, "ok.rs");
    }

    #[test]
    fn rows_are_a_header_then_the_excerpts_with_gaps_between_them() {
        let locs = [loc("a.rs", 2, 0), loc("a.rs", 30, 0), loc("b.rs", 1, 0)];
        let mb = MultiBuf::new(Kind::Search, "t".into(), groups(&locs, 100));
        // a.rs: header, lines 0..5 (5 rows), gap, lines 28..33 (5 rows); b.rs: header, lines 0..4 (4 rows).
        assert_eq!(mb.rows.len(), 1 + 5 + 1 + 5 + 1 + 4);
        assert_eq!(mb.rows[0], Row::File(0));
        assert_eq!(mb.rows[6], Row::Gap);
        assert_eq!(mb.rows[12], Row::File(1));
        assert_eq!(mb.line_no(1), Some(0));
        assert_eq!(mb.line_no(0), None);
        assert_eq!(mb.hits, vec![3, 9, 14]);
        assert_eq!(mb.ex_first, vec![3, 9, 14]);
        assert_eq!(mb.sel, 3, "starts on the first match");
        assert_eq!(mb.max_line, 33);
        assert_eq!(mb.summary(), "3 results in 2 files");
    }

    #[test]
    fn arrows_walk_the_matches_and_excerpts_stop_at_the_ends() {
        let locs = [loc("a.rs", 2, 0), loc("a.rs", 3, 0), loc("a.rs", 30, 0)];
        let mut mb = MultiBuf::new(Kind::Search, "t".into(), groups(&locs, 100));
        assert_eq!(mb.hits.len(), 3);
        assert_eq!(mb.ex_first.len(), 2, "the first two share an excerpt");
        let first = mb.sel;
        mb.step_hit(-1);
        assert_eq!(mb.sel, first, "already on the first match");
        mb.step_hit(1);
        assert_eq!(mb.line_no(mb.sel), Some(3));
        mb.step_excerpt(1);
        assert_eq!(mb.line_no(mb.sel), Some(30));
        mb.step_excerpt(1);
        assert_eq!(mb.line_no(mb.sel), Some(30), "no next excerpt");
        mb.step_excerpt(-1);
        assert_eq!(mb.line_no(mb.sel), Some(2), "back to the first match of the first excerpt");
        mb.step_hit(1);
        mb.step_excerpt(-1);
        assert_eq!(mb.line_no(mb.sel), Some(2), "from the middle of an excerpt: its start");
        mb.select_last();
        assert_eq!(mb.line_no(mb.sel), Some(30));
        mb.select_first();
        assert_eq!(mb.line_no(mb.sel), Some(2));
    }

    #[test]
    fn a_jump_lands_on_the_match_a_header_on_the_first_one_and_a_gap_nowhere() {
        let locs = [loc("a.rs", 2, 3), loc("a.rs", 30, 0)];
        let mb = MultiBuf::new(Kind::Search, "t".into(), groups(&locs, 100));
        assert_eq!(mb.loc_at(0), Some((PathBuf::from("a.rs"), 2, 3)));
        assert_eq!(mb.loc_at(mb.hits[0]), Some((PathBuf::from("a.rs"), 2, 3)));
        assert_eq!(mb.loc_at(mb.hits[0] - 1), Some((PathBuf::from("a.rs"), 1, 0)), "a context line");
        assert_eq!(mb.loc_at(6), None, "the gap");
        assert_eq!(mb.loc_at(999), None);
    }

    #[test]
    fn scrolling_follows_the_selection_and_rows_are_found_by_position() {
        let locs = [loc("a.rs", 2, 0), loc("a.rs", 60, 0)];
        let mut mb = MultiBuf::new(Kind::Search, "t".into(), groups(&locs, 100));
        mb.rect = Rect::new(0, 0, 400, 100);
        let lh = 20;
        mb.clamp_scroll(lh);
        assert_eq!(mb.scroll, 0.0);
        mb.step_hit(1);
        mb.ensure_visible(lh);
        assert!(mb.scroll > 0.0);
        let top = mb.sel as f64 * 20.0;
        assert!(top >= mb.scroll && top + 20.0 <= mb.scroll + 100.0, "the selected row is on screen");
        mb.scroll_by(-1e9, lh);
        assert_eq!(mb.scroll, 0.0);
        mb.scroll_by(1e9, lh);
        assert_eq!(mb.scroll, mb.content_h(lh) - 100.0);
        mb.scroll = 0.0;
        assert_eq!(mb.row_at(45.0, lh), Some(2));
        assert_eq!(mb.row_at(-1.0, lh), None);
        mb.click_row(6);
        assert_ne!(mb.sel, 6, "a gap is not selectable");
        mb.click_row(2);
        assert_eq!(mb.sel, 2);
    }
}
