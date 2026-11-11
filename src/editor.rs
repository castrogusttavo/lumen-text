//! Cursor, selection and editing commands. Pure logic over `Buffer`, so it can
//! be unit-tested without a window.

use crate::buffer::{Buffer, EditKind, Sel};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' || c == '$' {
        Class::Word
    } else {
        Class::Punct
    }
}

pub struct Editor {
    pub sel: Sel,
    pub extra: Vec<Sel>, // additional cursors (multi-cursor); `sel` is the primary one
    want: Option<usize>, // desired visual column while moving vertically
    pub indent: String,  // "  ", "    " or "\t"
    pub comment: String, // line-comment prefix, "" if none
    pub tab_w: usize,
    pub jsx: bool, // auto-close tags (tsx/jsx/html)
    /// The last column selection, so Ctrl+Shift+Alt+arrows and Shift+Alt+click keep growing it.
    boxsel: Option<BoxSel>,
}

/// A column (box) selection: corners as (line, visual column) and the cursors it produced. It
/// only counts as current while the cursors are still exactly those (any other move forgets it).
#[derive(Clone, Debug, PartialEq)]
struct BoxSel {
    anchor: (usize, usize),
    active: (usize, usize),
    sels: Vec<Sel>,
}

/// Is `line` inside one of the inclusive `hidden` ranges (lines folded away)?
fn in_ranges(hidden: &[(usize, usize)], line: usize) -> bool {
    hidden.iter().any(|r| r.0 <= line && line <= r.1)
}

/// Visual column of char column `col` (tabs expand to the next tab stop).
static DEFAULT_TAB_W: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(4);

/// Columns a tab character spans in documents opened from now on (the `tab_width` setting).
pub fn default_tab_w() -> usize {
    DEFAULT_TAB_W.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_default_tab_w(n: usize) {
    DEFAULT_TAB_W.store(n.clamp(1, 16), std::sync::atomic::Ordering::Relaxed);
}

pub fn visual_col(line: &str, col: usize, tab_w: usize) -> usize {
    let mut v = 0;
    for ch in line.chars().take(col) {
        v += if ch == '\t' { tab_w - v % tab_w } else { 1 };
    }
    v
}

/// Char column nearest to visual column `vcol` (clamped to the line end).
pub fn col_from_visual(line: &str, vcol: usize, tab_w: usize) -> usize {
    let mut v = 0;
    for (i, ch) in line.chars().enumerate() {
        let w = if ch == '\t' { tab_w - v % tab_w } else { 1 };
        if vcol < v + w {
            return if vcol - v >= w.div_ceil(2) && w > 1 { i + 1 } else { i };
        }
        v += w;
    }
    line.chars().count()
}

fn closer(c: char) -> Option<char> {
    Some(match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    })
}

impl Editor {
    pub fn new(buf: &Buffer, comment: &str) -> Self {
        Self {
            sel: Sel::caret(0),
            extra: Vec::new(),
            want: None,
            indent: detect_indent(buf),
            comment: comment.to_string(),
            tab_w: default_tab_w(),
            jsx: false,
            boxsel: None,
        }
    }

    pub fn head(&self) -> usize {
        self.sel.head
    }

    fn set(&mut self, buf: &mut Buffer, anchor: usize, head: usize, keep_want: bool) {
        self.sel = Sel { anchor, head };
        if !keep_want {
            self.want = None;
        }
        buf.close_group();
    }

    fn move_to(&mut self, buf: &mut Buffer, head: usize, extend: bool, keep_want: bool) {
        let anchor = if extend { self.sel.anchor } else { head };
        self.set(buf, anchor, head, keep_want);
    }

    // ── positions ──

    fn prev_pos(&self, buf: &Buffer, p: usize) -> usize {
        if p == 0 {
            return 0;
        }
        if buf.char_at(p - 1) == Some('\n') && p >= 2 && buf.char_at(p - 2) == Some('\r') { p - 2 } else { p - 1 }
    }

    fn next_pos(&self, buf: &Buffer, p: usize) -> usize {
        if p >= buf.len_chars() {
            return p;
        }
        if buf.char_at(p) == Some('\r') && buf.char_at(p + 1) == Some('\n') { p + 2 } else { p + 1 }
    }

    fn word_left(&self, buf: &Buffer, mut p: usize) -> usize {
        while p > 0 && class(buf.char_at(p - 1).unwrap()) == Class::Space {
            // stop at line boundaries so Ctrl+Left across lines is predictable
            if buf.char_at(p - 1) == Some('\n') {
                return self.prev_pos(buf, p);
            }
            p -= 1;
        }
        if p == 0 {
            return 0;
        }
        let cl = class(buf.char_at(p - 1).unwrap());
        while p > 0 && class(buf.char_at(p - 1).unwrap()) == cl {
            p -= 1;
        }
        p
    }

    /// Ctrl+Right: skip blanks, then move to the end of the next run of same-class chars.
    fn word_right(&self, buf: &Buffer, mut p: usize) -> usize {
        let n = buf.len_chars();
        if p >= n {
            return n;
        }
        if matches!(buf.char_at(p), Some('\n') | Some('\r')) {
            return self.next_pos(buf, p);
        }
        while p < n
            && class(buf.char_at(p).unwrap()) == Class::Space
            && !matches!(buf.char_at(p), Some('\n') | Some('\r'))
        {
            p += 1;
        }
        if p < n && matches!(buf.char_at(p), Some('\n') | Some('\r')) {
            return p;
        }
        if p < n {
            let cl = class(buf.char_at(p).unwrap());
            while p < n && class(buf.char_at(p).unwrap()) == cl {
                p += 1;
            }
        }
        p
    }

    // ── multi-cursor ──

    /// Run a non-editing command (movement/selection) for every cursor.
    pub fn move_multi<F: FnMut(&mut Editor, &mut Buffer)>(&mut self, buf: &mut Buffer, mut f: F) {
        if self.extra.is_empty() {
            return f(self, buf);
        }
        let primary = self.sel;
        let others = std::mem::take(&mut self.extra);
        self.sel = primary;
        f(self, buf);
        let new_primary = self.sel;
        let mut out = Vec::new();
        for s in others {
            self.sel = s;
            f(self, buf);
            out.push(self.sel);
        }
        self.sel = new_primary;
        self.extra = out;
        self.normalize_cursors();
    }

    /// Run an editing command for every cursor, bottom to top so earlier positions stay valid,
    /// as a single undo step. Cursors that end up identical are merged.
    pub fn edit_multi<F: FnMut(&mut Editor, &mut Buffer)>(&mut self, buf: &mut Buffer, kind: EditKind, mut f: F) {
        if self.extra.is_empty() {
            return f(self, buf);
        }
        let before = self.sel;
        let mut sels: Vec<(Sel, bool)> = vec![(self.sel, true)];
        sels.extend(self.extra.iter().map(|s| (*s, false)));
        sels.sort_by(|a, b| b.0.start().cmp(&a.0.start()).then(b.0.end().cmp(&a.0.end())));
        buf.begin_multi(before, kind);
        let mut done: Vec<(Sel, bool)> = Vec::with_capacity(sels.len());
        for (s, primary) in sels {
            let len_before = buf.len_chars() as isize;
            self.sel = s;
            f(self, buf);
            let delta = buf.len_chars() as isize - len_before;
            for (r, _) in done.iter_mut() {
                r.anchor = (r.anchor as isize + delta).max(0) as usize;
                r.head = (r.head as isize + delta).max(0) as usize;
            }
            done.push((self.sel, primary));
        }
        let primary = done.iter().find(|x| x.1).map(|x| x.0).unwrap_or(before);
        self.extra = done.iter().filter(|x| !x.1).map(|x| x.0).collect();
        self.sel = primary;
        self.normalize_cursors();
        buf.end_multi(self.sel, kind);
    }

    /// Drop cursors that coincide with another (e.g. after Backspace merged two carets).
    fn normalize_cursors(&mut self) {
        let mut seen: Vec<Sel> = vec![self.sel];
        self.extra.retain(|s| {
            let dup = seen
                .iter()
                .any(|t| t.start() < s.end().max(s.start()) && s.start() < t.end().max(t.start()) || *t == *s);
            if !dup {
                seen.push(*s);
            }
            !dup
        });
        self.want = None;
    }

    pub fn clear_extra(&mut self) {
        self.extra.clear();
    }

    /// All cursors in document order.
    pub fn all_sels(&self) -> Vec<Sel> {
        let mut v = vec![self.sel];
        v.extend(self.extra.iter().copied());
        v.sort_by_key(|s| (s.start(), s.end()));
        v
    }

    fn next_occurrence(buf: &Buffer, needle: &str, from: usize) -> Option<usize> {
        if needle.is_empty() || needle.contains('\n') {
            return None;
        }
        let (l0, c0) = buf.line_col(from);
        let nlines = buf.line_count();
        for k in 0..=nlines {
            let l = (l0 + k) % nlines;
            let text = buf.line(l);
            let start_byte = if k == 0 { text.char_indices().nth(c0).map_or(text.len(), |(b, _)| b) } else { 0 };
            if let Some(b) = text[start_byte..].find(needle) {
                let col = text[..start_byte + b].chars().count();
                return Some(buf.line_start(l) + col);
            }
            if k == nlines && l == l0 {
                break;
            }
        }
        None
    }

    /// Ctrl+D: select the word under the cursor; again: add the next occurrence as a new cursor.
    pub fn add_next_occurrence(&mut self, buf: &mut Buffer) {
        if self.sel.is_empty() {
            let h = self.sel.head;
            self.select_word_at(buf, h);
            return;
        }
        let needle = buf.slice(self.sel.start(), self.sel.end());
        let after = self.all_sels().iter().map(|s| s.end()).max().unwrap_or(0);
        let Some(p) = Self::next_occurrence(buf, &needle, after) else { return };
        let end = p + needle.chars().count();
        if self.all_sels().iter().any(|s| s.start() == p) {
            return;
        }
        self.extra.push(self.sel);
        self.sel = Sel { anchor: p, head: end };
        self.want = None;
    }

    /// Ctrl+Shift+L: a cursor on every occurrence of the selection (or the word under the cursor).
    pub fn select_all_occurrences(&mut self, buf: &mut Buffer) {
        if self.sel.is_empty() {
            let h = self.sel.head;
            self.select_word_at(buf, h);
        }
        let needle = buf.slice(self.sel.start(), self.sel.end());
        if needle.is_empty() || needle.contains('\n') {
            return;
        }
        let len = needle.chars().count();
        let mut found: Vec<usize> = Vec::new();
        for l in 0..buf.line_count() {
            let text = buf.line(l);
            let mut from = 0;
            while let Some(b) = text[from..].find(&needle) {
                let col = text[..from + b].chars().count();
                found.push(buf.line_start(l) + col);
                from += b + needle.len();
                if found.len() >= 5000 {
                    break;
                }
            }
        }
        if found.len() < 2 {
            return;
        }
        let primary = found.iter().position(|&p| p == self.sel.start()).unwrap_or(0);
        self.sel = Sel { anchor: found[primary], head: found[primary] + len };
        self.extra = found
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != primary)
            .map(|(_, &p)| Sel { anchor: p, head: p + len })
            .collect();
        self.want = None;
    }

    /// Ctrl+Alt+Up/Down: add a cursor on the line above/below the outermost cursor.
    pub fn add_cursor_vertical(&mut self, buf: &mut Buffer, delta: isize) {
        let sels = self.all_sels();
        let edge = if delta < 0 { sels.first() } else { sels.last() }.copied().unwrap_or(self.sel);
        let (l, col) = buf.line_col(edge.head);
        let vcol = visual_col(&buf.line(l), col, self.tab_w);
        let t = l as isize + delta;
        if t < 0 || t as usize >= buf.line_count() {
            return;
        }
        let c = col_from_visual(&buf.line(t as usize), vcol, self.tab_w);
        let p = buf.pos(t as usize, c);
        self.extra.push(self.sel);
        self.sel = Sel::caret(p);
        self.want = None;
        self.normalize_cursors();
    }

    /// Alt+Click: add a cursor at `p`.
    pub fn add_cursor_at(&mut self, p: usize) {
        if self.all_sels().iter().any(|s| s.head == p) {
            return;
        }
        self.extra.push(self.sel);
        self.sel = Sel::caret(p);
        self.want = None;
    }

    /// Text for copy with several cursors: the selections joined by newlines, in document order.
    pub fn copy_text_multi(&self, buf: &Buffer) -> String {
        if self.extra.is_empty() {
            return self.copy_text(buf);
        }
        self.all_sels().iter().map(|s| buf.slice(s.start(), s.end())).collect::<Vec<_>>().join(buf.eol())
    }

    /// Paste; with as many clipboard lines as cursors each cursor gets its own line.
    pub fn paste_multi(&mut self, buf: &mut Buffer, text: &str) {
        if self.extra.is_empty() {
            return self.paste(buf, text);
        }
        let lines: Vec<&str> = text.lines().collect();
        let n = self.extra.len() + 1;
        if lines.len() != n {
            return self.edit_multi(buf, EditKind::Other, |e, b| e.paste(b, text));
        }
        // Assign lines in document order; edit bottom-to-top.
        let order = self.all_sels();
        let rank = |s: &Sel| order.iter().position(|o| o == s).unwrap_or(0);
        let mut per: Vec<(Sel, &str)> = order.iter().zip(lines.iter()).map(|(s, l)| (*s, *l)).collect();
        per.reverse();
        let primary = self.sel;
        let _ = rank;
        let before = self.sel;
        buf.begin_multi(before, EditKind::Other);
        let mut done: Vec<(Sel, bool)> = Vec::new();
        for (s, line) in per {
            let len_before = buf.len_chars() as isize;
            self.sel = s;
            self.paste(buf, line);
            let delta = buf.len_chars() as isize - len_before;
            for (r, _) in done.iter_mut() {
                r.anchor = (r.anchor as isize + delta).max(0) as usize;
                r.head = (r.head as isize + delta).max(0) as usize;
            }
            done.push((self.sel, s == primary));
        }
        self.sel = done.iter().find(|x| x.1).map(|x| x.0).unwrap_or(before);
        self.extra = done.iter().filter(|x| !x.1).map(|x| x.0).collect();
        buf.end_multi(self.sel, EditKind::Other);
        self.normalize_cursors();
    }

    // ── movement ──

    pub fn left(&mut self, buf: &mut Buffer, extend: bool, word: bool) {
        if !extend && !self.sel.is_empty() {
            let s = self.sel.start();
            return self.move_to(buf, s, false, false);
        }
        let h = self.sel.head;
        let t = if word { self.word_left(buf, h) } else { self.prev_pos(buf, h) };
        self.move_to(buf, t, extend, false);
    }

    pub fn right(&mut self, buf: &mut Buffer, extend: bool, word: bool) {
        if !extend && !self.sel.is_empty() {
            let e = self.sel.end();
            return self.move_to(buf, e, false, false);
        }
        let h = self.sel.head;
        let t = if word { self.word_right(buf, h) } else { self.next_pos(buf, h) };
        self.move_to(buf, t, extend, false);
    }

    pub fn vertical(&mut self, buf: &mut Buffer, extend: bool, delta: isize) {
        let (l, col) = buf.line_col(self.sel.head);
        let vcol = match self.want {
            Some(v) => v,
            None => visual_col(&buf.line(l), col, self.tab_w),
        };
        let target = l as isize + delta;
        if target < 0 {
            return self.move_to(buf, 0, extend, false);
        }
        if target as usize >= buf.line_count() {
            let end = buf.len_chars();
            return self.move_to(buf, end, extend, false);
        }
        let tl = target as usize;
        let c = col_from_visual(&buf.line(tl), vcol, self.tab_w);
        let p = buf.pos(tl, c);
        self.want = Some(vcol);
        self.move_to(buf, p, extend, true);
    }

    /// Goal column kept while moving vertically.
    pub fn want_col(&self) -> Option<usize> {
        self.want
    }

    /// Move the head to `p` as a vertical move; `vcol` is the goal column to remember
    /// (None forgets it). Used by the word-wrap/fold aware movement in docview.rs.
    pub fn vertical_to(&mut self, buf: &mut Buffer, extend: bool, p: usize, vcol: Option<usize>) {
        self.want = vcol;
        self.move_to(buf, p, extend, vcol.is_some());
    }

    pub fn home(&mut self, buf: &mut Buffer, extend: bool) {
        let (l, col) = buf.line_col(self.sel.head);
        let text = buf.line(l);
        let fnb = text.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        let target_col = if col == fnb || fnb == text.chars().count() && col == 0 { 0 } else { fnb };
        let p = buf.pos(l, target_col);
        self.move_to(buf, p, extend, false);
    }

    pub fn end(&mut self, buf: &mut Buffer, extend: bool) {
        let (l, _) = buf.line_col(self.sel.head);
        let p = buf.pos(l, usize::MAX);
        self.move_to(buf, p, extend, false);
    }

    pub fn doc_start(&mut self, buf: &mut Buffer, extend: bool) {
        self.move_to(buf, 0, extend, false);
    }

    pub fn doc_end(&mut self, buf: &mut Buffer, extend: bool) {
        let e = buf.len_chars();
        self.move_to(buf, e, extend, false);
    }

    pub fn select_all(&mut self, buf: &mut Buffer) {
        let e = buf.len_chars();
        self.set(buf, 0, e, false);
    }

    /// The word (or run of spaces / punctuation) at `p`, as a char range. None past the end.
    pub fn word_range(buf: &Buffer, p: usize) -> Option<(usize, usize)> {
        let n = buf.len_chars();
        let at = |i: usize| buf.char_at(i);
        let probe = if p < n && !matches!(at(p), Some('\n') | Some('\r')) { p } else { p.saturating_sub(1) };
        let c = at(probe)?;
        let cl = class(c);
        let (mut s, mut e) = (probe, probe + 1);
        while s > 0 && at(s - 1).is_some_and(|c| class(c) == cl && c != '\n') {
            s -= 1;
        }
        while e < n && at(e).is_some_and(|c| class(c) == cl && c != '\n' && c != '\r') {
            e += 1;
        }
        Some((s, e))
    }

    /// The whole line holding `p`, including its line break.
    pub fn line_range(buf: &Buffer, p: usize) -> (usize, usize) {
        let (l, _) = buf.line_col(p);
        let s = buf.line_start(l);
        let e = if l + 1 < buf.line_count() { buf.line_start(l + 1) } else { buf.len_chars() };
        (s, e)
    }

    pub fn select_word_at(&mut self, buf: &mut Buffer, p: usize) {
        if let Some((s, e)) = Self::word_range(buf, p) {
            self.set(buf, s, e, false);
        }
    }

    /// Select `anchor..head` keeping the anchor (drag by words / lines).
    pub fn select_range(&mut self, buf: &mut Buffer, anchor: usize, head: usize) {
        self.set(buf, anchor, head, false);
    }

    /// Column (block) selection between visual positions `a = (line, vcol)` and `b = (line, vcol)`:
    /// one selection per buffer line (lines shorter than the box get a caret at their end), the
    /// pointer's line being the primary one. Lines inside `hidden` (folded away) are skipped.
    /// `base` are cursors that were there before the drag and stay.
    pub fn block_select(
        &mut self,
        buf: &Buffer,
        a: (usize, usize),
        b: (usize, usize),
        base: &[Sel],
        hidden: &[(usize, usize)],
    ) {
        let (l0, l1) = (a.0.min(b.0), a.0.max(b.0));
        let (vl, vr) = (a.1.min(b.1), a.1.max(b.1));
        let forward = b.1 >= a.1;
        let mut sels: Vec<Sel> = Vec::new();
        let mut primary = None;
        for l in l0..=l1.min(buf.line_count().saturating_sub(1)) {
            if in_ranges(hidden, l) {
                continue;
            }
            let text = buf.line(l);
            let (c0, c1) = (col_from_visual(&text, vl, self.tab_w), col_from_visual(&text, vr, self.tab_w));
            let (p0, p1) = (buf.pos(l, c0), buf.pos(l, c1));
            let s = if forward { Sel { anchor: p0, head: p1 } } else { Sel { anchor: p1, head: p0 } };
            if l == b.0 {
                primary = Some(s);
            } else {
                sels.push(s);
            }
        }
        let primary = primary.unwrap_or_else(|| sels.pop().unwrap_or(self.sel));
        self.extra = base.iter().copied().chain(sels).collect();
        self.sel = primary;
        self.want = None;
        self.normalize_cursors();
        self.boxsel = base.is_empty().then(|| BoxSel { anchor: a, active: b, sels: self.all_sels() });
    }

    /// The fixed corner for a new column selection: the corner of the current one while its cursors
    /// are untouched, else the primary selection's anchor (line, visual column).
    pub fn box_anchor(&self, buf: &Buffer) -> (usize, usize) {
        self.current_box().map_or_else(|| self.visual_point(buf, self.sel.anchor), |b| b.anchor)
    }

    fn current_box(&self) -> Option<&BoxSel> {
        self.boxsel.as_ref().filter(|b| b.sels == self.all_sels())
    }

    /// (line, visual column) of char index `p`.
    fn visual_point(&self, buf: &Buffer, p: usize) -> (usize, usize) {
        let (l, c) = buf.line_col(p);
        (l, visual_col(&buf.line(l), c, self.tab_w))
    }

    /// Ctrl+Shift+Alt+arrows: move the free corner of the column selection by `dl` buffer lines
    /// (folded lines in `hidden` are stepped over) and `dc` visual columns. Continues the current
    /// box; otherwise one starts from the primary selection (anchor to head).
    pub fn column_select(&mut self, buf: &Buffer, dl: isize, dc: isize, hidden: &[(usize, usize)]) {
        let (anchor, mut active) = match self.current_box() {
            Some(b) => (b.anchor, b.active),
            None => (self.visual_point(buf, self.sel.anchor), self.visual_point(buf, self.sel.head)),
        };
        let n = buf.line_count() as isize;
        let step = dl.signum();
        let mut l = active.0 as isize;
        for _ in 0..dl.unsigned_abs() {
            let mut t = l + step;
            while t >= 0 && t < n && in_ranges(hidden, t as usize) {
                t += step;
            }
            if t < 0 || t >= n {
                break;
            }
            l = t;
        }
        active.0 = l as usize;
        // Right stops at the widest line of the box: past it there is nothing left to select.
        let (lo, hi) = (anchor.0.min(active.0), anchor.0.max(active.0));
        let widest = (lo..=hi.min(buf.line_count().saturating_sub(1)))
            .filter(|&l| !in_ranges(hidden, l))
            .map(|l| {
                let t = buf.line(l);
                visual_col(&t, t.chars().count(), self.tab_w)
            })
            .max()
            .unwrap_or(0);
        let v = active.1 as isize + dc;
        active.1 = if dc > 0 { (v as usize).min(widest.max(active.1)) } else { v.max(0) as usize };
        self.block_select(buf, anchor, active, &[], hidden);
    }

    pub fn click(&mut self, buf: &mut Buffer, p: usize, extend: bool) {
        self.move_to(buf, p, extend, false);
    }

    // ── text editing ──

    fn delete_range(&mut self, buf: &mut Buffer, s: usize, e: usize, kind: EditKind) {
        let before = self.sel;
        buf.replace(s, e, "", kind, before);
        self.set_caret_keep_group(buf, s);
    }

    fn set_caret_keep_group(&mut self, buf: &mut Buffer, p: usize) {
        self.sel = Sel::caret(p);
        self.want = None;
        buf.set_after(self.sel);
    }

    pub fn selected_text(&self, buf: &Buffer) -> String {
        buf.slice(self.sel.start(), self.sel.end())
    }

    /// Text to copy: the selection, or the whole current line when nothing is selected.
    pub fn copy_text(&self, buf: &Buffer) -> String {
        if !self.sel.is_empty() {
            return self.selected_text(buf);
        }
        let (l, _) = buf.line_col(self.sel.head);
        format!("{}{}", buf.line(l), buf.eol())
    }

    /// Whole lines `l0..=l1` as clipboard text (each one ends with the line break).
    pub fn lines_text(buf: &Buffer, l0: usize, l1: usize) -> String {
        let n = buf.line_count();
        let l1 = l1.min(n.saturating_sub(1));
        let (s, e) = (buf.line_start(l0), if l1 + 1 < n { buf.line_start(l1 + 1) } else { buf.len_chars() });
        let mut t = buf.slice(s, e);
        if !t.ends_with('\n') {
            t.push_str(buf.eol());
        }
        t
    }

    /// Cut the whole lines `l0..=l1` (what Ctrl+X does with no selection); returns their text.
    pub fn cut_lines(&mut self, buf: &mut Buffer, l0: usize, l1: usize) -> String {
        let text = Self::lines_text(buf, l0, l1);
        let n = buf.line_count();
        let l1 = l1.min(n.saturating_sub(1));
        let s = buf.line_start(l0);
        let e = if l1 + 1 < n { buf.line_start(l1 + 1) } else { buf.len_chars() };
        buf.close_group();
        self.delete_range(buf, s, e, EditKind::Other);
        buf.close_group();
        text
    }

    pub fn cut(&mut self, buf: &mut Buffer) -> String {
        let text = self.copy_text(buf);
        if self.sel.is_empty() {
            let (l, _) = buf.line_col(self.sel.head);
            return self.cut_lines(buf, l, l);
        } else {
            buf.close_group();
            let (s, e) = (self.sel.start(), self.sel.end());
            self.delete_range(buf, s, e, EditKind::Other);
        }
        buf.close_group();
        text
    }

    pub fn paste(&mut self, buf: &mut Buffer, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = if buf.crlf { text.replace('\n', "\r\n") } else { text };
        buf.close_group();
        let before = self.sel;
        let (s, e) = (self.sel.start(), self.sel.end());
        buf.replace(s, e, &text, EditKind::Other, before);
        self.set_caret_keep_group(buf, s + text.chars().count());
        buf.close_group();
    }

    /// Insert plain text at the cursor (no auto-pairing).
    pub fn insert_text(&mut self, buf: &mut Buffer, text: &str) {
        let before = self.sel;
        let (s, e) = (self.sel.start(), self.sel.end());
        buf.replace(s, e, text, EditKind::Typing, before);
        self.set_caret_keep_group(buf, s + text.chars().count());
    }

    /// A character typed by the user: handles auto-closing pairs and skipping over closers.
    pub fn type_char(&mut self, buf: &mut Buffer, ch: char) {
        let (s, e) = (self.sel.start(), self.sel.end());
        let next = buf.char_at(e);
        let prev = if s > 0 { buf.char_at(s - 1) } else { None };

        if let Some(cl) = closer(ch) {
            // Surround a selection.
            if !self.sel.is_empty() {
                let inner = buf.slice(s, e);
                let before = self.sel;
                buf.close_group();
                buf.replace(s, e, &format!("{ch}{inner}{cl}"), EditKind::Other, before);
                let len = inner.chars().count();
                self.sel = Sel { anchor: s + 1, head: s + 1 + len };
                buf.set_after(self.sel);
                buf.close_group();
                return;
            }
            let is_quote = ch == cl;
            if is_quote && next == Some(ch) {
                return self.move_to(buf, e + 1, false, false);
            }
            let word_adjacent = prev.is_some_and(|c| c.is_alphanumeric() || c == '_')
                || next.is_some_and(|c| c.is_alphanumeric() || c == '_');
            let next_ok = next.is_none_or(|c| c.is_whitespace() || ")]};,.:>".contains(c));
            if next_ok && !(is_quote && (word_adjacent || prev == Some(ch))) {
                let before = self.sel;
                buf.replace(s, e, &format!("{ch}{cl}"), EditKind::Typing, before);
                self.set_caret_keep_group(buf, s + 1);
                return;
            }
        } else if matches!(ch, ')' | ']' | '}') && self.sel.is_empty() && next == Some(ch) {
            return self.move_to(buf, e + 1, false, false);
        }
        let mut tmp = [0u8; 4];
        self.insert_text(buf, ch.encode_utf8(&mut tmp));
        if ch == '>'
            && self.jsx
            && self.sel.is_empty()
            && let Some(close) = closing_tag_for(buf, self.sel.head)
        {
            let h = self.sel.head;
            let before = self.sel;
            buf.replace(h, h, &close, EditKind::Typing, before);
            self.set_caret_keep_group(buf, h);
        }
    }

    fn line_indent(buf: &Buffer, l: usize) -> String {
        buf.line(l).chars().take_while(|c| *c == ' ' || *c == '\t').collect()
    }

    pub fn newline(&mut self, buf: &mut Buffer) {
        let (s, e) = (self.sel.start(), self.sel.end());
        let (l, col) = buf.line_col(s);
        let indent_full = Self::line_indent(buf, l);
        let indent: String = indent_full.chars().take(col).collect();
        let prev = if s > 0 { buf.char_at(s - 1) } else { None };
        let next = buf.char_at(e);
        let eol = buf.eol();
        buf.close_group();
        let before = self.sel;
        let opens = matches!(prev, Some('{') | Some('[') | Some('('));
        let between_tags = self.jsx && prev == Some('>') && next == Some('<') && buf.char_at(e + 1) == Some('/');
        let opens = opens || between_tags;
        let between = (opens && prev.and_then(closer) == next) || between_tags;
        if between {
            let text = format!("{eol}{indent}{}{eol}{indent}", self.indent);
            buf.replace(s, e, &text, EditKind::Other, before);
            let caret = s + eol.chars().count() + indent.chars().count() + self.indent.chars().count();
            self.set_caret_keep_group(buf, caret);
        } else {
            let extra = if opens { self.indent.as_str() } else { "" };
            let text = format!("{eol}{indent}{extra}");
            buf.replace(s, e, &text, EditKind::Other, before);
            self.set_caret_keep_group(buf, s + text.chars().count());
        }
        buf.close_group();
    }

    pub fn backspace(&mut self, buf: &mut Buffer) {
        if !self.sel.is_empty() {
            buf.close_group();
            let (s, e) = (self.sel.start(), self.sel.end());
            self.delete_range(buf, s, e, EditKind::Other);
            return buf.close_group();
        }
        let h = self.sel.head;
        if h == 0 {
            return;
        }
        let prev = buf.char_at(h - 1);
        let next = buf.char_at(h);
        // Delete an empty auto-closed pair together.
        if let (Some(p), Some(n)) = (prev, next)
            && closer(p) == Some(n)
        {
            return self.delete_range(buf, h - 1, h + 1, EditKind::Backspace);
        }
        // Smart backspace inside leading spaces: delete back to the previous indent stop.
        let (l, col) = buf.line_col(h);
        let unit = self.indent.chars().count();
        if !self.indent.contains('\t') && col > 0 && unit > 0 && buf.line(l).chars().take(col).all(|c| c == ' ') {
            let n = match col % unit {
                0 => unit,
                r => r,
            };
            return self.delete_range(buf, h - n, h, EditKind::Backspace);
        }
        let p = self.prev_pos(buf, h);
        self.delete_range(buf, p, h, EditKind::Backspace);
    }

    pub fn delete(&mut self, buf: &mut Buffer) {
        if !self.sel.is_empty() {
            buf.close_group();
            let (s, e) = (self.sel.start(), self.sel.end());
            self.delete_range(buf, s, e, EditKind::Other);
            return buf.close_group();
        }
        let h = self.sel.head;
        let n = self.next_pos(buf, h);
        if n > h {
            self.delete_range(buf, h, n, EditKind::Delete);
        }
    }

    pub fn delete_word(&mut self, buf: &mut Buffer, forward: bool) {
        if !self.sel.is_empty() {
            return self.backspace(buf);
        }
        let h = self.sel.head;
        let t = if forward { self.word_right(buf, h) } else { self.word_left(buf, h) };
        buf.close_group();
        self.delete_range(buf, t.min(h), t.max(h), EditKind::Other);
        buf.close_group();
    }

    // ── line-oriented commands ──

    fn selected_lines(&self, buf: &Buffer) -> (usize, usize) {
        let (l0, _) = buf.line_col(self.sel.start());
        let (mut l1, c1) = buf.line_col(self.sel.end());
        if !self.sel.is_empty() && c1 == 0 && l1 > l0 {
            l1 -= 1;
        }
        (l0, l1)
    }

    /// Apply per-line edits (start, removed_len, inserted) top-to-bottom as one undo step,
    /// shifting the selection accordingly.
    fn apply_line_edits(&mut self, buf: &mut Buffer, edits: Vec<(usize, usize, String)>) {
        if edits.is_empty() {
            return;
        }
        buf.close_group();
        let before = self.sel;
        buf.begin_compound(before);
        for (start, rem, ins) in edits.iter().rev() {
            buf.replace(*start, start + rem, ins, EditKind::Other, before);
        }
        let adj = |p: usize| {
            let mut np = p as isize;
            for (start, rem, ins) in &edits {
                let ins_len = ins.chars().count();
                if p >= start + rem {
                    np += ins_len as isize - *rem as isize;
                } else if p > *start {
                    np -= (p - start) as isize;
                }
            }
            np.max(0) as usize
        };
        self.sel = Sel { anchor: adj(self.sel.anchor), head: adj(self.sel.head) };
        buf.end_compound(self.sel);
        self.want = None;
    }

    pub fn tab(&mut self, buf: &mut Buffer, shift: bool) {
        let (l0, l1) = self.selected_lines(buf);
        if shift || l1 > l0 {
            return self.indent_lines(buf, l0, l1, shift);
        }
        if !self.sel.is_empty() {
            let unit = self.indent.clone();
            return self.insert_text(buf, &unit);
        }
        if self.indent.contains('\t') {
            return self.insert_text(buf, "\t");
        }
        let (_, col) = buf.line_col(self.sel.head);
        let unit = self.indent.chars().count().max(1);
        let n = unit - col % unit;
        self.insert_text(buf, &" ".repeat(n));
    }

    fn indent_lines(&mut self, buf: &mut Buffer, l0: usize, l1: usize, outdent: bool) {
        let unit_w = self.indent.chars().count().max(1);
        let mut edits = Vec::new();
        for l in l0..=l1 {
            let start = buf.line_start(l);
            let text = buf.line(l);
            if outdent {
                let mut n = 0;
                let mut chars = text.chars();
                match chars.next() {
                    Some('\t') => n = 1,
                    Some(' ') => {
                        n = 1;
                        for c in chars.take(unit_w - 1) {
                            if c == ' ' {
                                n += 1
                            } else {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
                if n > 0 {
                    edits.push((start, n, String::new()));
                }
            } else if !text.is_empty() {
                edits.push((start, 0, self.indent.clone()));
            }
        }
        self.apply_line_edits(buf, edits);
    }

    pub fn toggle_comment(&mut self, buf: &mut Buffer) {
        if self.comment.is_empty() {
            return;
        }
        let (l0, l1) = self.selected_lines(buf);
        let prefix = self.comment.clone();
        let lines: Vec<(usize, String)> = (l0..=l1).map(|l| (l, buf.line(l))).collect();
        let nonblank: Vec<&(usize, String)> = lines.iter().filter(|(_, t)| !t.trim().is_empty()).collect();
        if nonblank.is_empty() {
            return;
        }
        let all_commented = nonblank.iter().all(|(_, t)| t.trim_start().starts_with(&prefix));
        let min_indent =
            nonblank.iter().map(|(_, t)| t.chars().take_while(|c| c.is_whitespace()).count()).min().unwrap_or(0);
        let mut edits = Vec::new();
        for (l, t) in &lines {
            if t.trim().is_empty() {
                continue;
            }
            let start = buf.line_start(*l);
            if all_commented {
                let lead = t.chars().take_while(|c| c.is_whitespace()).count();
                let after = &t.trim_start()[prefix.len()..];
                let extra = usize::from(after.starts_with(' '));
                edits.push((start + lead, prefix.chars().count() + extra, String::new()));
            } else {
                edits.push((start + min_indent, 0, format!("{prefix} ")));
            }
        }
        self.apply_line_edits(buf, edits);
    }

    /// First and last line of the selection (a selection ending at column 0 stops above it).
    pub fn selected_line_range(&self, buf: &Buffer) -> (usize, usize) {
        self.selected_lines(buf)
    }

    pub fn duplicate(&mut self, buf: &mut Buffer) {
        if self.sel.is_empty() {
            let (l, _) = buf.line_col(self.sel.head);
            return self.duplicate_lines(buf, l, l);
        }
        buf.close_group();
        let before = self.sel;
        let (s, e) = (self.sel.start(), self.sel.end());
        let text = buf.slice(s, e);
        let len = text.chars().count();
        buf.replace(e, e, &text, EditKind::Other, before);
        self.sel = Sel { anchor: e, head: e + len };
        buf.set_after(self.sel);
        buf.close_group();
    }

    /// Copy lines `l0..=l1` right below themselves; the caret goes to the copy.
    pub fn duplicate_lines(&mut self, buf: &mut Buffer, l0: usize, l1: usize) {
        buf.close_group();
        let before = self.sel;
        let (_, col) = buf.line_col(self.sel.head);
        let eol = buf.eol();
        let text = (l0..=l1).map(|l| buf.line(l)).collect::<Vec<_>>().join(eol);
        let end = buf.pos(l1, usize::MAX);
        buf.replace(end, end, &format!("{eol}{text}"), EditKind::Other, before);
        let (cl, _) = buf.line_col(before.head);
        self.sel = Sel::caret(buf.pos(cl + (l1 - l0 + 1), col));
        buf.set_after(self.sel);
        buf.close_group();
    }

    #[allow(dead_code)]
    pub fn delete_line(&mut self, buf: &mut Buffer) {
        let (l0, l1) = self.selected_lines(buf);
        self.delete_lines(buf, l0, l1);
    }

    /// Remove lines `l0..=l1` entirely.
    pub fn delete_lines(&mut self, buf: &mut Buffer, l0: usize, l1: usize) {
        let s = buf.line_start(l0);
        let e = if l1 + 1 < buf.line_count() { buf.line_start(l1 + 1) } else { buf.len_chars() };
        // On the last line also remove the break before it so no empty line is left behind.
        let s = if l1 + 1 >= buf.line_count() && l0 > 0 { self.prev_pos(buf, s) } else { s };
        buf.close_group();
        self.delete_range(buf, s, e, EditKind::Other);
        buf.close_group();
    }

    #[allow(dead_code)]
    pub fn move_lines(&mut self, buf: &mut Buffer, down: bool) {
        let (l0, l1) = self.selected_lines(buf);
        let other = if down { (l1 + 1, l1 + 1) } else { (l0.wrapping_sub(1), l0.wrapping_sub(1)) };
        self.move_block(buf, down, (l0, l1), other);
    }

    /// Move the whole lines `x` past the whole lines `y`, which must touch it (`y` below for
    /// `down`, above otherwise). The selection moves with `x`. The buffer records the exchange as
    /// a swap of two blocks so folds and other views follow the lines instead of the positions.
    pub fn move_block(&mut self, buf: &mut Buffer, down: bool, x: (usize, usize), y: (usize, usize)) {
        let n = buf.line_count();
        if (!down && x.0 == 0) || (down && x.1 + 1 >= n) || y.1 >= n || y.0 > y.1 {
            return;
        }
        let adjacent = if down { y.0 == x.1 + 1 } else { y.1 + 1 == x.0 };
        if !adjacent {
            return;
        }
        let (a, b) = if down { (x, y) } else { (y, x) }; // `a` is the upper block
        let eol = buf.eol();
        let text_of = |buf: &Buffer, r: (usize, usize)| (r.0..=r.1).map(|l| buf.line(l)).collect::<Vec<_>>().join(eol);
        let (ta, tb) = (text_of(buf, a), text_of(buf, b));
        let (ca, cb) = (ta.chars().count(), tb.chars().count());
        let text = format!("{tb}{eol}{ta}");
        let start = buf.line_start(a.0);
        let end = buf.pos(b.1, usize::MAX);
        let shift = if down { cb + eol.len() } else { ca + eol.len() };
        buf.close_group();
        let before = self.sel;
        buf.tag_swap(a.1 - a.0 + 1, b.1 - b.0 + 1, ca, cb, eol.len());
        buf.replace(start, end, &text, EditKind::Other, before);
        let mv = |p: usize| if down { p + shift } else { p - shift };
        self.sel = Sel { anchor: mv(self.sel.anchor), head: mv(self.sel.head) };
        buf.set_after(self.sel);
        buf.close_group();
    }

    // ── history ──

    pub fn undo(&mut self, buf: &mut Buffer) {
        if let Some(s) = buf.undo() {
            self.sel = Sel { anchor: s.anchor.min(buf.len_chars()), head: s.head.min(buf.len_chars()) };
            self.want = None;
        }
    }

    pub fn redo(&mut self, buf: &mut Buffer) {
        if let Some(s) = buf.redo() {
            self.sel = Sel { anchor: s.anchor.min(buf.len_chars()), head: s.head.min(buf.len_chars()) };
            self.want = None;
        }
    }
}

const VOID_TAGS: &[&str] =
    &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];

/// After `>` was typed at `caret` (char index just past it): the `</tag>` to insert, if the
/// text before it is an opening JSX/HTML tag. Generic type arguments (`Array<T>`) are skipped
/// because a tag can't directly follow an identifier or `)`.
pub fn closing_tag_for(buf: &Buffer, caret: usize) -> Option<String> {
    let (l, col) = buf.line_col(caret);
    let line: Vec<char> = buf.line(l).chars().take(col).collect();
    // Last '<' on this line before the '>'.
    let lt = line[..line.len().checked_sub(1)?].iter().rposition(|c| *c == '<')?;
    let inner: String = line[lt + 1..line.len() - 1].iter().collect();
    if inner.starts_with('/') || inner.ends_with('/') || inner.contains('>') || inner.contains('<') {
        return None;
    }
    if lt > 0 {
        let p = line[lt - 1];
        if p.is_alphanumeric() || p == '_' || p == ')' || p == '$' || p == '.' {
            return None;
        }
    }
    let name: String =
        inner.chars().take_while(|c| c.is_alphanumeric() || matches!(c, '.' | ':' | '-' | '_')).collect();
    if name.is_empty() || !name.chars().next().is_some_and(|c| c.is_alphabetic()) {
        return None;
    }
    if inner[name.len()..].chars().next().is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    if VOID_TAGS.contains(&name.to_lowercase().as_str()) {
        return None;
    }
    // Already closed right after the caret?
    let rest = buf.slice(caret, (caret + name.chars().count() + 3).min(buf.len_chars()));
    if rest.starts_with(&format!("</{name}")) {
        return None;
    }
    Some(format!("</{name}>"))
}

/// Matching bracket for the one just before or at `pos` (naive: ignores strings/comments).
pub fn match_bracket(buf: &Buffer, pos: usize) -> Option<(usize, usize)> {
    const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
    let candidates = [pos.checked_sub(1), Some(pos)];
    for p in candidates.into_iter().flatten() {
        let Some(c) = buf.char_at(p) else { continue };
        for (o, cl) in PAIRS {
            let (forward, target, same) = if c == o {
                (true, cl, o)
            } else if c == cl {
                (false, o, cl)
            } else {
                continue;
            };
            let mut depth = 0i32;
            let limit = 50_000usize;
            if forward {
                let end = buf.len_chars().min(p + limit);
                for i in p..end {
                    let ch = buf.char_at(i)?;
                    if ch == same {
                        depth += 1;
                    } else if ch == target {
                        depth -= 1;
                        if depth == 0 {
                            return Some((p, i));
                        }
                    }
                }
            } else {
                let start = p.saturating_sub(limit);
                for i in (start..=p).rev() {
                    let ch = buf.char_at(i)?;
                    if ch == same {
                        depth += 1;
                    } else if ch == target {
                        depth -= 1;
                        if depth == 0 {
                            return Some((i, p));
                        }
                    }
                }
            }
            return None;
        }
    }
    None
}

/// What to highlight elsewhere in the document while text is selected (Sublime/Zed style).
#[derive(Debug, PartialEq, Eq)]
pub struct Occurrence {
    pub needle: String,
    /// The selection is a whole word (word boundaries at both ends): match whole words only.
    pub whole_word: bool,
}

/// Selections longer than this (chars) do not highlight their other occurrences.
pub const OCCURRENCE_MAX: usize = 200;

/// The occurrence query for the current selection: only one cursor, holding a non-empty, single-line
/// selection of 2..=[`OCCURRENCE_MAX`] chars that is not all whitespace.
pub fn occurrence_query(buf: &Buffer, ed: &Editor) -> Option<Occurrence> {
    if !ed.extra.is_empty() {
        return None;
    }
    let (a, b) = (ed.sel.start(), ed.sel.end());
    if !(2..=OCCURRENCE_MAX).contains(&(b - a)) {
        return None;
    }
    let needle = buf.slice(a, b);
    if needle.contains(['\n', '\r']) || needle.trim().is_empty() {
        return None;
    }
    let word = |c: Option<char>| c.is_some_and(|c| class(c) == Class::Word);
    let whole_word = word(needle.chars().next())
        && word(needle.chars().next_back())
        && !word(a.checked_sub(1).and_then(|p| buf.char_at(p)))
        && !word(buf.char_at(b));
    Some(Occurrence { needle, whole_word })
}

/// Pushes the char-column ranges of `q` in `text` (case-sensitive, non-overlapping) onto `out`,
/// stopping once `out` holds `cap` ranges.
pub fn line_occurrences(text: &str, q: &Occurrence, cap: usize, out: &mut Vec<(usize, usize)>) {
    let n = q.needle.len();
    let nchars = q.needle.chars().count();
    let is_word = |c: Option<char>| c.is_some_and(|c| class(c) == Class::Word);
    // Char column of byte `seen_b`, advanced incrementally so a long line is walked once.
    let (mut seen_b, mut seen_c) = (0usize, 0usize);
    let mut from = 0usize;
    while out.len() < cap {
        let Some(m) = text[from..].find(q.needle.as_str()).map(|m| from + m) else { break };
        if q.whole_word && (is_word(text[..m].chars().next_back()) || is_word(text[m + n..].chars().next())) {
            from = m + text[m..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        seen_c += text[seen_b..m].chars().count();
        seen_b = m;
        out.push((seen_c, seen_c + nchars));
        from = m + n;
    }
}

/// Guess the indent unit from the first lines of the file.
pub fn detect_indent(buf: &Buffer) -> String {
    let mut tabs = 0;
    let mut gcd = 0usize;
    for l in 0..buf.line_count().min(400) {
        let t = buf.line(l);
        if t.starts_with('\t') {
            tabs += 1;
        } else {
            let n = t.chars().take_while(|c| *c == ' ').count();
            if n > 0 && n < t.chars().count() {
                gcd = if gcd == 0 { n } else { num_gcd(gcd, n) };
            }
        }
    }
    if tabs > 5 && tabs > gcd {
        "\t".into()
    } else if (2..=8).contains(&gcd) {
        " ".repeat(gcd)
    } else {
        "  ".into()
    }
}

fn num_gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { num_gcd(b, a % b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(text: &str) -> (Buffer, Editor) {
        let b = Buffer::from_str(text);
        let e = Editor::new(&b, "//");
        (b, e)
    }

    fn typ(b: &mut Buffer, e: &mut Editor, s: &str) {
        for c in s.chars() {
            if c == '\n' { e.newline(b) } else { e.type_char(b, c) }
        }
    }

    #[test]
    fn typing_and_undo_groups() {
        let (mut b, mut e) = setup("");
        typ(&mut b, &mut e, "hello");
        assert_eq!(b.to_string(), "hello");
        e.undo(&mut b);
        assert_eq!(b.to_string(), "", "typing run is one undo step");
        e.redo(&mut b);
        assert_eq!(b.to_string(), "hello");
        assert_eq!(e.sel, Sel::caret(5));
    }

    #[test]
    fn dirty_tracking_returns_to_clean_after_undo() {
        let (mut b, mut e) = setup("abc");
        assert!(!b.is_dirty());
        e.doc_end(&mut b, false);
        typ(&mut b, &mut e, "d");
        assert!(b.is_dirty());
        e.undo(&mut b);
        assert!(!b.is_dirty());
    }

    #[test]
    fn auto_pair_and_skip_over() {
        let (mut b, mut e) = setup("");
        typ(&mut b, &mut e, "f(");
        assert_eq!(b.to_string(), "f()");
        typ(&mut b, &mut e, "a)");
        assert_eq!(b.to_string(), "f(a)");
        assert_eq!(e.head(), 4);
    }

    #[test]
    fn backspace_removes_empty_pair() {
        let (mut b, mut e) = setup("");
        typ(&mut b, &mut e, "[");
        e.backspace(&mut b);
        assert_eq!(b.to_string(), "");
    }

    #[test]
    fn quote_not_paired_after_word() {
        let (mut b, mut e) = setup("");
        typ(&mut b, &mut e, "don't");
        assert_eq!(b.to_string(), "don't");
    }

    #[test]
    fn newline_keeps_indent_and_expands_braces() {
        let (mut b, mut e) = setup("  if (x) ");
        e.doc_end(&mut b, false);
        typ(&mut b, &mut e, "{");
        assert_eq!(b.to_string(), "  if (x) {}");
        e.newline(&mut b);
        assert_eq!(b.to_string(), "  if (x) {\n    \n  }");
        assert_eq!(e.head(), "  if (x) {\n    ".chars().count());
        e.undo(&mut b);
        assert_eq!(b.to_string(), "  if (x) {}");
    }

    #[test]
    fn word_movement() {
        let (mut b, mut e) = setup("foo.bar baz");
        e.right(&mut b, false, true);
        assert_eq!(e.head(), 3);
        e.right(&mut b, false, true);
        assert_eq!(e.head(), 4);
        e.right(&mut b, false, true);
        assert_eq!(e.head(), 7);
        e.right(&mut b, false, true);
        assert_eq!(e.head(), 11);
        e.left(&mut b, false, true);
        assert_eq!(e.head(), 8);
    }

    #[test]
    fn vertical_keeps_desired_column() {
        let (mut b, mut e) = setup("abcdef\nab\nabcdef");
        e.click(&mut b, 5, false);
        e.vertical(&mut b, false, 1);
        assert_eq!(b.line_col(e.head()), (1, 2));
        e.vertical(&mut b, false, 1);
        assert_eq!(b.line_col(e.head()), (2, 5));
    }

    #[test]
    fn selection_replace_and_surround() {
        let (mut b, mut e) = setup("hello world");
        e.click(&mut b, 0, false);
        e.right(&mut b, true, true);
        assert_eq!(e.selected_text(&b), "hello");
        typ(&mut b, &mut e, "(");
        assert_eq!(b.to_string(), "(hello) world");
        assert_eq!(e.selected_text(&b), "hello");
        typ(&mut b, &mut e, "X");
        assert_eq!(b.to_string(), "(X) world");
    }

    #[test]
    fn tab_and_outdent_multi_line() {
        let (mut b, mut e) = setup("a\nb\nc");
        e.select_all(&mut b);
        e.tab(&mut b, false);
        assert_eq!(b.to_string(), "  a\n  b\n  c");
        e.tab(&mut b, true);
        assert_eq!(b.to_string(), "a\nb\nc");
        e.undo(&mut b);
        assert_eq!(b.to_string(), "  a\n  b\n  c");
        e.undo(&mut b);
        assert_eq!(b.to_string(), "a\nb\nc");
    }

    #[test]
    fn toggle_comment_roundtrip() {
        let (mut b, mut e) = setup("  a\n\n  b");
        e.select_all(&mut b);
        e.toggle_comment(&mut b);
        assert_eq!(b.to_string(), "  // a\n\n  // b");
        e.toggle_comment(&mut b);
        assert_eq!(b.to_string(), "  a\n\n  b");
    }

    #[test]
    fn move_lines_up_down() {
        let (mut b, mut e) = setup("one\ntwo\nthree");
        e.click(&mut b, 4, false); // on "two"
        e.move_lines(&mut b, false);
        assert_eq!(b.to_string(), "two\none\nthree");
        assert_eq!(b.line_col(e.head()).0, 0);
        e.move_lines(&mut b, true);
        e.move_lines(&mut b, true);
        assert_eq!(b.to_string(), "one\nthree\ntwo");
        e.move_lines(&mut b, true); // already last: no-op
        assert_eq!(b.to_string(), "one\nthree\ntwo");
    }

    #[test]
    fn duplicate_and_delete_line() {
        let (mut b, mut e) = setup("a\nb");
        e.duplicate(&mut b);
        assert_eq!(b.to_string(), "a\na\nb");
        e.delete_line(&mut b);
        assert_eq!(b.to_string(), "a\nb");
        e.doc_end(&mut b, false);
        e.delete_line(&mut b);
        assert_eq!(b.to_string(), "a");
    }

    #[test]
    fn crlf_is_preserved_and_navigated() {
        let (mut b, mut e) = setup("ab\r\ncd");
        assert!(b.crlf);
        assert_eq!(b.line(0), "ab");
        assert_eq!(b.line_len(0), 2);
        e.click(&mut b, 2, false);
        e.right(&mut b, false, false);
        assert_eq!(e.head(), 4, "moves over the whole CRLF");
        e.left(&mut b, false, false);
        assert_eq!(e.head(), 2);
        e.newline(&mut b);
        assert_eq!(b.to_string(), "ab\r\n\r\ncd");
    }

    #[test]
    fn paste_normalizes_line_endings() {
        let (mut b, mut e) = setup("x");
        e.paste(&mut b, "a\r\nb");
        assert_eq!(b.to_string(), "a\nbx");
    }

    #[test]
    fn cut_without_selection_takes_line() {
        let (mut b, mut e) = setup("one\ntwo\n");
        let t = e.cut(&mut b);
        assert_eq!(t, "one\n");
        assert_eq!(b.to_string(), "two\n");
    }

    #[test]
    fn smart_backspace_in_indent() {
        let (mut b, mut e) = setup("    x");
        e.indent = "  ".into();
        e.click(&mut b, 4, false);
        e.backspace(&mut b);
        assert_eq!(b.to_string(), "  x");
    }

    #[test]
    fn auto_close_jsx_tags_but_not_generics() {
        let (mut b, mut e) = setup("const a = (\n  ");
        e.jsx = true;
        e.doc_end(&mut b, false);
        typ(&mut b, &mut e, "<div className=\"x\">");
        assert_eq!(b.to_string(), "const a = (\n  <div className=\"x\"></div>");
        assert_eq!(e.head(), "const a = (\n  <div className=\"x\">".chars().count());

        let (mut b, mut e) = setup("const a = new Array");
        e.jsx = true;
        e.doc_end(&mut b, false);
        typ(&mut b, &mut e, "<T>");
        assert_eq!(b.to_string(), "const a = new Array<T>", "generic, not a tag");

        let (mut b, mut e) = setup("");
        e.jsx = true;
        typ(&mut b, &mut e, "<br>");
        assert_eq!(b.to_string(), "<br>", "void element");
        let (mut b, mut e) = setup("");
        e.jsx = true;
        typ(&mut b, &mut e, "<Foo />");
        assert_eq!(b.to_string(), "<Foo />", "self-closing");
        let (mut b, mut e) = setup("");
        e.jsx = false;
        typ(&mut b, &mut e, "<div>");
        assert_eq!(b.to_string(), "<div>", "off outside jsx files");
    }

    #[test]
    fn enter_between_tags_expands() {
        let (mut b, mut e) = setup("<div></div>");
        e.jsx = true;
        e.click(&mut b, 5, false);
        e.newline(&mut b);
        assert_eq!(b.to_string(), "<div>\n  \n</div>");
    }

    #[test]
    fn bracket_matching_nested_and_direction() {
        let b = Buffer::from_str("f(a, [b, {c}], d)");
        assert_eq!(match_bracket(&b, 2), Some((1, 16)), "just after '('");
        assert_eq!(match_bracket(&b, 17), Some((1, 16)), "just after ')'");
        assert_eq!(match_bracket(&b, 11), Some((9, 11).min((9, 11))), "braces inside brackets");
        assert_eq!(match_bracket(&Buffer::from_str("(("), 1), None, "unbalanced");
    }

    #[test]
    fn ctrl_d_adds_occurrences_and_typing_edits_all() {
        let (mut b, mut e) = setup("foo bar\nfoo baz\nxfoo foo\n");
        e.click(&mut b, 1, false);
        e.add_next_occurrence(&mut b); // selects "foo"
        assert_eq!(e.selected_text(&b), "foo");
        e.add_next_occurrence(&mut b);
        e.add_next_occurrence(&mut b);
        e.add_next_occurrence(&mut b);
        assert_eq!(e.extra.len(), 3, "four occurrences in total");
        e.edit_multi(&mut b, EditKind::Typing, |ed, bf| ed.type_char(bf, 'X'));
        assert_eq!(b.to_string(), "X bar\nX baz\nxX X\n");
        e.edit_multi(&mut b, EditKind::Typing, |ed, bf| ed.type_char(bf, 'Y'));
        assert_eq!(b.to_string(), "XY bar\nXY baz\nxXY XY\n");
        e.undo(&mut b);
        assert_eq!(
            b.to_string(),
            "foo bar\nfoo baz\nxfoo foo\n",
            "one undo reverts the whole typing run on all cursors"
        );
    }

    #[test]
    fn vertical_cursors_comment_out_and_backspace_merges() {
        let (mut b, mut e) = setup("a\nb\nc");
        e.click(&mut b, 0, false);
        e.add_cursor_vertical(&mut b, 1);
        e.add_cursor_vertical(&mut b, 1);
        assert_eq!(e.extra.len(), 2);
        e.edit_multi(&mut b, EditKind::Typing, |ed, bf| {
            ed.type_char(bf, '#');
            ed.type_char(bf, ' ');
        });
        assert_eq!(b.to_string(), "# a\n# b\n# c");
        e.move_multi(&mut b, |ed, bf| ed.end(bf, false));
        e.edit_multi(&mut b, EditKind::Backspace, |ed, bf| ed.backspace(bf));
        assert_eq!(b.to_string(), "# \n# \n# ");
        // Collapsing everything to the first cursor.
        e.clear_extra();
        assert!(e.extra.is_empty());
    }

    #[test]
    fn select_all_occurrences_and_multi_paste() {
        let (mut b, mut e) = setup("let a = 1\nlet b = a\nreturn a");
        e.click(&mut b, 4, false);
        e.select_all_occurrences(&mut b);
        assert_eq!(e.all_sels().len(), 3);
        assert_eq!(e.copy_text_multi(&b), "a\na\na");
        e.paste_multi(&mut b, "x\ny\nz");
        assert_eq!(b.to_string(), "let x = 1\nlet b = y\nreturn z", "one clipboard line per cursor");
        e.undo(&mut b);
        assert_eq!(b.to_string(), "let a = 1\nlet b = a\nreturn a");
    }

    #[test]
    fn alt_click_adds_and_duplicate_positions_are_ignored() {
        let (mut b, mut e) = setup("abc\ndef");
        e.add_cursor_at(5);
        e.add_cursor_at(5);
        assert_eq!(e.extra.len(), 1);
        e.edit_multi(&mut b, EditKind::Typing, |ed, bf| ed.type_char(bf, '!'));
        assert_eq!(b.to_string(), "!abc\nd!ef");
    }

    /// Every cursor as (line, start col, end col, head is at the end), in document order.
    fn box_cols(b: &Buffer, e: &Editor) -> Vec<(usize, usize, usize, bool)> {
        e.all_sels()
            .iter()
            .map(|s| {
                let (l, c0) = b.line_col(s.start());
                (l, c0, b.line_col(s.end()).1, s.head == s.end())
            })
            .collect()
    }

    #[test]
    fn block_select_uses_visual_columns_and_clamps_short_lines() {
        // Line 1 starts with a tab (4 columns): visual column 5 is its char column 2.
        let (b, mut e) = setup("abcdefgh\n\tbcdefg\nab\n\nabcdefgh");
        e.tab_w = 4;
        e.block_select(&b, (0, 2), (4, 5), &[], &[]);
        assert_eq!(
            box_cols(&b, &e),
            vec![(0, 2, 5, true), (1, 1, 2, true), (2, 2, 2, true), (3, 0, 0, true), (4, 2, 5, true)],
            "tab expanded, short and empty lines get a caret at their end"
        );
        assert_eq!(b.line_col(e.sel.head).0, 4, "the pointer's line is the primary cursor");
        // Dragging leftwards puts the heads on the left edge.
        e.block_select(&b, (0, 5), (1, 2), &[], &[]);
        assert_eq!(box_cols(&b, &e), vec![(0, 2, 5, false), (1, 1, 2, false)]);
    }

    #[test]
    fn block_select_skips_folded_lines() {
        let (b, mut e) = setup("aaaa\nbbbb\ncccc\ndddd\neeee");
        e.block_select(&b, (0, 1), (4, 3), &[], &[(1, 2)]);
        let lines: Vec<usize> = box_cols(&b, &e).iter().map(|c| c.0).collect();
        assert_eq!(lines, vec![0, 3, 4]);
    }

    #[test]
    fn box_typing_paste_and_delete_go_through_multi_cursor_editing() {
        let (mut b, mut e) = setup("one = 1\ntwo = 2\nsix = 6\nx");
        e.block_select(&b, (0, 0), (2, 3), &[], &[]);
        assert_eq!(e.copy_text_multi(&b), "one\ntwo\nsix");
        e.edit_multi(&mut b, EditKind::Typing, |ed, bf| ed.type_char(bf, 'v'));
        assert_eq!(b.to_string(), "v = 1\nv = 2\nv = 6\nx", "typing replaces every line's piece");
        e.edit_multi(&mut b, EditKind::Backspace, |ed, bf| ed.backspace(bf));
        assert_eq!(b.to_string(), " = 1\n = 2\n = 6\nx");
        e.paste_multi(&mut b, "aa\nbb\ncc");
        assert_eq!(b.to_string(), "aa = 1\nbb = 2\ncc = 6\nx", "one clipboard line per cursor");
        e.paste_multi(&mut b, "z");
        assert_eq!(b.to_string(), "aaz = 1\nbbz = 2\nccz = 6\nx", "other counts paste everywhere");
        e.undo(&mut b);
        e.undo(&mut b);
        assert_eq!(b.to_string(), " = 1\n = 2\n = 6\nx", "each paste is one undo step");
    }

    #[test]
    fn keyboard_column_select_grows_and_shrinks_the_box() {
        let (mut b, mut e) = setup("abcdef\nabcdef\nab\nabcdef\nabcdef");
        let p = b.pos(0, 1);
        e.click(&mut b, p, false);
        e.column_select(&b, 1, 0, &[]);
        e.column_select(&b, 1, 0, &[]);
        assert_eq!(box_cols(&b, &e), vec![(0, 1, 1, true), (1, 1, 1, true), (2, 1, 1, true)]);
        e.column_select(&b, 0, 3, &[]);
        assert_eq!(box_cols(&b, &e), vec![(0, 1, 4, true), (1, 1, 4, true), (2, 1, 2, true)]);
        e.column_select(&b, -1, -1, &[]);
        assert_eq!(box_cols(&b, &e), vec![(0, 1, 3, true), (1, 1, 3, true)], "up shrinks, left narrows");
        // Right stops at the widest line of the box.
        for _ in 0..20 {
            e.column_select(&b, 0, 1, &[]);
        }
        assert_eq!(box_cols(&b, &e), vec![(0, 1, 6, true), (1, 1, 6, true)]);
        // Any other move forgets the box: the next one starts at the new caret.
        e.clear_extra();
        let p = b.pos(3, 2);
        e.click(&mut b, p, false);
        e.column_select(&b, 1, 0, &[]);
        assert_eq!(box_cols(&b, &e), vec![(3, 2, 2, true), (4, 2, 2, true)]);
        // Folded lines are stepped over.
        let p = b.pos(0, 0);
        e.click(&mut b, p, false);
        e.column_select(&b, 1, 0, &[(1, 2)]);
        let lines: Vec<usize> = box_cols(&b, &e).iter().map(|c| c.0).collect();
        assert_eq!(lines, vec![0, 3]);
        assert_eq!(e.box_anchor(&b), (0, 0));
    }

    fn occ(text: &str, a: usize, b: usize) -> Option<Occurrence> {
        let (buf, mut e) = setup(text);
        e.sel = Sel { anchor: a, head: b };
        occurrence_query(&buf, &e)
    }

    fn occ_cols(text: &str, needle: &str, whole_word: bool) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        line_occurrences(text, &Occurrence { needle: needle.into(), whole_word }, 100, &mut out);
        out
    }

    #[test]
    fn occurrence_query_rules() {
        let text = "let createFakeProject = 1;\ncreateFakeProject();\n  x  \nab";
        // A whole word (double-click) matches whole words only.
        assert_eq!(occ(text, 4, 21), Some(Occurrence { needle: "createFakeProject".into(), whole_word: true }));
        // Part of a word matches the literal text anywhere.
        assert_eq!(occ(text, 10, 14), Some(Occurrence { needle: "Fake".into(), whole_word: false }));
        // Reversed selections behave the same.
        assert_eq!(occ(text, 21, 4).map(|o| o.whole_word), Some(true));
        // Mixed word/punctuation selection: literal.
        assert_eq!(occ(text, 22, 25).map(|o| o.whole_word), Some(false));
        // Ignored: empty, one char, multi-line, whitespace only, too long, several cursors.
        assert_eq!(occ(text, 4, 4), None);
        assert_eq!(occ(text, 4, 5), None);
        assert_eq!(occ(text, 18, 32), None);
        assert_eq!(occ(text, 48, 50), None);
        let long = "a".repeat(OCCURRENCE_MAX + 1);
        assert_eq!(occ(&long, 0, OCCURRENCE_MAX + 1), None);
        assert!(occ(&long, 0, OCCURRENCE_MAX).is_some());
        let (buf, mut e) = setup(text);
        e.sel = Sel { anchor: 4, head: 21 };
        e.extra.push(Sel { anchor: 27, head: 44 });
        assert_eq!(occurrence_query(&buf, &e), None);
    }

    #[test]
    fn line_occurrences_whole_word_vs_substring() {
        let line = "foo foobar barfoo foo_x (foo) foo";
        assert_eq!(occ_cols(line, "foo", true), vec![(0, 3), (25, 28), (30, 33)]);
        assert_eq!(occ_cols(line, "foo", false), vec![(0, 3), (4, 7), (14, 17), (18, 21), (25, 28), (30, 33)]);
        // Case-sensitive.
        assert_eq!(occ_cols("Foo foo FOO", "foo", false), vec![(4, 7)]);
        // Non-overlapping, columns in chars (not bytes).
        assert_eq!(occ_cols("aaaa", "aa", false), vec![(0, 2), (2, 4)]);
        assert_eq!(occ_cols("ção ção", "ção", true), vec![(0, 3), (4, 7)]);
        // The cap stops the scan.
        let mut out = Vec::new();
        line_occurrences("x x x x", &Occurrence { needle: "x".into(), whole_word: true }, 2, &mut out);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn indent_detection() {
        assert_eq!(detect_indent(&Buffer::from_str("a\n    b\n        c\n")), "    ");
        assert_eq!(detect_indent(&Buffer::from_str("a\n  b\n    c\n")), "  ");
    }

    #[test]
    fn visual_columns_with_tabs() {
        assert_eq!(visual_col("\tab", 1, 4), 4);
        assert_eq!(visual_col("a\tb", 2, 4), 4);
        assert_eq!(col_from_visual("a\tb", 4, 4), 2);
        assert_eq!(col_from_visual("abc", 10, 4), 3);
    }

    #[test]
    fn save_roundtrip_is_atomic_and_clears_dirty() {
        let dir = std::env::temp_dir().join(format!("lumen-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.ts");
        std::fs::write(&p, "const a = 1\n").unwrap();
        let mut b = Buffer::open(&p).unwrap();
        let mut e = Editor::new(&b, "//");
        e.doc_end(&mut b, false);
        typ(&mut b, &mut e, "x");
        assert!(b.is_dirty());
        b.save().unwrap();
        assert!(!b.is_dirty());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "const a = 1\nx");
        assert!(!dir.join("a.ts.lumen-tmp").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
