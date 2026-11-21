//! Single-line text input used by the palette, find bar, rename prompt and the SQLite query box.
//!
//! Besides the caret it keeps an optional selection anchor: `select_all` (Ctrl+A) and the
//! Shift+motions of [`TextField::key`] set it, any edit replaces the selected range and a plain
//! move collapses it. `sel_all` mirrors "the whole text is selected" for the callers that only
//! highlight that case.

use crate::textarea::KeyAct;
use winit::keyboard::{Key, NamedKey};

/// Undo steps kept.
const UNDO_MAX: usize = 100;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Last {
    #[default]
    Other,
    Type,
    Delete,
}

#[derive(Default, Clone)]
pub struct TextField {
    pub text: String,
    pub cursor: usize, // char index
    /// Whole text selected (Ctrl+A): the next edit replaces it, any cursor move clears it.
    pub sel_all: bool,
    /// Other end of the selection (char index); `None` without one.
    anchor: Option<usize>,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    last: Last,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl TextField {
    pub fn with(text: &str) -> Self {
        Self { text: text.to_string(), cursor: text.chars().count(), ..Default::default() }
    }

    fn byte(&self, ci: usize) -> usize {
        self.text.char_indices().nth(ci).map_or(self.text.len(), |(b, _)| b)
    }

    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Selected char range, ordered; `None` when nothing is selected.
    pub fn range(&self) -> Option<(usize, usize)> {
        let n = self.len();
        let a = self.anchor?.min(n);
        let c = self.cursor.min(n);
        (a != c).then(|| (a.min(c), a.max(c)))
    }

    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.range()?;
        Some(self.text.chars().skip(a).take(b - a).collect())
    }

    /// Recompute `sel_all` and drop an empty selection.
    fn sync(&mut self) {
        if self.range().is_none() {
            self.anchor = None;
        }
        let n = self.len();
        self.sel_all = n > 0 && self.range() == Some((0, n));
    }

    fn clear_sel(&mut self) {
        self.anchor = None;
        self.sel_all = false;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.len();
        self.sync();
    }

    /// Remember the state before an edit; typing / deleting runs coalesce into one step.
    fn snap(&mut self, kind: Last) {
        let join = kind != Last::Other && self.last == kind && self.range().is_none();
        if !join {
            self.undo.push((self.text.clone(), self.cursor));
            if self.undo.len() > UNDO_MAX {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last = kind;
    }

    fn take_selection(&mut self) -> bool {
        let Some((a, b)) = self.range() else {
            self.clear_sel();
            return false;
        };
        let (ba, bb) = (self.byte(a), self.byte(b));
        self.text.replace_range(ba..bb, "");
        self.cursor = a;
        self.clear_sel();
        true
    }

    pub fn insert(&mut self, s: &str) {
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        if s.is_empty() && self.range().is_none() {
            return;
        }
        let kind = if s.chars().next().is_some_and(char::is_whitespace) { Last::Other } else { Last::Type };
        self.snap(kind);
        self.take_selection();
        let b = self.byte(self.cursor);
        self.text.insert_str(b, &s);
        self.cursor += s.chars().count();
    }

    /// Paste: line breaks become spaces (the field has one line); one undo step.
    pub fn paste(&mut self, s: &str) {
        self.insert(&s.replace(['\r', '\n'], " "));
        self.last = Last::Other;
    }

    pub fn backspace(&mut self) -> bool {
        if self.range().is_none() && self.cursor == 0 {
            self.clear_sel();
            return false;
        }
        self.snap(Last::Delete);
        if self.take_selection() {
            return true;
        }
        let (a, b) = (self.byte(self.cursor - 1), self.byte(self.cursor));
        self.text.replace_range(a..b, "");
        self.cursor -= 1;
        true
    }

    pub fn delete(&mut self) -> bool {
        if self.range().is_none() && self.cursor >= self.len() {
            self.clear_sel();
            return false;
        }
        self.snap(Last::Delete);
        if self.take_selection() {
            return true;
        }
        let (a, b) = (self.byte(self.cursor), self.byte(self.cursor + 1));
        self.text.replace_range(a..b, "");
        true
    }

    /// Start of the word before `from`: spaces first, then a run of word chars (or one symbol).
    fn word_left(&self, from: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = from.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        let at = i;
        while i > 0 && is_word(chars[i - 1]) {
            i -= 1;
        }
        if i == at && i > 0 {
            i -= 1;
        }
        i
    }

    /// End of the word after `from`.
    fn word_right(&self, from: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = from.min(chars.len());
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let at = i;
        while i < chars.len() && is_word(chars[i]) {
            i += 1;
        }
        if i == at && i < chars.len() {
            i += 1;
        }
        i
    }

    /// Ctrl+Backspace: the selection, else the word before the caret.
    pub fn delete_word(&mut self) -> bool {
        if self.range().is_some() {
            return self.backspace();
        }
        let i = self.word_left(self.cursor);
        if i == self.cursor {
            return false;
        }
        self.snap(Last::Other);
        let (a, b) = (self.byte(i), self.byte(self.cursor));
        self.text.replace_range(a..b, "");
        self.cursor = i;
        true
    }

    /// Ctrl+Delete: the selection, else the word after the caret.
    pub fn delete_word_right(&mut self) -> bool {
        if self.range().is_some() {
            return self.delete();
        }
        let i = self.word_right(self.cursor);
        if i == self.cursor {
            return false;
        }
        self.snap(Last::Other);
        let (a, b) = (self.byte(self.cursor), self.byte(i));
        self.text.replace_range(a..b, "");
        true
    }

    /// Move the caret to `to`; `shift` extends the selection, otherwise it is dropped.
    pub fn move_to(&mut self, to: usize, shift: bool) {
        if shift {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = to.min(self.len());
        self.last = Last::Other;
        self.sync();
    }

    pub fn left(&mut self) {
        match self.range() {
            Some((a, _)) => self.move_to(a, false),
            None => self.move_to(self.cursor.saturating_sub(1), false),
        }
    }
    pub fn right(&mut self) {
        match self.range() {
            Some((_, b)) => self.move_to(b, false),
            None => self.move_to(self.cursor + 1, false),
        }
    }
    pub fn home(&mut self) {
        self.move_to(0, false);
    }
    pub fn end(&mut self) {
        self.move_to(self.len(), false);
    }

    fn restore(&mut self, (t, c): (String, usize)) {
        self.text = t;
        self.cursor = c.min(self.len());
        self.clear_sel();
        self.last = Last::Other;
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else { return false };
        self.redo.push((self.text.clone(), self.cursor));
        self.restore(s);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else { return false };
        self.undo.push((self.text.clone(), self.cursor));
        self.restore(s);
        true
    }

    fn cut(&mut self) -> KeyAct {
        let Some(t) = self.selected_text() else { return KeyAct::Handled };
        self.snap(Last::Other);
        self.take_selection();
        KeyAct::Copy(t)
    }

    /// The editing keys of a single-line field: Ctrl+A / C / X / V / Z / Y, caret motions with
    /// Shift (select) and Ctrl (by word), Home / End, Backspace / Delete (Ctrl: by word). Plain
    /// characters, Enter, Esc, Tab and unknown Ctrl / Alt combos are `Ignored` (the caller types
    /// the event's text with [`insert`](Self::insert)); `Copy` / `Paste` ask for the clipboard.
    pub fn key(&mut self, key: &Key, ctrl: bool, shift: bool, alt: bool) -> KeyAct {
        use NamedKey as N;
        if alt {
            return KeyAct::Ignored;
        }
        let n = self.len();
        match key {
            Key::Character(c) if ctrl => match c.to_lowercase().as_str() {
                "a" => self.select_all(),
                "c" if !shift => {
                    return self.selected_text().map_or(KeyAct::Handled, KeyAct::Copy);
                }
                "x" if !shift => return self.cut(),
                "v" => return KeyAct::Paste,
                "z" if shift => {
                    self.redo();
                }
                "z" => {
                    self.undo();
                }
                "y" if !shift => {
                    self.redo();
                }
                _ => return KeyAct::Ignored,
            },
            Key::Named(N::ArrowLeft) if ctrl => self.move_to(self.word_left(self.cursor), shift),
            Key::Named(N::ArrowRight) if ctrl => self.move_to(self.word_right(self.cursor), shift),
            Key::Named(N::ArrowLeft) if shift => self.move_to(self.cursor.saturating_sub(1), true),
            Key::Named(N::ArrowRight) if shift => self.move_to(self.cursor + 1, true),
            Key::Named(N::ArrowLeft) => self.left(),
            Key::Named(N::ArrowRight) => self.right(),
            Key::Named(N::Home) => self.move_to(0, shift),
            Key::Named(N::End) => self.move_to(n, shift),
            Key::Named(N::Backspace) if ctrl => {
                self.delete_word();
            }
            Key::Named(N::Backspace) => {
                self.backspace();
            }
            Key::Named(N::Delete) if shift && !ctrl => return self.cut(),
            Key::Named(N::Delete) if ctrl => {
                self.delete_word_right();
            }
            Key::Named(N::Delete) => {
                self.delete();
            }
            Key::Named(N::Insert) if shift && !ctrl => return KeyAct::Paste,
            Key::Named(N::Insert) if ctrl && !shift => {
                return self.selected_text().map_or(KeyAct::Handled, KeyAct::Copy);
            }
            _ => return KeyAct::Ignored,
        }
        KeyAct::Handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    fn named(n: NamedKey) -> Key {
        Key::Named(n)
    }

    #[test]
    fn edit_with_multibyte() {
        let mut f = TextField::with("aç");
        f.insert("b");
        assert_eq!(f.text, "açb");
        f.left();
        f.left();
        assert!(f.backspace());
        assert_eq!(f.text, "çb");
        assert!(f.delete());
        assert_eq!(f.text, "b");
        assert!(!f.backspace());
        f.end();
        f.insert("x\ny");
        assert_eq!(f.text, "bxy", "control chars are dropped");
    }

    #[test]
    fn select_all_replaces_on_next_edit() {
        let mut f = TextField::with("hello");
        f.select_all();
        assert!(f.sel_all);
        f.insert("x");
        assert_eq!(f.text, "x");
        assert!(!f.sel_all);
        let mut g = TextField::with("abc");
        g.select_all();
        assert!(g.backspace());
        assert_eq!(g.text, "");
        let mut h = TextField::with("abc");
        h.select_all();
        h.left();
        assert_eq!((h.cursor, h.sel_all), (0, false));
        let mut e = TextField::default();
        e.select_all();
        assert!(!e.sel_all, "nothing to select in an empty field");
    }

    #[test]
    fn ctrl_a_selects_all_through_key_and_typing_overwrites() {
        let mut f = TextField::with("SELECT 1");
        assert_eq!(f.key(&ch("a"), true, false, false), KeyAct::Handled);
        assert!(f.sel_all);
        assert_eq!(f.selected_text().as_deref(), Some("SELECT 1"));
        f.insert("x");
        assert_eq!(f.text, "x");
        // Upper case letter (Caps Lock / layout) works too.
        let mut g = TextField::with("abc");
        assert_eq!(g.key(&ch("A"), true, false, false), KeyAct::Handled);
        assert!(g.sel_all);
        // Ctrl+A then Delete / Backspace empties it.
        assert_eq!(g.key(&named(NamedKey::Delete), false, false, false), KeyAct::Handled);
        assert_eq!(g.text, "");
        // AltGr+A must not select.
        let mut h = TextField::with("abc");
        assert_eq!(h.key(&ch("a"), true, false, true), KeyAct::Ignored);
        assert!(!h.sel_all);
    }

    #[test]
    fn shift_motions_select_and_plain_motions_collapse() {
        let mut f = TextField::with("héllo wörld");
        f.key(&named(NamedKey::Home), false, false, false);
        f.key(&named(NamedKey::ArrowRight), false, true, false);
        f.key(&named(NamedKey::ArrowRight), false, true, false);
        assert_eq!(f.selected_text().as_deref(), Some("hé"));
        assert!(!f.sel_all);
        f.key(&named(NamedKey::ArrowLeft), false, true, false);
        assert_eq!(f.selected_text().as_deref(), Some("h"));
        // Shift+End selects to the end; typing replaces it.
        f.key(&named(NamedKey::End), false, true, false);
        assert_eq!(f.selected_text().as_deref(), Some("héllo wörld"));
        assert!(f.sel_all, "Home..End by Shift is the whole text");
        f.insert("Z");
        assert_eq!(f.text, "Z");
        // Shift+Home from the end selects to the start.
        let mut g = TextField::with("abc def");
        g.key(&named(NamedKey::Home), false, true, false);
        assert_eq!((g.range(), g.sel_all), (Some((0, 7)), true));
        // A plain arrow collapses to the matching end.
        g.key(&named(NamedKey::ArrowRight), false, false, false);
        assert_eq!((g.cursor, g.range()), (7, None));
        // Ctrl+Shift+Left selects a word.
        g.key(&named(NamedKey::ArrowLeft), true, true, false);
        assert_eq!(g.selected_text().as_deref(), Some("def"));
        g.key(&named(NamedKey::ArrowLeft), true, false, false);
        assert_eq!((g.cursor, g.range()), (0, None), "Ctrl+Left jumps a word and drops the selection");
        g.key(&named(NamedKey::ArrowRight), true, false, false);
        assert_eq!(g.cursor, 3);
        // Moving past the ends is harmless.
        g.key(&named(NamedKey::End), false, false, false);
        g.key(&named(NamedKey::ArrowRight), false, true, false);
        assert_eq!((g.cursor, g.range()), (7, None));
        g.key(&named(NamedKey::Home), false, false, false);
        g.key(&named(NamedKey::ArrowLeft), false, false, false);
        assert_eq!(g.cursor, 0);
    }

    #[test]
    fn copy_cut_paste_ask_for_the_clipboard() {
        let mut f = TextField::with("one two");
        assert_eq!(f.key(&ch("c"), true, false, false), KeyAct::Handled, "nothing selected, nothing copied");
        f.key(&ch("a"), true, false, false);
        assert_eq!(f.key(&ch("c"), true, false, false), KeyAct::Copy("one two".into()));
        assert_eq!(f.text, "one two", "copy leaves the text");
        assert_eq!(f.key(&ch("x"), true, false, false), KeyAct::Copy("one two".into()));
        assert_eq!(f.text, "");
        assert_eq!(f.key(&ch("v"), true, false, false), KeyAct::Paste);
        f.paste("a\r\nb");
        assert_eq!(f.text, "a  b", "line breaks of a paste become spaces");
        // Paste over a selection replaces it.
        f.key(&ch("a"), true, false, false);
        f.paste("SELECT 1");
        assert_eq!(f.text, "SELECT 1");
        // Partial cut.
        f.home();
        f.key(&named(NamedKey::ArrowRight), true, true, false);
        assert_eq!(f.key(&ch("x"), true, false, false), KeyAct::Copy("SELECT".into()));
        assert_eq!(f.text, " 1");
    }

    #[test]
    fn undo_and_redo_restore_the_text() {
        let mut f = TextField::default();
        f.insert("a");
        f.insert("b");
        f.insert("c");
        assert_eq!(f.text, "abc");
        f.key(&ch("z"), true, false, false);
        assert_eq!(f.text, "", "typing in one run is one undo step");
        f.key(&ch("z"), true, true, false);
        assert_eq!(f.text, "abc", "Ctrl+Shift+Z redoes");
        f.select_all();
        f.insert("x");
        assert_eq!(f.text, "x");
        f.key(&ch("z"), true, false, false);
        assert_eq!(f.text, "abc", "overwriting the selection is its own step");
        f.key(&ch("y"), true, false, false);
        assert_eq!(f.text, "x");
        // Nothing to undo is harmless.
        let mut g = TextField::with("keep");
        assert_eq!(g.key(&ch("z"), true, false, false), KeyAct::Handled);
        assert_eq!(g.text, "keep");
    }

    #[test]
    fn ctrl_backspace_and_ctrl_delete_work_by_word() {
        let mut f = TextField::with("SELECT * FROM users");
        f.key(&named(NamedKey::Backspace), true, false, false);
        assert_eq!(f.text, "SELECT * FROM ");
        f.key(&named(NamedKey::Backspace), true, false, false);
        assert_eq!(f.text, "SELECT * ");
        f.key(&named(NamedKey::Backspace), true, false, false);
        assert_eq!(f.text, "SELECT ", "a symbol goes alone");
        f.home();
        f.key(&named(NamedKey::Delete), true, false, false);
        assert_eq!(f.text, " ");
        // With a selection Ctrl+Backspace deletes just the selection.
        let mut g = TextField::with("alpha beta");
        g.key(&named(NamedKey::ArrowLeft), false, true, false);
        g.key(&named(NamedKey::Backspace), true, false, false);
        assert_eq!(g.text, "alpha bet");
    }

    #[test]
    fn delete_word_stops_at_boundaries() {
        let mut f = TextField::with("foo bar_baz");
        f.delete_word();
        assert_eq!(f.text, "foo ");
        f.delete_word();
        assert_eq!(f.text, "");
    }

    #[test]
    fn unrelated_keys_are_left_to_the_caller() {
        let mut f = TextField::with("x");
        assert_eq!(f.key(&ch("q"), false, false, false), KeyAct::Ignored);
        assert_eq!(f.key(&named(NamedKey::Enter), false, false, false), KeyAct::Ignored);
        assert_eq!(f.key(&ch("p"), true, false, false), KeyAct::Ignored, "global shortcuts keep working");
    }
}
