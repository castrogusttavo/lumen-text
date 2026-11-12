//! The in-server document store: open documents, kept parsed.

use serde_json::Value;

use super::conv::range_from;
use crate::line_index::{LineIndex, Position};
use crate::parser::{Parse, parse};

/// One open document with its derived data, rebuilt on every change.
pub struct Doc {
    pub version: i64,
    pub text: String,
    pub index: LineIndex,
    pub parse: Parse,
}

impl Doc {
    pub fn new(version: i64, text: String) -> Doc {
        let index = LineIndex::new(&text);
        let parse = parse(&text);
        Doc { version, text, index, parse }
    }

    fn set_text(&mut self, text: String) {
        self.index = LineIndex::new(&text);
        self.parse = parse(&text);
        self.text = text;
    }

    /// Apply `contentChanges` in order. A change without a range replaces the whole text.
    pub fn apply_changes(&mut self, version: i64, changes: &[Value]) {
        let mut text = std::mem::take(&mut self.text);
        for ch in changes {
            let new = ch.get("text").and_then(Value::as_str).unwrap_or("");
            match ch.get("range").and_then(range_from) {
                Some(r) => {
                    let s = LineIndex::new(&text).span(r);
                    text.replace_range(s.start..s.end, new);
                }
                None => text = new.to_string(),
            }
        }
        self.version = version;
        self.set_text(text);
    }

    pub fn offset(&self, p: Position) -> usize {
        self.index.offset(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn change(sl: u32, sc: u32, el: u32, ec: u32, text: &str) -> Value {
        json!({"range": {"start": {"line": sl, "character": sc}, "end": {"line": el, "character": ec}}, "text": text})
    }

    #[test]
    fn incremental_edits() {
        let mut d = Doc::new(1, "<p>a\nb</p>".into());
        d.apply_changes(2, &[change(0, 3, 0, 4, "xyz")]);
        assert_eq!(d.text, "<p>xyz\nb</p>");
        d.apply_changes(3, &[change(0, 6, 1, 1, "-"), change(0, 0, 0, 0, "<go:if a/>")]);
        assert_eq!(d.text, "<go:if a/><p>xyz-</p>");
        assert_eq!(d.version, 3);
        assert_eq!(d.parse.diagnostics.len(), 1);
    }

    #[test]
    fn full_replace_and_utf16() {
        let mut d = Doc::new(1, "a😀b".into());
        // The emoji is two UTF-16 units: replace "b" at column 3.
        d.apply_changes(2, &[change(0, 3, 0, 4, "c")]);
        assert_eq!(d.text, "a😀c");
        d.apply_changes(3, &[json!({"text": "new"})]);
        assert_eq!(d.text, "new");
    }
}
