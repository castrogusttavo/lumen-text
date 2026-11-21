//! Multibuffer: one virtual tab made of excerpts of many files (project search results, LSP
//! references). This is the pure model: locations in, excerpts (merged context ranges) and a flat
//! list of rows out, plus the selection and scroll state. Drawing and input are in `app_multibuf`.
//!
//! The excerpts are snapshots of the text when the tab was opened (open unsaved buffers included).
//! They can be edited in place: every excerpt keeps the lines it was read with (`orig`, at line
//! `orig_start` of the file) next to the lines as edited (`raw`), so a save can check the file still
//! has the old lines and replace exactly that range, whatever other excerpts did to the line count.
//! Ctrl+Enter or a click on a file header jumps to the real file.

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
    /// First line (0-based) of the excerpt as edited (earlier excerpts of the file may have grown
    /// or shrunk since it was read).
    pub start: usize,
    /// The first line of the excerpt in the file as last read or saved.
    pub orig_start: usize,
    /// The lines as last read or saved.
    pub orig: Vec<String>,
    /// The lines as edited, whole.
    pub raw: Vec<String>,
    /// The shown text of each line (`raw`, tabs expanded and cut).
    pub lines: Vec<String>,
    pub marks: Vec<Mark>,
    /// Highlighting, computed when first drawn (keyed by the theme's text colour).
    pub runs: Option<(Rgba, Runs)>,
}

impl Excerpt {
    pub fn is_dirty(&self) -> bool {
        self.raw != self.orig
    }

    /// Refresh what is shown from `raw` after an edit.
    fn refresh(&mut self) {
        self.lines = self.raw.iter().map(|l| expand_tabs(l)).collect();
        let start = self.start;
        for m in &mut self.marks {
            if let Some(raw) = m.line.checked_sub(start).and_then(|i| self.raw.get(i)) {
                m.dcol = display_col(raw, m.col);
            }
        }
        self.runs = None;
    }

    /// Insert `s` (no line break) at char `col` of line `i`; the new column.
    pub fn insert_at(&mut self, i: usize, col: usize, s: &str) -> usize {
        let line = &mut self.raw[i];
        let at = byte_at(line, col);
        line.insert_str(at, s);
        self.refresh();
        col + s.chars().count()
    }

    /// Break line `i` at char `col`: the rest becomes a new line `i + 1`. Marks after the break move down.
    pub fn split_at(&mut self, i: usize, col: usize) {
        let abs = self.start + i;
        let at = byte_at(&self.raw[i], col);
        let rest = self.raw[i].split_off(at);
        self.raw.insert(i + 1, rest);
        for m in &mut self.marks {
            if m.line > abs {
                m.line += 1;
            } else if m.line == abs && m.col >= col {
                m.line += 1;
                m.col -= col;
            }
        }
        self.refresh();
    }

    /// Join line `i` to the end of the line before it; the new position (line, col). `None` on the first line.
    pub fn join_prev(&mut self, i: usize) -> Option<(usize, usize)> {
        if i == 0 || i >= self.raw.len() {
            return None;
        }
        let abs = self.start + i;
        let line = self.raw.remove(i);
        let prev_len = self.raw[i - 1].chars().count();
        self.raw[i - 1].push_str(&line);
        for m in &mut self.marks {
            if m.line == abs {
                m.line -= 1;
                m.col += prev_len;
            } else if m.line > abs {
                m.line -= 1;
            }
        }
        self.refresh();
        Some((i - 1, prev_len))
    }

    /// Remove the char before `col` of line `i` (joining lines at column 0); the new position.
    pub fn backspace(&mut self, i: usize, col: usize) -> Option<(usize, usize)> {
        if col == 0 {
            return self.join_prev(i);
        }
        let line = &mut self.raw[i];
        let (a, b) = (byte_at(line, col - 1), byte_at(line, col));
        line.replace_range(a..b, "");
        self.refresh();
        Some((i, col - 1))
    }

    /// Remove the char at `col` of line `i` (joining the next line at the end of this one).
    pub fn delete(&mut self, i: usize, col: usize) -> Option<(usize, usize)> {
        if col < self.raw[i].chars().count() {
            let line = &mut self.raw[i];
            let (a, b) = (byte_at(line, col), byte_at(line, col + 1));
            line.replace_range(a..b, "");
            self.refresh();
            return Some((i, col));
        }
        self.join_prev(i + 1).map(|_| (i, col))
    }
}

/// Byte offset of char `col` in `s` (the end when past it).
pub fn byte_at(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(b, _)| b)
}

/// The char column of `line` under shown column `dcol` (the char whose cells hold it; the end of
/// the line when past it).
pub fn raw_col(line: &str, dcol: usize) -> usize {
    let mut w = 0;
    for (n, ch) in line.chars().enumerate() {
        w += if ch == '\t' { TAB - w % TAB } else { 1 };
        if w > dcol {
            return n;
        }
    }
    line.chars().count()
}

/// A change of a file: the lines `old` at `start` become `new`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub old: Vec<String>,
    pub new: Vec<String>,
}

/// Do `lines` still hold the `old` lines of every edit? `Err(i)` names the first edit that does not.
pub fn check_edits(lines: &[String], edits: &[Edit]) -> Result<(), usize> {
    for (n, e) in edits.iter().enumerate() {
        if lines.get(e.start..e.start + e.old.len()) != Some(e.old.as_slice()) {
            return Err(n);
        }
    }
    Ok(())
}

/// `edits` (ascending, not overlapping) applied to `text`, keeping the BOM, the line breaks and
/// whether the file ends with one. `Err(i)` when edit `i` no longer matches the text.
pub fn apply_text(text: &str, edits: &[Edit]) -> Result<String, usize> {
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(b) => ("\u{feff}", b),
        None => ("", text),
    };
    let lines: Vec<(&str, &str)> = body
        .split_inclusive('\n')
        .map(|l| match l.strip_suffix("\r\n") {
            Some(b) => (b, "\r\n"),
            None => l.strip_suffix('\n').map_or((l, ""), |b| (b, "\n")),
        })
        .collect();
    let eol = lines.iter().map(|l| l.1).find(|e| !e.is_empty()).unwrap_or("\n");
    let mut out = String::from(bom);
    let mut at = 0;
    for (n, e) in edits.iter().enumerate() {
        let end = e.start + e.old.len();
        if e.start < at
            || end > lines.len()
            || lines[e.start..end].iter().map(|l| l.0).ne(e.old.iter().map(String::as_str))
        {
            return Err(n);
        }
        for (b, t) in &lines[at..e.start] {
            out.push_str(b);
            out.push_str(t);
        }
        let last = lines.get(end.wrapping_sub(1)).filter(|_| end > 0 && !e.old.is_empty()).map_or(eol, |l| l.1);
        for (j, l) in e.new.iter().enumerate() {
            out.push_str(l);
            out.push_str(if j + 1 == e.new.len() { last } else { eol });
        }
        at = end;
    }
    for (b, t) in &lines[at..] {
        out.push_str(b);
        out.push_str(t);
    }
    Ok(out)
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

    /// The edited excerpts as changes of the file, ascending.
    pub fn edits(&self) -> Vec<Edit> {
        let mut v: Vec<Edit> = self
            .excerpts
            .iter()
            .filter(|e| e.is_dirty())
            .map(|e| Edit { start: e.orig_start, old: e.orig.clone(), new: e.raw.clone() })
            .collect();
        v.sort_by_key(|e| e.start);
        v
    }

    pub fn is_dirty(&self) -> bool {
        self.excerpts.iter().any(Excerpt::is_dirty)
    }

    /// The file now holds the edited lines: they are the new baseline.
    pub fn mark_saved(&mut self) {
        for e in &mut self.excerpts {
            e.orig = e.raw.clone();
            e.orig_start = e.start;
        }
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
            let raw = text[s..e].to_vec();
            excerpts.push(Excerpt { start: s, orig_start: s, orig: raw.clone(), raw, lines, marks, runs: None });
        }
        if !excerpts.is_empty() {
            groups.push(FileGroup { path: path.clone(), label: label(path), excerpts });
        }
    }
    groups
}

// ── the tab's state ──

/// An excerpt as it was before an edit, for undo and redo.
struct Snap {
    g: usize,
    e: usize,
    raw: Vec<String>,
    marks: Vec<Mark>,
    /// Line in the excerpt and char column of the caret.
    at: (usize, usize),
}

/// How many edits undo remembers.
const UNDO_MAX: usize = 500;

pub struct MultiBuf {
    pub kind: Kind,
    pub title: String,
    pub groups: Vec<FileGroup>,
    pub rows: Vec<Row>,
    /// Rows of lines with a match, and the first one of every excerpt (both ascending).
    pub hits: Vec<usize>,
    pub ex_first: Vec<usize>,
    /// The selected row (the caret is on it, at char column `col`, when it is a line).
    pub sel: usize,
    pub col: usize,
    /// Scroll offset in logical px.
    pub scroll: f64,
    /// The selection must be scrolled into view on the next draw.
    pub follow: bool,
    /// Where it was drawn last frame.
    pub rect: Rect,
    /// Where the code starts (px) and the width of a cell, as drawn last frame.
    pub code_x: i32,
    pub cell: f32,
    /// The highest line number shown (1-based), for the gutter width.
    pub max_line: usize,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// The line the last edit typed on (typing in one place is one undo step).
    typing: Option<(usize, usize, usize)>,
}

type Layout = (Vec<Row>, Vec<usize>, Vec<usize>, usize);

/// The flat rows of `groups`, the rows of matches, the first match row of every excerpt and the
/// highest line number.
fn layout(groups: &[FileGroup]) -> Layout {
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
    (rows, hits, ex_first, max_line)
}

impl MultiBuf {
    pub fn new(kind: Kind, title: String, groups: Vec<FileGroup>) -> Self {
        let (rows, hits, ex_first, max_line) = layout(&groups);
        let sel = hits.first().copied().unwrap_or(0);
        let mut mb = MultiBuf {
            kind,
            title,
            groups,
            rows,
            hits,
            ex_first,
            sel,
            col: 0,
            scroll: 0.0,
            follow: true,
            rect: Rect::new(0, 0, 0, 0),
            code_x: 0,
            cell: 0.0,
            max_line,
            undo: Vec::new(),
            redo: Vec::new(),
            typing: None,
        };
        mb.col = mb.mark_at(sel).map_or(0, |m| m.col).min(mb.line_len(sel));
        mb
    }

    fn reindex(&mut self) {
        (self.rows, self.hits, self.ex_first, self.max_line) = layout(&self.groups);
    }

    /// Any excerpt with edits that are not in its file yet.
    pub fn is_dirty(&self) -> bool {
        self.groups.iter().any(FileGroup::is_dirty)
    }

    /// Chars of the line a row shows (0 for other rows).
    pub fn line_len(&self, row: usize) -> usize {
        match self.rows.get(row) {
            Some(Row::Line { g, e, i }) => self.groups[*g].excerpts[*e].raw[*i].chars().count(),
            _ => 0,
        }
    }

    fn row_of(&self, g: usize, e: usize, i: usize) -> Option<usize> {
        self.rows.iter().position(|r| *r == Row::Line { g, e, i })
    }

    // ── editing ──

    /// The caret: group, excerpt, line in the excerpt, char column. `None` unless a line is selected.
    pub fn caret(&self) -> Option<(usize, usize, usize, usize)> {
        match self.rows.get(self.sel)? {
            Row::Line { g, e, i } => Some((*g, *e, *i, self.col.min(self.line_len(self.sel)))),
            _ => None,
        }
    }

    fn snap(&self, g: usize, e: usize, at: (usize, usize)) -> Snap {
        let ex = &self.groups[g].excerpts[e];
        Snap { g, e, raw: ex.raw.clone(), marks: ex.marks.clone(), at }
    }

    /// Put an excerpt back to `s`, moving the later excerpts of its file by the change in lines.
    fn restore(&mut self, s: &Snap) {
        let ex = &mut self.groups[s.g].excerpts[s.e];
        let delta = s.raw.len() as isize - ex.raw.len() as isize;
        ex.raw = s.raw.clone();
        ex.marks = s.marks.clone();
        ex.refresh();
        self.shift_after(s.g, s.e, delta);
        self.reindex();
        if let Some(r) = self.row_of(s.g, s.e, s.at.0) {
            self.sel = r;
            self.col = s.at.1;
            self.follow = true;
        }
        self.typing = None;
    }

    /// The excerpts after excerpt `e` of group `g` start `delta` lines further down.
    fn shift_after(&mut self, g: usize, e: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        for ex in &mut self.groups[g].excerpts[e + 1..] {
            ex.start = ex.start.saturating_add_signed(delta);
            for m in &mut ex.marks {
                m.line = m.line.saturating_add_signed(delta);
            }
        }
    }

    /// Run an edit `f` (line, col -> the caret after it, `None` when nothing changed) on the caret's
    /// excerpt: the lines below it move, the rows are rebuilt and the edit can be undone. Typing
    /// (`typing`) on one line is one undo step.
    fn edit(&mut self, typing: bool, f: impl FnOnce(&mut Excerpt, usize, usize) -> Option<(usize, usize)>) -> bool {
        let Some((g, e, i, col)) = self.caret() else { return false };
        let before = self.snap(g, e, (i, col));
        let old_len = self.groups[g].excerpts[e].raw.len();
        let Some((ni, ncol)) = f(&mut self.groups[g].excerpts[e], i, col) else { return false };
        let delta = self.groups[g].excerpts[e].raw.len() as isize - old_len as isize;
        let joined = typing && self.typing == Some((g, e, i));
        if !joined {
            self.undo.push(before);
            if self.undo.len() > UNDO_MAX {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.typing = typing.then_some((g, e, ni));
        self.shift_after(g, e, delta);
        self.reindex();
        if let Some(r) = self.row_of(g, e, ni) {
            self.sel = r;
            self.col = ncol;
            self.follow = true;
        }
        true
    }

    /// Type `s` (one line) at the caret.
    pub fn type_str(&mut self, s: &str) -> bool {
        if s.is_empty() {
            return false;
        }
        self.edit(true, |ex, i, col| Some((i, ex.insert_at(i, col, s))))
    }

    /// Enter: break the line at the caret.
    pub fn newline(&mut self) -> bool {
        self.edit(false, |ex, i, col| {
            ex.split_at(i, col);
            Some((i + 1, 0))
        })
    }

    pub fn backspace(&mut self) -> bool {
        self.edit(true, |ex, i, col| ex.backspace(i, col))
    }

    pub fn delete(&mut self) -> bool {
        self.edit(true, |ex, i, col| ex.delete(i, col))
    }

    /// The caret as (line in its excerpt, col).
    fn caret_pos(&self) -> (usize, usize) {
        self.caret().map_or((0, 0), |c| (c.2, c.3))
    }

    /// Undo the last edit. False when there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else { return false };
        let now = self.snap(s.g, s.e, self.caret_pos());
        self.restore(&s);
        self.redo.push(now);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else { return false };
        let now = self.snap(s.g, s.e, self.caret_pos());
        self.restore(&s);
        self.undo.push(now);
        true
    }

    // ── caret movement ──

    fn put(&mut self, row: usize, col: usize) {
        self.select(row);
        self.col = col.min(self.line_len(self.sel));
        self.typing = None;
    }

    /// Left / Right by one char, across the line breaks of an excerpt.
    pub fn step_col(&mut self, dir: i32) {
        let col = self.col.min(self.line_len(self.sel));
        if !matches!(self.rows.get(self.sel), Some(Row::Line { .. })) {
            return;
        }
        if dir < 0 {
            if col > 0 {
                self.put(self.sel, col - 1);
            } else if self.sel > 0 && matches!(self.rows[self.sel - 1], Row::Line { .. }) {
                self.put(self.sel - 1, usize::MAX);
            }
        } else if col < self.line_len(self.sel) {
            self.put(self.sel, col + 1);
        } else if matches!(self.rows.get(self.sel + 1), Some(Row::Line { .. })) {
            self.put(self.sel + 1, 0);
        }
    }

    /// Up / Down one row (skipping the gaps), keeping the column.
    pub fn step_row(&mut self, dir: i32) {
        let to = if dir > 0 {
            (self.sel + 1..self.rows.len()).find(|r| self.rows[*r] != Row::Gap)
        } else {
            (0..self.sel).rev().find(|r| self.rows[*r] != Row::Gap)
        };
        if let Some(r) = to {
            self.put(r, self.col);
        }
    }

    pub fn line_home(&mut self) {
        self.put(self.sel, 0);
    }

    pub fn line_end(&mut self) {
        self.put(self.sel, usize::MAX);
    }

    /// Put the caret on `row` under the shown column `dcol`.
    pub fn click_at(&mut self, row: usize, dcol: usize) {
        if !matches!(self.rows.get(row), Some(Row::File(_) | Row::Line { .. })) {
            return;
        }
        let col = match self.rows[row] {
            Row::Line { g, e, i } => raw_col(&self.groups[g].excerpts[e].raw[i], dcol),
            _ => 0,
        };
        self.put(row, col);
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
        mb.click_at(6, 0);
        assert_ne!(mb.sel, 6, "a gap is not selectable");
        mb.click_at(2, 0);
        assert_eq!(mb.sel, 2);
    }

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// One file of 100 lines with matches on line 2 and line 30 (two excerpts).
    fn two_excerpts() -> MultiBuf {
        MultiBuf::new(Kind::Search, "t".into(), groups(&[loc("a.rs", 2, 0), loc("a.rs", 30, 0)], 100))
    }

    #[test]
    fn typing_edits_the_raw_line_and_the_shown_one() {
        let mut mb = two_excerpts();
        assert_eq!(mb.caret(), Some((0, 0, 2, 0)));
        assert!(mb.type_str("\tab"));
        let ex = &mb.groups[0].excerpts[0];
        assert_eq!(ex.raw[2], "\tabline 2");
        assert_eq!(ex.lines[2], "    abline 2", "tabs shown as spaces");
        assert_eq!(mb.col, 3);
        assert!(ex.is_dirty() && mb.is_dirty());
        assert_eq!(mb.groups[0].edits().len(), 1);
        assert!(!mb.groups[0].excerpts[1].is_dirty());
    }

    #[test]
    fn breaking_and_joining_lines_moves_the_later_excerpts() {
        let mut mb = two_excerpts();
        let (s0, s1) = (mb.groups[0].excerpts[0].start, mb.groups[0].excerpts[1].start);
        mb.step_col(1);
        mb.step_col(1);
        assert!(mb.newline());
        let g = &mb.groups[0];
        assert_eq!(g.excerpts[0].raw[2], "li");
        assert_eq!(g.excerpts[0].raw[3], "ne 2");
        assert_eq!(g.excerpts[0].start, s0);
        assert_eq!(g.excerpts[1].start, s1 + 1, "the next excerpt is one line further down");
        assert_eq!(g.excerpts[1].marks[0].line, 30 + 1);
        assert_eq!(g.excerpts[1].orig_start, s1, "the file still has it where it was read");
        assert_eq!(mb.caret(), Some((0, 0, 3, 0)));
        assert_eq!(mb.rows.len(), 1 + 6 + 1 + 5);
        // Backspace at the start of the new line joins it back.
        assert!(mb.backspace());
        assert_eq!(mb.groups[0].excerpts[0].raw[2], "line 2");
        assert_eq!(mb.groups[0].excerpts[1].start, s1);
        assert_eq!(mb.caret(), Some((0, 0, 2, 2)));
        assert!(!mb.groups[0].is_dirty(), "back to the original lines");
    }

    #[test]
    fn nothing_happens_at_the_edges_of_an_excerpt_or_on_a_header() {
        let mut mb = two_excerpts();
        mb.click_at(1, 0);
        assert_eq!(mb.caret(), Some((0, 0, 0, 0)));
        assert!(!mb.backspace(), "no line before the first one");
        assert!(!mb.undo());
        mb.click_at(0, 0);
        assert_eq!(mb.caret(), None);
        assert!(!mb.type_str("x"));
        assert!(!mb.newline());
        let last = mb.rows.len() - 1;
        mb.click_at(last, 99);
        assert!(!mb.delete(), "no line after the last one");
    }

    #[test]
    fn undo_and_redo_restore_the_lines_and_the_starts_below() {
        let mut mb = two_excerpts();
        let s1 = mb.groups[0].excerpts[1].start;
        mb.type_str("a");
        mb.type_str("b");
        mb.newline();
        assert_eq!(mb.groups[0].excerpts[1].start, s1 + 1);
        assert!(mb.undo(), "the line break");
        assert_eq!(mb.groups[0].excerpts[1].start, s1);
        assert_eq!(mb.groups[0].excerpts[0].raw[2], "abline 2");
        assert!(mb.undo(), "typing on one line is one step");
        assert_eq!(mb.groups[0].excerpts[0].raw[2], "line 2");
        assert!(!mb.is_dirty());
        assert!(!mb.undo());
        assert!(mb.redo());
        assert_eq!(mb.groups[0].excerpts[0].raw[2], "abline 2");
        assert!(mb.redo());
        assert_eq!(mb.groups[0].excerpts[1].start, s1 + 1);
        assert!(!mb.redo());
        mb.undo();
        mb.type_str("z");
        assert!(!mb.redo(), "a new edit drops the redo steps");
    }

    #[test]
    fn the_caret_walks_lines_and_clicks_land_on_chars() {
        let mut mb = two_excerpts();
        mb.line_end();
        assert_eq!(mb.col, 6);
        mb.step_col(1);
        assert_eq!(mb.line_no(mb.sel), Some(3), "past the end goes to the next line");
        assert_eq!(mb.col, 0);
        mb.step_col(-1);
        assert_eq!((mb.line_no(mb.sel), mb.col), (Some(2), 6));
        mb.line_home();
        mb.step_row(-1);
        assert_eq!(mb.line_no(mb.sel), Some(1));
        let last = mb.rows.len() - 1;
        mb.click_at(last, 3);
        mb.step_row(-1);
        assert_ne!(mb.rows[mb.sel], Row::Gap);
        assert_eq!(raw_col("\tfoo", 4), 1);
        assert_eq!(raw_col("\tfoo", 1), 0, "inside the tab: the tab");
        assert_eq!(raw_col("abc", 99), 3);
    }

    #[test]
    fn an_edit_is_checked_against_the_old_lines_and_applied_to_the_text() {
        let e = Edit { start: 1, old: lines(&["b", "c"]), new: lines(&["B", "x", "y", "C"]) };
        let file = lines(&["a", "b", "c", "d"]);
        assert_eq!(check_edits(&file, std::slice::from_ref(&e)), Ok(()));
        assert_eq!(check_edits(&lines(&["a", "b", "z", "d"]), std::slice::from_ref(&e)), Err(0));
        assert_eq!(check_edits(&lines(&["a", "b"]), std::slice::from_ref(&e)), Err(0), "past the end");
        assert_eq!(apply_text("a\nb\nc\nd\n", std::slice::from_ref(&e)).unwrap(), "a\nB\nx\ny\nC\nd\n");
        assert_eq!(apply_text("a\r\nb\r\nc\r\nd", std::slice::from_ref(&e)).unwrap(), "a\r\nB\r\nx\r\ny\r\nC\r\nd");
        assert_eq!(
            apply_text("\u{feff}a\nb\nc", std::slice::from_ref(&e)).unwrap(),
            "\u{feff}a\nB\nx\ny\nC",
            "BOM and a missing last break stay"
        );
        assert_eq!(apply_text("a\nb\nq\n", std::slice::from_ref(&e)), Err(0));
    }

    #[test]
    fn several_edits_apply_in_order_and_overlaps_are_refused() {
        let es = [
            Edit { start: 0, old: lines(&["a"]), new: lines(&["a", "a2"]) },
            Edit { start: 3, old: lines(&["d"]), new: lines(&["D"]) },
        ];
        assert_eq!(apply_text("a\nb\nc\nd\ne", &es).unwrap(), "a\na2\nb\nc\nD\ne");
        let bad = [
            Edit { start: 1, old: lines(&["b", "c"]), new: lines(&["x"]) },
            Edit { start: 2, old: lines(&["c"]), new: lines(&["y"]) },
        ];
        assert_eq!(apply_text("a\nb\nc\n", &bad), Err(1));
    }

    #[test]
    fn saved_excerpts_become_the_new_baseline() {
        let mut mb = two_excerpts();
        mb.newline();
        let g = &mb.groups[0];
        let edits = g.edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start, 0);
        assert_eq!(edits[0].old.len(), 5);
        assert_eq!(edits[0].new.len(), 6);
        mb.groups[0].mark_saved();
        assert!(!mb.is_dirty());
        assert_eq!(mb.groups[0].excerpts[1].orig_start, mb.groups[0].excerpts[1].start);
        // Undo after a save is an edit against the saved text.
        assert!(mb.undo());
        assert!(mb.is_dirty());
        assert_eq!(mb.groups[0].edits()[0].new.len(), 5);
    }

    #[test]
    fn marks_follow_their_line_when_a_line_is_broken() {
        let mut mb = MultiBuf::new(Kind::Search, "t".into(), groups(&[loc("a.rs", 2, 3), loc("a.rs", 3, 1)], 100));
        // One excerpt; the caret starts on the first match (line 2, col 3): break the line there.
        assert_eq!(mb.caret(), Some((0, 0, 2, 3)));
        mb.newline();
        let m = &mb.groups[0].excerpts[0].marks;
        assert_eq!((m[0].line, m[0].col), (3, 0), "the match went down with the rest of its line");
        assert_eq!((m[1].line, m[1].col), (4, 1));
        assert!(mb.backspace());
        let m = &mb.groups[0].excerpts[0].marks;
        assert_eq!((m[0].line, m[0].col, m[1].line), (2, 3, 3));
    }
}
