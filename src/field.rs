//! Single-line text input used by the palette, find bar and rename prompt.

#[derive(Default, Clone)]
pub struct TextField {
    pub text: String,
    pub cursor: usize, // char index
    /// Whole text selected (Ctrl+A): the next edit replaces it, any cursor move clears it.
    pub sel_all: bool,
}

impl TextField {
    pub fn with(text: &str) -> Self {
        Self { text: text.to_string(), cursor: text.chars().count(), sel_all: false }
    }

    fn byte(&self, ci: usize) -> usize {
        self.text.char_indices().nth(ci).map_or(self.text.len(), |(b, _)| b)
    }

    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn select_all(&mut self) {
        self.sel_all = !self.text.is_empty();
    }

    fn take_selection(&mut self) -> bool {
        if self.sel_all {
            self.sel_all = false;
            self.text.clear();
            self.cursor = 0;
            return true;
        }
        false
    }

    pub fn insert(&mut self, s: &str) {
        self.take_selection();
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        let b = self.byte(self.cursor);
        self.text.insert_str(b, &s);
        self.cursor += s.chars().count();
    }

    pub fn backspace(&mut self) -> bool {
        if self.take_selection() {
            return true;
        }
        if self.cursor == 0 {
            return false;
        }
        let (a, b) = (self.byte(self.cursor - 1), self.byte(self.cursor));
        self.text.replace_range(a..b, "");
        self.cursor -= 1;
        true
    }

    pub fn delete(&mut self) -> bool {
        if self.take_selection() {
            return true;
        }
        if self.cursor >= self.len() {
            return false;
        }
        let (a, b) = (self.byte(self.cursor), self.byte(self.cursor + 1));
        self.text.replace_range(a..b, "");
        true
    }

    pub fn delete_word(&mut self) -> bool {
        if self.take_selection() {
            return true;
        }
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
            i -= 1;
        }
        if i == self.cursor && i > 0 {
            i -= 1;
        }
        if i == self.cursor {
            return false;
        }
        let (a, b) = (self.byte(i), self.byte(self.cursor));
        self.text.replace_range(a..b, "");
        self.cursor = i;
        true
    }

    pub fn left(&mut self) {
        if self.sel_all {
            self.sel_all = false;
            self.cursor = 0;
            return;
        }
        self.cursor = self.cursor.saturating_sub(1);
    }
    pub fn right(&mut self) {
        if self.sel_all {
            self.sel_all = false;
            self.cursor = self.len();
            return;
        }
        self.cursor = (self.cursor + 1).min(self.len());
    }
    pub fn home(&mut self) {
        self.sel_all = false;
        self.cursor = 0;
    }
    pub fn end(&mut self) {
        self.sel_all = false;
        self.cursor = self.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        f.insert("x");
        assert_eq!(f.text, "x");
        let mut g = TextField::with("abc");
        g.select_all();
        assert!(g.backspace());
        assert_eq!(g.text, "");
        let mut h = TextField::with("abc");
        h.select_all();
        h.left();
        assert_eq!((h.cursor, h.sel_all), (0, false));
    }

    #[test]
    fn delete_word_stops_at_boundaries() {
        let mut f = TextField::with("foo bar_baz");
        f.delete_word();
        assert_eq!(f.text, "foo ");
        f.delete_word();
        assert_eq!(f.text, "");
    }
}
