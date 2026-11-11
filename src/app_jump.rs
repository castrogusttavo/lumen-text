//! "Go to: Jump to…" (`Alt+J`): flash / easymotion-style jumps inside the focused pane. Type one or
//! two characters; every visible match gets a one- or two-letter label drawn over the text in an
//! accent chip, and typing a label moves the cursor there (`Shift` extends the selection). `Esc`
//! cancels; `Backspace` takes back the last key.
//!
//! After one typed character the label letters leave out every character that follows a match, so
//! the next key is never ambiguous: it is either a label or the second search character. Matching
//! is smart-case (a lowercase query ignores case). Nothing animates: the state only changes on key
//! presses, so waiting for input never repaints.

use crate::{
    app::{App, Focus, KeyIn},
    doc::Doc,
    ui::{BOLD, Canvas, Rect, line_h},
};
use winit::keyboard::{Key, NamedKey};

/// Label letters, easiest to reach first (home row, top row, bottom row).
pub const LABEL_KEYS: &str = "asdfghjklqwertyuiopzxcvbnm";

/// A labelled match.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub pos: usize,
    pub label: String,
}

/// A "Jump to…" waiting for keys.
#[derive(Default, Debug)]
pub struct Jump {
    /// The document the labels belong to (another tab or pane drops the jump).
    pub doc: usize,
    /// The search characters typed so far (at most two).
    pub query: String,
    /// First letter of a two-letter label, once typed.
    pub prefix: Option<char>,
    /// Labelled matches, nearest to the cursor first.
    pub targets: Vec<Target>,
    /// Number of visible matches (also those left without a label).
    pub found: usize,
    /// The nearest match.
    first: Option<usize>,
}

/// What a key did to the jump.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// Keep waiting for keys.
    Wait,
    /// Move to `pos`; `label` is true when a label was typed (only then may Shift extend).
    Go { pos: usize, label: bool },
    /// Give up (no match, or a key that is no label).
    Cancel,
}

/// `m` labels from [`LABEL_KEYS`] minus `excluded`: single letters while they suffice, else the
/// last few letters become prefixes of two-letter labels (and are never labels on their own), so
/// no label is the start of another. The nearest matches get the short ones. Fewer than `m` labels
/// come back when even two letters cannot cover them all.
pub fn assign_labels(m: usize, excluded: &[char]) -> Vec<String> {
    let keys: Vec<char> = LABEL_KEYS.chars().filter(|c| !excluded.contains(c)).collect();
    let n = keys.len();
    if m == 0 || n == 0 {
        return Vec::new();
    }
    if m <= n {
        return keys[..m].iter().map(|c| c.to_string()).collect();
    }
    let mut p = 1;
    while p < n && (n - p) + p * n < m {
        p += 1;
    }
    let singles = n - p;
    let mut out: Vec<String> = keys[..singles].iter().map(|c| c.to_string()).collect();
    'outer: for &a in &keys[singles..] {
        for &b in &keys {
            if out.len() >= m {
                break 'outer;
            }
            out.push(format!("{a}{b}"));
        }
    }
    out
}

fn same(q: char, c: char, fold: bool) -> bool {
    if fold { c == q || c.to_lowercase().eq(q.to_lowercase()) } else { c == q }
}

/// Matches of `query` in one line of text: (char column, the character right after the match).
/// `fold` ignores case.
pub fn line_matches(text: &str, query: &[char], fold: bool, out: &mut Vec<(usize, Option<char>)>) {
    let chars: Vec<char> = text.chars().collect();
    let n = query.len();
    if n == 0 || chars.len() < n {
        return;
    }
    for i in 0..=chars.len() - n {
        if (0..n).all(|k| same(query[k], chars[i + k], fold)) {
            out.push((i, chars.get(i + n).copied()));
        }
    }
}

/// Matches of `query` that are on screen in `d` (vertically and horizontally inside the text
/// area, folded lines skipped): (char index, the character after it), nearest to the cursor first.
pub fn visible_matches(d: &Doc, query: &str) -> Vec<(usize, Option<char>)> {
    let q: Vec<char> = query.chars().collect();
    let fold = !q.iter().any(|c| c.is_uppercase());
    let (first, last) = d.visible_line_span();
    let (left, right) = (d.rect.x + d.gutter_w(), d.rect.right());
    let (cw, half) = (d.cell_w.ceil() as i32, line_h() as i32 / 2);
    let mut found = Vec::new();
    let mut cols = Vec::new();
    for l in first..=last {
        if d.view.folds.is_hidden(l) {
            continue;
        }
        cols.clear();
        line_matches(&d.buf.line(l), &q, fold, &mut cols);
        let base = d.buf.line_start(l);
        for &(c, next) in &cols {
            let p = base + c;
            if let Some((x, y)) = d.screen_pos(p)
                && x >= left
                && x + cw <= right
                && y + half >= d.rect.y
                && y + half < d.rect.bottom()
            {
                found.push((p, next));
            }
        }
    }
    let head = d.ed.head();
    found.sort_by_key(|(p, _)| p.abs_diff(head));
    found
}

impl Jump {
    pub fn new(doc: usize) -> Self {
        Jump { doc, ..Default::default() }
    }

    /// Re-run the search for the current query and hand out the labels.
    fn refresh(&mut self, d: &Doc) {
        self.prefix = None;
        let found = visible_matches(d, &self.query);
        self.found = found.len();
        self.first = found.first().map(|f| f.0);
        // With one character typed, a letter that continues some match must stay free for it.
        let excluded: Vec<char> = if self.query.chars().count() < 2 {
            found.iter().filter_map(|f| f.1).flat_map(char::to_lowercase).collect()
        } else {
            Vec::new()
        };
        let labels = assign_labels(found.len(), &excluded);
        self.targets = found.iter().zip(labels).map(|(f, label)| Target { pos: f.0, label }).collect();
    }

    /// A typed character: a label (letters compare in lowercase, so Shift+label still is one) or
    /// the next search character.
    pub fn feed(&mut self, d: &Doc, ch: char) -> Step {
        let lc = ch.to_lowercase().next().unwrap_or(ch);
        if let Some(p) = self.prefix {
            return match self.targets.iter().find(|t| t.label.chars().eq([p, lc])) {
                Some(t) => Step::Go { pos: t.pos, label: true },
                None => Step::Cancel,
            };
        }
        if !self.query.is_empty() {
            if let Some(t) = self.targets.iter().find(|t| t.label.chars().eq([lc])) {
                return Step::Go { pos: t.pos, label: true };
            }
            if self.targets.iter().any(|t| t.label.chars().count() == 2 && t.label.starts_with(lc)) {
                self.prefix = Some(lc);
                return Step::Wait;
            }
            if self.query.chars().count() >= 2 {
                return Step::Cancel;
            }
        }
        self.query.push(ch);
        self.refresh(d);
        match self.found {
            0 => Step::Cancel,
            1 => Step::Go { pos: self.first.unwrap_or(0), label: false },
            _ => Step::Wait,
        }
    }

    /// Backspace: forget the typed label letter, else the last search character.
    pub fn back(&mut self, d: &Doc) {
        if self.prefix.take().is_some() {
            return;
        }
        self.query.pop();
        if self.query.is_empty() {
            self.targets.clear();
            self.found = 0;
            self.first = None;
        } else {
            self.refresh(d);
        }
    }

    /// Status bar text while waiting.
    pub fn status(&self) -> String {
        match (self.query.is_empty(), self.prefix) {
            (true, _) => "Jump to… type 1–2 characters".to_string(),
            (false, Some(p)) => format!("Jump “{}”: {p}…", self.query),
            (false, None) => format!("Jump “{}”: type a label", self.query),
        }
    }
}

impl App {
    /// "Go to: Jump to…": wait for characters in the focused pane.
    pub fn jump_start(&mut self) {
        if self.docs.get(self.active).is_none() {
            return;
        }
        self.focus = Focus::Editor;
        self.completion = None;
        self.jump = Some(Jump::new(self.active));
        self.request_redraw();
    }

    /// A key while a jump waits. True when the key was used up; false cancels the jump and lets the
    /// key do its normal work (arrows, Ctrl / Alt shortcuts).
    pub fn jump_key(&mut self, ev: &KeyIn) -> bool {
        let Some(mut j) = self.jump.take() else { return false };
        if matches!(
            &ev.key,
            Key::Named(
                NamedKey::Shift
                    | NamedKey::Control
                    | NamedKey::Alt
                    | NamedKey::AltGraph
                    | NamedKey::Super
                    | NamedKey::Meta
                    | NamedKey::CapsLock
            )
        ) {
            self.jump = Some(j);
            return true;
        }
        if j.doc != self.active || self.docs.get(self.active).is_none() {
            return false;
        }
        self.request_redraw();
        match &ev.key {
            Key::Named(NamedKey::Escape | NamedKey::Enter | NamedKey::Tab) => return true,
            Key::Named(NamedKey::Backspace) => {
                j.back(&self.docs[self.active]);
                self.jump = Some(j);
                return true;
            }
            _ => {}
        }
        if self.mods.control_key() || self.mods.alt_key() || self.mods.super_key() {
            return false;
        }
        let ch = ev.text.as_deref().and_then(|t| t.chars().find(|c| !c.is_control())).or(match &ev.key {
            Key::Character(c) => c.chars().next(),
            _ => None,
        });
        let Some(ch) = ch else { return false };
        match j.feed(&self.docs[self.active], ch) {
            Step::Wait => self.jump = Some(j),
            Step::Go { pos, label } => self.jump_label_go(pos, label && self.mods.shift_key()),
            Step::Cancel => {
                if j.found == 0 {
                    self.say(format!("Jump: nothing matches “{}” on screen", j.query));
                }
            }
        }
        true
    }

    fn jump_label_go(&mut self, pos: usize, extend: bool) {
        self.push_nav();
        let d = &mut self.docs[self.active];
        let pos = pos.min(d.buf.len_chars());
        d.ed.clear_extra();
        d.ed.click(&mut d.buf, pos, extend);
        d.reveal = true;
    }

    /// The labels over the focused pane's text: accent chips with the theme's on-accent text.
    pub fn draw_jump(&mut self, cv: &mut Canvas) {
        let Some(j) = &self.jump else { return };
        let Some(d) = self.docs.get(self.active).filter(|_| j.doc == self.active) else { return };
        let (bg, fg) = (self.theme.chrome.accent, self.theme.chrome.on_accent);
        let lh = line_h() as i32;
        let gw = d.gutter_w();
        let body = Rect::new(d.rect.x + gw, d.rect.y, (d.rect.w - gw).max(0), d.rect.h);
        let len = d.buf.len_chars();
        for t in &j.targets {
            let rest: String = match j.prefix {
                Some(p) if t.label.starts_with(p) => t.label.chars().skip(1).collect(),
                Some(_) => continue,
                None => t.label.clone(),
            };
            if t.pos > len {
                continue;
            }
            let Some((x, y)) = d.screen_pos(t.pos) else { continue };
            let chip = Rect::new(x, y, self.tx.cells(rest.chars().count()).max(1), lh);
            cv.fill_round_clip(chip, 3, bg, body);
            self.tx.draw_styled(cv, x, y, &[(rest, fg, BOLD)], body);
        }
    }

    /// Status bar text while a jump waits for keys.
    pub fn jump_status(&self) -> Option<String> {
        self.jump.as_ref().map(Jump::status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app_unsaved::testkit, buffer::Buffer};
    use winit::keyboard::ModifiersState;

    #[test]
    fn labels_are_single_letters_while_they_suffice_and_never_prefixes() {
        assert_eq!(assign_labels(3, &[]), vec!["a", "s", "d"]);
        assert_eq!(assign_labels(3, &['s']), vec!["a", "d", "f"], "excluded letters are skipped");
        assert!(assign_labels(0, &[]).is_empty());
        let all: Vec<char> = LABEL_KEYS.chars().collect();
        assert!(assign_labels(5, &all).is_empty());
        for m in [26, 27, 40, 100, 676] {
            let l = assign_labels(m, &[]);
            assert_eq!(l.len(), m);
            let set: std::collections::HashSet<&String> = l.iter().collect();
            assert_eq!(set.len(), m, "unique labels for {m}");
            for a in &l {
                assert!(a.chars().count() <= 2);
                assert!(!l.iter().any(|b| b != a && b.starts_with(a.as_str())), "{a} is a prefix ({m})");
            }
        }
        // 27 matches: one letter turns into a prefix; the nearest 25 keep a single letter.
        let l = assign_labels(27, &[]);
        assert_eq!(l.iter().filter(|s| s.len() == 1).count(), 25);
        assert_eq!(l[25], "ma");
        assert_eq!(assign_labels(1000, &[]).len(), 676, "two letters cover at most 26 * 26");
    }

    #[test]
    fn matching_is_smart_case_and_reports_the_next_char() {
        let mut out = Vec::new();
        line_matches("Foo fob", &['f', 'o'], true, &mut out);
        assert_eq!(out, vec![(0, Some('o')), (4, Some('b'))]);
        out.clear();
        line_matches("Foo fob", &['F'], false, &mut out);
        assert_eq!(out, vec![(0, Some('o'))]);
        out.clear();
        line_matches("ab", &['b'], true, &mut out);
        assert_eq!(out, vec![(1, None)]);
    }

    fn doc(text: &str) -> Doc {
        let mut d = Doc::new(Buffer::from_str(text), None, None, "//", 8.0);
        d.rect = Rect::new(0, 0, 600, 400);
        d
    }

    #[test]
    fn labels_skip_letters_that_continue_a_match() {
        let d = doc("xa xb xc\nxd\n");
        let mut j = Jump::new(0);
        assert_eq!(j.feed(&d, 'x'), Step::Wait);
        assert_eq!(j.found, 4);
        for t in &j.targets {
            assert!(!["a", "b", "c", "d"].contains(&t.label.as_str()), "{} could be a second character", t.label);
        }
        // Nearest first: the cursor is at 0, so the first match gets the first free letter.
        assert_eq!(j.targets[0], Target { pos: 0, label: "s".into() });
        // A second character narrows the search; then every letter is a label again.
        assert_eq!(j.feed(&d, 'c'), Step::Go { pos: 6, label: false }, "a single match jumps at once");
    }

    #[test]
    fn two_letter_labels_take_two_keys() {
        let d = doc(&"z ".repeat(30));
        let mut j = Jump::new(0);
        assert_eq!(j.feed(&d, 'z'), Step::Wait);
        assert_eq!(j.found, 30);
        let far = j.targets.iter().find(|t| t.label.len() == 2).cloned().unwrap();
        let mut keys = far.label.chars();
        assert_eq!(j.feed(&d, keys.next().unwrap()), Step::Wait);
        assert!(j.prefix.is_some());
        j.back(&d);
        assert!(j.prefix.is_none(), "Backspace forgets the label letter");
        let mut keys = far.label.chars();
        j.feed(&d, keys.next().unwrap());
        assert_eq!(j.feed(&d, keys.next().unwrap()), Step::Go { pos: far.pos, label: true });
    }

    #[test]
    fn only_visible_matches_get_labels() {
        let text: String = (0..200).map(|i| format!("q{i}\n")).collect();
        let d = doc(&text);
        let rows = (400.0 / line_h()) as usize;
        let m = visible_matches(&d, "q");
        assert!(m.len() <= rows + 1 && m.len() >= rows.saturating_sub(1), "{} matches for {rows} rows", m.len());
        assert!(m.iter().all(|(p, _)| d.buf.line_col(*p).0 <= rows));
    }

    fn key(app: &mut App, k: Key, text: Option<&str>, mods: ModifiersState) {
        app.mods = mods;
        app.handle_key(&KeyIn { key: k, text: text.map(str::to_string) });
        app.mods = ModifiersState::empty();
    }

    fn ch(app: &mut App, c: &str, mods: ModifiersState) {
        key(app, Key::Character(c.into()), Some(c), mods);
    }

    #[test]
    fn alt_j_then_characters_and_a_label_moves_the_cursor() {
        let _g = testkit::lock();
        let dir = testkit::tmp("jumpkeys");
        let mut app = testkit::test_app(&dir);
        let i = testkit::untitled(&mut app, "let foo = 1;\nfoo(bar);\nreturn foo;\n");
        app.active = i;
        app.focus = Focus::Editor;
        app.docs[i].rect = Rect::new(0, 0, 600, 400);
        let none = ModifiersState::empty();
        ch(&mut app, "j", ModifiersState::ALT);
        assert!(app.jump.is_some(), "Alt+J waits for characters");
        assert!(app.jump_status().unwrap().contains("type"));
        ch(&mut app, "f", none);
        ch(&mut app, "o", none);
        let j = app.jump.as_ref().expect("three matches: still waiting");
        assert_eq!(j.found, 3);
        assert_eq!(app.docs[i].buf.to_string(), "let foo = 1;\nfoo(bar);\nreturn foo;\n", "nothing typed");
        let target = j.targets.iter().find(|t| app.docs[i].buf.line_col(t.pos).0 == 2).cloned().unwrap();
        ch(&mut app, &target.label, none);
        assert!(app.jump.is_none());
        assert_eq!(app.docs[i].ed.sel, crate::buffer::Sel::caret(target.pos));
        // Shift + label extends the selection from the cursor.
        ch(&mut app, "j", ModifiersState::ALT);
        ch(&mut app, "(", none);
        // A single match jumps right away (no label, so no extension either).
        let paren = app.docs[i].buf.to_string().find('(').unwrap();
        assert_eq!(app.docs[i].ed.sel, crate::buffer::Sel::caret(paren));
        let d = &mut app.docs[i];
        d.ed.click(&mut d.buf, 0, false);
        ch(&mut app, "j", ModifiersState::ALT);
        ch(&mut app, "f", none);
        ch(&mut app, "o", none);
        let t = app.jump.as_ref().unwrap().targets.iter().find(|t| t.pos > 20).cloned().unwrap();
        let upper = t.label.to_uppercase();
        ch(&mut app, &upper, ModifiersState::SHIFT);
        assert_eq!((app.docs[i].ed.sel.anchor, app.docs[i].ed.sel.head), (0, t.pos));
        // Esc cancels without moving; an arrow cancels and moves as usual.
        ch(&mut app, "j", ModifiersState::ALT);
        ch(&mut app, "f", none);
        key(&mut app, Key::Named(NamedKey::Escape), None, none);
        assert!(app.jump.is_none());
        assert_eq!((app.docs[i].ed.sel.anchor, app.docs[i].ed.sel.head), (0, t.pos), "Esc moved nothing");
        let d = &mut app.docs[i];
        d.ed.click(&mut d.buf, t.pos, false);
        ch(&mut app, "j", ModifiersState::ALT);
        key(&mut app, Key::Named(NamedKey::ArrowLeft), None, none);
        assert!(app.jump.is_none());
        assert_eq!(app.docs[i].ed.sel.head, t.pos - 1, "the arrow still moved the cursor");
        // Nothing on screen: the jump gives up and says so.
        ch(&mut app, "j", ModifiersState::ALT);
        ch(&mut app, "#", none);
        assert!(app.jump.is_none());
        assert!(app.status.as_ref().is_some_and(|s| s.0.contains("nothing matches")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
