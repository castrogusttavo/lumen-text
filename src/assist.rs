//! Editing aids drawn by the document: rulers, inline diagnostics (Error Lens style), document
//! highlights (`textDocument/documentHighlight`, with a plain same-word fallback) and linked
//! editing (`textDocument/linkedEditingRange`: renaming an opening tag renames the closing one).
//!
//! The pure logic and the per-document state live here; the App glue (debounce, requests to the
//! servers, responses, settings, palette) is in `app_assist.rs`.

use crate::buffer::{Buffer, EditRec, EditShape, Sel};
use crate::doc::{Diag, Doc};
use serde_json::Value;
use std::time::Instant;

/// Most rulers kept from the config (more would only be noise).
const MAX_RULERS: usize = 8;
/// Highest column a ruler can sit at.
const MAX_RULER_COL: usize = 1000;

/// `inline_diagnostics` setting: where the message of a line's most severe diagnostic is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InlineDiags {
    Off,
    /// Only on the caret's line (the default: calm while typing elsewhere).
    #[default]
    CurrentLine,
    All,
}

impl InlineDiags {
    pub const NAMES: &'static [&'static str] = &["off", "current_line", "all"];

    /// Anything unknown (or missing) is the default, `current_line`.
    pub fn from_name(s: &str) -> InlineDiags {
        match s {
            "off" => InlineDiags::Off,
            "all" => InlineDiags::All,
            _ => InlineDiags::CurrentLine,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            InlineDiags::Off => "off",
            InlineDiags::CurrentLine => "current_line",
            InlineDiags::All => "all",
        }
    }

    /// Palette cycle: off -> current line -> all -> off.
    pub fn next(self) -> InlineDiags {
        match self {
            InlineDiags::Off => InlineDiags::CurrentLine,
            InlineDiags::CurrentLine => InlineDiags::All,
            InlineDiags::All => InlineDiags::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            InlineDiags::Off => "off",
            InlineDiags::CurrentLine => "current line",
            InlineDiags::All => "all lines",
        }
    }
}

// ───────────────────────────── rulers ─────────────────────────────

/// Ruler columns from the config: a JSON array of numbers, one number, or the comma/space separated
/// text of the settings field. Sorted, deduplicated, 1..=1000, at most 8.
pub fn parse_rulers(v: &Value) -> Vec<usize> {
    let num = |x: &Value| x.as_u64().or_else(|| x.as_f64().filter(|f| *f >= 0.0).map(|f| f.round() as u64));
    let mut out: Vec<usize> = match v {
        Value::Array(a) => a.iter().filter_map(num).map(|n| n as usize).collect(),
        Value::Number(_) => num(v).map(|n| n as usize).into_iter().collect(),
        Value::String(s) => {
            s.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter_map(|t| t.parse().ok()).collect()
        }
        _ => Vec::new(),
    };
    out.retain(|c| (1..=MAX_RULER_COL).contains(c));
    out.sort_unstable();
    out.dedup();
    out.truncate(MAX_RULERS);
    out
}

/// The settings field's text for a ruler list ("80, 120").
pub fn rulers_text(r: &[usize]) -> String {
    r.iter().map(usize::to_string).collect::<Vec<_>>().join(", ")
}

/// Rulers of a document: the config's plus the EditorConfig `max_line_length`, if any.
pub fn merge_rulers(config: &[usize], max_line_length: Option<usize>) -> Vec<usize> {
    let mut v = config.to_vec();
    v.extend(max_line_length.filter(|c| (1..=MAX_RULER_COL).contains(c)));
    v.sort_unstable();
    v.dedup();
    v
}

/// Window x of each ruler that falls inside the text area `[body_x, body_right)`: column `c` sits at
/// the left edge of cell `c` (the same grid the text uses), shifted by the horizontal scroll.
pub fn ruler_xs(rulers: &[usize], body_x: i32, body_right: i32, cell_w: f32, left: f64) -> Vec<i32> {
    rulers
        .iter()
        .map(|&c| body_x + (c as f32 * cell_w).round() as i32 - left.round() as i32)
        .filter(|&x| x >= body_x && x < body_right)
        .collect()
}

// ───────────────────────────── inline diagnostics ─────────────────────────────

/// The most severe diagnostic (error, warning, info: hints are never shown inline) of each line in
/// `first..last`, keyed by the line it starts on; among equals the first by position wins. `only`
/// restricts it to one line (the caret's). `diags` is sorted by start (see `Doc::diags`).
pub fn worst_per_line(diags: &[Diag], first: usize, last: usize, only: Option<usize>) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let lo = diags.partition_point(|d| d.sl < first);
    for (i, d) in diags.iter().enumerate().skip(lo) {
        if d.sl >= last {
            break;
        }
        if d.severity == 0 || d.severity > 3 || only.is_some_and(|l| l != d.sl) {
            continue;
        }
        if let Some((l, best)) = out.last_mut()
            && *l == d.sl
        {
            if d.severity < diags[*best].severity {
                *best = i;
            }
            continue;
        }
        out.push((d.sl, i));
    }
    out
}

/// Where an inline message may go on a row: `gap` px after `end_x` (the end of the text, the hints
/// and anything already drawn after them) up to `right`. `(x, room)`, or None when less than
/// `min_w` px remain (a wrapped or long line keeps its text uncovered).
pub fn inline_slot(end_x: i32, right: i32, gap: i32, min_w: i32) -> Option<(i32, i32)> {
    let x = end_x + gap;
    let room = right - x;
    (room >= min_w).then_some((x, room))
}

/// The first non-empty line of `msg` (whitespace collapsed), cut with "…" to fit `max_w` px as
/// measured by `measure`. None when not even a few characters fit.
pub fn fit_message(msg: &str, max_w: i32, measure: &mut dyn FnMut(&str) -> i32) -> Option<String> {
    let first = msg.lines().map(str::trim).find(|l| !l.is_empty())?;
    let s = first.split_whitespace().collect::<Vec<_>>().join(" ");
    if measure(&s) <= max_w {
        return Some(s);
    }
    let chars: Vec<char> = s.chars().collect();
    let cut = |n: usize| format!("{}…", chars[..n].iter().collect::<String>().trim_end());
    let (mut lo, mut hi) = (0usize, chars.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&cut(mid)) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    (lo >= 3).then(|| cut(lo))
}

// ───────────────────────────── document highlights ─────────────────────────────

/// One occurrence of the symbol under the caret (char range); `write` for a write access
/// (`DocumentHighlightKind.Write`), drawn with the stronger color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WordHl {
    pub a: usize,
    pub b: usize,
    pub write: bool,
}

/// `DocumentHighlight[]` as (start line, start UTF-16 col, end line, end UTF-16 col, write).
pub fn parse_highlights(v: &Value) -> Vec<(usize, usize, usize, usize, bool)> {
    let mut out = Vec::new();
    for h in v.as_array().into_iter().flatten() {
        let r = &h["range"];
        let (Some(sl), Some(sc), Some(el), Some(ec)) = (
            r["start"]["line"].as_u64(),
            r["start"]["character"].as_u64(),
            r["end"]["line"].as_u64(),
            r["end"]["character"].as_u64(),
        ) else {
            continue;
        };
        out.push((sl as usize, sc as usize, el as usize, ec as usize, h["kind"].as_u64() == Some(3)));
    }
    out
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The identifier the caret at `at` touches (char range), if any.
pub fn word_range(buf: &Buffer, at: usize) -> Option<(usize, usize)> {
    let mut s = at;
    while s > 0 && buf.char_at(s - 1).is_some_and(is_word) {
        s -= 1;
    }
    let mut e = at;
    while buf.char_at(e).is_some_and(is_word) {
        e += 1;
    }
    (s < e).then_some((s, e))
}

/// An answer the document is waiting for: a request sent to several servers (`expected` answers).
#[derive(Debug)]
pub struct HlReq {
    pub serial: u64,
    /// Buffer version and caret the request was made for (a later edit or move drops the answer).
    pub ver: u64,
    pub at: usize,
    pub expected: usize,
    /// (order: 0 = primary, server name, answer).
    pub parts: Vec<(usize, String, Value)>,
}

// ───────────────────────────── linked editing ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClassItem {
    Range(char, char),
    Word,
    Digit,
}

/// The characters a linked range may hold: the server's `wordPattern` when it is a plain character
/// class (`[a-zA-Z0-9:\-\._$]*`, what tsgo and the HTML server send), else the usual tag-name set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WordPattern {
    class: Option<Vec<ClassItem>>,
}

impl WordPattern {
    pub fn parse(p: &str) -> WordPattern {
        WordPattern { class: parse_class(p) }
    }

    pub fn allows(&self, s: &str) -> bool {
        s.chars().all(|c| match &self.class {
            None => c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.' | '$'),
            Some(items) => items.iter().any(|it| match *it {
                ClassItem::Range(a, b) => (a..=b).contains(&c),
                ClassItem::Word => c.is_alphanumeric() || c == '_',
                ClassItem::Digit => c.is_ascii_digit(),
            }),
        })
    }
}

/// `^?[...][*+]?$?` -> the class items; anything fancier -> None.
fn parse_class(p: &str) -> Option<Vec<ClassItem>> {
    let p = p.strip_prefix('^').unwrap_or(p);
    let p = p.strip_suffix('$').unwrap_or(p);
    let p = p.strip_suffix(['*', '+']).unwrap_or(p);
    let inner = p.strip_prefix('[')?.strip_suffix(']')?;
    if inner.starts_with('^') || inner.is_empty() {
        return None;
    }
    let src: Vec<char> = inner.chars().collect();
    let mut items = Vec::new();
    let mut lits: Vec<(char, bool)> = Vec::new(); // (char, written escaped)
    let mut k = 0;
    while k < src.len() {
        match src[k] {
            '\\' => {
                match *src.get(k + 1)? {
                    'w' => items.push(ClassItem::Word),
                    'd' => items.push(ClassItem::Digit),
                    e => lits.push((e, true)),
                }
                k += 2;
            }
            '[' | ']' => return None,
            c => {
                lits.push((c, false));
                k += 1;
            }
        }
    }
    // An unescaped '-' between two characters is a range (`a-z`); anywhere else it is itself.
    let mut j = 0;
    while j < lits.len() {
        let c = lits[j].0;
        if j + 2 < lits.len() && lits[j + 1] == ('-', false) {
            let hi = lits[j + 2].0;
            if hi < c {
                return None;
            }
            items.push(ClassItem::Range(c, hi));
            j += 3;
        } else {
            items.push(ClassItem::Range(c, c));
            j += 1;
        }
    }
    Some(items)
}

/// Ranges that are edited together (an opening and a closing tag name), as char ranges in the text of
/// buffer version `ver`; every range holds the same text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    pub ranges: Vec<(usize, usize)>,
    pub pattern: WordPattern,
    pub ver: u64,
}

/// What an edit means for the linked ranges.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkStep {
    /// Still in step (nothing touched, or every range already holds the same text).
    Keep,
    /// The edit broke the link (it crossed a range edge, or left a character the pattern refuses).
    Drop,
    /// One range was edited: give the others its text, `(start, end, text)` in current coordinates.
    Mirror(Vec<(usize, usize, String)>),
}

/// `LinkedEditingRanges` as (start line, start UTF-16 col, end line, end UTF-16 col) and the word pattern.
pub fn parse_linked(v: &Value) -> Option<(Vec<(usize, usize, usize, usize)>, WordPattern)> {
    let mut out = Vec::new();
    for r in v["ranges"].as_array()? {
        out.push((
            r["start"]["line"].as_u64()? as usize,
            r["start"]["character"].as_u64()? as usize,
            r["end"]["line"].as_u64()? as usize,
            r["end"]["character"].as_u64()? as usize,
        ));
    }
    let pattern = v["wordPattern"].as_str().map(WordPattern::parse).unwrap_or_default();
    (out.len() >= 2).then_some((out, pattern))
}

impl Linked {
    /// Does the caret at `at` sit in (or at an edge of) one of the ranges?
    pub fn contains(&self, at: usize) -> bool {
        self.ranges.iter().any(|&(s, e)| s <= at && at <= e)
    }

    /// Follow `recs` (applied in order) and say what to do; `text(a, b)` reads the current text.
    pub fn on_edits(&mut self, recs: &[EditRec], text: &dyn Fn(usize, usize) -> String) -> LinkStep {
        let mut touched = vec![false; self.ranges.len()];
        for r in recs {
            if r.shape != EditShape::Splice {
                return LinkStep::Drop;
            }
            let (a, b) = (r.at, r.at + r.removed);
            let inside = self.ranges.iter().position(|&(s, e)| a >= s && b <= e);
            for (k, rg) in self.ranges.iter_mut().enumerate() {
                if Some(k) == inside {
                    rg.1 = rg.1 + r.inserted - r.removed;
                    touched[k] = true;
                } else if a < rg.1 && b > rg.0 {
                    return LinkStep::Drop; // crosses this range's edge
                } else {
                    *rg = (r.map_pos(rg.0), r.map_pos(rg.1));
                }
            }
        }
        let texts: Vec<String> = self.ranges.iter().map(|&(s, e)| text(s, e)).collect();
        if texts.iter().all(|t| *t == texts[0]) {
            return LinkStep::Keep;
        }
        let edited: Vec<usize> = (0..touched.len()).filter(|&k| touched[k]).collect();
        let [k] = edited[..] else { return LinkStep::Drop };
        let others_agree =
            texts.iter().enumerate().filter(|(j, _)| *j != k).all(|(_, t)| *t == texts[(k + 1) % texts.len()]);
        if !others_agree || !self.pattern.allows(&texts[k]) {
            return LinkStep::Drop;
        }
        let new = texts[k].clone();
        LinkStep::Mirror(
            self.ranges.iter().enumerate().filter(|(j, _)| *j != k).map(|(_, &(s, e))| (s, e, new.clone())).collect(),
        )
    }
}

// ───────────────────────────── per-document state ─────────────────────────────

/// What a document keeps for these aids (all empty = no cost while drawing).
#[derive(Debug, Default)]
pub struct Assist {
    /// Ruler columns (config, plus the EditorConfig line length when known).
    pub rulers: Vec<usize>,
    pub inline: InlineDiags,
    /// Occurrences of the symbol under the caret from a language server, for buffer version `hl_ver`.
    pub hl: Vec<WordHl>,
    /// No server offers document highlights: the word under the caret, matched (whole word) on the
    /// visible lines while drawing.
    pub hl_word: Option<String>,
    pub hl_ver: u64,
    /// (caret, buffer version) last seen by the tick: a change starts the debounce again.
    pub hl_key: Option<(usize, u64)>,
    pub hl_due: Option<Instant>,
    pub hl_req: Option<HlReq>,
    /// Serial of the newest request (answers to older ones are dropped).
    pub serial: u64,
    pub linked: Option<Linked>,
}

impl Assist {
    /// Is the caret at `at` inside one of the highlighted occurrences of buffer version `ver`?
    pub fn caret_in_hl(&self, at: usize, ver: u64, buf: &Buffer) -> bool {
        if self.hl_ver != ver {
            return false;
        }
        if self.hl.iter().any(|h| h.a <= at && at <= h.b) {
            return true;
        }
        self.hl_word.as_ref().is_some_and(|w| word_range(buf, at).is_some_and(|(s, e)| buf.slice(s, e) == *w))
    }

    /// Forget the highlights and drop any answer still on its way. True when something was shown.
    pub fn clear_hl(&mut self) -> bool {
        let shown = !self.hl.is_empty() || self.hl_word.is_some();
        self.hl.clear();
        self.hl_word = None;
        self.hl_req = None;
        shown
    }

    /// Highlights to draw for buffer version `ver` (stale ones never show).
    pub fn live_hl(&self, ver: u64) -> (&[WordHl], Option<&str>) {
        if self.hl_ver != ver {
            return (&[], None);
        }
        (&self.hl, self.hl_word.as_deref())
    }
}

impl Doc {
    /// Linked editing, called with the edits `sync_edits` consumed: the mirror edits to apply, if a
    /// linked range was edited by the user (never for undo/redo, whose group is closed).
    pub(crate) fn linked_follow(&mut self, recs: &[EditRec]) -> Option<Vec<(usize, usize, String)>> {
        let joinable = self.buf.joinable();
        let l = self.assist.linked.as_mut()?;
        let buf = &self.buf;
        let step = l.on_edits(recs, &|a, b| buf.slice(a, b));
        l.ver = buf.version();
        match step {
            LinkStep::Keep => None,
            LinkStep::Mirror(m) if joinable => Some(m),
            _ => {
                self.assist.linked = None;
                None
            }
        }
    }

    /// Apply mirror edits as part of the user's undo step, shifting the carets past them.
    pub(crate) fn linked_apply(&mut self, mut edits: Vec<(usize, usize, String)>) {
        edits.sort_by_key(|e| std::cmp::Reverse(e.0));
        let shift = |p: usize| {
            let mut np = p as isize;
            for (s, e, t) in &edits {
                let n = t.chars().count() as isize;
                if *e <= p {
                    np += n - (*e - *s) as isize;
                } else if *s < p {
                    np = *s as isize + n.min((p - *s) as isize);
                }
            }
            np.max(0) as usize
        };
        let before = self.ed.sel;
        let sel = Sel { anchor: shift(before.anchor), head: shift(before.head) };
        let extra: Vec<Sel> =
            self.ed.extra.iter().map(|x| Sel { anchor: shift(x.anchor), head: shift(x.head) }).collect();
        for (s, e, t) in &edits {
            self.buf.replace_joined(*s, *e, t, before);
        }
        let len = self.buf.len_chars();
        self.ed.sel = Sel { anchor: sel.anchor.min(len), head: sel.head.min(len) };
        for (x, n) in self.ed.extra.iter_mut().zip(extra) {
            *x = Sel { anchor: n.anchor.min(len), head: n.head.min(len) };
        }
        self.buf.set_after(self.ed.sel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn diag(sl: usize, sc: usize, sev: u8, msg: &str) -> Diag {
        Diag {
            sl,
            sc,
            el: sl,
            ec: sc + 1,
            severity: sev,
            message: msg.into(),
            code: String::new(),
            source: String::new(),
        }
    }

    #[test]
    fn rulers_parse_from_array_number_and_text() {
        assert_eq!(parse_rulers(&json!([120, 80, 80, 0, 5000])), vec![80, 120]);
        assert_eq!(parse_rulers(&json!(100)), vec![100]);
        assert_eq!(parse_rulers(&json!("80, 120;  100")), vec![80, 100, 120]);
        assert_eq!(parse_rulers(&json!(null)), Vec::<usize>::new());
        assert_eq!(parse_rulers(&json!("x, -3")), Vec::<usize>::new());
        assert_eq!(parse_rulers(&json!((1..=20).collect::<Vec<_>>())).len(), MAX_RULERS);
        assert_eq!(rulers_text(&[80, 120]), "80, 120");
        assert_eq!(merge_rulers(&[80, 120], Some(100)), vec![80, 100, 120]);
        assert_eq!(merge_rulers(&[80], Some(80)), vec![80]);
        assert_eq!(merge_rulers(&[], None), Vec::<usize>::new());
    }

    #[test]
    fn ruler_positions_follow_the_cell_grid_and_the_scroll() {
        // Text area from x=40 to x=440, 8 px cells.
        assert_eq!(ruler_xs(&[10, 20, 80], 40, 440, 8.0, 0.0), vec![120, 200]);
        // Scrolled 100 px right: column 20 moves to 100, column 10 (x=20) is off to the left.
        assert_eq!(ruler_xs(&[10, 20, 50], 40, 440, 8.0, 100.0), vec![100, 340]);
        // Fractional cells round like the text grid does.
        assert_eq!(ruler_xs(&[3], 0, 100, 7.6, 0.0), vec![23]);
    }

    #[test]
    fn worst_diagnostic_per_line_prefers_severity_then_position() {
        let ds = vec![
            diag(1, 0, 2, "w1"),
            diag(1, 4, 1, "e1"),
            diag(1, 8, 1, "e2"),
            diag(3, 0, 4, "hint"),
            diag(4, 2, 3, "i"),
        ];
        let got: Vec<(usize, &str)> =
            worst_per_line(&ds, 0, 10, None).into_iter().map(|(l, i)| (l, ds[i].message.as_str())).collect();
        assert_eq!(got, vec![(1, "e1"), (4, "i")], "hints never show; first error of line 1 wins");
        assert_eq!(worst_per_line(&ds, 2, 10, None).len(), 1, "lines before the window are skipped");
        assert_eq!(worst_per_line(&ds, 0, 4, None).len(), 1, "lines after the window too");
        let only: Vec<usize> = worst_per_line(&ds, 0, 10, Some(4)).into_iter().map(|(l, _)| l).collect();
        assert_eq!(only, vec![4], "current line only");
        assert!(worst_per_line(&ds, 0, 10, Some(2)).is_empty());
    }

    #[test]
    fn inline_slot_needs_room_after_the_text() {
        // Text ends at 300, 24 px gap, room until 600.
        assert_eq!(inline_slot(300, 600, 24, 48), Some((324, 276)));
        // A line that (almost) fills the row keeps its text uncovered.
        assert_eq!(inline_slot(560, 600, 24, 48), None);
        assert_eq!(inline_slot(700, 600, 24, 48), None, "text past the edge (scrolled)");
    }

    #[test]
    fn messages_are_cut_to_the_room_left() {
        let mut m = |s: &str| s.chars().count() as i32 * 10; // 10 px per char
        assert_eq!(fit_message("Cannot find name 'x'.", 400, &mut m).as_deref(), Some("Cannot find name 'x'."));
        assert_eq!(fit_message("Cannot find name 'x'.", 100, &mut m).as_deref(), Some("Cannot fi…"));
        assert_eq!(fit_message("abc def", 50, &mut m).as_deref(), Some("abc…"), "no space before the ellipsis");
        assert_eq!(fit_message("\n  first   line \nsecond", 400, &mut m).as_deref(), Some("first line"));
        assert_eq!(fit_message("long message", 30, &mut m), None, "too little room says nothing");
        assert_eq!(fit_message("  \n ", 400, &mut m), None);
    }

    #[test]
    fn highlights_parse_with_kinds() {
        let v = json!([
            {"range": {"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 7}}, "kind": 3},
            {"range": {"start": {"line": 2, "character": 6}, "end": {"line": 2, "character": 9}}, "kind": 2},
            {"range": {"start": {"line": 3, "character": 0}, "end": {"line": 3, "character": 3}}},
            {"bogus": true}
        ]);
        assert_eq!(parse_highlights(&v), vec![(1, 4, 1, 7, true), (2, 6, 2, 9, false), (3, 0, 3, 3, false)]);
        assert!(parse_highlights(&Value::Null).is_empty());
    }

    #[test]
    fn word_under_the_caret() {
        let b = Buffer::from_str("let foo_1 = $x + 2;");
        assert_eq!(word_range(&b, 5), Some((4, 9)));
        assert_eq!(word_range(&b, 9), Some((4, 9)), "caret right after the word");
        assert_eq!(word_range(&b, 12), Some((12, 14)), "$ is part of identifiers");
        assert_eq!(word_range(&b, 11), None, "between '=' and ' '");
    }

    #[test]
    fn word_patterns() {
        let p = WordPattern::parse("[a-zA-Z0-9:\\-\\._$]*");
        assert!(p.allows("my-el.x:y_$1") && p.allows(""));
        assert!(!p.allows("div ") && !p.allows("a>"));
        let p = WordPattern::parse("^[\\w-]+$");
        assert!(p.allows("x-y_1") && !p.allows("x.y"));
        let d = WordPattern::parse("(?:[a-z]|x)+"); // not a plain class: default tag-name set
        assert_eq!(d, WordPattern::default());
        assert!(d.allows("Foo.Bar") && !d.allows("a b"));
    }

    #[test]
    fn linked_ranges_parse() {
        let v = json!({"ranges": [
            {"start": {"line": 0, "character": 17}, "end": {"line": 0, "character": 20}},
            {"start": {"line": 0, "character": 52}, "end": {"line": 0, "character": 55}}
        ], "wordPattern": "[a-z]*"});
        let (r, p) = parse_linked(&v).unwrap();
        assert_eq!(r, vec![(0, 17, 0, 20), (0, 52, 0, 55)]);
        assert!(p.allows("abc") && !p.allows("A"));
        assert!(
            parse_linked(
                &json!({"ranges": [{"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 2}}]})
            )
            .is_none()
        );
        assert!(parse_linked(&Value::Null).is_none());
    }

    /// A document with `<div>x</div>` linked, the caret at `at`.
    fn tagged(text: &str, at: usize) -> Doc {
        let mut d = Doc::new(Buffer::from_str(text), None, Some("html"), "//", 8.0);
        let open = text.find('<').unwrap() + 1;
        let close = text.find("</").unwrap() + 2;
        let n = text[open..].find(['>', ' ']).unwrap();
        d.assist.linked = Some(Linked {
            ranges: vec![(open, open + n), (close, close + n)],
            pattern: WordPattern::default(),
            ver: d.buf.version(),
        });
        d.ed.click(&mut d.buf, at, false);
        d.sync_edits();
        d
    }

    #[test]
    fn typing_in_the_opening_tag_renames_the_closing_one_in_one_undo_step() {
        let mut d = tagged("<div>x</div>", 4);
        for ch in "ider".chars() {
            d.ed.type_char(&mut d.buf, ch);
            d.sync_edits();
        }
        assert_eq!(d.buf.to_string(), "<divider>x</divider>");
        assert_eq!(d.ed.head(), 8, "the caret stays where the user typed");
        d.ed.backspace(&mut d.buf);
        d.sync_edits();
        assert_eq!(d.buf.to_string(), "<divide>x</divide>");
        d.ed.undo(&mut d.buf);
        d.sync_edits();
        assert_eq!(d.buf.to_string(), "<divider>x</divider>", "the backspace and its mirror undo together");
        d.ed.undo(&mut d.buf);
        d.sync_edits();
        assert_eq!(d.buf.to_string(), "<div>x</div>", "the typing run and its mirror undo together");
        d.ed.redo(&mut d.buf);
        d.sync_edits();
        assert_eq!(d.buf.to_string(), "<divider>x</divider>");
    }

    #[test]
    fn editing_the_closing_tag_renames_the_opening_one_and_the_caret_follows() {
        let mut d = tagged("<p>x</p>", 7); // after "</p"
        d.ed.backspace(&mut d.buf);
        d.sync_edits();
        assert_eq!(d.buf.to_string(), "<>x</>");
        for ch in "span".chars() {
            d.ed.type_char(&mut d.buf, ch);
            d.sync_edits();
        }
        assert_eq!(d.buf.to_string(), "<span>x</span>");
        assert_eq!(d.ed.head(), 13, "caret after the closing name it was typed into");
        assert_eq!(d.assist.linked.as_ref().unwrap().ranges, vec![(1, 5), (9, 13)]);
    }

    #[test]
    fn a_space_or_an_edit_across_the_edge_breaks_the_link() {
        let mut d = tagged("<div>x</div>", 4);
        d.ed.type_char(&mut d.buf, ' ');
        d.sync_edits();
        assert_eq!(d.buf.to_string(), "<div >x</div>", "attributes start: the closing tag is left alone");
        assert!(d.assist.linked.is_none());

        let mut d = tagged("<div>x</div>", 2);
        let r = d.assist.linked.clone().unwrap();
        d.ed.sel = Sel { anchor: 0, head: 3 }; // "<di" crosses the start of the opening name
        d.ed.type_char(&mut d.buf, 'z');
        d.sync_edits();
        assert_eq!(d.buf.to_string(), "zv>x</div>");
        assert!(d.assist.linked.is_none(), "{r:?}");
    }

    #[test]
    fn edits_elsewhere_shift_the_ranges() {
        let mut d = tagged("<a>x</a>", 3); // caret before the 'x'
        d.ed.type_char(&mut d.buf, 'y');
        d.sync_edits();
        assert_eq!(d.buf.to_string(), "<a>yx</a>");
        assert_eq!(d.assist.linked.as_ref().unwrap().ranges, vec![(1, 2), (7, 8)]);
        d.ed.click(&mut d.buf, 2, false);
        d.ed.type_char(&mut d.buf, 'b');
        d.sync_edits();
        assert_eq!(d.buf.to_string(), "<ab>yx</ab>");
    }

    #[test]
    fn undo_of_an_unlinked_edit_never_mirrors() {
        // Both names were edited by hand before any link existed (two undo steps).
        let mut d = Doc::new(Buffer::from_str("<b>x</b>"), None, Some("html"), "//", 8.0);
        d.ed.click(&mut d.buf, 2, false);
        d.ed.type_char(&mut d.buf, 'r');
        d.sync_edits();
        d.buf.close_group();
        d.ed.click(&mut d.buf, 8, false);
        d.ed.type_char(&mut d.buf, 'r');
        d.sync_edits();
        d.buf.close_group();
        assert_eq!(d.buf.to_string(), "<br>x</br>");
        let ver = d.buf.version();
        d.assist.linked = Some(Linked { ranges: vec![(1, 3), (7, 9)], pattern: WordPattern::default(), ver });
        d.ed.undo(&mut d.buf);
        d.sync_edits();
        assert_eq!(d.buf.to_string(), "<br>x</b>", "undo restores exactly, nothing mirrored");
        assert!(d.assist.linked.is_none());
    }
}
