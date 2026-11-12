//! Right-to-left and bidirectional text (Arabic, Hebrew, Persian, ...).
//!
//! The editor lays code out on a monospace grid: logical char `i` of a row sits in cell `i` (a tab
//! spans several). A line that holds a strong right-to-left char goes through the Unicode
//! Bidirectional Algorithm instead (UAX #9, the `unicode-bidi` crate that cosmic-text uses too):
//! its chars keep one cell each but sit in *visual* order, so caret, selection and click mapping
//! stay cell arithmetic ([`Row`]). Pure left-to-right lines never get that far: [`has_rtl`] is a
//! byte scan that turns them away before anything is allocated.
//!
//! Editing stays logical: arrows, Home/End and deletion walk the text order, only drawing and
//! hit-testing use the visual positions computed here. The paragraph direction of a line comes
//! from its first strong char, or is forced to left-to-right by `bidi_paragraph: "ltr"`.
//!
//! Prose (Markdown preview, hover cards) is laid out word by word; [`place`] orders the words of
//! one wrapped line visually.

use unicode_bidi::{BidiClass, Level, ParagraphBidiInfo, bidi_class};

/// Paragraph direction of a line (`bidi_paragraph` setting).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Para {
    /// The first strong char decides (UAX #9 rules P2/P3).
    #[default]
    Auto,
    /// Always left to right; right-to-left runs are still reordered inside the line.
    Ltr,
}

impl Para {
    pub fn from_name(s: &str) -> Para {
        if s == "ltr" { Para::Ltr } else { Para::Auto }
    }
}

/// `bidi_paragraph` config values; anything else falls back to "auto".
pub fn normalize(v: Option<&str>) -> String {
    if v == Some("ltr") { "ltr".into() } else { "auto".into() }
}

#[cfg(not(test))]
mod store {
    use std::sync::atomic::{AtomicBool, Ordering};
    static LTR: AtomicBool = AtomicBool::new(false);
    pub fn get() -> bool {
        LTR.load(Ordering::Relaxed)
    }
    pub fn set(v: bool) {
        LTR.store(v, Ordering::Relaxed)
    }
}

// Tests run in parallel: each thread keeps its own setting.
#[cfg(test)]
mod store {
    use std::cell::Cell;
    thread_local! {
        static LTR: Cell<bool> = const { Cell::new(false) };
    }
    pub fn get() -> bool {
        LTR.with(Cell::get)
    }
    pub fn set(v: bool) {
        LTR.with(|c| c.set(v))
    }
}

/// The paragraph direction setting in effect.
pub fn para() -> Para {
    if store::get() { Para::Ltr } else { Para::Auto }
}

pub fn set_para(p: Para) {
    store::set(p == Para::Ltr)
}

/// True for chars of the right-to-left blocks (Hebrew, Arabic, Syriac, Thaana, N'Ko, Samaritan,
/// Mandaic, their presentation forms and the historic RTL scripts of the SMP) and the explicit
/// RTL marks and overrides.
pub fn is_rtl_char(c: char) -> bool {
    matches!(
        c as u32,
        0x0590..=0x08FF
            | 0xFB1D..=0xFDFF
            | 0xFE70..=0xFEFF
            | 0x10800..=0x10FFF
            | 0x1E800..=0x1EFFF
            | 0x200F
            | 0x202B
            | 0x202E
            | 0x2067
    )
}

/// Does `s` need the bidirectional layout? Every RTL char is encoded with a UTF-8 lead byte of
/// 0xD6 or more, so ASCII and Latin text is rejected by a plain byte scan.
pub fn has_rtl(s: &str) -> bool {
    s.bytes().any(|b| b >= 0xD6) && s.chars().any(is_rtl_char)
}

fn strong(c: char) -> Option<bool> {
    match bidi_class(c) {
        BidiClass::L => Some(false),
        BidiClass::R | BidiClass::AL => Some(true),
        _ => None,
    }
}

/// Direction of the first strong char of `s` (`Some(true)` = right to left), `None` without one.
pub fn first_strong_rtl(s: &str) -> Option<bool> {
    s.chars().find_map(strong)
}

/// Byte offsets of chars `lo` and `hi` of `s` (the length past its end).
pub fn byte_range(s: &str, lo: usize, hi: usize) -> (usize, usize) {
    let mut it = s.char_indices().map(|(b, _)| b).chain(std::iter::once(s.len()));
    let a = it.clone().nth(lo).unwrap_or(s.len());
    let b = it.nth(hi).unwrap_or(s.len());
    (a, b)
}

/// Visual layout of one row (a line, or one wrap segment of it) that holds right-to-left text.
/// Indices are chars of the row; cells count from the row's left edge.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// First cell and cell count of each char (a tab spans to the next tab stop, in visual order).
    pub cells: Vec<(usize, usize)>,
    /// Each char's resolved embedding level is odd: it runs right to left.
    pub rtl: Vec<bool>,
    /// The char shown in each visual slot, left to right.
    pub order: Vec<usize>,
    /// Cells the row spans.
    pub width: usize,
    /// Paragraph direction of the whole line.
    pub para_rtl: bool,
}

/// Lays out chars `c0..c1` of `line` (the whole logical line: embedding levels are resolved over
/// the paragraph, then each row is reordered on its own, UAX #9 L1/L2). `None` when the line has
/// no right-to-left char: the caller keeps its left-to-right fast path.
pub fn row(line: &str, c0: usize, c1: usize, tab_w: usize, para: Para) -> Option<Row> {
    if !has_rtl(line) {
        return None;
    }
    let level = match para {
        Para::Auto => None,
        Para::Ltr => Some(Level::ltr()),
    };
    let info = ParagraphBidiInfo::new(line, level);
    let starts: Vec<usize> = line.char_indices().map(|(b, _)| b).collect();
    let n = starts.len();
    let c1 = c1.min(n);
    let c0 = c0.min(c1);
    let byte = |c: usize| starts.get(c).copied().unwrap_or(line.len());
    let (b0, b1) = (byte(c0), byte(c1));
    let levels = info.reordered_levels_per_char(b0..b1);
    let lv = &levels[c0..c1];
    let order = if lv.is_empty() { Vec::new() } else { ParagraphBidiInfo::reorder_visual(lv) };
    let chars: Vec<char> = line[b0..b1].chars().collect();
    let tab_w = tab_w.max(1);
    let mut cells = vec![(0usize, 1usize); chars.len()];
    let mut x = 0usize;
    for &i in &order {
        let w = if chars[i] == '\t' { tab_w - x % tab_w } else { 1 };
        cells[i] = (x, w);
        x += w;
    }
    Some(Row {
        cells,
        rtl: lv.iter().map(|l| l.is_rtl()).collect(),
        order,
        width: x,
        para_rtl: info.paragraph_level.is_rtl(),
    })
}

impl Row {
    /// Chars in the row.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The edge of char `i` where its reading starts (right edge for a right-to-left char).
    fn leading(&self, i: usize) -> usize {
        let (x, w) = self.cells[i];
        if self.rtl[i] { x + w } else { x }
    }

    /// The edge of char `i` where its reading ends.
    fn trailing(&self, i: usize) -> usize {
        let (x, w) = self.cells[i];
        if self.rtl[i] { x } else { x + w }
    }

    /// Cell boundary of the caret at logical position `col` (0..=len): right after the char
    /// before it, on that char's reading side, so typed text grows where the caret is; position
    /// 0 sits where the first char starts.
    pub fn caret(&self, col: usize) -> usize {
        match (self.len(), col) {
            (0, _) => 0,
            (_, 0) => self.leading(0),
            (n, c) => self.trailing(c.min(n) - 1),
        }
    }

    /// Direction of the text at caret position `col` (the char [`caret`](Self::caret) follows).
    pub fn caret_rtl(&self, col: usize) -> bool {
        match (self.len(), col) {
            (0, _) => self.para_rtl,
            (_, 0) => self.rtl[0],
            (n, c) => self.rtl[c.min(n) - 1],
        }
    }

    /// First cell of char `col`, or the row's width past its end (where after-text marks go).
    pub fn cell(&self, col: usize) -> usize {
        self.cells.get(col).map_or(self.width, |c| c.0)
    }

    /// Logical caret position for a click at cell coordinate `x` (fractional: 2.5 is the middle
    /// of the third cell). The half of a char nearer to where its reading starts puts the caret
    /// before it, the other half after it.
    pub fn hit(&self, x: f64) -> usize {
        let n = self.len();
        if n == 0 {
            return 0;
        }
        let k = self.order.partition_point(|&i| ((self.cells[i].0 + self.cells[i].1) as f64) <= x).min(n - 1);
        let i = self.order[k];
        let (cx, w) = self.cells[i];
        let left_half = x < cx as f64 + w as f64 / 2.0;
        if left_half != self.rtl[i] { i } else { i + 1 }
    }

    /// Visual cell ranges `[start, end)` covered by chars `a..b`, left to right, merged where
    /// they touch (a selection across direction runs may come out in several pieces).
    pub fn spans(&self, a: usize, b: usize) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for &i in &self.order {
            if i < a || i >= b {
                continue;
            }
            let (x, w) = self.cells[i];
            match out.last_mut() {
                Some(l) if l.1 == x => l.1 = x + w,
                _ => out.push((x, x + w)),
            }
        }
        out
    }

    /// Chars `lo..hi` worth shaping to paint cells `v0..v1`: those with a cell inside, plus
    /// `pad` chars of context on each side (Arabic letters join their neighbours).
    pub fn window(&self, v0: usize, v1: usize, pad: usize) -> Option<(usize, usize)> {
        let (mut lo, mut hi) = (usize::MAX, 0);
        for (i, &(x, w)) in self.cells.iter().enumerate() {
            if x + w > v0 && x < v1 {
                lo = lo.min(i);
                hi = hi.max(i + 1);
            }
        }
        (lo < hi).then(|| (lo.saturating_sub(pad), (hi + pad).min(self.len())))
    }
}

/// Left x of each inline unit (word, space, code chip, ...) of one wrapped line of prose, laid out
/// in visual order and packed from 0. `texts` give the units' bidi classes (the first strong char
/// of a unit decides its level), `widths` their widths; `para_rtl` is the paragraph direction.
/// Right-to-left paragraphs are right-aligned by the caller.
pub fn place(texts: &[&str], widths: &[f32], para_rtl: bool) -> Vec<f32> {
    let n = texts.len().min(widths.len());
    if n == 0 {
        return Vec::new();
    }
    let mut joined = String::new();
    let mut starts = Vec::with_capacity(n + 1);
    for t in &texts[..n] {
        starts.push(joined.len());
        joined.push_str(if t.is_empty() { " " } else { t });
    }
    starts.push(joined.len());
    let level = if para_rtl { Level::rtl() } else { Level::ltr() };
    let info = ParagraphBidiInfo::new(&joined, Some(level));
    let lv = info.reordered_levels(0..joined.len());
    let unit_lv: Vec<Level> = (0..n)
        .map(|u| {
            let seg = &joined[starts[u]..starts[u + 1]];
            let off = seg.char_indices().find(|(_, c)| strong(*c).is_some()).map_or(0, |(b, _)| b);
            lv[starts[u] + off]
        })
        .collect();
    let order = ParagraphBidiInfo::reorder_visual(&unit_lv);
    let mut xs = vec![0.0f32; n];
    let mut x = 0.0f32;
    for &u in &order {
        xs[u] = x;
        x += widths[u];
    }
    xs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row's text in visual order (what the screen shows, left to right).
    fn visual(line: &str, r: &Row) -> String {
        let chars: Vec<char> = line.chars().collect();
        r.order.iter().map(|&i| chars[i]).collect()
    }

    fn whole(line: &str) -> Row {
        row(line, 0, usize::MAX, 4, Para::Auto).expect("has RTL")
    }

    #[test]
    fn detection_is_cheap_and_exact() {
        assert!(!has_rtl("fn main() { println!(\"olá, привет, ü\"); }"));
        assert!(!has_rtl("րու: Armenian shares the lead byte"));
        assert!(!has_rtl("日本語 → ✓"));
        assert!(has_rtl("let s = \"שלום\";"));
        assert!(has_rtl("// مرحبا"));
        assert!(has_rtl("سلام دنیا"));
        assert!(has_rtl("a\u{200F}b"), "RLM forces the bidi path");
        assert!(row("plain ascii", 0, 5, 4, Para::Auto).is_none(), "LTR lines keep the fast path");
        assert_eq!(first_strong_rtl("  123 שלום abc"), Some(true));
        assert_eq!(first_strong_rtl("(abc) שלום"), Some(false));
        assert_eq!(first_strong_rtl("123 ..."), None);
    }

    #[test]
    fn hebrew_with_numbers_and_latin() {
        // RTL paragraph: Hebrew runs right to left, the number and the Latin word keep their order.
        let line = "שלום 123 abc";
        let r = whole(line);
        assert!(r.para_rtl);
        assert_eq!(visual(line, &r), "abc 123 םולש");
        // Logical char 0 (ש) is the rightmost cell, the last char (c) sits at cell 2.
        assert_eq!(r.cells[0], (11, 1));
        assert_eq!(r.cells[11], (2, 1));
        // The digits stay in reading order inside the RTL line.
        assert_eq!((r.cells[5].0, r.cells[6].0, r.cells[7].0), (4, 5, 6));
        assert_eq!(r.width, line.chars().count());
    }

    #[test]
    fn ltr_paragraph_with_hebrew_word() {
        let line = "let x = \"שלום\";";
        let r = whole(line);
        assert!(!r.para_rtl);
        assert_eq!(visual(line, &r), "let x = \"םולש\";");
        // The code around stays where it was: only the RTL run is mirrored in place.
        assert_eq!(r.cells[0], (0, 1));
        assert_eq!(r.cells[9], (12, 1), "ש, first of the word, is drawn at its right end");
        assert_eq!(r.cells[12], (9, 1));
    }

    #[test]
    fn arabic_with_punctuation() {
        // "مرحبا، عالم!" : the Arabic comma and the exclamation mark follow the RTL flow.
        let line = "مرحبا، عالم!";
        let r = whole(line);
        assert!(r.para_rtl);
        assert_eq!(visual(line, &r), "!ملاع ،ابحرم");
        // A forced LTR paragraph leaves the trailing "!" at the right end.
        let l = row(line, 0, usize::MAX, 4, Para::Ltr).unwrap();
        assert!(!l.para_rtl);
        assert_eq!(visual(line, &l), "ملاع ،ابحرم!");
    }

    #[test]
    fn caret_follows_the_char_before_it() {
        let line = "abc אבג";
        let r = whole(line);
        // Visual: a b c ␠ ג ב א  (cells 0..7); א (char 4) is at cell 6.
        assert_eq!(visual(line, &r), "abc גבא");
        assert_eq!(r.caret(0), 0);
        assert_eq!(r.caret(3), 3, "after 'c'");
        assert_eq!(r.caret(4), 4, "after the space, before the Hebrew word");
        assert_eq!(r.caret(5), 6, "after א: its left edge");
        assert_eq!(r.caret(6), 5);
        assert_eq!(r.caret(7), 4, "end of the line: left end of the Hebrew word, where typing continues");
        assert!(!r.caret_rtl(4) && r.caret_rtl(5) && r.caret_rtl(7));
        // Pure RTL: position 0 is at the right end.
        let h = whole("אבג");
        assert_eq!((h.caret(0), h.caret(1), h.caret(3)), (3, 2, 0));
        assert!(h.caret_rtl(0));
    }

    #[test]
    fn click_maps_visual_x_to_logical_offsets() {
        let line = "abc אבג";
        let r = whole(line);
        assert_eq!(r.hit(-3.0), 0);
        assert_eq!(r.hit(0.2), 0);
        assert_eq!(r.hit(0.8), 1);
        assert_eq!(r.hit(2.9), 3);
        // Cell 6 holds א (char 4): its right half is before it, its left half after it.
        assert_eq!(r.hit(6.8), 4);
        assert_eq!(r.hit(6.2), 5);
        // Cell 4 holds ג (char 6, the last): its left half is the end of the line.
        assert_eq!(r.hit(4.1), 7);
        assert_eq!(r.hit(99.0), 4, "past the right end: before the rightmost char (א)");
        // Clicks and carets agree: the caret of every hit lands on the clicked boundary (the right
        // end, 7, is the leading edge of א, which shares position 4 with the end of the space).
        for v in 0..7 {
            let c = r.hit(v as f64);
            assert_eq!(r.caret(c), v, "boundary {v} -> col {c}");
        }
    }

    #[test]
    fn selections_over_mixed_runs_split_visually() {
        let line = "abc אבג def";
        let r = whole(line);
        assert_eq!(visual(line, &r), "abc גבא def");
        // "c א" (chars 2..5) = 'c', space, א: cells 2..4 and א's cell 6 -> two pieces.
        assert_eq!(r.spans(2, 5), vec![(2, 4), (6, 7)]);
        // The whole Hebrew word is contiguous.
        assert_eq!(r.spans(4, 7), vec![(4, 7)]);
        assert_eq!(r.spans(0, 11), vec![(0, 11)]);
        assert!(r.spans(3, 3).is_empty());
    }

    #[test]
    fn wrapped_rows_reorder_on_their_own() {
        // Two rows of one RTL line: each row is reordered alone (levels come from the whole line).
        let line = "אבג דהו";
        let first = row(line, 0, 4, 4, Para::Auto).unwrap();
        let second = row(line, 4, 7, 4, Para::Auto).unwrap();
        assert_eq!(first.width, 4);
        assert_eq!(first.order, vec![3, 2, 1, 0]);
        assert_eq!(second.order, vec![2, 1, 0]);
        assert!(second.para_rtl);
    }

    #[test]
    fn tabs_expand_in_visual_order() {
        let line = "\tאב";
        let r = whole(line);
        // RTL paragraph: the tab (a segment separator at paragraph level) is visually last.
        assert_eq!(r.order, vec![2, 1, 0]);
        assert_eq!(r.cells[0], (2, 2), "tab from cell 2 to the stop at 4");
        assert_eq!(r.width, 4);
    }

    #[test]
    fn window_and_cells() {
        let line = "abc אבג def";
        let r = whole(line);
        assert_eq!(r.window(0, 3, 0), Some((0, 3)));
        assert_eq!(r.window(6, 7, 0), Some((4, 5)), "cell 6 is א");
        assert_eq!(r.window(6, 7, 2), Some((2, 7)));
        assert_eq!(r.window(50, 60, 2), None);
        assert_eq!(r.cell(4), 6);
        assert_eq!(r.cell(99), 11);
        assert_eq!(byte_range(line, 4, 7), (4, 10));
        assert_eq!(byte_range(line, 0, 99), (0, line.len()));
    }

    #[test]
    fn prose_units_in_visual_order() {
        // "שלום API עולם" in an RTL paragraph: words right to left, the Latin word in between.
        let texts = ["שלום", " ", "API", " ", "עולם"];
        let widths = [40.0, 5.0, 30.0, 5.0, 50.0];
        let xs = place(&texts, &widths, true);
        assert_eq!(xs, vec![90.0, 85.0, 55.0, 50.0, 0.0]);
        // Two Latin words in an RTL paragraph keep their own order.
        let xs = place(&["שלום", " ", "Hello", " ", "World"], &[40.0, 5.0, 50.0, 5.0, 50.0], true);
        assert!(xs[2] < xs[4], "Hello stays left of World: {xs:?}");
        assert!(xs[0] > xs[4], "the Hebrew word is rightmost");
        // An LTR paragraph keeps logical order for Latin text.
        assert_eq!(place(&["a", " ", "b"], &[1.0, 1.0, 1.0], false), vec![0.0, 1.0, 2.0]);
        assert!(place(&[], &[], true).is_empty());
    }

    #[test]
    fn setting_round_trip() {
        assert_eq!(para(), Para::Auto);
        set_para(Para::from_name("ltr"));
        assert_eq!(para(), Para::Ltr);
        set_para(Para::from_name("bogus"));
        assert_eq!(para(), Para::Auto);
        assert_eq!(normalize(Some("ltr")), "ltr");
        assert_eq!(normalize(Some("rtl")), "auto");
        assert_eq!(normalize(None), "auto");
    }
}
