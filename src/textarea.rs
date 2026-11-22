//! Multi-line text area (the Git panel's commit message box): soft-wrapped visual lines, an
//! anchor + cursor selection, word motions, undo / redo with typing coalesced, and a vertical
//! scroll in pixels.
//!
//! Pure state and logic. The caller measures text (`layout` takes a closure returning the x of
//! every char boundary of a logical line), draws from `lines()` / `sel_spans()` / `caret()`,
//! routes keys through `key` (clipboard requests come back as `KeyAct`) and the pointer through
//! `press` / `drag_to` / `release`. Wrapping is visual only: the text keeps the user's `\n`s.

use winit::keyboard::{Key, NamedKey};

/// Width given to a selected line break at the end of a line.
const NL_W: f32 = 6.0;
/// Undo steps kept.
const UNDO_MAX: usize = 200;

/// One visual (wrapped) line.
#[derive(Clone, Debug, PartialEq)]
pub struct VLine {
    /// Char index of its first char.
    pub start: usize,
    /// Char index after its last char (a `\n` ending the logical line is not part of it).
    pub end: usize,
    /// Wrapped: the next visual line continues the same logical line.
    pub soft: bool,
    /// X of every char boundary `start..=end`, relative to the line start (`xs[0] == 0`).
    pub xs: Vec<f32>,
}

impl VLine {
    pub fn x_of(&self, i: usize) -> f32 {
        let k = i.saturating_sub(self.start).min(self.xs.len() - 1);
        self.xs[k]
    }

    /// Nearest char boundary to `x`: (index, "at the end of this wrapped line").
    fn hit(&self, x: f32) -> (usize, bool) {
        let last = self.xs.len() - 1;
        let mut k = 0;
        while k < last && x > (self.xs[k] + self.xs[k + 1]) / 2.0 {
            k += 1;
        }
        (self.start + k, self.soft && k == last)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum EditKind {
    Type,
    Delete,
    Other,
}

#[derive(Clone, Debug)]
struct Snap {
    chars: Vec<char>,
    cursor: usize,
    anchor: usize,
}

/// What a key did.
#[derive(Debug, PartialEq)]
pub enum KeyAct {
    /// Not a key of the text area (the caller may type `ev.text` or pass it on).
    Ignored,
    Handled,
    /// Put this on the clipboard (copy / cut).
    Copy(String),
    /// Insert the clipboard (`insert`).
    Paste,
}

#[derive(Default)]
pub struct TextArea {
    chars: Vec<char>,
    text: String,
    cursor: usize,
    anchor: usize,
    /// At a soft-wrap boundary the caret shows at the end of the upper visual line.
    after: bool,
    /// Pixel x kept by Up / Down.
    goal_x: Option<f32>,
    /// Pixels scrolled from the top.
    pub scroll: f32,
    /// Scroll the caret into view at the next `set_view`.
    reveal: bool,
    lines: Vec<VLine>,
    width: f32,
    laid: bool,
    line_h: f32,
    view_h: f32,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// Kind of the last edit, None after anything that ends an undo group (moves, undo).
    last: Option<EditKind>,
    /// Cursor after the last edit (a typing group continues only from there).
    last_at: usize,
    /// The last typed text ended with whitespace (the next word starts a new undo group).
    last_ws: bool,
    /// Pointer drag: (granularity 1 char / 2 word / 3 line, origin start, origin end).
    drag: Option<(u32, usize, usize)>,
}

fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// CRLF / CR become LF, tabs four spaces, other control characters are dropped.
fn clean(s: &str) -> String {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str("    "),
            '\n' => out.push('\n'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Greedy wrap of one logical line (`chars`, boundary x's `xs`) at `width`: breaks after spaces
/// (which hang past the edge), hard-breaks words longer than a line. `width <= 0`: no wrapping.
fn wrap_line(chars: &[char], xs: &[f32], width: f32, base: usize, out: &mut Vec<VLine>) {
    let n = chars.len();
    let push = |out: &mut Vec<VLine>, s: usize, e: usize, soft: bool| {
        out.push(VLine { start: base + s, end: base + e, soft, xs: xs[s..=e].iter().map(|x| x - xs[s]).collect() });
    };
    let mut s = 0;
    loop {
        let fits = |e: usize| xs[e] - xs[s] <= width + 0.01;
        if width <= 0.0 || fits(n) {
            return push(out, s, n, false);
        }
        // The longest prefix that fits (at least one char).
        let mut e = s + 1;
        while e < n && fits(e + 1) {
            e += 1;
        }
        let mut cut = e;
        while cut < n && chars[cut].is_whitespace() {
            cut += 1;
        }
        if cut == n {
            return push(out, s, n, false);
        }
        if cut == e
            && let Some(j) = (s + 1..=e).rev().find(|&j| chars[j - 1].is_whitespace())
        {
            cut = j;
        }
        push(out, s, cut, true);
        s = cut;
    }
}

impl TextArea {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replace everything (Amend, commit done): caret at the end, no selection, scroll and undo
    /// history reset.
    pub fn set_text(&mut self, s: &str) {
        self.chars = clean(s).chars().collect();
        self.cursor = self.chars.len();
        self.anchor = self.cursor;
        self.after = false;
        self.goal_x = None;
        self.scroll = 0.0;
        self.undo.clear();
        self.redo.clear();
        self.last = None;
        self.drag = None;
        self.changed();
    }

    /// Selected range, ordered.
    pub fn selection(&self) -> (usize, usize) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.cursor
    }

    fn selected_text(&self) -> String {
        let (a, b) = self.selection();
        self.chars[a..b].iter().collect()
    }

    fn changed(&mut self) {
        self.text = self.chars.iter().collect();
        self.laid = false;
        self.reveal = true;
    }

    // ── layout ──

    /// Wrap at `width` px (`<= 0`: no wrapping). `measure(line)` returns the x of each of the
    /// `n + 1` char boundaries of a logical line. Cached until the text or the width changes.
    pub fn layout(&mut self, width: f32, measure: &mut dyn FnMut(&str) -> Vec<f32>) {
        if self.laid && (width - self.width).abs() < 0.5 {
            return;
        }
        self.width = width;
        self.laid = true;
        self.lines.clear();
        let (chars, out) = (&self.chars, &mut self.lines);
        let mut start = 0;
        for logical in self.text.split('\n') {
            let n = logical.chars().count();
            let mut xs = if n == 0 { vec![0.0] } else { measure(logical) };
            if xs.len() != n + 1 {
                xs = (0..=n).map(|i| i as f32 * 8.0).collect();
            }
            wrap_line(&chars[start..start + n], &xs, width, start, out);
            start += n + 1;
        }
    }

    /// Same width as the last layout (call before keys / pointer events: the text may have
    /// changed since the last frame).
    pub fn relayout(&mut self, measure: &mut dyn FnMut(&str) -> Vec<f32>) {
        let w = self.width;
        self.layout(w, measure);
    }

    /// Safety net for operations that need lines when nobody re-laid the text out.
    fn ensure(&mut self) {
        if !self.laid {
            let w = self.width;
            self.layout(w, &mut |s: &str| (0..=s.chars().count()).map(|i| i as f32 * 8.0).collect());
        }
    }

    pub fn lines(&self) -> &[VLine] {
        &self.lines
    }

    /// Visual line count (at least one).
    pub fn line_count(&self) -> usize {
        self.lines.len().max(1)
    }

    /// Text of a visual line.
    pub fn line_text(&self, l: &VLine) -> String {
        self.chars[l.start..l.end.min(self.chars.len())].iter().collect()
    }

    // ── scrolling ──

    /// Row height and visible height of the text, in px; clamps the scroll and brings the caret
    /// into view when it moved.
    pub fn set_view(&mut self, line_h: f32, view_h: f32) {
        self.line_h = line_h;
        self.view_h = view_h;
        if std::mem::take(&mut self.reveal) {
            self.scroll_to_caret();
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    pub fn max_scroll(&self) -> f32 {
        (self.line_count() as f32 * self.line_h - self.view_h).max(0.0)
    }

    /// Wheel / autoscroll: true when the view moved.
    pub fn scroll_by(&mut self, px: f32) -> bool {
        let before = self.scroll;
        self.scroll = (self.scroll + px).clamp(0.0, self.max_scroll());
        self.scroll != before
    }

    fn scroll_to_caret(&mut self) {
        if self.line_h <= 0.0 || self.view_h <= 0.0 {
            return;
        }
        let top = self.caret_line() as f32 * self.line_h;
        if top < self.scroll {
            self.scroll = top;
        } else if top + self.line_h > self.scroll + self.view_h {
            self.scroll = top + self.line_h - self.view_h;
        }
    }

    // ── caret / selection geometry ──

    fn line_of(&self, i: usize, after: bool) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let k = self.lines.partition_point(|l| l.start <= i).saturating_sub(1);
        if after && k > 0 && self.lines[k].start == i && self.lines[k - 1].soft && self.lines[k - 1].end == i {
            k - 1
        } else {
            k
        }
    }

    /// Visual line of the caret.
    pub fn caret_line(&self) -> usize {
        self.line_of(self.cursor, self.after)
    }

    /// (visual line, x) of the caret.
    pub fn caret(&self) -> (usize, f32) {
        let k = self.caret_line();
        (k, self.lines.get(k).map_or(0.0, |l| l.x_of(self.cursor)))
    }

    /// Selection pieces per visual line: (line, x0, x1). A selected line break adds a sliver.
    pub fn sel_spans(&self) -> Vec<(usize, f32, f32)> {
        let (a, b) = self.selection();
        if a == b {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (k, l) in self.lines.iter().enumerate() {
            if l.end < a || l.start > b {
                continue;
            }
            let (s, e) = (a.max(l.start), b.min(l.end));
            let nl = !l.soft && b > l.end && l.end < self.chars.len();
            if s > e || (s == e && !nl) {
                continue;
            }
            out.push((k, l.x_of(s), l.x_of(e) + if nl { NL_W } else { 0.0 }));
        }
        out
    }

    /// Char boundary under a point relative to the text's top-left corner (view coordinates:
    /// the scroll is added here). Above the text: first line; below: the end.
    fn pos_at(&mut self, x: f32, y: f32) -> (usize, bool) {
        self.ensure();
        let lh = if self.line_h > 0.0 { self.line_h } else { 18.0 };
        let row = ((y + self.scroll) / lh).floor();
        if self.lines.is_empty() {
            return (0, false);
        }
        if row >= self.lines.len() as f32 {
            return (self.chars.len(), false);
        }
        self.lines[row.max(0.0) as usize].hit(x)
    }

    // ── words ──

    fn word_left(&self, i: usize) -> usize {
        let c = &self.chars;
        let mut j = i;
        if j > 0 && c[j - 1] == '\n' {
            return j - 1;
        }
        while j > 0 && c[j - 1] != '\n' && class(c[j - 1]) == 0 {
            j -= 1;
        }
        if j > 0 && c[j - 1] != '\n' {
            let k = class(c[j - 1]);
            while j > 0 && c[j - 1] != '\n' && class(c[j - 1]) == k {
                j -= 1;
            }
        }
        j
    }

    fn word_right(&self, i: usize) -> usize {
        let c = &self.chars;
        let n = c.len();
        let mut j = i;
        if j < n && c[j] == '\n' {
            return j + 1;
        }
        while j < n && c[j] != '\n' && class(c[j]) == 0 {
            j += 1;
        }
        if j < n && c[j] != '\n' {
            let k = class(c[j]);
            while j < n && c[j] != '\n' && class(c[j]) == k {
                j += 1;
            }
        }
        j
    }

    /// The run of same-class chars around `i` (double click).
    fn word_at(&self, i: usize) -> (usize, usize) {
        let c = &self.chars;
        let n = c.len();
        let at = if i < n && c[i] != '\n' {
            i
        } else if i > 0 && c[i - 1] != '\n' {
            i - 1
        } else {
            return (i, i);
        };
        let k = class(c[at]);
        let same = |ch: char| ch != '\n' && class(ch) == k;
        let (mut a, mut b) = (at, at + 1);
        while a > 0 && same(c[a - 1]) {
            a -= 1;
        }
        while b < n && same(c[b]) {
            b += 1;
        }
        (a, b)
    }

    /// The logical line around `i`, with its line break (triple click).
    fn line_at(&self, i: usize) -> (usize, usize) {
        let c = &self.chars;
        let i = i.min(c.len());
        let a = c[..i].iter().rposition(|&ch| ch == '\n').map_or(0, |p| p + 1);
        let b = c[i..].iter().position(|&ch| ch == '\n').map_or(c.len(), |p| i + p + 1);
        (a, b)
    }

    // ── moves ──

    fn move_to(&mut self, i: usize, shift: bool) {
        self.cursor = i.min(self.chars.len());
        if !shift {
            self.anchor = self.cursor;
        }
        self.after = false;
        self.goal_x = None;
        self.reveal = true;
        self.last = None;
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.move_to(self.chars.len(), true);
    }

    fn horizontal(&mut self, dir: i32, ctrl: bool, shift: bool) {
        if !shift && !ctrl && self.has_selection() {
            let (a, b) = self.selection();
            return self.move_to(if dir < 0 { a } else { b }, false);
        }
        let t = match (dir < 0, ctrl) {
            (true, true) => self.word_left(self.cursor),
            (true, false) => self.cursor.saturating_sub(1),
            (false, true) => self.word_right(self.cursor),
            (false, false) => self.cursor + 1,
        };
        self.move_to(t, shift);
    }

    /// Up / Down by visual lines, keeping the pixel column.
    fn vertical(&mut self, dir: isize, shift: bool) {
        self.ensure();
        let (k, x) = self.caret();
        let x = self.goal_x.unwrap_or(x);
        let t = k as isize + dir;
        if t < 0 {
            return self.move_to(0, shift);
        }
        if t >= self.lines.len() as isize {
            return self.move_to(self.chars.len(), shift);
        }
        let (i, after) = self.lines[t as usize].hit(x);
        self.move_to(i, shift);
        self.after = after;
        self.goal_x = Some(x);
    }

    fn home(&mut self, shift: bool) {
        self.ensure();
        let k = self.caret_line();
        let s = self.lines.get(k).map_or(0, |l| l.start);
        self.move_to(s, shift);
    }

    fn end(&mut self, shift: bool) {
        self.ensure();
        let k = self.caret_line();
        let (e, soft) = self.lines.get(k).map_or((self.chars.len(), false), |l| (l.end, l.soft));
        self.move_to(e, shift);
        self.after = soft;
    }

    fn page(&self) -> isize {
        if self.line_h > 0.0 { ((self.view_h / self.line_h) as isize - 1).max(1) } else { 1 }
    }

    // ── edits ──

    fn snap(&self) -> Snap {
        Snap { chars: self.chars.clone(), cursor: self.cursor, anchor: self.anchor }
    }

    fn restore(&mut self, s: Snap) {
        self.chars = s.chars;
        self.cursor = s.cursor.min(self.chars.len());
        self.anchor = s.anchor.min(self.chars.len());
        self.after = false;
        self.goal_x = None;
        self.last = None;
        self.changed();
    }

    /// Replace `a..b` with `s`, recording undo (consecutive typing / deleting is one step).
    fn edit(&mut self, a: usize, b: usize, s: &str, kind: EditKind) {
        let starts_word = s.chars().next().is_some_and(|c| !c.is_whitespace());
        let coalesce = match (self.last, kind) {
            (Some(EditKind::Type), EditKind::Type) => {
                a == b && a == self.last_at && !s.contains('\n') && !(self.last_ws && starts_word)
            }
            (Some(EditKind::Delete), EditKind::Delete) => b == self.last_at || a == self.last_at,
            _ => false,
        };
        if !coalesce {
            self.undo.push(self.snap());
            if self.undo.len() > UNDO_MAX {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        let n = s.chars().count();
        let tail = self.chars.split_off(b);
        self.chars.truncate(a);
        self.chars.extend(s.chars());
        self.chars.extend(tail);
        self.cursor = a + n;
        self.anchor = self.cursor;
        self.after = false;
        self.goal_x = None;
        self.last = Some(kind);
        self.last_at = self.cursor;
        if kind == EditKind::Type {
            self.last_ws = s.chars().next_back().is_some_and(char::is_whitespace);
        }
        self.changed();
    }

    /// Typing (coalesced into undo groups by words); replaces the selection.
    pub fn type_text(&mut self, s: &str) -> bool {
        let s = clean(s);
        if s.is_empty() {
            return false;
        }
        let (a, b) = self.selection();
        self.edit(a, b, &s, EditKind::Type);
        true
    }

    /// Paste: one undo step, replaces the selection.
    pub fn insert(&mut self, s: &str) -> bool {
        let s = clean(s);
        let (a, b) = self.selection();
        if s.is_empty() && a == b {
            return false;
        }
        self.edit(a, b, &s, EditKind::Other);
        true
    }

    fn delete_selection(&mut self) -> bool {
        let (a, b) = self.selection();
        if a == b {
            return false;
        }
        self.edit(a, b, "", EditKind::Other);
        true
    }

    fn backspace(&mut self) {
        if !self.delete_selection() && self.cursor > 0 {
            self.edit(self.cursor - 1, self.cursor, "", EditKind::Delete);
        }
    }

    fn delete(&mut self) {
        if !self.delete_selection() && self.cursor < self.chars.len() {
            self.edit(self.cursor, self.cursor + 1, "", EditKind::Delete);
        }
    }

    fn delete_word(&mut self, dir: i32) {
        if self.delete_selection() {
            return;
        }
        let t = if dir < 0 { self.word_left(self.cursor) } else { self.word_right(self.cursor) };
        if t != self.cursor {
            self.edit(t.min(self.cursor), t.max(self.cursor), "", EditKind::Other);
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else { return false };
        self.redo.push(self.snap());
        self.restore(s);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else { return false };
        self.undo.push(self.snap());
        self.restore(s);
        true
    }

    // ── input ──

    /// Keys of a text area. Plain characters (and Space) are `Ignored`: the caller types the
    /// event's text with `type_text`. Unknown Ctrl / Alt combos are `Ignored` too, so global
    /// shortcuts keep working.
    pub fn key(&mut self, key: &Key, ctrl: bool, shift: bool, alt: bool) -> KeyAct {
        use NamedKey as N;
        let Key::Named(named) = key else {
            let Key::Character(c) = key else { return KeyAct::Ignored };
            if !ctrl || alt {
                return KeyAct::Ignored;
            }
            match c.to_lowercase().as_str() {
                "a" => self.select_all(),
                "c" if !shift => {
                    return if self.has_selection() { KeyAct::Copy(self.selected_text()) } else { KeyAct::Handled };
                }
                "x" if !shift => return self.cut(),
                "v" => return KeyAct::Paste,
                "z" => {
                    if shift {
                        self.redo();
                    } else {
                        self.undo();
                    }
                }
                "y" if !shift => {
                    self.redo();
                }
                _ => return KeyAct::Ignored,
            }
            return KeyAct::Handled;
        };
        if alt {
            return KeyAct::Ignored;
        }
        match named {
            N::ArrowLeft => self.horizontal(-1, ctrl, shift),
            N::ArrowRight => self.horizontal(1, ctrl, shift),
            N::ArrowUp if !ctrl => self.vertical(-1, shift),
            N::ArrowDown if !ctrl => self.vertical(1, shift),
            N::PageUp if !ctrl => self.vertical(-self.page(), shift),
            N::PageDown if !ctrl => self.vertical(self.page(), shift),
            N::Home if ctrl => self.move_to(0, shift),
            N::Home => self.home(shift),
            N::End if ctrl => self.move_to(self.chars.len(), shift),
            N::End => self.end(shift),
            N::Backspace if ctrl => self.delete_word(-1),
            N::Backspace => self.backspace(),
            N::Delete if ctrl => self.delete_word(1),
            N::Delete if shift => return self.cut(),
            N::Delete => self.delete(),
            N::Insert if shift && !ctrl => return KeyAct::Paste,
            N::Insert if ctrl && !shift => {
                return if self.has_selection() { KeyAct::Copy(self.selected_text()) } else { KeyAct::Handled };
            }
            N::Enter if !ctrl => {
                self.type_text("\n");
            }
            _ => return KeyAct::Ignored,
        }
        KeyAct::Handled
    }

    fn cut(&mut self) -> KeyAct {
        if !self.has_selection() {
            return KeyAct::Handled;
        }
        let t = self.selected_text();
        self.delete_selection();
        KeyAct::Copy(t)
    }

    /// Pointer press at (x, y) relative to the text's top-left corner: 1 click places the caret
    /// (Shift extends), 2 select a word, 3 the logical line. Starts a drag.
    pub fn press(&mut self, x: f32, y: f32, clicks: u32, shift: bool) {
        let (i, after) = self.pos_at(x, y);
        self.goal_x = None;
        self.last = None;
        self.reveal = true;
        match clicks {
            1 => {
                if !shift {
                    self.anchor = i;
                }
                self.cursor = i;
                self.after = after;
                self.drag = Some((1, self.anchor, self.anchor));
            }
            n => {
                let (a, b) = if n == 2 { self.word_at(i) } else { self.line_at(i) };
                self.anchor = a;
                self.cursor = b;
                self.after = false;
                self.drag = Some((n.min(3), a, b));
            }
        }
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Pointer moved with the button down: extend the selection (by words / lines after a
    /// double / triple click).
    pub fn drag_to(&mut self, x: f32, y: f32) {
        let Some((g, a0, b0)) = self.drag else { return };
        let (i, after) = self.pos_at(x, y);
        self.reveal = true;
        if g == 1 {
            self.anchor = a0;
            self.cursor = i;
            self.after = after;
            return;
        }
        let (a, b) = if g == 2 { self.word_at(i) } else { self.line_at(i) };
        self.after = false;
        if i < a0 {
            self.anchor = b0;
            self.cursor = a;
        } else {
            self.anchor = a0;
            self.cursor = b.max(b0);
        }
    }

    pub fn release(&mut self) -> bool {
        self.drag.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl TextArea {
        fn len(&self) -> usize {
            self.chars.len()
        }
        fn cursor(&self) -> usize {
            self.cursor
        }
    }

    /// Every char 10 px wide.
    fn mono(s: &str) -> Vec<f32> {
        (0..=s.chars().count()).map(|i| i as f32 * 10.0).collect()
    }

    fn area(text: &str, width: f32) -> TextArea {
        let mut t = TextArea::default();
        t.set_text(text);
        t.layout(width, &mut mono);
        t.set_view(18.0, 54.0);
        t
    }

    fn rows(t: &TextArea) -> Vec<String> {
        t.lines().iter().map(|l| t.line_text(l)).collect()
    }

    fn k(t: &mut TextArea, key: NamedKey, ctrl: bool, shift: bool) -> KeyAct {
        let r = t.key(&Key::Named(key), ctrl, shift, false);
        t.relayout(&mut mono);
        r
    }

    fn ch(t: &mut TextArea, c: &str, shift: bool) -> KeyAct {
        let r = t.key(&Key::Character(c.into()), true, shift, false);
        t.relayout(&mut mono);
        r
    }

    fn typ(t: &mut TextArea, s: &str) {
        for c in s.chars() {
            t.type_text(&c.to_string());
            t.relayout(&mut mono);
        }
    }

    #[test]
    fn wraps_at_spaces_and_hard_breaks_long_words() {
        // 10 chars per line.
        let t = area("hello world foo", 100.0);
        assert_eq!(rows(&t), ["hello ", "world foo"]);
        assert!(t.lines()[0].soft && !t.lines()[1].soft);
        let t = area("abcdefghijklmnopqrstuvwxyz", 100.0);
        assert_eq!(rows(&t), ["abcdefghij", "klmnopqrst", "uvwxyz"], "a word longer than a line is cut");
        let t = area("ab abcdefghijklmnop", 100.0);
        assert_eq!(rows(&t), ["ab ", "abcdefghij", "klmnop"]);
        // Trailing spaces hang past the edge instead of starting a line of blanks.
        let t = area("abcdefghij      x", 100.0);
        assert_eq!(rows(&t), ["abcdefghij      ", "x"]);
        let t = area("abcdefghij      ", 100.0);
        assert_eq!(rows(&t), ["abcdefghij      "], "only spaces left: no empty visual line");
        // Hard line breaks stay; empty lines are lines; nothing visible ever exceeds the width.
        let t = area("one\n\ntwo three four five six", 100.0);
        assert_eq!(rows(&t), ["one", "", "two three ", "four five ", "six"]);
        for l in t.lines() {
            let ink = t.line_text(l).trim_end().chars().count();
            assert!(l.xs[ink] <= 100.0);
        }
        // No width: no wrapping. The stored text keeps only the user's line breaks.
        let t = area("a b c d e f g h i j k l m n", 0.0);
        assert_eq!(t.line_count(), 1);
        assert_eq!(area("x y z w", 30.0).text(), "x y z w");
    }

    #[test]
    fn up_down_move_by_visual_lines_keeping_the_column() {
        let mut t = area("hello world foo\nab\nlonger line here", 100.0);
        assert_eq!(rows(&t), ["hello ", "world foo", "ab", "longer ", "line here"]);
        k(&mut t, NamedKey::ArrowUp, true, false); // Ctrl+Up: not ours
        k(&mut t, NamedKey::Home, true, false);
        k(&mut t, NamedKey::ArrowRight, false, false);
        k(&mut t, NamedKey::ArrowRight, false, false);
        k(&mut t, NamedKey::ArrowRight, false, false);
        assert_eq!(t.cursor(), 3);
        k(&mut t, NamedKey::ArrowDown, false, false);
        assert_eq!((t.cursor(), t.caret_line()), (9, 1), "same x on the wrapped continuation");
        k(&mut t, NamedKey::ArrowDown, false, false);
        assert_eq!((t.cursor(), t.caret_line()), (18, 2), "clamped to the short line");
        k(&mut t, NamedKey::ArrowDown, false, false);
        assert_eq!((t.cursor(), t.caret_line()), (22, 3), "the goal column comes back");
        k(&mut t, NamedKey::ArrowDown, false, false);
        k(&mut t, NamedKey::ArrowDown, false, false);
        assert_eq!(t.cursor(), t.len(), "Down on the last line goes to the end");
        k(&mut t, NamedKey::ArrowUp, false, true);
        assert!(t.has_selection());
        k(&mut t, NamedKey::Home, true, false);
        k(&mut t, NamedKey::ArrowUp, false, false);
        assert_eq!(t.cursor(), 0, "Up on the first line goes home");
    }

    #[test]
    fn home_end_follow_the_visual_line() {
        let mut t = area("hello world foo", 100.0);
        k(&mut t, NamedKey::Home, true, false);
        k(&mut t, NamedKey::End, false, false);
        assert_eq!((t.cursor(), t.caret_line()), (6, 0), "end of the first visual line, caret stays on it");
        assert_eq!(t.caret().1, 60.0);
        k(&mut t, NamedKey::ArrowRight, false, false);
        k(&mut t, NamedKey::End, false, false);
        assert_eq!((t.cursor(), t.caret_line()), (15, 1));
        k(&mut t, NamedKey::Home, false, true);
        assert_eq!(t.selection(), (6, 15));
        k(&mut t, NamedKey::Home, true, true);
        assert_eq!(t.selection(), (0, 15), "Ctrl+Shift+Home extends to the start");
        k(&mut t, NamedKey::End, true, true);
        assert_eq!(t.selection(), (15, 15));
    }

    #[test]
    fn selection_word_motions_and_deletes() {
        let mut t = area("foo bar_baz, qux", 0.0);
        k(&mut t, NamedKey::ArrowLeft, true, false);
        assert_eq!(t.cursor(), 13);
        k(&mut t, NamedKey::ArrowLeft, true, false);
        assert_eq!(t.cursor(), 11, "punctuation is its own word");
        k(&mut t, NamedKey::ArrowLeft, true, true);
        assert_eq!(t.selection(), (4, 11));
        k(&mut t, NamedKey::ArrowLeft, false, false);
        assert_eq!((t.cursor(), t.has_selection()), (4, false), "Left collapses to the start");
        k(&mut t, NamedKey::ArrowRight, true, true);
        k(&mut t, NamedKey::ArrowRight, false, true);
        assert_eq!(t.selection(), (4, 12));
        k(&mut t, NamedKey::Backspace, false, false);
        assert_eq!(t.text(), "foo  qux", "Backspace deletes the selection");
        k(&mut t, NamedKey::End, true, false);
        k(&mut t, NamedKey::Backspace, true, false);
        assert_eq!(t.text(), "foo  ");
        k(&mut t, NamedKey::Home, true, false);
        k(&mut t, NamedKey::Delete, true, false);
        assert_eq!(t.text(), "  ");
        // Ctrl+Backspace at a line start only joins the lines.
        let mut t = area("ab\ncd", 0.0);
        t.press(0.0, 18.0, 1, false);
        assert_eq!(t.cursor(), 3);
        k(&mut t, NamedKey::Backspace, true, false);
        assert_eq!(t.text(), "abcd");
        // Unicode: char boundaries, never bytes.
        let mut t = area("aç é", 0.0);
        k(&mut t, NamedKey::Backspace, false, false);
        k(&mut t, NamedKey::ArrowLeft, false, true);
        assert_eq!(t.selected_text(), " ");
        k(&mut t, NamedKey::ArrowLeft, true, true);
        assert_eq!(t.selected_text(), "aç ");
    }

    #[test]
    fn select_all_then_type_or_paste_replaces_everything() {
        let mut t = area("first line\nsecond", 100.0);
        assert_eq!(ch(&mut t, "a", false), KeyAct::Handled);
        assert_eq!(t.selection(), (0, t.len()));
        typ(&mut t, "x");
        assert_eq!((t.text(), t.cursor()), ("x", 1));
        ch(&mut t, "a", false);
        assert_eq!(ch(&mut t, "c", false), KeyAct::Copy("x".into()));
        assert_eq!(ch(&mut t, "v", true), KeyAct::Paste, "Ctrl+Shift+V pastes too");
        t.insert("pasted\r\ntext\tend");
        assert_eq!(t.text(), "pasted\ntext    end", "CRLF and tabs are normalised");
        k(&mut t, NamedKey::ArrowLeft, true, true);
        assert_eq!(ch(&mut t, "x", false), KeyAct::Copy("end".into()));
        assert_eq!(t.text(), "pasted\ntext    ");
        assert_eq!(ch(&mut t, "x", false), KeyAct::Handled, "nothing selected: nothing cut");
        assert_eq!(ch(&mut t, "p", false), KeyAct::Ignored, "other Ctrl keys stay global");
        assert_eq!(
            t.key(&Key::Character("a".into()), false, false, false),
            KeyAct::Ignored,
            "plain text is typed by the caller"
        );
        assert_eq!(t.key(&Key::Named(NamedKey::Tab), false, false, false), KeyAct::Ignored);
        assert_eq!(t.key(&Key::Named(NamedKey::Enter), true, false, false), KeyAct::Ignored, "Ctrl+Enter commits");
        k(&mut t, NamedKey::Enter, false, false);
        assert_eq!(t.text(), "pasted\ntext    \n");
    }

    #[test]
    fn undo_groups_typing_by_words_and_redo_comes_back() {
        let mut t = area("", 0.0);
        typ(&mut t, "hello world");
        ch(&mut t, "z", false);
        assert_eq!(t.text(), "hello ");
        ch(&mut t, "z", false);
        assert_eq!(t.text(), "");
        ch(&mut t, "z", true);
        assert_eq!(t.text(), "hello ");
        ch(&mut t, "y", false);
        assert_eq!(t.text(), "hello world");
        // A move ends the group; backspaces coalesce; a paste is one step.
        k(&mut t, NamedKey::ArrowLeft, false, false);
        typ(&mut t, "X");
        k(&mut t, NamedKey::End, false, false);
        assert_eq!(t.text(), "hello worlXd");
        k(&mut t, NamedKey::Backspace, false, false);
        k(&mut t, NamedKey::Backspace, false, false);
        assert_eq!(t.text(), "hello worl");
        t.insert(" big paste");
        assert_eq!(t.text(), "hello worl big paste");
        ch(&mut t, "z", false);
        assert_eq!(t.text(), "hello worl");
        ch(&mut t, "z", false);
        assert_eq!(t.text(), "hello worlXd", "the backspaces were one step");
        ch(&mut t, "z", false);
        assert_eq!(t.text(), "hello world");
        typ(&mut t, "!");
        assert_eq!(t.text(), "hello worl!d");
        assert!(!t.redo(), "a new edit drops the redo history");
        // Select all + type is one step back to the old text, Enter starts a group.
        ch(&mut t, "a", false);
        typ(&mut t, "new");
        k(&mut t, NamedKey::Enter, false, false);
        typ(&mut t, "body");
        ch(&mut t, "z", false);
        assert_eq!(t.text(), "new\n");
        ch(&mut t, "z", false);
        assert_eq!(t.text(), "new");
        ch(&mut t, "z", false);
        assert_eq!(t.text(), "hello worl!d", "replacing the selection and typing the word: one step");
        // set_text (Amend, commit) forgets the history.
        t.set_text("other");
        assert!(!t.undo());
    }

    #[test]
    fn mouse_click_double_triple_shift_and_drag() {
        let mut t = area("hello world foo\nsecond line", 100.0);
        t.set_view(18.0, 200.0);
        assert_eq!(t.scroll, 0.0);
        // Rows: "hello " / "world foo" / "second " / "line".
        t.press(24.0, 0.0, 1, false);
        assert_eq!((t.cursor(), t.has_selection()), (2, false));
        t.press(500.0, 0.0, 1, false);
        assert_eq!((t.cursor(), t.caret_line()), (6, 0), "past the end of a wrapped line: its end");
        t.release();
        t.press(10.0, 18.0, 1, true);
        assert_eq!(t.selection(), (6, 7), "Shift+click extends");
        t.press(16.0, 18.0, 2, false);
        assert_eq!(t.selection(), (6, 11), "double click: the word");
        t.drag_to(85.0, 18.0);
        assert_eq!(t.selection(), (6, 15), "dragging extends by words");
        t.drag_to(0.0, 0.0);
        assert_eq!((t.anchor, t.cursor), (11, 0));
        t.release();
        t.press(10.0, 40.0, 3, false);
        assert_eq!(t.selection(), (16, 27), "triple click: the logical line");
        t.press(10.0, 0.0, 3, false);
        assert_eq!(t.selection(), (0, 16), "with its line break");
        t.release();
        t.press(20.0, 0.0, 1, false);
        t.drag_to(20.0, 18.0);
        assert_eq!(t.selection(), (2, 8));
        t.drag_to(0.0, 999.0);
        assert_eq!(t.selection(), (2, t.len()), "below the text: the end");
        assert!(t.release() && !t.dragging());
        // Selection pieces: one per visual line, the selected line break gets a sliver.
        let spans = t.sel_spans();
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0], (0, 20.0, 60.0));
        assert_eq!(spans[1], (1, 0.0, 90.0 + NL_W));
        assert_eq!(spans[3], (3, 0.0, 40.0));
    }

    #[test]
    fn scroll_keeps_the_caret_visible() {
        let mut t = area("1\n2\n3\n4\n5\n6\n7\n8", 100.0);
        // 3 rows of 18 px visible, the caret on row 7.
        assert_eq!(t.scroll, 5.0 * 18.0);
        assert_eq!(t.max_scroll(), 5.0 * 18.0);
        k(&mut t, NamedKey::Home, true, false);
        t.set_view(18.0, 54.0);
        assert_eq!(t.scroll, 0.0);
        assert!(t.scroll_by(30.0) && t.scroll == 30.0, "the wheel scrolls without moving the caret");
        t.scroll_by(-100.0);
        assert_eq!(t.scroll, 0.0);
        assert!(!t.scroll_by(-5.0), "clamped at the top");
        t.scroll_by(1000.0);
        assert_eq!(t.scroll, t.max_scroll());
        t.press(0.0, 0.0, 1, false);
        assert_eq!(t.cursor(), 10, "a click lands on the scrolled row (row 5)");
        k(&mut t, NamedKey::PageDown, false, false);
        t.set_view(18.0, 54.0);
        assert_eq!(t.caret_line(), 7);
        k(&mut t, NamedKey::PageUp, false, false);
        k(&mut t, NamedKey::PageUp, false, false);
        k(&mut t, NamedKey::PageUp, false, false);
        t.set_view(18.0, 54.0);
        assert_eq!((t.caret_line(), t.scroll), (1, 18.0));
        // Shrinking the text pulls the scroll back in range.
        t.set_text("short");
        t.layout(100.0, &mut mono);
        t.set_view(18.0, 54.0);
        assert_eq!(t.scroll, 0.0);
    }
}
