//! Data/math layer for LSP inlay hints (virtual text drawn inline). No drawing here.
//!
//! Conventions
//! * A hint at char column `c` sits visually *between* char `c-1` and char `c`. A hint whose
//!   column is past the end of the line is clamped to the line end.
//! * Tabs expand from the *text-only* visual column (hints do not move tab stops), exactly like
//!   `editor::visual_col` / `doc::window_pieces`. Hint cells are added on top.
//! * `include_at`: whether hints located exactly AT the queried column count. Use `true` for the
//!   position of the glyph at that column (text starts after the hint) and `false` for a caret
//!   that should sit *before* the hint (e.g. the caret at `name|: number` before the hint).
//! * Hint widths are in cells: the label is drawn in a smaller font (`HINT_SCALE` of the code
//!   size) so it needs `label_cells(chars)` = ceil(chars * HINT_SCALE) cells, plus one cell per
//!   padding. The width is therefore a per-hint value (not the char count) and every conversion
//!   (`vcol`, `col_from_vcol`, `hint_cells`, `window_pieces_hinted`) uses it, so the cursor,
//!   selections and clicks stay exact on the cell grid. Labels never contain tabs/newlines.
//! * Wrapped rows: `row_hints` slices a line's hints for one visual row (columns relative to the
//!   row start); all the `*_in` functions work on such a slice.
#![allow(dead_code)]

use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    Type,
    Parameter,
    Other,
}

/// Font size of hint labels relative to the code font (cells are still the code font's).
pub const HINT_SCALE: f32 = 0.85;

/// Cells needed by `chars` label characters drawn at [`HINT_SCALE`] (rounded up to whole cells).
pub fn label_cells(chars: usize) -> usize {
    (chars * 85).div_ceil(100)
}

/// A clickable stretch of a hint label (an LSP label part with a `location`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintLink {
    /// Char range `[start, end)` inside the label.
    pub start: usize,
    pub end: usize,
    /// Target (LSP position: UTF-16 character).
    pub uri: String,
    pub line: usize,
    pub character: usize,
}

/// A text edit that comes with a hint (`textEdits`): applied on double click. Positions are char
/// columns; lines are relative to the hint's line, and so are columns on the hint's own line
/// (relative to the hint's column), which keeps the edit valid when the hint is shifted by edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintEdit {
    pub dl0: i64,
    pub c0: i64,
    pub dl1: i64,
    pub c1: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub line: usize,
    /// Char column in the line (already converted from UTF-16 by the caller).
    pub col: usize,
    pub label: String,
    pub kind: HintKind,
    pub pad_left: bool,
    pub pad_right: bool,
    /// Clickable label parts (Ctrl+click goes to `location`).
    pub links: Vec<HintLink>,
    /// Edits from the server (double click applies them as one undo step).
    pub edits: Vec<HintEdit>,
}

impl Hint {
    /// Cells taken by the label itself (smaller font, rounded up).
    pub fn label_width(&self) -> usize {
        label_cells(self.label.chars().count())
    }

    /// Displayed width in cells (label + paddings).
    pub fn width(&self) -> usize {
        self.label_width() + self.pad_left as usize + self.pad_right as usize
    }

    /// The link under label char `off`, if any.
    pub fn link_at(&self, off: usize) -> Option<&HintLink> {
        self.links.iter().find(|l| off >= l.start && off < l.end)
    }

    /// The label char under `px` pixels from the start of the hint (cell width `cw`), or `None`
    /// over the paddings / past the label. The label is centred in its whole-cell slot.
    pub fn char_at_px(&self, px: f32, cw: f32) -> Option<usize> {
        let n = self.label.chars().count();
        let slack = (self.label_width() as f32 * cw - n as f32 * cw * HINT_SCALE) / 2.0;
        let x = px - self.pad_left as usize as f32 * cw - slack;
        if x < 0.0 {
            return None;
        }
        let i = (x / (cw * HINT_SCALE)) as usize;
        (i < n).then_some(i)
    }

    /// The edits to apply on double click: the server's, or for a type hint (`: T`) without any,
    /// inserting the label itself at the hint position (what editors do for "add type annotation").
    pub fn annotation_edits(&self) -> Vec<HintEdit> {
        if !self.edits.is_empty() {
            return self.edits.clone();
        }
        if self.kind == HintKind::Type && self.label.starts_with(':') {
            return vec![HintEdit { dl0: 0, c0: 0, dl1: 0, c1: 0, text: self.label.clone() }];
        }
        Vec::new()
    }

    /// The displayed string (paddings as spaces).
    pub fn text(&self) -> String {
        let mut s = String::new();
        if self.pad_left {
            s.push(' ');
        }
        s.push_str(&self.label);
        if self.pad_right {
            s.push(' ');
        }
        s
    }
}

/// Parse a `textDocument/inlayHint` result. `to_char_col(line, utf16_character)` converts the
/// LSP column to a char column. Hints with an empty label are dropped; control chars in labels
/// (tabs, newlines) become spaces. Label parts with a `location` become [`HintLink`]s and
/// `textEdits` become [`HintEdit`]s.
pub fn parse(v: &Value, to_char_col: &mut dyn FnMut(usize, usize) -> usize) -> Vec<Hint> {
    let Some(arr) = v.as_array() else { return Vec::new() };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let pos = &item["position"];
        let (Some(line), Some(ch)) = (pos["line"].as_u64(), pos["character"].as_u64()) else { continue };
        let clean = |s: &str| -> String { s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect() };
        let mut label = String::new();
        let mut links = Vec::new();
        match &item["label"] {
            Value::String(s) => label = clean(s),
            Value::Array(parts) => {
                for p in parts {
                    let Some(val) = p["value"].as_str() else { continue };
                    let start = label.chars().count();
                    label.push_str(&clean(val));
                    let end = label.chars().count();
                    let loc = &p["location"];
                    if let (Some(uri), Some(l), Some(c)) = (loc["uri"].as_str(), loc["range"]["start"]["line"].as_u64(), loc["range"]["start"]["character"].as_u64()) {
                        if end > start {
                            links.push(HintLink { start, end, uri: uri.to_string(), line: l as usize, character: c as usize });
                        }
                    }
                }
            }
            _ => continue,
        }
        if label.is_empty() {
            continue;
        }
        let kind = match item["kind"].as_u64() {
            Some(1) => HintKind::Type,
            Some(2) => HintKind::Parameter,
            _ => HintKind::Other,
        };
        let line = line as usize;
        let col = to_char_col(line, ch as usize);
        let mut edits = Vec::new();
        for e in item["textEdits"].as_array().into_iter().flatten() {
            let r = &e["range"];
            let (Some(sl), Some(sc), Some(el), Some(ec), Some(text)) =
                (r["start"]["line"].as_u64(), r["start"]["character"].as_u64(), r["end"]["line"].as_u64(), r["end"]["character"].as_u64(), e["newText"].as_str())
            else {
                continue;
            };
            let (sl, el) = (sl as usize, el as usize);
            let (sc, ec) = (to_char_col(sl, sc as usize), to_char_col(el, ec as usize));
            let rel = |l: usize, c: usize| -> (i64, i64) {
                let dl = l as i64 - line as i64;
                (dl, if dl == 0 { c as i64 - col as i64 } else { c as i64 })
            };
            let ((dl0, c0), (dl1, c1)) = (rel(sl, sc), rel(el, ec));
            edits.push(HintEdit { dl0, c0, dl1, c1, text: text.to_string() });
        }
        out.push(Hint {
            line,
            col,
            label,
            kind,
            pad_left: item["paddingLeft"].as_bool().unwrap_or(false),
            pad_right: item["paddingRight"].as_bool().unwrap_or(false),
            links,
            edits,
        });
    }
    out
}

/// Lines to request for a viewport: `first..=last` padded by [`REQUEST_PAD`] lines and clamped to
/// the document. Returns inclusive `(first_line, last_line)`; `(0, 0)` for an empty document.
pub fn visible_range_request(first_line: usize, last_line: usize, line_count: usize) -> (usize, usize) {
    if line_count == 0 {
        return (0, 0);
    }
    let max = line_count - 1;
    let first = first_line.saturating_sub(REQUEST_PAD).min(max);
    let last = last_line.max(first_line).saturating_add(REQUEST_PAD).min(max);
    (first, last)
}

/// Lines requested above and below the viewport (so a fast scroll mostly lands on known hints).
pub const REQUEST_PAD: usize = 40;

/// Tab-aware visual column of char column `col` (same semantics as `editor::visual_col`).
fn visual_col(text: &str, col: usize, tab_w: usize) -> usize {
    let mut v = 0;
    for ch in text.chars().take(col) {
        v += if ch == '\t' { tab_w - v % tab_w } else { 1 };
    }
    v
}

/// Visual column (tabs + virtual hint cells) of char column `col` in `text` given the hints of
/// that text (sorted by column, columns relative to `text`). See the module docs for `include_at`.
pub fn vcol_in(hints: &[Hint], text: &str, col: usize, tab_w: usize, include_at: bool) -> usize {
    if hints.is_empty() {
        return visual_col(text, col, tab_w);
    }
    let len = text.chars().count();
    let col = col.min(len);
    let extra: usize = hints
        .iter()
        .take_while(|h| {
            let c = h.col.min(len);
            c < col || (include_at && c == col)
        })
        .map(|h| h.width())
        .sum();
    visual_col(text, col, tab_w) + extra
}

/// Where a pointer at visual column `vcol` is on `text`: the char column it maps to and, when it
/// is over a hint, `(index into hints, visual column where that hint starts)`. The returned
/// column over a hint is the hint's own column (clamped to the text end). Past the end of the
/// text gives its length. Over text it rounds like `editor::col_from_visual`.
pub fn hit_in(hints: &[Hint], text: &str, vcol: usize, tab_w: usize) -> (usize, Option<(usize, usize)>) {
    let len = text.chars().count();
    let mut hi = 0;
    let mut pos = 0; // total visual position
    let mut tvc = 0; // text-only visual column (tab stops)
    let mut chars = text.chars();
    for i in 0..=len {
        while hi < hints.len() && hints[hi].col.min(len) <= i {
            let w = hints[hi].width();
            if vcol < pos + w {
                return (i, Some((hi, pos)));
            }
            pos += w;
            hi += 1;
        }
        let Some(ch) = chars.next() else { break };
        let w = if ch == '\t' { tab_w - tvc % tab_w } else { 1 };
        if vcol < pos + w {
            let c = if vcol - pos >= w.div_ceil(2) && w > 1 { i + 1 } else { i };
            return (c, None);
        }
        pos += w;
        tvc += w;
    }
    (len, None)
}

/// The hints of one visual row of a wrapped line: row chars `[c0, c1)` (`last` = the final row,
/// which also owns the hints at the line end). `idx` are the indexes into the line's hints,
/// `hints` the same hints with columns relative to `c0` (and clamped to the line length `len`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowHints {
    pub idx: Vec<usize>,
    pub hints: Vec<Hint>,
}

pub fn row_hints(line_hints: &[Hint], len: usize, c0: usize, c1: usize, last: bool) -> RowHints {
    let mut out = RowHints::default();
    for (i, h) in line_hints.iter().enumerate() {
        let c = h.col.min(len);
        if c >= c0 && (c < c1 || (last && c == c1)) {
            out.idx.push(i);
            out.hints.push(Hint { col: c - c0, ..h.clone() });
        }
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct LineHints {
    by_line: HashMap<usize, Vec<Hint>>,
}

impl From<Vec<Hint>> for LineHints {
    fn from(hints: Vec<Hint>) -> Self {
        let mut by_line: HashMap<usize, Vec<Hint>> = HashMap::new();
        for h in hints {
            by_line.entry(h.line).or_default().push(h);
        }
        for v in by_line.values_mut() {
            v.sort_by_key(|h| h.col); // stable: LSP order kept among equal columns
        }
        LineHints { by_line }
    }
}

impl LineHints {
    pub fn is_empty(&self) -> bool {
        self.by_line.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_line.values().map(|v| v.len()).sum()
    }

    /// Hints of `line`, sorted by column.
    pub fn hints_on(&self, line: usize) -> &[Hint] {
        self.by_line.get(&line).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn width_cells(&self, h: &Hint) -> usize {
        h.width()
    }

    /// Virtual cells inserted at columns strictly before `col`, plus those AT `col` if `include_at`.
    pub fn extra_before(&self, line: usize, col: usize, include_at: bool) -> usize {
        self.hints_on(line)
            .iter()
            .take_while(|h| h.col < col || (include_at && h.col == col))
            .map(|h| h.width())
            .sum()
    }

    /// Visual column (tabs + virtual hint cells) of char column `col` in `text`.
    pub fn vcol(&self, line: usize, text: &str, col: usize, tab_w: usize, include_at: bool) -> usize {
        vcol_in(self.hints_on(line), text, col, tab_w, include_at)
    }

    /// Inverse of [`vcol`]: buffer char column for visual column `vcol`, and `Some(index into
    /// hints_on(line))` when the pointer is over a hint (the returned column is then the hint's
    /// own column, clamped to the line end). Past the end of the line gives the line length.
    /// Over text it rounds like `editor::col_from_visual`.
    pub fn col_from_vcol(&self, line: usize, text: &str, vcol: usize, tab_w: usize) -> (usize, Option<usize>) {
        let (c, h) = hit_in(self.hints_on(line), text, vcol, tab_w);
        (c, h.map(|(i, _)| i))
    }
}

// ───────────────────────────── editor integration ─────────────────────────────

impl LineHints {
    /// Replace every hint on lines `first..=last` with `new` (the answer for that range) and drop
    /// hints farther than 400 lines from it (they would be stale by the time they scroll back).
    pub fn replace_range(&mut self, first: usize, last: usize, new: Vec<Hint>) {
        let (lo, hi) = (first.saturating_sub(400), last.saturating_add(400));
        self.by_line.retain(|&l, _| l >= lo && l <= hi && !(l >= first && l <= last));
        let mut merged = std::mem::take(self);
        for h in new {
            if h.line >= first && h.line <= last {
                merged.by_line.entry(h.line).or_default().push(h);
            }
        }
        for v in merged.by_line.values_mut() {
            v.sort_by_key(|h| h.col);
        }
        *self = merged;
    }

    /// An edit starting on `line` changed the line count by `delta`: hints on the edited line are
    /// dropped (their columns are unknown now), later ones follow their text, hints of deleted
    /// lines vanish.
    pub fn invalidate_edit(&mut self, line: usize, delta: isize) {
        let old = std::mem::take(&mut self.by_line);
        for (l, v) in old {
            if l < line {
                self.by_line.insert(l, v);
            } else if l > line {
                let n = l as isize + delta;
                if n > line as isize {
                    let n = n as usize;
                    let v: Vec<Hint> = v.into_iter().map(|h| Hint { line: n, ..h }).collect();
                    self.by_line.insert(n, v);
                }
            }
        }
    }

    /// Follow one text edit: `removed` chars at (`line`, `col`) were replaced by `inserted` chars,
    /// the removed text spanned `rem_lines` line breaks and the inserted one `ins_lines`.
    /// `end_col` is the column where the inserted text ends on its last line (`None` = unknown,
    /// only needed when `ins_lines > 0`). Hints before the edit stay, hints after it on the same
    /// line move with their text (also across an inserted line break), hints inside the removed
    /// text vanish, later lines shift. A hint exactly at an insertion point moves after it.
    pub fn apply_edit(&mut self, line: usize, col: usize, removed: usize, inserted: usize, rem_lines: usize, ins_lines: usize, end_col: Option<usize>) {
        let delta = ins_lines as isize - rem_lines as isize;
        let end_col = if ins_lines == 0 { Some(col + inserted) } else { end_col };
        let old = std::mem::take(&mut self.by_line);
        let mut moved: Vec<Hint> = Vec::new();
        for (l, v) in old {
            if l < line {
                self.by_line.insert(l, v);
            } else if l == line {
                let mut stay = Vec::new();
                for h in v {
                    if h.col < col || (h.col == col && removed > 0) {
                        stay.push(h);
                    } else if rem_lines == 0 && h.col >= col + removed {
                        if let Some(e) = end_col {
                            moved.push(Hint { line: line + ins_lines, col: e + (h.col - col - removed), ..h });
                        }
                    }
                }
                if !stay.is_empty() {
                    self.by_line.insert(l, stay);
                }
            } else if l <= line + rem_lines {
                // tail of a removed line break: gone
            } else {
                let n = (l as isize + delta) as usize;
                let v: Vec<Hint> = v.into_iter().map(|h| Hint { line: n, ..h }).collect();
                self.by_line.insert(n, v);
            }
        }
        if !moved.is_empty() {
            let e = self.by_line.entry(line + ins_lines).or_default();
            e.extend(moved);
            e.sort_by_key(|h| h.col);
        }
    }

    /// Lines `first..first + n` were exchanged / rewritten wholesale: forget their hints.
    pub fn drop_lines(&mut self, first: usize, n: usize) {
        self.by_line.retain(|&l, _| l < first || l >= first + n);
    }
}

/// Per-document inlay state: the hints plus the bookkeeping for asking the server again.
#[derive(Debug, Default)]
pub struct InlayState {
    pub hints: LineHints,
    /// When to ask again (set by edits; the tick debounces on top of the server sync).
    pub due: Option<std::time::Instant>,
    /// Inclusive line range the last request covered (padded around the viewport).
    pub cover: Option<(usize, usize)>,
    /// Line count when `hints` was last adjusted (to follow inserted/removed lines).
    lines: usize,
    /// When the last request went out (throttles the urgent ones during a fast scroll).
    last_request: Option<std::time::Instant>,
}

impl InlayState {
    /// Text changed (first touched line `line`, new line count `line_count`) and nothing is known
    /// about where: hints of the touched line are dropped, later ones follow their lines.
    pub fn on_edit(&mut self, line: usize, line_count: usize) {
        if !self.hints.is_empty() {
            let delta = if self.lines == 0 { 0 } else { line_count as isize - self.lines as isize };
            self.hints.invalidate_edit(line, delta);
        }
        self.lines = line_count;
        self.due = Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
    }

    /// Text changed by the recorded edits `recs` (in order); `end_of(pos)` gives the line and
    /// column of a char position in the *current* text (valid for the last record only). The
    /// hints follow their text, so nothing flashes while typing; the server's answer (asked for
    /// after a pause) replaces them.
    pub fn on_edits(&mut self, recs: &[crate::buffer::EditRec], first_line: usize, line_count: usize, end_of: &dyn Fn(usize) -> (usize, usize)) {
        use crate::buffer::EditShape;
        if recs.is_empty() {
            return self.on_edit(first_line, line_count);
        }
        if !self.hints.is_empty() {
            for (k, r) in recs.iter().enumerate() {
                if let EditShape::Swap { a_lines, b_lines, .. } = r.shape {
                    self.hints.drop_lines(r.line, a_lines + b_lines + 1);
                    continue;
                }
                let end_col = (k + 1 == recs.len() && r.ins_lines > 0).then(|| end_of(r.at + r.inserted).1);
                self.hints.apply_edit(r.line, r.col, r.removed, r.inserted, r.rem_lines, r.ins_lines, end_col);
            }
        }
        self.lines = line_count;
        self.due = Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
    }

    pub fn mark_requested(&mut self) {
        self.last_request = Some(std::time::Instant::now());
    }

    pub fn since_last_request(&self) -> Option<std::time::Duration> {
        self.last_request.map(|t| t.elapsed())
    }

    /// Forget everything (feature switched off, file reloaded).
    pub fn reset(&mut self) {
        self.hints = LineHints::default();
        self.due = None;
        self.cover = None;
    }

    /// A response for lines `first..=last` arrived while the document has `line_count` lines.
    pub fn apply(&mut self, first: usize, last: usize, hints: Vec<Hint>, line_count: usize) {
        self.hints.replace_range(first, last, hints);
        self.lines = line_count;
    }

    /// Does the viewport `vf..=vl` need a new request? True when the last covered range is
    /// missing or no longer includes it (with a margin so it refreshes before the edge shows).
    pub fn needs_request(&self, vf: usize, vl: usize, line_count: usize) -> bool {
        let Some((a, b)) = self.cover else { return true };
        let margin = 8;
        let lo_ok = a == 0 || vf >= a + margin;
        let hi_ok = b + 1 >= line_count || vl + margin <= b;
        !(lo_ok && hi_ok && vf >= a && vl <= b)
    }

    /// Is the viewport itself (not just its margin) outside the last covered range? Then the
    /// request goes out at once instead of waiting for the scroll to settle.
    pub fn viewport_uncovered(&self, vf: usize, vl: usize, line_count: usize) -> bool {
        let Some((a, b)) = self.cover else { return true };
        !((a == 0 || vf >= a) && (b + 1 >= line_count || vl <= b))
    }
}

/// Convert an LSP UTF-16 column on `line_text` to a char column.
pub fn utf16_to_char_col(line_text: &str, u16col: usize) -> usize {
    let mut n = 0;
    let mut col = 0;
    for ch in line_text.chars() {
        if n >= u16col {
            break;
        }
        n += ch.len_utf16();
        col += 1;
    }
    col
}

/// Visual start column and width in cells of each hint's label pill (paddings excluded: they stay
/// as a gap) of a line or row, in the order of `hints`.
pub fn hint_cells(text: &str, hints: &[Hint], tab_w: usize) -> Vec<(usize, usize)> {
    let len = text.chars().count();
    let mut extra = 0;
    hints
        .iter()
        .map(|h| {
            let start = visual_col(text, h.col.min(len), tab_w) + extra + h.pad_left as usize;
            extra += h.width();
            (start, h.label_width())
        })
        .collect()
}

/// Like `doc::window_pieces` but with the row's hints reserved: pieces `(text, color, flags)` for
/// the row `line[b0..b1]` (`spans` hold byte offsets of the whole `line`) over visual columns
/// `[vc0, vc1)` counted from the row start, hint cells included. Each hint's cells are blank (the
/// labels are drawn separately, in a smaller font, over them). `hints` are the row's hints with
/// columns relative to the row start (see [`row_hints`]), sorted by column.
pub fn window_pieces_hinted(
    line: &str,
    spans: Option<&crate::highlight::Spans>,
    b0: usize,
    b1: usize,
    hints: &[Hint],
    vc0: usize,
    vc1: usize,
    tab_w: usize,
    fg: crate::theme::Rgba,
    out: &mut Vec<(String, crate::theme::Rgba, u8)>,
) {
    fn put(out: &mut Vec<(String, crate::theme::Rgba, u8)>, ch: char, c: crate::theme::Rgba, f: u8) {
        match out.last_mut() {
            Some((s, lc, lf)) if *lc == c && *lf == f && s.as_bytes().last() != Some(&b'\n') => s.push(ch),
            _ => out.push((ch.to_string(), c, f)),
        }
    }
    let row = &line[b0..b1];
    let len = row.chars().count();
    let mut pos = 0usize;
    let mut tvc = 0usize;
    let mut hi = 0;
    let mut si = 0;
    let mut chars = row.char_indices();
    for i in 0..=len {
        while hi < hints.len() && hints[hi].col.min(len) <= i {
            for _ in 0..hints[hi].width() {
                if pos >= vc0 && pos < vc1 {
                    put(out, ' ', fg, 0);
                }
                pos += 1;
            }
            hi += 1;
        }
        if pos >= vc1 {
            break;
        }
        let Some((b, ch)) = chars.next() else { break };
        let b = b + b0;
        let (color, flags) = match spans {
            Some(sp) => {
                while si < sp.len() && sp[si].1 <= b {
                    si += 1;
                }
                match sp.get(si) {
                    Some(&(s, _, c, f)) if s <= b => (c, f),
                    _ => (fg, 0),
                }
            }
            None => (fg, 0),
        };
        if ch == '\t' {
            let n = tab_w - tvc % tab_w;
            for _ in 0..n {
                if pos >= vc0 && pos < vc1 {
                    put(out, ' ', color, flags);
                }
                pos += 1;
            }
            tvc += n;
        } else {
            if pos >= vc0 && pos < vc1 {
                put(out, ch, color, flags);
            }
            pos += 1;
            tvc += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    fn h(line: usize, col: usize, label: &str, l: bool, r: bool) -> Hint {
        Hint { line, col, label: label.into(), kind: HintKind::Type, pad_left: l, pad_right: r, links: Vec::new(), edits: Vec::new() }
    }

    fn ident(_l: usize, c: usize) -> usize {
        c
    }

    #[test]
    fn parse_string_label_and_kinds() {
        let v = json!([
            {"position": {"line": 1, "character": 5}, "label": ": number", "kind": 1},
            {"position": {"line": 2, "character": 0}, "label": "name:", "kind": 2, "paddingRight": true},
            {"position": {"line": 3, "character": 1}, "label": "x"},
        ]);
        let hs = parse(&v, &mut ident);
        assert_eq!(hs.len(), 3);
        assert_eq!((hs[0].line, hs[0].col, hs[0].kind), (1, 5, HintKind::Type));
        assert_eq!(hs[0].label, ": number");
        assert_eq!(hs[1].kind, HintKind::Parameter);
        assert!(hs[1].pad_right && !hs[1].pad_left);
        assert_eq!(hs[2].kind, HintKind::Other);
    }

    #[test]
    fn parse_label_parts() {
        let v = json!([{
            "position": {"line": 0, "character": 3},
            "label": [{"value": ": "}, {"value": "Foo", "location": {}}, {"value": "<T>"}],
            "kind": 1, "paddingLeft": true
        }]);
        let hs = parse(&v, &mut ident);
        assert_eq!(hs[0].label, ": Foo<T>");
        assert!(hs[0].pad_left);
    }

    #[test]
    fn parse_uses_utf16_callback() {
        // "😀a" : utf16 col 3 == char col 2.
        let v = json!([{"position": {"line": 4, "character": 3}, "label": "t"}]);
        let mut seen = Vec::new();
        let hs = parse(&v, &mut |l, c| {
            seen.push((l, c));
            c - 1
        });
        assert_eq!(seen, vec![(4, 3)]);
        assert_eq!(hs[0].col, 2);
    }

    #[test]
    fn parse_garbage_is_tolerated() {
        assert!(parse(&Value::Null, &mut ident).is_empty());
        assert!(parse(&json!({"a": 1}), &mut ident).is_empty());
        let v = json!([
            {"label": "no position"},
            {"position": {"line": 0, "character": 0}, "label": ""},
            {"position": {"line": 0, "character": 0}, "label": 5},
            {"position": {"line": 0, "character": 0}, "label": "ok", "kind": 9}
        ]);
        let hs = parse(&v, &mut ident);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].kind, HintKind::Other);
    }

    #[test]
    fn parse_sanitizes_control_chars() {
        let v = json!([{"position": {"line": 0, "character": 0}, "label": "a\tb\nc"}]);
        assert_eq!(parse(&v, &mut ident)[0].label, "a b c");
    }

    #[test]
    fn width_with_paddings() {
        let lh = LineHints::default();
        assert_eq!(lh.width_cells(&h(0, 0, "ab", false, false)), 2);
        assert_eq!(lh.width_cells(&h(0, 0, "ab", true, false)), 3);
        assert_eq!(lh.width_cells(&h(0, 0, "ab", true, true)), 4);
        assert_eq!(lh.width_cells(&h(0, 0, "日本", false, true)), 3);
        assert_eq!(lh.width_cells(&h(0, 0, ": number", true, true)), 9, "8 chars at 85% = 7 cells + paddings");
    }

    #[test]
    fn line_hints_sorted_and_grouped() {
        let lh = LineHints::from(vec![h(1, 9, "c", false, false), h(0, 3, "z", false, false), h(1, 2, "a", false, false), h(1, 2, "b", false, false)]);
        assert_eq!(lh.len(), 4);
        let l1: Vec<_> = lh.hints_on(1).iter().map(|x| x.label.as_str()).collect();
        assert_eq!(l1, vec!["a", "b", "c"]); // stable among equal columns
        assert!(lh.hints_on(7).is_empty());
        assert!(LineHints::default().is_empty());
    }

    #[test]
    fn extra_before_include_at() {
        let lh = LineHints::from(vec![h(0, 2, ": T", false, false), h(0, 5, "x:", false, true)]);
        assert_eq!(lh.extra_before(0, 2, false), 0);
        assert_eq!(lh.extra_before(0, 2, true), 3);
        assert_eq!(lh.extra_before(0, 3, false), 3);
        assert_eq!(lh.extra_before(0, 5, false), 3);
        assert_eq!(lh.extra_before(0, 5, true), 6);
        assert_eq!(lh.extra_before(0, 99, false), 6);
        assert_eq!(lh.extra_before(1, 99, true), 0);
    }

    #[test]
    fn vcol_basic_and_tabs() {
        let lh = LineHints::from(vec![h(0, 3, ": T", false, false)]);
        // "let x = 1"; hint after "let"
        assert_eq!(lh.vcol(0, "let x = 1", 2, 4, true), 2);
        assert_eq!(lh.vcol(0, "let x = 1", 3, 4, false), 3);
        assert_eq!(lh.vcol(0, "let x = 1", 3, 4, true), 6);
        assert_eq!(lh.vcol(0, "let x = 1", 4, 4, false), 7);
        // tabs: text-only tab stops; hint cells are added on top
        let lh = LineHints::from(vec![h(0, 2, "ab", false, false)]);
        assert_eq!(lh.vcol(0, "\tx\ty", 1, 4, true), 4);
        assert_eq!(lh.vcol(0, "\tx\ty", 3, 4, true), 8 + 2);
        // no hints on the line: equals visual_col
        assert_eq!(lh.vcol(5, "\tab", 2, 4, true), 5);
    }

    #[test]
    fn hint_past_end_is_clamped() {
        let lh = LineHints::from(vec![h(0, 50, "T", false, false)]);
        assert_eq!(lh.vcol(0, "ab", 2, 4, false), 2);
        assert_eq!(lh.vcol(0, "ab", 2, 4, true), 3);
        assert_eq!(lh.vcol(0, "ab", 99, 4, true), 3);
        assert_eq!(lh.col_from_vcol(0, "ab", 2, 4), (2, Some(0)));
        assert_eq!(lh.col_from_vcol(0, "ab", 3, 4), (2, None));
    }

    #[test]
    fn hint_hit_detection() {
        let lh = LineHints::from(vec![h(0, 3, ": num", false, true)]); // width 6, cells 3..9
        let t = "let x = 1";
        for v in 3..9 {
            assert_eq!(lh.col_from_vcol(0, t, v, 4), (3, Some(0)), "v={v}");
        }
        assert_eq!(lh.col_from_vcol(0, t, 2, 4), (2, None));
        assert_eq!(lh.col_from_vcol(0, t, 9, 4), (3, None)); // first text cell after the hint
        assert_eq!(lh.col_from_vcol(0, t, 10, 4), (4, None));
        assert_eq!(lh.col_from_vcol(0, t, 500, 4), (9, None));
        assert_eq!(lh.col_from_vcol(3, t, 4, 4), (4, None)); // no hints: plain mapping
    }

    #[test]
    fn two_hints_same_column_both_hit() {
        let lh = LineHints::from(vec![h(0, 1, "ab", false, false), h(0, 1, "cd", false, false)]);
        assert_eq!(lh.col_from_vcol(0, "xyz", 1, 4), (1, Some(0)));
        assert_eq!(lh.col_from_vcol(0, "xyz", 3, 4), (1, Some(1)));
        assert_eq!(lh.col_from_vcol(0, "xyz", 5, 4), (1, None));
        assert_eq!(lh.vcol(0, "xyz", 1, 4, true), 5);
    }

    #[test]
    fn tab_rounding_matches_col_from_visual() {
        // Same rounding as editor::col_from_visual inside a tab expansion.
        let lh = LineHints::default();
        let t = "\tx";
        assert_eq!(lh.col_from_vcol(0, t, 0, 4), (0, None));
        assert_eq!(lh.col_from_vcol(0, t, 1, 4), (0, None));
        assert_eq!(lh.col_from_vcol(0, t, 2, 4), (1, None));
        assert_eq!(lh.col_from_vcol(0, t, 3, 4), (1, None));
        assert_eq!(lh.col_from_vcol(0, t, 4, 4), (1, None));
        assert_eq!(lh.col_from_vcol(0, t, 5, 4), (2, None));
    }

    fn random_line(r: &mut Rng) -> String {
        let n = r.below(14);
        (0..n)
            .map(|_| match r.below(6) {
                0 => '\t',
                1 => 'é',
                2 => '日',
                _ => (b'a' + r.below(26) as u8) as char,
            })
            .collect()
    }

    fn random_hints(r: &mut Rng, len: usize) -> LineHints {
        let n = r.below(5);
        let v: Vec<Hint> = (0..n)
            .map(|_| {
                let lbl: String = (0..1 + r.below(5)).map(|_| 'q').collect();
                h(0, r.below(len + 1), &lbl, r.below(2) == 0, r.below(2) == 0)
            })
            .collect();
        LineHints::from(v)
    }

    #[test]
    fn vcol_round_trip_property() {
        let mut r = Rng(0x9E3779B97F4A7C15);
        for _ in 0..400 {
            let text = random_line(&mut r);
            let len = text.chars().count();
            let tab_w = [2, 4, 8][r.below(3)];
            let lh = random_hints(&mut r, len);
            for col in 0..=len {
                let v = lh.vcol(0, &text, col, tab_w, true);
                assert_eq!(lh.col_from_vcol(0, &text, v, tab_w), (col, None), "text={text:?} col={col} v={v} hints={:?}", lh.hints_on(0));
                let v0 = lh.vcol(0, &text, col, tab_w, false);
                let (c0, hit) = lh.col_from_vcol(0, &text, v0, tab_w);
                assert_eq!(c0, col, "text={text:?} col={col}");
                let has_hint_at = lh.hints_on(0).iter().any(|x| x.col == col);
                assert_eq!(hit.is_some(), has_hint_at && v0 != v, "text={text:?} col={col}");
            }
        }
    }

    #[test]
    fn vcol_is_monotonic_property() {
        let mut r = Rng(12345);
        for _ in 0..300 {
            let text = random_line(&mut r);
            let len = text.chars().count();
            let lh = random_hints(&mut r, len);
            let mut prev = 0;
            for col in 0..=len {
                let a = lh.vcol(0, &text, col, 4, false);
                let b = lh.vcol(0, &text, col, 4, true);
                assert!(a >= prev && b >= a);
                prev = b;
            }
        }
    }

    #[test]
    fn visible_range() {
        assert_eq!(visible_range_request(100, 140, 1000), (60, 180));
        assert_eq!(visible_range_request(5, 30, 1000), (0, 70));
        assert_eq!(visible_range_request(990, 999, 1000), (950, 999));
        assert_eq!(visible_range_request(0, 10, 3), (0, 2));
        assert_eq!(visible_range_request(50, 60, 0), (0, 0));
        assert_eq!(visible_range_request(500, 10, 100), (99, 99));
    }

    // ── editor integration ──

    /// Reference for `window_pieces_hinted`: the row text with `width()` blanks per hint.
    fn expected_cells(text: &str, hs: &[Hint], tab_w: usize) -> String {
        let len = text.chars().count();
        let mut out = String::new();
        let (mut hi, mut tvc) = (0, 0);
        let mut chars = text.chars();
        for i in 0..=len {
            while hi < hs.len() && hs[hi].col.min(len) <= i {
                out.push_str(&" ".repeat(hs[hi].width()));
                hi += 1;
            }
            let Some(ch) = chars.next() else { break };
            if ch == '\t' {
                let n = tab_w - tvc % tab_w;
                out.push_str(&" ".repeat(n));
                tvc += n;
            } else {
                out.push(ch);
                tvc += 1;
            }
        }
        out
    }

    #[test]
    fn hinted_pieces_reserve_blank_cells_and_slice_windows() {
        use crate::theme::Rgba;
        let fg = Rgba(1, 1, 1, 255);
        let kw = Rgba(5, 5, 5, 255);
        let mut r = Rng(0x9e3779b97f4a7c15);
        let alphabet: Vec<char> = "ab\tcé x".chars().collect();
        for _ in 0..300 {
            let n = r.below(12);
            let text: String = (0..n).map(|_| alphabet[r.below(alphabet.len())]).collect();
            let len = text.chars().count();
            let mut hs: Vec<Hint> = (0..r.below(4)).map(|_| h(0, r.below(len + 3), &"T".repeat(1 + r.below(9)), r.below(2) == 0, r.below(2) == 0)).collect();
            hs.sort_by_key(|x| x.col);
            let spans: crate::highlight::Spans = std::rc::Rc::new(vec![(0, text.len().min(2), kw, 0)]);
            let full: Vec<char> = expected_cells(&text, &hs, 4).chars().collect();
            let (vc0, vc1) = (r.below(6), 6 + r.below(30));
            let mut out = Vec::new();
            window_pieces_hinted(&text, Some(&spans), 0, text.len(), &hs, vc0, vc1, 4, fg, &mut out);
            let got: String = out.iter().map(|p| p.0.as_str()).collect();
            let want: String = full.iter().skip(vc0).take(vc1.saturating_sub(vc0)).collect();
            assert_eq!(got, want, "{text:?} {hs:?} {vc0}..{vc1}");
            assert!(out.iter().all(|p| !p.0.is_empty()));
        }
    }

    #[test]
    fn hinted_pieces_color_code_by_span() {
        use crate::theme::Rgba;
        let (fg, kw) = (Rgba(1, 1, 1, 255), Rgba(5, 5, 5, 255));
        let spans: crate::highlight::Spans = std::rc::Rc::new(vec![(0, 5, kw, 1)]); // "const"
        let hs = vec![h(0, 7, ": number", false, false)]; // 8 chars -> 7 cells
        let mut out = Vec::new();
        window_pieces_hinted("const x = 1", Some(&spans), 0, 11, &hs, 0, 100, 4, fg, &mut out);
        assert_eq!(out[0], ("const".to_string(), kw, 1));
        // the hint's 7 blank cells share the code's style, so they merge with its neighbours
        assert_eq!(out[1], (format!(" x{} = 1", " ".repeat(7)), fg, 0));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn hinted_pieces_of_a_wrapped_row_use_row_relative_columns() {
        use crate::theme::Rgba;
        let (fg, kw) = (Rgba(1, 1, 1, 255), Rgba(5, 5, 5, 255));
        // line "abcdefghij", row = "efghij" (bytes 4..10); keyword span covers "ef" (bytes 4..6)
        let spans: crate::highlight::Spans = std::rc::Rc::new(vec![(4, 6, kw, 0)]);
        let all = vec![h(0, 6, "XY", false, false), h(0, 2, "no", false, false)];
        let mut sorted = all.clone();
        sorted.sort_by_key(|x| x.col);
        let rh = row_hints(&sorted, 10, 4, 10, true);
        assert_eq!(rh.idx, vec![1]);
        assert_eq!(rh.hints[0].col, 2);
        let mut out = Vec::new();
        window_pieces_hinted("abcdefghij", Some(&spans), 4, 10, &rh.hints, 0, 100, 4, fg, &mut out);
        assert_eq!(out[0], ("ef".to_string(), kw, 0));
        assert_eq!(out[1].0, "  ghij", "the hint at row column 2 reserves 2 cells right after \"ef\"");
    }

    #[test]
    fn row_hints_split_a_line_between_rows() {
        let mut hs = vec![h(0, 0, "a", false, false), h(0, 4, "b", false, false), h(0, 7, "c", false, false), h(0, 20, "d", false, false)];
        hs.sort_by_key(|x| x.col);
        // line of 10 chars wrapped as [0,4) [4,8) [8,10)
        let r0 = row_hints(&hs, 10, 0, 4, false);
        let r1 = row_hints(&hs, 10, 4, 8, false);
        let r2 = row_hints(&hs, 10, 8, 10, true);
        assert_eq!(r0.idx, vec![0]);
        assert_eq!(r1.idx, vec![1, 2]);
        assert_eq!(r1.hints.iter().map(|x| x.col).collect::<Vec<_>>(), vec![0, 3]);
        assert_eq!(r2.idx, vec![3], "hint past the end belongs to the last row at the line end");
        assert_eq!(r2.hints[0].col, 2);
        // a hint exactly at a break belongs to the row that starts there, not the one that ends
        assert!(!r0.idx.contains(&1));
    }

    #[test]
    fn row_slices_agree_with_whole_line_geometry() {
        // vcol/hit on a row slice equal the same computation on the row text alone.
        let text = "abcdefghij";
        let mut hs = vec![h(0, 6, ": number", true, false), h(0, 9, "x", false, true)];
        hs.sort_by_key(|x| x.col);
        let rh = row_hints(&hs, 10, 4, 10, true);
        let row = &text[4..];
        for col in 0..=6 {
            let v = vcol_in(&rh.hints, row, col, 4, true);
            assert_eq!(hit_in(&rh.hints, row, v, 4).0, col);
        }
        assert_eq!(vcol_in(&rh.hints, row, 2, 4, false), 2);
        assert_eq!(vcol_in(&rh.hints, row, 2, 4, true), 2 + 1 + 7); // pad + 8 chars -> 7 cells
        let (c, hit) = hit_in(&rh.hints, row, 4, 4);
        assert_eq!((c, hit), (2, Some((0, 2))));
    }

    #[test]
    fn label_width_is_scaled_and_rounded_up() {
        assert_eq!((0..=8).map(label_cells).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4, 5, 6, 6, 7]);
        assert_eq!(label_cells(20), 17);
        assert_eq!(h(0, 0, ": number", true, true).width(), 7 + 2);
        assert!(h(0, 0, "x: string[]", false, false).width() < "x: string[]".len());
    }

    #[test]
    fn pixel_to_label_char_follows_the_smaller_font() {
        // 8 chars at 0.85 -> 6.8 px/8px cells: 7 cells (56 px), text 54.4 px centred (0.8 px slack each side)
        let hint = h(0, 0, ": number", true, false);
        let cw = 8.0;
        assert_eq!(hint.char_at_px(0.0, cw), None, "left padding");
        assert_eq!(hint.char_at_px(8.0, cw), None, "still inside the slack");
        assert_eq!(hint.char_at_px(8.0 + 0.8 + 1.0, cw), Some(0));
        assert_eq!(hint.char_at_px(8.0 + 0.8 + 6.8 * 3.0 + 0.1, cw), Some(3));
        assert_eq!(hint.char_at_px(8.0 + 56.0 - 0.1, cw), None, "past the last char (slack)");
        assert_eq!(hint.char_at_px(8.0 + 0.8 + 6.8 * 7.5, cw), Some(7));
    }

    #[test]
    fn parse_collects_links_and_edits() {
        let v = json!([{
            "position": {"line": 3, "character": 10},
            "label": [{"value": ": "}, {"value": "User", "location": {"uri": "file:///a.ts", "range": {"start": {"line": 0, "character": 10}, "end": {"line": 0, "character": 14}}}}, {"value": "[]"}],
            "kind": 1, "paddingLeft": true,
            "textEdits": [{"range": {"start": {"line": 3, "character": 10}, "end": {"line": 3, "character": 10}}, "newText": ": User[]"}]
        }]);
        let hs = parse(&v, &mut ident);
        let hint = &hs[0];
        assert_eq!(hint.label, ": User[]");
        assert_eq!(hint.links, vec![HintLink { start: 2, end: 6, uri: "file:///a.ts".into(), line: 0, character: 10 }]);
        assert!(hint.link_at(1).is_none() && hint.link_at(2).is_some() && hint.link_at(5).is_some() && hint.link_at(6).is_none());
        assert_eq!(hint.edits, vec![HintEdit { dl0: 0, c0: 0, dl1: 0, c1: 0, text: ": User[]".into() }]);
        assert_eq!(hint.annotation_edits(), hint.edits);
    }

    #[test]
    fn annotation_falls_back_to_the_label_for_type_hints_only() {
        let t = h(0, 5, ": number", true, false);
        assert_eq!(t.annotation_edits(), vec![HintEdit { dl0: 0, c0: 0, dl1: 0, c1: 0, text: ": number".into() }]);
        let mut p = h(0, 5, "name:", false, true);
        p.kind = HintKind::Parameter;
        assert!(p.annotation_edits().is_empty());
        let mut o = h(0, 5, "= 0", true, false);
        o.kind = HintKind::Other;
        assert!(o.annotation_edits().is_empty());
        let mut r = h(0, 5, "-> x", true, false);
        r.kind = HintKind::Type;
        assert!(r.annotation_edits().is_empty(), "only ': T' labels are annotations");
    }

    #[test]
    fn edit_follows_single_line_changes() {
        let mk = || LineHints::from(vec![h(2, 3, "A", false, false), h(2, 8, "B", false, false), h(3, 1, "C", false, false)]);
        // typing 2 chars at col 5 (between the hints): B moves, A stays
        let mut lh = mk();
        lh.apply_edit(2, 5, 0, 2, 0, 0, None);
        assert_eq!(lh.hints_on(2).iter().map(|x| x.col).collect::<Vec<_>>(), vec![3, 10]);
        // typing exactly at a hint position: the hint stays attached to the text after it
        let mut lh = mk();
        lh.apply_edit(2, 3, 0, 1, 0, 0, None);
        assert_eq!(lh.hints_on(2).iter().map(|x| x.col).collect::<Vec<_>>(), vec![4, 9]);
        // deleting 3 chars at col 4..7: neither hint touched, B shifts left
        let mut lh = mk();
        lh.apply_edit(2, 4, 3, 0, 0, 0, None);
        assert_eq!(lh.hints_on(2).iter().map(|x| x.col).collect::<Vec<_>>(), vec![3, 5]);
        // deleting across a hint removes it
        let mut lh = mk();
        lh.apply_edit(2, 2, 3, 0, 0, 0, None);
        assert_eq!(lh.hints_on(2).iter().map(|x| (x.label.as_str(), x.col)).collect::<Vec<_>>(), vec![("B", 5)]);
        // replacing text right before B ("xy" -> "abcd" at 6..8): B (at 8) follows its text
        let mut lh = mk();
        lh.apply_edit(2, 6, 2, 4, 0, 0, None);
        assert_eq!(lh.hints_on(2).iter().map(|x| x.col).collect::<Vec<_>>(), vec![3, 10]);
        assert_eq!(lh.hints_on(3).len(), 1, "other lines untouched");
    }

    #[test]
    fn edit_follows_line_breaks() {
        let mk = || LineHints::from(vec![h(1, 1, "pre", false, false), h(2, 3, "A", false, false), h(2, 8, "B", false, false), h(3, 2, "C", false, false)]);
        // Enter + 4 spaces of indent at (2, 5): inserted 5 chars, ends at col 4 of line 3
        let mut lh = mk();
        lh.apply_edit(2, 5, 0, 5, 0, 1, Some(4));
        assert_eq!(lh.hints_on(1).len(), 1);
        assert_eq!(lh.hints_on(2).iter().map(|x| x.col).collect::<Vec<_>>(), vec![3]);
        assert_eq!(lh.hints_on(3).iter().map(|x| (x.line, x.col)).collect::<Vec<_>>(), vec![(3, 7)], "B: 4 + (8 - 5)");
        assert_eq!(lh.hints_on(4).iter().map(|x| (x.line, x.col)).collect::<Vec<_>>(), vec![(4, 2)], "later lines shift down");
        // unknown end column: the tail's hints are dropped, never misplaced
        let mut lh = mk();
        lh.apply_edit(2, 5, 0, 5, 0, 1, None);
        assert!(lh.hints_on(3).is_empty());
        // joining lines 2 and 3 (delete the break at the end of line 2): later lines shift up, line 3's tail is gone
        let mut lh = mk();
        lh.apply_edit(2, 20, 1, 0, 1, 0, None);
        assert_eq!(lh.hints_on(2).len(), 2);
        assert!(lh.hints_on(3).is_empty());
    }

    #[test]
    fn state_follows_recorded_edits() {
        use crate::buffer::{EditRec, EditShape};
        let rec = |line, col, removed, inserted| EditRec { at: 0, removed, inserted, line, col, rem_lines: 0, ins_lines: 0, tail_empty: false, shape: EditShape::Splice };
        let mut st = InlayState::default();
        st.apply(0, 10, vec![h(2, 3, ": T", false, false), h(2, 9, "x:", false, true), h(5, 0, "z", false, false)], 100);
        st.on_edits(&[rec(2, 4, 0, 1)], 2, 100, &|_| (0, 0));
        assert_eq!(st.hints.hints_on(2).iter().map(|x| x.col).collect::<Vec<_>>(), vec![3, 10], "no flash: hints kept and shifted");
        assert!(st.due.is_some());
        // a Swap drops the exchanged lines
        let swap = EditRec { shape: EditShape::Swap { a_lines: 1, b_lines: 1, a_chars: 3, b_chars: 3, eol: 1 }, ..rec(4, 0, 0, 0) };
        st.on_edits(&[swap], 4, 100, &|_| (0, 0));
        assert!(st.hints.hints_on(5).is_empty());
        assert_eq!(st.hints.hints_on(2).len(), 2);
    }

    #[test]
    fn uncovered_viewport_is_urgent() {
        let mut st = InlayState::default();
        st.cover = Some((100, 200));
        assert!(!st.viewport_uncovered(120, 160, 1000));
        assert!(st.viewport_uncovered(300, 340, 1000));
        assert!(st.viewport_uncovered(90, 130, 1000));
        assert!(!st.viewport_uncovered(105, 195, 1000), "edge margin refreshes lazily, not urgently");
        assert!(st.needs_request(105, 195, 1000));
    }

    #[test]
    fn hint_cells_follow_tabs_and_earlier_hints() {
        let hs = vec![h(0, 1, "ab", false, false), h(0, 1, "c", true, false), h(0, 9, "z", false, false)];
        let cells = hint_cells("a\tb", &hs, 4);
        assert_eq!(cells, vec![(1, 2), (4, 1), (4 + 1 + 2 + 2, 1)]);
    }

    #[test]
    fn invalidate_edit_shifts_and_drops() {
        let mut lh = LineHints::from(vec![h(1, 0, "a", false, false), h(3, 0, "b", false, false), h(5, 0, "c", false, false)]);
        lh.invalidate_edit(3, 2); // two lines inserted at line 3
        assert_eq!(lh.hints_on(1).len(), 1);
        assert!(lh.hints_on(3).is_empty());
        assert_eq!(lh.hints_on(7).len(), 1);
        assert_eq!(lh.hints_on(7)[0].line, 7);
        let mut lh = LineHints::from(vec![h(1, 0, "a", false, false), h(2, 0, "b", false, false), h(5, 0, "c", false, false)]);
        lh.invalidate_edit(1, -2); // lines 2 and 3 removed
        assert!(lh.hints_on(1).is_empty() && lh.hints_on(2).is_empty());
        assert_eq!(lh.hints_on(3).len(), 1);
        assert_eq!(lh.len(), 1);
    }

    #[test]
    fn replace_range_keeps_outside_and_prunes_far() {
        let mut lh = LineHints::from(vec![h(0, 0, "a", false, false), h(50, 0, "b", false, false), h(900, 0, "far", false, false)]);
        lh.replace_range(40, 60, vec![h(55, 3, "n", false, false), h(55, 1, "m", false, false), h(99, 0, "out", false, false)]);
        assert_eq!(lh.hints_on(0).len(), 1);
        assert!(lh.hints_on(50).is_empty());
        assert_eq!(lh.hints_on(55).iter().map(|x| x.col).collect::<Vec<_>>(), vec![1, 3]);
        assert!(lh.hints_on(99).is_empty(), "hints outside the asked range are ignored");
        assert!(lh.hints_on(900).is_empty(), "far hints dropped");
    }

    #[test]
    fn state_needs_request_logic() {
        let mut st = InlayState::default();
        assert!(st.needs_request(0, 30, 1000));
        st.cover = Some((0, 50));
        assert!(!st.needs_request(0, 30, 1000));
        assert!(st.needs_request(0, 45, 1000), "near the bottom edge refreshes early");
        st.cover = Some((80, 160));
        assert!(!st.needs_request(100, 140, 1000));
        assert!(st.needs_request(85, 140, 1000));
        assert!(st.needs_request(10, 40, 1000));
        st.cover = Some((970, 999));
        assert!(!st.needs_request(990, 999, 1000));
    }

    #[test]
    fn state_on_edit_follows_inserted_lines() {
        let mut st = InlayState::default();
        st.apply(0, 10, vec![h(2, 0, "a", false, false), h(8, 0, "b", false, false)], 100);
        st.on_edit(4, 101);
        assert_eq!(st.hints.hints_on(9).len(), 1);
        assert!(st.due.is_some());
        st.on_edit(2, 101); // same line count: just drops the edited line
        assert!(st.hints.hints_on(2).is_empty());
        assert_eq!(st.hints.len(), 1);
    }

    #[test]
    fn utf16_columns() {
        assert_eq!(utf16_to_char_col("a😀b", 3), 2);
        assert_eq!(utf16_to_char_col("abc", 99), 3);
    }
}
