//! The composer's pure logic: what a key means in it, the sent-message history (Up / Down in an
//! empty box), the box's height, what "attach file" / "attach selection" insert, and the bytes
//! that carry a message into the agent's terminal. No App, no drawing: unit-tested below. Text
//! editing and wrapping itself is `crate::textarea::TextArea`.

use winit::keyboard::{Key, NamedKey};

/// Lines of text the box shows at least / at most (more scroll inside it).
pub const MIN_LINES: usize = 2;
pub const MAX_LINES: usize = 8;
/// Messages the history keeps.
pub const HISTORY_MAX: usize = 100;
/// Largest piece of the editor's selection that "attach selection" inserts, in bytes.
pub const SELECTION_MAX: usize = 32 * 1024;

/// Lines the box shows for `n` wrapped lines of text.
pub fn visible_lines(n: usize) -> usize {
    n.clamp(MIN_LINES, MAX_LINES)
}

/// What a key does in the composer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// Send the message.
    Send,
    /// Older message from the history.
    HistoryPrev,
    /// Newer message (or the draft) from the history.
    HistoryNext,
    /// The keyboard moves to the agent's terminal.
    ToTerminal,
    /// Esc: goes to the agent (it interrupts what it is doing).
    Interrupt,
    /// Everything else: the text area edits.
    Edit,
}

/// Route a key. `empty`: the box holds no text; `browsing`: it shows an entry of the history.
/// Enter sends, Shift+Enter is a new line (the text area), Ctrl+Enter sends too; Up / Down walk
/// the history only from an empty box (or while walking it), so they still move the caret in text.
pub fn route(key: &Key, ctrl: bool, shift: bool, alt: bool, empty: bool, browsing: bool) -> Route {
    let Key::Named(n) = key else { return Route::Edit };
    match n {
        NamedKey::Enter if shift && !ctrl => Route::Edit,
        NamedKey::Enter if !alt => Route::Send,
        NamedKey::Tab if !ctrl && !alt && !shift => Route::ToTerminal,
        NamedKey::Escape if !ctrl && !shift && !alt => Route::Interrupt,
        NamedKey::ArrowUp if !ctrl && !shift && !alt && (empty || browsing) => Route::HistoryPrev,
        NamedKey::ArrowDown if !ctrl && !shift && !alt && browsing => Route::HistoryNext,
        _ => Route::Edit,
    }
}

/// Messages sent from the composer, newest last, walked with Up / Down.
#[derive(Default, Clone)]
pub struct History {
    items: Vec<String>,
    /// Entry shown now (`None`: the draft, what was being typed).
    at: Option<usize>,
    /// The draft kept while an entry is shown.
    draft: String,
}

impl History {
    /// Remember a sent message (empty ones and a repeat of the last are not kept).
    pub fn push(&mut self, text: &str) {
        self.reset();
        let t = text.trim_end();
        if t.trim().is_empty() || self.items.last().is_some_and(|l| l == t) {
            return;
        }
        self.items.push(t.to_string());
        if self.items.len() > HISTORY_MAX {
            self.items.remove(0);
        }
    }

    /// An entry is shown in the box.
    pub fn browsing(&self) -> bool {
        self.at.is_some()
    }

    /// Back in time: the message to show instead of `current` (kept as the draft the first time).
    /// `None` when there is nothing older.
    pub fn prev(&mut self, current: &str) -> Option<String> {
        let i = match self.at {
            None if self.items.is_empty() => return None,
            None => {
                self.draft = current.to_string();
                self.items.len() - 1
            }
            Some(0) => return None,
            Some(i) => i - 1,
        };
        self.at = Some(i);
        Some(self.items[i].clone())
    }

    /// Forward in time; past the newest entry the draft comes back.
    pub fn next(&mut self) -> Option<String> {
        let i = self.at?;
        if i + 1 < self.items.len() {
            self.at = Some(i + 1);
            return Some(self.items[i + 1].clone());
        }
        self.at = None;
        Some(std::mem::take(&mut self.draft))
    }

    /// Stop walking (the user edited the text, or sent it).
    pub fn reset(&mut self) {
        self.at = None;
        self.draft.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }
}

/// What "attach file" inserts: an `@` mention of the file (quoted when the path has spaces), and
/// a space after it so typing goes on.
pub fn mention(rel: &str) -> String {
    if rel.chars().any(char::is_whitespace) { format!("@\"{rel}\" ") } else { format!("@{rel} ") }
}

/// What "attach selection" inserts: the place (`path:first-last`, 1-based) and the text fenced.
/// A selection over [`SELECTION_MAX`] is cut at a line end and says so.
pub fn selection_block(rel: &str, first: usize, text: &str) -> String {
    let text = text.trim_end_matches(['\n', '\r']);
    let (body, cut) = if text.len() > SELECTION_MAX {
        let mut end = SELECTION_MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let end = text[..end].rfind('\n').unwrap_or(end);
        (&text[..end], true)
    } else {
        (text, false)
    };
    let last = first + body.lines().count().saturating_sub(1);
    let place = if last > first { format!("{rel}:{first}-{last}") } else { format!("{rel}:{first}") };
    let more = if cut { "\n… (selection cut)" } else { "" };
    format!("{place}\n```\n{body}{more}\n```\n")
}

/// The two writes that send `text` to the agent: the message as one paste (bracketed when the
/// agent asked for it, so its lines are not submitted one by one), then Enter. `None` for a
/// message with nothing in it.
pub fn submit_bytes(text: &str, bracketed: bool) -> Option<(Vec<u8>, Vec<u8>)> {
    let t = text.trim_end();
    if t.trim().is_empty() {
        return None;
    }
    Some((super::paste_payload(t, bracketed), b"\r".to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::textarea::{KeyAct, TextArea};

    fn named(n: NamedKey) -> Key {
        Key::Named(n)
    }

    #[test]
    fn enter_sends_shift_enter_is_a_new_line() {
        let enter = named(NamedKey::Enter);
        assert_eq!(route(&enter, false, false, false, false, false), Route::Send);
        assert_eq!(route(&enter, true, false, false, false, false), Route::Send, "Ctrl+Enter sends too");
        assert_eq!(route(&enter, false, true, false, false, false), Route::Edit, "Shift+Enter breaks the line");
        let mut t = TextArea::default();
        t.type_text("a");
        assert_eq!(t.key(&enter, false, true, false), KeyAct::Handled);
        t.type_text("b");
        assert_eq!(t.text(), "a\nb");
    }

    #[test]
    fn tab_esc_and_arrows_route_by_state() {
        assert_eq!(route(&named(NamedKey::Tab), false, false, false, false, false), Route::ToTerminal);
        assert_eq!(route(&named(NamedKey::Tab), false, true, false, false, false), Route::Edit);
        assert_eq!(route(&named(NamedKey::Escape), false, false, false, true, false), Route::Interrupt);
        let up = named(NamedKey::ArrowUp);
        let down = named(NamedKey::ArrowDown);
        assert_eq!(route(&up, false, false, false, true, false), Route::HistoryPrev, "empty box");
        assert_eq!(route(&up, false, false, false, false, false), Route::Edit, "text: Up moves the caret");
        assert_eq!(route(&up, false, false, false, false, true), Route::HistoryPrev, "walking the history");
        assert_eq!(route(&down, false, false, false, true, false), Route::Edit);
        assert_eq!(route(&down, false, false, false, false, true), Route::HistoryNext);
        assert_eq!(route(&Key::Character("a".into()), false, false, false, true, false), Route::Edit);
    }

    #[test]
    fn history_walks_back_and_returns_the_draft() {
        let mut h = History::default();
        assert_eq!(h.prev("draft"), None, "nothing sent yet");
        h.push("first");
        h.push("second\n");
        h.push("second");
        h.push("   ");
        assert_eq!(h.len(), 2, "empty and repeated messages are not kept");
        assert_eq!(h.prev("my draft").as_deref(), Some("second"));
        assert!(h.browsing());
        assert_eq!(h.prev("second").as_deref(), Some("first"));
        assert_eq!(h.prev("first"), None, "the oldest stays");
        assert_eq!(h.next().as_deref(), Some("second"));
        assert_eq!(h.next().as_deref(), Some("my draft"), "the draft comes back");
        assert!(!h.browsing());
        assert_eq!(h.next(), None);
    }

    #[test]
    fn history_is_capped_and_sending_stops_the_walk() {
        let mut h = History::default();
        for i in 0..HISTORY_MAX + 5 {
            h.push(&format!("m{i}"));
        }
        assert_eq!(h.len(), HISTORY_MAX);
        assert_eq!(h.prev("").as_deref(), Some(format!("m{}", HISTORY_MAX + 4).as_str()));
        h.push("new");
        assert!(!h.browsing());
    }

    #[test]
    fn the_box_grows_to_a_limit() {
        assert_eq!(visible_lines(0), MIN_LINES);
        assert_eq!(visible_lines(1), MIN_LINES);
        assert_eq!(visible_lines(5), 5);
        assert_eq!(visible_lines(40), MAX_LINES);
    }

    #[test]
    fn long_text_wraps_by_the_box_width_and_keeps_its_own_line_breaks() {
        let mut t = TextArea::default();
        t.type_text("hello world again\nx");
        // Every char 10 px wide, 90 px box: "hello " / "world " / "again" / "x".
        t.layout(90.0, &mut |s: &str| (0..=s.chars().count()).map(|i| i as f32 * 10.0).collect());
        assert_eq!(t.line_count(), 4);
        assert_eq!(visible_lines(t.line_count()), 4);
        assert_eq!(t.text(), "hello world again\nx", "wrapping is visual only");
    }

    #[test]
    fn mentions_and_selection_blocks() {
        assert_eq!(mention("src/main.rs"), "@src/main.rs ");
        assert_eq!(mention("my dir/a b.rs"), "@\"my dir/a b.rs\" ");
        assert_eq!(selection_block("a.rs", 3, "let x = 1;\n"), "a.rs:3\n```\nlet x = 1;\n```\n");
        assert_eq!(selection_block("a.rs", 3, "a\nb\nc"), "a.rs:3-5\n```\na\nb\nc\n```\n");
        let big = "line of text\n".repeat(SELECTION_MAX / 10);
        let b = selection_block("big.rs", 1, &big);
        assert!(b.contains("(selection cut)") && b.len() < SELECTION_MAX + 200, "{}", b.len());
    }

    #[test]
    fn submit_is_one_paste_then_enter() {
        assert_eq!(submit_bytes("  \n", true), None);
        let (paste, enter) = submit_bytes("a\nb \n", true).unwrap();
        assert_eq!(paste, b"\x1b[200~a\rb\x1b[201~", "lines stay inside one bracketed paste");
        assert_eq!(enter, b"\r");
        let (plain, _) = submit_bytes("hi", false).unwrap();
        assert_eq!(plain, b"hi");
    }
}
