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
//! * Hint widths are in cells = chars of label + paddings (labels never contain tabs/newlines).
#![allow(dead_code)]

use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    Type,
    Parameter,
    Other,
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
}

impl Hint {
    /// Displayed width in cells (label chars + paddings).
    pub fn width(&self) -> usize {
        self.label.chars().count() + self.pad_left as usize + self.pad_right as usize
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
/// (tabs, newlines) become spaces.
pub fn parse(v: &Value, to_char_col: &mut dyn FnMut(usize, usize) -> usize) -> Vec<Hint> {
    let Some(arr) = v.as_array() else { return Vec::new() };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let pos = &item["position"];
        let (Some(line), Some(ch)) = (pos["line"].as_u64(), pos["character"].as_u64()) else { continue };
        let raw: String = match &item["label"] {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts.iter().filter_map(|p| p["value"].as_str()).collect(),
            _ => continue,
        };
        let label: String = raw.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
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
        out.push(Hint {
            line,
            col,
            label,
            kind,
            pad_left: item["paddingLeft"].as_bool().unwrap_or(false),
            pad_right: item["paddingRight"].as_bool().unwrap_or(false),
        });
    }
    out
}

/// Lines to request for a viewport: `first..=last` padded by 20 lines and clamped to the
/// document. Returns inclusive `(first_line, last_line)`; `(0, 0)` for an empty document.
pub fn visible_range_request(first_line: usize, last_line: usize, line_count: usize) -> (usize, usize) {
    if line_count == 0 {
        return (0, 0);
    }
    let max = line_count - 1;
    let first = first_line.saturating_sub(20).min(max);
    let last = last_line.max(first_line).saturating_add(20).min(max);
    (first, last)
}

/// Tab-aware visual column of char column `col` (same semantics as `editor::visual_col`).
fn visual_col(text: &str, col: usize, tab_w: usize) -> usize {
    let mut v = 0;
    for ch in text.chars().take(col) {
        v += if ch == '\t' { tab_w - v % tab_w } else { 1 };
    }
    v
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
        let hints = self.hints_on(line);
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

    /// Inverse of [`vcol`]: buffer char column for visual column `vcol`, and `Some(index into
    /// hints_on(line))` when the pointer is over a hint (the returned column is then the hint's
    /// own column, clamped to the line end). Past the end of the line gives the line length.
    /// Over text it rounds like `editor::col_from_visual`.
    pub fn col_from_vcol(&self, line: usize, text: &str, vcol: usize, tab_w: usize) -> (usize, Option<usize>) {
        let hints = self.hints_on(line);
        let len = text.chars().count();
        let mut hi = 0;
        let mut pos = 0; // total visual position
        let mut tvc = 0; // text-only visual column (tab stops)
        let mut chars = text.chars();
        for i in 0..=len {
            while hi < hints.len() && hints[hi].col.min(len) <= i {
                let w = hints[hi].width();
                if vcol < pos + w {
                    return (i, Some(hi));
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Code(String),
    Hint(String, HintKind),
}

impl Piece {
    pub fn text(&self) -> &str {
        match self {
            Piece::Code(s) | Piece::Hint(s, _) => s,
        }
    }
}

/// The visible window `[vc0, vc1)` of one line (visual columns including hint cells) as pieces:
/// code (tabs expanded to spaces like `doc::window_pieces`) interleaved with hint text at their
/// columns. `hints` are the hints of this line, sorted by column (as `LineHints::hints_on`).
/// Adjacent code is merged; empty pieces are never produced; hint text is clipped to the window.
pub fn weave(text: &str, hints: &[Hint], vc0: usize, vc1: usize, tab_w: usize) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    let mut code = String::new();
    let len = text.chars().count();
    let mut pos = 0usize; // total visual position
    let mut tvc = 0usize; // text-only visual column
    let mut hi = 0;

    fn flush(code: &mut String, pieces: &mut Vec<Piece>) {
        if !code.is_empty() {
            pieces.push(Piece::Code(std::mem::take(code)));
        }
    }

    let mut chars = text.chars();
    for i in 0..=len {
        while hi < hints.len() && hints[hi].col.min(len) <= i {
            let h = &hints[hi];
            hi += 1;
            let mut s = String::new();
            for ch in h.text().chars() {
                if pos >= vc0 && pos < vc1 {
                    s.push(ch);
                }
                pos += 1;
            }
            if !s.is_empty() {
                flush(&mut code, &mut pieces);
                pieces.push(Piece::Hint(s, h.kind));
            }
        }
        if pos >= vc1 {
            break;
        }
        let Some(ch) = chars.next() else { break };
        if ch == '\t' {
            let n = tab_w - tvc % tab_w;
            for _ in 0..n {
                if pos >= vc0 && pos < vc1 {
                    code.push(' ');
                }
                pos += 1;
            }
            tvc += n;
        } else {
            if pos >= vc0 && pos < vc1 {
                code.push(ch);
            }
            pos += 1;
            tvc += 1;
        }
    }
    flush(&mut code, &mut pieces);
    pieces
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
}

impl InlayState {
    /// Text changed (first touched line `line`, new line count `line_count`).
    pub fn on_edit(&mut self, line: usize, line_count: usize) {
        if !self.hints.is_empty() {
            let delta = if self.lines == 0 { 0 } else { line_count as isize - self.lines as isize };
            self.hints.invalidate_edit(line, delta);
        }
        self.lines = line_count;
        self.due = Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
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

/// Visual start column and width of each hint's label (paddings excluded: they stay as a gap)
/// of a line, in the order of `hints`.
pub fn hint_cells(text: &str, hints: &[Hint], tab_w: usize) -> Vec<(usize, usize)> {
    let len = text.chars().count();
    let mut extra = 0;
    hints
        .iter()
        .map(|h| {
            let start = visual_col(text, h.col.min(len), tab_w) + extra + h.pad_left as usize;
            extra += h.width();
            (start, h.label.chars().count())
        })
        .collect()
}

/// Like `doc::window_pieces` but with the line's hints woven in: pieces `(text, color, flags)`
/// for visual columns `[vc0, vc1)` (hint cells included), code colored by `spans`, hints in
/// `hint_fg` flagged `hint_flags`. Same text as [`weave`] for the same window.
pub fn window_pieces_hinted(
    line: &str,
    spans: Option<&crate::highlight::Spans>,
    hints: &[Hint],
    vc0: usize,
    vc1: usize,
    tab_w: usize,
    fg: crate::theme::Rgba,
    hint_fg: crate::theme::Rgba,
    hint_flags: u8,
    out: &mut Vec<(String, crate::theme::Rgba, u8)>,
) {
    fn put(out: &mut Vec<(String, crate::theme::Rgba, u8)>, ch: char, c: crate::theme::Rgba, f: u8) {
        match out.last_mut() {
            Some((s, lc, lf)) if *lc == c && *lf == f && s.as_bytes().last() != Some(&b'\n') => s.push(ch),
            _ => out.push((ch.to_string(), c, f)),
        }
    }
    let len = line.chars().count();
    let mut pos = 0usize;
    let mut tvc = 0usize;
    let mut hi = 0;
    let mut si = 0;
    let mut chars = line.char_indices();
    for i in 0..=len {
        while hi < hints.len() && hints[hi].col.min(len) <= i {
            let h = &hints[hi];
            hi += 1;
            for ch in h.text().chars() {
                if pos >= vc0 && pos < vc1 {
                    put(out, ch, hint_fg, hint_flags);
                }
                pos += 1;
            }
        }
        if pos >= vc1 {
            break;
        }
        let Some((b, ch)) = chars.next() else { break };
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
        Hint { line, col, label: label.into(), kind: HintKind::Type, pad_left: l, pad_right: r }
    }

    fn ident(_l: usize, c: usize) -> usize {
        c
    }

    fn joined(p: &[Piece]) -> String {
        p.iter().map(|p| p.text()).collect()
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
        assert_eq!(h(0, 0, "ab", true, true).text(), " ab ");
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
        assert_eq!(joined(&weave("ab", lh.hints_on(0), 0, 100, 4)), "abT");
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
    fn weave_basic() {
        let hs = vec![h(0, 3, ": number", false, false), h(0, 9, "n:", false, true)];
        let p = weave("let x = foo", &hs, 0, 100, 4);
        assert_eq!(
            p,
            vec![
                Piece::Code("let".into()),
                Piece::Hint(": number".into(), HintKind::Type),
                Piece::Code(" x = f".into()),
                Piece::Hint("n: ".into(), HintKind::Type),
                Piece::Code("oo".into()),
            ]
        );
    }

    #[test]
    fn weave_tabs_like_window_pieces() {
        let p = weave("a\tb", &[], 0, 100, 4);
        assert_eq!(p, vec![Piece::Code("a   b".into())]);
        // window starting inside the tab expansion
        assert_eq!(weave("a\tb", &[], 2, 100, 4), vec![Piece::Code("  b".into())]);
        assert_eq!(weave("a\tb", &[], 1, 3, 4), vec![Piece::Code("  ".into())]);
        assert!(weave("a\tb", &[], 5, 9, 4).is_empty());
        assert!(weave("abc", &[], 2, 2, 4).is_empty());
    }

    #[test]
    fn weave_window_inside_hint() {
        let hs = vec![h(0, 2, "abcdef", false, false)]; // cells 2..8
        assert_eq!(weave("xyzw", &hs, 4, 6, 4), vec![Piece::Hint("cd".into(), HintKind::Type)]);
        assert_eq!(
            weave("xyzw", &hs, 5, 10, 4),
            vec![Piece::Hint("def".into(), HintKind::Type), Piece::Code("zw".into())]
        );
        assert_eq!(
            weave("xyzw", &hs, 0, 4, 4),
            vec![Piece::Code("xy".into()), Piece::Hint("ab".into(), HintKind::Type)]
        );
    }

    #[test]
    fn weave_hint_with_tab_before_it() {
        // hint cells do not shift tab stops: tab after the hint still uses text-only columns
        let hs = vec![h(0, 1, "ZZ", false, false)];
        assert_eq!(joined(&weave("a\tb\tc", &hs, 0, 100, 4)), "aZZ   b   c");
    }

    #[test]
    fn weave_window_slices_equal_full_property() {
        let mut r = Rng(0xDEADBEEF);
        for _ in 0..300 {
            let text = random_line(&mut r);
            let len = text.chars().count();
            let tab_w = [2, 4, 8][r.below(3)];
            let lh = random_hints(&mut r, len);
            let hs = lh.hints_on(0);
            let full: Vec<char> = joined(&weave(&text, hs, 0, usize::MAX, tab_w)).chars().collect();
            // total width = text vcol at end + all hint widths
            assert_eq!(full.len(), lh.vcol(0, &text, len, tab_w, true));
            for _ in 0..8 {
                let a = r.below(full.len() + 3);
                let b = a + r.below(full.len() + 3);
                let got = joined(&weave(&text, hs, a, b, tab_w));
                let want: String = full.iter().skip(a).take(b.saturating_sub(a)).collect();
                assert_eq!(got, want, "text={text:?} window=[{a},{b}) hints={hs:?}");
            }
            // Adjacent windows tile the full output.
            let mid = r.below(full.len() + 1);
            let tiled = joined(&weave(&text, hs, 0, mid, tab_w)) + &joined(&weave(&text, hs, mid, usize::MAX, tab_w));
            assert_eq!(tiled, full.iter().collect::<String>());
        }
    }

    #[test]
    fn weave_no_empty_or_adjacent_code_pieces() {
        let mut r = Rng(777);
        for _ in 0..200 {
            let text = random_line(&mut r);
            let lh = random_hints(&mut r, text.chars().count());
            let p = weave(&text, lh.hints_on(0), r.below(4), 4 + r.below(30), 4);
            for w in p.windows(2) {
                assert!(!matches!((&w[0], &w[1]), (Piece::Code(_), Piece::Code(_))));
            }
            assert!(p.iter().all(|x| !x.text().is_empty()));
        }
    }

    #[test]
    fn weave_positions_agree_with_vcol() {
        let hs = vec![h(0, 2, ": T", false, false)];
        let lh = LineHints::from(hs);
        let text = "ab\tcd";
        let full: Vec<char> = joined(&weave(text, lh.hints_on(0), 0, 1000, 4)).chars().collect();
        // char 'c' (col 3) must be drawn at vcol(col 3, include_at=true)
        let v = lh.vcol(0, text, 3, 4, true);
        assert_eq!(full[v], 'c');
        let v = lh.vcol(0, text, 4, 4, true);
        assert_eq!(full[v], 'd');
    }

    #[test]
    fn visible_range() {
        assert_eq!(visible_range_request(100, 140, 1000), (80, 160));
        assert_eq!(visible_range_request(5, 30, 1000), (0, 50));
        assert_eq!(visible_range_request(990, 999, 1000), (970, 999));
        assert_eq!(visible_range_request(0, 10, 3), (0, 2));
        assert_eq!(visible_range_request(50, 60, 0), (0, 0));
        assert_eq!(visible_range_request(500, 10, 100), (99, 99));
    }

    // ── editor integration ──

    #[test]
    fn hinted_pieces_match_weave_text_and_colors() {
        use crate::theme::Rgba;
        let (fg, hg) = (Rgba(1, 1, 1, 255), Rgba(9, 9, 9, 255));
        let kw = Rgba(5, 5, 5, 255);
        let mut r = Rng(0x9e3779b97f4a7c15);
        let alphabet: Vec<char> = "ab\tcé x".chars().collect();
        for _ in 0..300 {
            let n = r.below(12);
            let text: String = (0..n).map(|_| alphabet[r.below(alphabet.len())]).collect();
            let len = text.chars().count();
            let mut hs: Vec<Hint> = (0..r.below(4)).map(|_| h(0, r.below(len + 3), "T", r.below(2) == 0, r.below(2) == 0)).collect();
            hs.sort_by_key(|x| x.col);
            let spans: crate::highlight::Spans = std::rc::Rc::new(vec![(0, text.len().min(2), kw, 0)]);
            let (vc0, vc1) = (r.below(6), 6 + r.below(30));
            let want = joined(&weave(&text, &hs, vc0, vc1, 4));
            let mut out = Vec::new();
            window_pieces_hinted(&text, Some(&spans), &hs, vc0, vc1, 4, fg, hg, 2, &mut out);
            let got: String = out.iter().map(|p| p.0.as_str()).collect();
            assert_eq!(got, want, "{text:?} {hs:?} {vc0}..{vc1}");
            assert!(out.iter().all(|p| !p.0.is_empty()));
            // hint cells are drawn with the hint style
            let hint_chars: usize = out.iter().filter(|p| p.1 == hg && p.2 == 2).map(|p| p.0.chars().count()).sum();
            let weave_hint: usize = weave(&text, &hs, vc0, vc1, 4).iter().filter(|p| matches!(p, Piece::Hint(..))).map(|p| p.text().chars().count()).sum();
            assert_eq!(hint_chars, weave_hint);
        }
    }

    #[test]
    fn hinted_pieces_color_code_by_span() {
        use crate::theme::Rgba;
        let (fg, hg, kw) = (Rgba(1, 1, 1, 255), Rgba(9, 9, 9, 255), Rgba(5, 5, 5, 255));
        let spans: crate::highlight::Spans = std::rc::Rc::new(vec![(0, 5, kw, 1)]); // "const"
        let hs = vec![h(0, 7, ": number", false, false)];
        let mut out = Vec::new();
        window_pieces_hinted("const x = 1", Some(&spans), &hs, 0, 100, 4, fg, hg, 2, &mut out);
        assert_eq!(out[0], ("const".to_string(), kw, 1));
        assert_eq!(out[1], (" x".to_string(), fg, 0));
        assert_eq!(out[2], (": number".to_string(), hg, 2));
        assert_eq!(out[3], (" = 1".to_string(), fg, 0));
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
