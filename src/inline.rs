//! Inline completions (ghost text) from LSP `textDocument/inlineCompletion` (LSP 3.18): the
//! request lifecycle, the suggestions, typing through them, partial acceptance and the geometry of
//! the faded text. Pure data and logic (no processes, no drawing); the app glue is `app_inline.rs`.
//!
//! Lifecycle: every frame the app takes a [`Snap`] of the active document and [`classify`]s it
//! against the previous one. An edit schedules an automatic request after [`DEBOUNCE`] (unless the
//! user typed exactly what the ghost text shows: it then simply shrinks); a cursor move, another
//! document, a selection, several cursors or an open completion popup dismiss the suggestion and
//! cancel the request in flight. An answer is only shown when the document still looks exactly
//! like it did when the request went out.
//!
//! Suggestions never come from Lumen itself: only from language servers the user installed (and,
//! for GitHub Copilot's `copilot-language-server`, signed into with their own account).

use crate::doc::Doc;
use crate::inlay::{Hint, HintKind};
use serde_json::Value;
use std::time::{Duration, Instant};

/// Typing pause before an automatic request.
pub const DEBOUNCE: Duration = Duration::from_millis(250);
/// Most suggestions kept from one answer (`Alt+]` / `Alt+[` cycle through them).
const MAX_ITEMS: usize = 10;
/// Most extra ghost lines drawn below the cursor line (a longer suggestion is still inserted whole).
pub const MAX_ROWS: usize = 40;

/// What the inline state compares from one frame to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snap {
    pub doc: usize,
    pub version: i32,
    pub cursor: usize,
    /// No suggestion may show now: feature off or paused, several cursors, a selection, a snippet
    /// session, the completion popup, an overlay, or the editor does not have the focus.
    pub blocked: bool,
}

/// LSP `InlineCompletionTriggerKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Asked for explicitly (`Alt+\`).
    Invoked = 1,
    /// After a typing pause.
    Automatic = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    None,
    Edit,
    Moved,
    Switched,
}

/// How `now` differs from the previous snapshot.
pub fn classify(last: Option<Snap>, now: Snap) -> Change {
    let Some(l) = last else { return Change::Switched };
    if l.doc != now.doc {
        Change::Switched
    } else if l.version != now.version {
        Change::Edit
    } else if l.cursor != now.cursor || (now.blocked && !l.blocked) {
        Change::Moved
    } else {
        Change::None
    }
}

/// A request on its way: which server (name and root, indexes move when servers stop), its id,
/// and the snapshot its answer must still match.
#[derive(Clone, Debug, PartialEq)]
pub struct Inflight {
    pub server: (String, std::path::PathBuf),
    pub id: i64,
    pub serial: u64,
    pub snap: Snap,
    pub trigger: Trigger,
}

/// One suggestion: replace chars `start..end` (positions when it arrived, `end` follows what is
/// typed at the cursor) with `insert`; `shown` is the part not typed yet (the ghost text).
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub start: usize,
    pub end: usize,
    pub insert: String,
    /// The snippet body when `insertText` was a `StringValue` (`insert` is then its expansion).
    pub snippet: Option<String>,
    /// LSP `Command` to execute after the suggestion was accepted.
    pub command: Option<Value>,
    pub shown: String,
}

/// The suggestions shown at `anchor` (the cursor) of document `doc`.
#[derive(Clone, Debug, PartialEq)]
pub struct Ghost {
    pub doc: usize,
    pub server: (String, std::path::PathBuf),
    pub anchor: usize,
    /// Length of the document (chars) for `anchor`: typing through must add exactly what it shows.
    pub len: usize,
    pub items: Vec<Item>,
    pub idx: usize,
}

impl Ghost {
    pub fn item(&self) -> &Item {
        &self.items[self.idx.min(self.items.len() - 1)]
    }

    /// The ghost text of the current suggestion.
    pub fn text(&self) -> &str {
        &self.item().shown
    }

    /// `Alt+]` / `Alt+[`: the next / previous suggestion (wrapping).
    pub fn cycle(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        self.idx = (self.idx as isize + delta).rem_euclid(n.max(1)) as usize;
    }

    /// `typed` was inserted at the anchor. Suggestions that start with it shrink by it; the others
    /// go. False when nothing is left to show (no match, or the whole suggestion was typed).
    pub fn consume(&mut self, typed: &str) -> bool {
        if typed.is_empty() || self.items.is_empty() {
            return !self.items.is_empty();
        }
        let n = typed.chars().count();
        let cur = self.item().shown.clone();
        self.items.retain(|it| it.shown.starts_with(typed) && it.shown.len() > typed.len());
        for it in &mut self.items {
            it.shown.drain(..typed.len());
            it.end += n;
        }
        self.anchor += n;
        self.len += n;
        // Stay on the same suggestion when it survived.
        self.idx =
            cur.strip_prefix(typed).and_then(|rest| self.items.iter().position(|it| it.shown == rest)).unwrap_or(0);
        !self.items.is_empty()
    }

    /// What the whole suggestion inserts.
    pub fn accept(&self) -> Accept {
        let it = self.item();
        match &it.snippet {
            Some(body) => Accept::Snippet { start: it.start, end: it.end, body: body.clone() },
            None => Accept::Text { start: it.start, end: it.end, text: it.insert.clone() },
        }
    }
}

/// The edit that accepting a suggestion makes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Accept {
    Text {
        start: usize,
        end: usize,
        text: String,
    },
    /// Inserted as a snippet (tab stops), like a snippet completion.
    Snippet {
        start: usize,
        end: usize,
        body: String,
    },
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `Ctrl+Right` takes this much of the ghost text: the whitespace (line breaks included) at its
/// start, then one word or one other character.
pub fn next_word(s: &str) -> &str {
    let mut end = 0;
    let mut it = s.char_indices().peekable();
    while let Some(&(i, c)) = it.peek() {
        if !c.is_whitespace() {
            break;
        }
        end = i + c.len_utf8();
        it.next();
    }
    match it.peek().copied() {
        Some((_, c)) if is_word(c) => {
            while let Some(&(i, c)) = it.peek() {
                if !is_word(c) {
                    break;
                }
                end = i + c.len_utf8();
                it.next();
            }
        }
        Some((i, c)) => end = i + c.len_utf8(),
        None => {}
    }
    &s[..end]
}

/// Chars `start..end` of an LSP range, through `pos(line, utf16_character)`.
fn range(v: &Value, pos: &dyn Fn(usize, usize) -> usize) -> Option<(usize, usize)> {
    let p = |p: &Value| Some(pos(p["line"].as_u64()? as usize, p["character"].as_u64()? as usize));
    let (a, b) = (p(&v["start"])?, p(&v["end"])?);
    (a <= b).then_some((a, b))
}

/// Parse an `InlineCompletionItem[]` / `InlineCompletionList` answer for a request made with the
/// cursor at `cursor`. `pos` converts an LSP position to a char index, `slice(a, b)` gives the
/// document's chars `a..b`, `indent` is the cursor line's indentation (snippets indent their
/// continuation lines with it, like snippet completions). Items whose range does not contain the
/// cursor, or whose text does not start with what is already typed in their range, are dropped,
/// and so are empty and duplicate ones.
pub fn parse_items(
    v: &Value,
    cursor: usize,
    pos: &dyn Fn(usize, usize) -> usize,
    slice: &dyn Fn(usize, usize) -> String,
    indent: &str,
) -> Vec<Item> {
    let list: &[Value] = match v {
        Value::Array(a) => a.as_slice(),
        _ => v["items"].as_array().map(Vec::as_slice).unwrap_or(&[]),
    };
    let mut out: Vec<Item> = Vec::new();
    for it in list {
        let (text, snippet) = match &it["insertText"] {
            Value::String(s) => (s.clone(), None),
            // `StringValue`: {"kind": "snippet", "value": "..."}
            Value::Object(o) => {
                let Some(body) = o.get("value").and_then(Value::as_str) else { continue };
                (crate::snippets::expand(body, indent).text, Some(body.to_string()))
            }
            _ => continue,
        };
        let text = text.replace("\r\n", "\n");
        let (start, end) = if it["range"].is_null() {
            (cursor, cursor)
        } else {
            match range(&it["range"], pos) {
                Some(r) => r,
                None => continue,
            }
        };
        if start > cursor || end < cursor {
            continue;
        }
        let typed = slice(start, cursor);
        let Some(shown) = text.strip_prefix(typed.as_str()) else { continue };
        if shown.is_empty() || out.iter().any(|o| o.shown == shown) {
            continue;
        }
        let command = it["command"].as_object().filter(|c| c.get("command").is_some_and(Value::is_string));
        out.push(Item {
            start,
            end,
            shown: shown.to_string(),
            insert: text,
            snippet,
            command: command.map(|c| Value::Object(c.clone())),
        });
        if out.len() >= MAX_ITEMS {
            break;
        }
    }
    out
}

/// What the document draws for a suggestion: `first` inline at (`line`, `col`) as a ghost inlay
/// hint (the rest of the line moves right, like after an inlay hint), and `rest` as virtual rows
/// below the cursor line. Tabs are expanded to spaces, control characters become spaces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GhostView {
    pub line: usize,
    pub col: usize,
    pub first: String,
    pub rest: Vec<String>,
}

fn expand_tabs(s: &str, start: usize, tab_w: usize) -> String {
    let tab_w = tab_w.max(1);
    let mut out = String::with_capacity(s.len());
    let mut v = start;
    for c in s.chars() {
        if c == '\t' {
            let n = tab_w - v % tab_w;
            out.extend(std::iter::repeat_n(' ', n));
            v += n;
        } else {
            out.push(if c.is_control() { ' ' } else { c });
            v += 1;
        }
    }
    out
}

/// The view of ghost text `shown` at (`line`, `col`); `start_vcol` is the visual column of the
/// cursor (tab stops), `after` the document text the suggestion replaces after the cursor on its
/// line (e.g. an auto-closed `)`): a one-line suggestion that ends with it does not draw it twice.
pub fn view(shown: &str, line: usize, col: usize, start_vcol: usize, tab_w: usize, after: &str) -> GhostView {
    let mut lines = shown.split('\n');
    let mut first = lines.next().unwrap_or("").trim_end_matches('\r');
    let rest: Vec<String> = lines.take(MAX_ROWS).map(|l| expand_tabs(l.trim_end_matches('\r'), 0, tab_w)).collect();
    if rest.is_empty() && !after.is_empty() {
        first = first.strip_suffix(after).unwrap_or(first);
    }
    GhostView { line, col, first: expand_tabs(first, start_vcol, tab_w), rest }
}

/// Screen row of document row `row` when `ghost = (g, n)`: `n` ghost rows follow row `g`.
pub fn shift_row(row: usize, ghost: Option<(usize, usize)>) -> usize {
    match ghost {
        Some((g, n)) if row > g => row + n,
        _ => row,
    }
}

/// Document row under screen row `row` (the inverse of [`shift_row`]); a pointer over the ghost
/// rows is on row `g` (the cursor line).
pub fn unshift_row(row: usize, ghost: Option<(usize, usize)>) -> usize {
    match ghost {
        Some((g, n)) if row > g => {
            if row <= g + n {
                g
            } else {
                row - n
            }
        }
        _ => row,
    }
}

impl Doc {
    /// Show (or hide) the ghost text of an inline suggestion: its first line becomes a ghost
    /// inlay hint at the cursor (so the caret, clicks and selections stay exact), the other lines
    /// are drawn as virtual rows below the cursor line (see `doc::draw`).
    pub fn set_ghost(&mut self, g: Option<GhostView>) {
        let want: Option<Hint> = g.as_ref().filter(|g| !g.first.is_empty()).map(|g| Hint {
            line: g.line,
            col: g.col,
            label: g.first.clone(),
            kind: HintKind::Ghost,
            pad_left: false,
            pad_right: false,
            links: Vec::new(),
            edits: Vec::new(),
        });
        let same = match (&want, self.inlay.hints.ghost()) {
            (None, None) => true,
            (Some(w), Some(p)) => w.line == p.line && w.col == p.col && w.label == p.label,
            _ => false,
        };
        if !same {
            self.inlay.hints.set_ghost(want);
        }
        if g.as_ref().is_none_or(|g| g.rest.is_empty()) {
            self.ghost_rows = None;
        }
        self.ghost = g;
    }
}

// ───────────────────────────── state ─────────────────────────────

/// GitHub Copilot's sign-in state, from `checkStatus` / `signIn` answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Auth {
    #[default]
    Unknown,
    SignedIn(String),
    SignedOut,
    /// Signed in, but the account has no Copilot subscription.
    NoAccess,
    /// The device code is shown; waiting for the browser step.
    Waiting {
        code: String,
        uri: String,
    },
}

/// `checkStatus` / `signIn` / `signInConfirm` status field -> [`Auth`].
pub fn auth_from_status(v: &Value) -> Auth {
    let user = v["user"].as_str().unwrap_or("").to_string();
    match v["status"].as_str() {
        Some("OK" | "AlreadySignedIn" | "MaybeOk") => Auth::SignedIn(user),
        Some("NotSignedIn") => Auth::SignedOut,
        Some("NotAuthorized") => Auth::NoAccess,
        _ => Auth::Unknown,
    }
}

/// Which Copilot request an answer belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopilotStep {
    Status,
    SignIn,
    /// `workspace/executeCommand` of the command `signIn` returned (or `signInConfirm`).
    Finish,
    SignOut,
}

#[derive(Debug, Default)]
pub struct Copilot {
    pub auth: Auth,
    /// The last `didChangeStatus` message and kind.
    pub message: String,
    pub kind: String,
    /// A `checkStatus` is on its way.
    pub checking: bool,
}

/// The status bar item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    On,
    /// Paused from the status bar.
    Off,
    SignedOut,
    Loading,
    /// No running server of the file offers inline completions.
    NoServer,
}

impl Badge {
    pub fn label(self) -> &'static str {
        match self {
            Badge::On => "on",
            Badge::Off => "off",
            Badge::SignedOut => "signed out",
            Badge::Loading => "loading",
            Badge::NoServer => "no server",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Badge::On => "ui/copilot",
            Badge::Off | Badge::NoServer => "ui/copilot_disabled",
            Badge::SignedOut => "ui/copilot_error",
            Badge::Loading => "ui/copilot_init",
        }
    }
}

/// Whether a server can answer for the active document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerState {
    None,
    Starting,
    Ready,
}

pub fn badge(paused: bool, waiting: bool, server: ServerState, signed_out: bool) -> Badge {
    if paused {
        return Badge::Off;
    }
    if signed_out {
        return Badge::SignedOut;
    }
    match server {
        ServerState::Ready if waiting => Badge::Loading,
        ServerState::Ready => Badge::On,
        ServerState::Starting => Badge::Loading,
        ServerState::None => Badge::NoServer,
    }
}

#[derive(Debug, Default)]
pub struct Inline {
    pub ghost: Option<Ghost>,
    /// When to ask, and how (automatic after a pause, or invoked).
    pub due: Option<(Instant, Trigger)>,
    pub inflight: Option<Inflight>,
    pub last: Option<Snap>,
    pub serial: u64,
    /// Paused from the status bar (the setting stays on).
    pub paused: bool,
    /// Document whose ghost text is currently placed (to take it away again).
    pub shown_in: Option<usize>,
    /// What the placed ghost text was computed from (skips recomputing it every frame).
    pub view_key: Option<(usize, i32, usize, usize, usize)>,
    pub copilot: Copilot,
    /// Links opened for the user (device-code page): (url, when), to avoid opening one twice.
    pub opened: Vec<(String, Instant)>,
}

impl Inline {
    /// Nothing shown, scheduled or waited for.
    pub fn idle(&self) -> bool {
        self.ghost.is_none() && self.due.is_none() && self.inflight.is_none() && self.shown_in.is_none()
    }

    /// Record `snap` after a change of kind `change`; `kept` says the edit typed through the ghost
    /// text (it stays and nothing is asked). Returns the request to cancel, if any.
    pub fn on_change(&mut self, change: Change, snap: Snap, kept: bool, now: Instant) -> Option<Inflight> {
        self.last = Some(snap);
        if snap.blocked {
            self.ghost = None;
            self.due = None;
            return self.inflight.take();
        }
        match change {
            Change::None => None,
            Change::Edit => {
                if kept {
                    self.due = None;
                } else {
                    self.ghost = None;
                    self.due = Some((now + DEBOUNCE, Trigger::Automatic));
                }
                self.inflight.take()
            }
            Change::Moved | Change::Switched => {
                self.ghost = None;
                self.due = None;
                self.inflight.take()
            }
        }
    }

    /// The trigger of a request that is due at `now`.
    pub fn take_due(&mut self, now: Instant) -> Option<Trigger> {
        match self.due {
            Some((t, trig)) if t <= now => {
                self.due = None;
                Some(trig)
            }
            _ => None,
        }
    }

    /// The answer to request `serial` arrived while the document looks like `now`: the request
    /// when it is the one in flight and still current (else the answer is dropped).
    pub fn take_answer(&mut self, serial: u64, now: Snap) -> Option<Inflight> {
        if self.inflight.as_ref().is_none_or(|f| f.serial != serial) {
            return None;
        }
        self.inflight.take().filter(|f| f.snap == now && !now.blocked)
    }

    /// Forget everything shown or asked; returns the request to cancel.
    pub fn dismiss(&mut self) -> Option<Inflight> {
        self.ghost = None;
        self.due = None;
        self.inflight.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A one-line document: positions are char indexes, LSP columns equal chars (ASCII).
    fn parse(text: &str, cursor: usize, v: Value) -> Vec<Item> {
        let chars: Vec<char> = text.chars().collect();
        parse_items(&v, cursor, &|_, c| c.min(chars.len()), &|a, b| chars[a..b].iter().collect(), "")
    }

    fn ghost(items: Vec<Item>, anchor: usize, len: usize) -> Ghost {
        Ghost { doc: 0, server: ("s".into(), "/".into()), anchor, len, items, idx: 0 }
    }

    #[test]
    fn items_parse_plain_snippet_ranged_and_listed_answers() {
        let text = "let x = fo";
        let v = json!([
            {"insertText": "o(1);\nbar();", "command": {"title": "t", "command": "accept.it", "arguments": [7]}},
            {"insertText": {"kind": "snippet", "value": "o(${1:a})$0"}},
            // Range over what is typed: "fo" -> "foo_ranged".
            {"insertText": "foo_ranged", "range": {"start": {"line": 0, "character": 8}, "end": {"line": 0, "character": 10}}},
            // Does not match what is typed in its range: dropped.
            {"insertText": "bar", "range": {"start": {"line": 0, "character": 8}, "end": {"line": 0, "character": 10}}},
            // A range that does not contain the cursor, an empty and a duplicate suggestion: dropped.
            {"insertText": "zz", "range": {"start": {"line": 0, "character": 2}, "end": {"line": 0, "character": 3}}},
            {"insertText": ""},
            {"insertText": "o(1);\nbar();"},
            {"label": "no insert text"}
        ]);
        let items = parse(text, 10, v);
        assert_eq!(items.len(), 3, "{items:?}");
        assert_eq!(items[0].shown, "o(1);\nbar();");
        assert_eq!((items[0].start, items[0].end), (10, 10));
        assert_eq!(items[0].command.as_ref().unwrap()["command"], "accept.it");
        assert_eq!(items[1].shown, "o(a)", "snippets show their expansion");
        assert_eq!(items[1].snippet.as_deref(), Some("o(${1:a})$0"));
        assert_eq!((items[2].start, items[2].end, items[2].shown.as_str()), (8, 10, "o_ranged"));
        assert!(items[2].command.is_none());
        // `InlineCompletionList` and garbage.
        let listed = parse(text, 10, json!({"items": [{"insertText": "x"}]}));
        assert_eq!(listed[0].shown, "x");
        assert!(parse(text, 10, json!(null)).is_empty());
        assert!(parse(text, 10, json!({"items": 3})).is_empty());
    }

    #[test]
    fn typing_through_shrinks_the_ghost_and_drops_what_no_longer_matches() {
        let mk = |s: &str| Item { start: 4, end: 4, insert: s.into(), snippet: None, command: None, shown: s.into() };
        let mut g = ghost(vec![mk("abc"), mk("abd"), mk("xyz")], 4, 10);
        g.idx = 1;
        assert!(g.consume("a"));
        assert_eq!(g.items.len(), 2, "xyz no longer matches");
        assert_eq!((g.anchor, g.len, g.items[0].end), (5, 11, 5));
        assert_eq!(g.text(), "bd", "the current suggestion stays selected");
        assert!(g.consume("b"));
        assert_eq!(g.text(), "d");
        // Typing the whole suggestion leaves nothing to show; a mismatch neither.
        assert!(!g.clone().consume("d"));
        assert!(!g.consume("q"));
        assert!(g.items.is_empty());
    }

    #[test]
    fn cycling_wraps_and_accept_uses_the_current_item() {
        let mk = |s: &str, sn: Option<&str>| Item {
            start: 0,
            end: 2,
            insert: s.into(),
            snippet: sn.map(str::to_string),
            command: None,
            shown: s.into(),
        };
        let mut g = ghost(vec![mk("one", None), mk("two()", Some("two(${1})"))], 0, 2);
        g.cycle(-1);
        assert_eq!(g.idx, 1);
        assert_eq!(g.accept(), Accept::Snippet { start: 0, end: 2, body: "two(${1})".into() });
        g.cycle(1);
        assert_eq!(g.accept(), Accept::Text { start: 0, end: 2, text: "one".into() });
    }

    #[test]
    fn next_word_takes_whitespace_then_one_word_or_symbol() {
        assert_eq!(next_word("foo(bar)"), "foo");
        assert_eq!(next_word("(bar)"), "(");
        assert_eq!(next_word("  bar baz"), "  bar");
        assert_eq!(next_word("\n    return x;"), "\n    return");
        assert_eq!(next_word("ção_1 x"), "ção_1");
        assert_eq!(next_word("   "), "   ");
        assert_eq!(next_word(""), "");
    }

    #[test]
    fn view_splits_lines_expands_tabs_and_skips_the_replaced_suffix() {
        let v = view("a\tb\n\tc\nd", 3, 5, 6, 4, "");
        assert_eq!((v.line, v.col), (3, 5));
        assert_eq!(v.first, "a b", "the tab stops count from the cursor's visual column 6");
        assert_eq!(v.rest, vec!["    c".to_string(), "d".to_string()]);
        // `foo(|)` with a suggestion replacing the auto-closed paren: drawn once.
        assert_eq!(view("a, b)", 0, 4, 4, 4, ")").first, "a, b");
        assert_eq!(view("a, b", 0, 4, 4, 4, ")").first, "a, b");
        let long: String = (0..100).map(|i| format!("l{i}\n")).collect();
        assert_eq!(view(&long, 0, 0, 0, 4, "").rest.len(), MAX_ROWS);
    }

    #[test]
    fn ghost_rows_shift_the_rows_below_and_map_back() {
        let g = Some((3, 2));
        assert_eq!((shift_row(2, g), shift_row(3, g), shift_row(4, g)), (2, 3, 6));
        assert_eq!((unshift_row(3, g), unshift_row(4, g), unshift_row(5, g), unshift_row(6, g)), (3, 3, 3, 4));
        for r in 0..20 {
            assert_eq!(unshift_row(shift_row(r, g), g), r);
            assert_eq!(shift_row(r, None), r);
        }
    }

    fn snap(version: i32, cursor: usize) -> Snap {
        Snap { doc: 0, version, cursor, blocked: false }
    }

    #[test]
    fn edits_debounce_a_request_and_moves_cancel_it() {
        let t0 = Instant::now();
        let mut s = Inline::default();
        assert_eq!(classify(None, snap(1, 0)), Change::Switched);
        assert!(s.on_change(Change::Switched, snap(1, 0), false, t0).is_none());
        assert!(s.due.is_none(), "opening a document asks nothing");
        // Typing schedules an automatic request after the pause.
        assert_eq!(classify(s.last, snap(2, 1)), Change::Edit);
        s.on_change(Change::Edit, snap(2, 1), false, t0);
        assert!(s.take_due(t0 + DEBOUNCE / 2).is_none());
        assert_eq!(s.take_due(t0 + DEBOUNCE), Some(Trigger::Automatic));
        assert!(s.take_due(t0 + DEBOUNCE * 4).is_none(), "asked once");
        // In flight; the cursor moves: the request is handed back for cancelling.
        let f = Inflight {
            server: ("s".into(), "/".into()),
            id: 7,
            serial: 1,
            snap: snap(2, 1),
            trigger: Trigger::Automatic,
        };
        s.inflight = Some(f.clone());
        assert_eq!(classify(s.last, snap(2, 0)), Change::Moved);
        assert_eq!(s.on_change(Change::Moved, snap(2, 0), false, t0), Some(f.clone()));
        assert!(s.inflight.is_none() && s.due.is_none());
        // More typing while a request is out cancels it and schedules the next.
        s.inflight = Some(f.clone());
        assert_eq!(s.on_change(Change::Edit, snap(3, 1), false, t0), Some(f));
        assert!(s.due.is_some());
        // A selection / popup / second cursor appears: everything goes.
        let blocked = Snap { blocked: true, ..snap(3, 1) };
        assert_eq!(classify(s.last, blocked), Change::Moved);
        s.on_change(Change::Moved, blocked, false, t0);
        assert!(s.due.is_none() && s.ghost.is_none());
        // Nothing changed: nothing happens.
        assert_eq!(classify(Some(snap(3, 1)), snap(3, 1)), Change::None);
        assert_eq!(classify(Some(snap(3, 1)), Snap { doc: 1, ..snap(3, 1) }), Change::Switched);
    }

    #[test]
    fn typing_through_keeps_the_ghost_without_a_new_request() {
        let t0 = Instant::now();
        let mut s = Inline::default();
        s.on_change(Change::Switched, snap(1, 0), false, t0);
        let it = Item { start: 0, end: 0, insert: "ab".into(), snippet: None, command: None, shown: "ab".into() };
        s.ghost = Some(ghost(vec![it], 0, 0));
        s.on_change(Change::Edit, snap(2, 1), true, t0);
        assert!(s.ghost.is_some() && s.due.is_none());
        s.on_change(Change::Edit, snap(3, 2), false, t0);
        assert!(s.ghost.is_none() && s.due.is_some());
    }

    #[test]
    fn only_the_current_answer_of_the_unchanged_document_is_shown() {
        let mut s = Inline::default();
        let f = |serial| Inflight {
            server: ("s".into(), "/".into()),
            id: serial as i64,
            serial,
            snap: snap(5, 3),
            trigger: Trigger::Invoked,
        };
        s.inflight = Some(f(2));
        assert!(s.take_answer(1, snap(5, 3)).is_none(), "an older request");
        assert!(s.inflight.is_some(), "the current one keeps waiting");
        assert_eq!(s.take_answer(2, snap(5, 3)), Some(f(2)));
        s.inflight = Some(f(3));
        assert!(s.take_answer(3, snap(6, 4)).is_none(), "the text changed meanwhile");
        assert!(s.inflight.is_none());
        s.inflight = Some(f(4));
        assert!(s.take_answer(4, Snap { blocked: true, ..snap(5, 3) }).is_none());
    }

    #[test]
    fn copilot_status_and_badge() {
        assert_eq!(auth_from_status(&json!({"status": "OK", "user": "octo"})), Auth::SignedIn("octo".into()));
        assert_eq!(auth_from_status(&json!({"status": "AlreadySignedIn", "user": "o"})), Auth::SignedIn("o".into()));
        assert_eq!(auth_from_status(&json!({"status": "NotSignedIn"})), Auth::SignedOut);
        assert_eq!(auth_from_status(&json!({"status": "NotAuthorized"})), Auth::NoAccess);
        assert_eq!(auth_from_status(&json!(null)), Auth::Unknown);
        assert_eq!(badge(true, true, ServerState::Ready, true), Badge::Off);
        assert_eq!(badge(false, false, ServerState::Ready, true), Badge::SignedOut);
        assert_eq!(badge(false, true, ServerState::Ready, false), Badge::Loading);
        assert_eq!(badge(false, false, ServerState::Ready, false), Badge::On);
        assert_eq!(badge(false, false, ServerState::Starting, false), Badge::Loading);
        assert_eq!(badge(false, false, ServerState::None, false), Badge::NoServer);
        for b in [Badge::On, Badge::Off, Badge::SignedOut, Badge::Loading, Badge::NoServer] {
            assert!(!b.label().is_empty() && b.icon().starts_with("ui/copilot"));
        }
    }

    #[test]
    fn set_ghost_places_one_ghost_hint_and_keeps_it_through_inlay_answers() {
        let mut d = Doc::new(crate::buffer::Buffer::from_str("let x = 1\nfoo\n"), None, None, "//", 8.0);
        d.set_ghost(Some(GhostView { line: 0, col: 9, first: "; // ok".into(), rest: vec!["bar".into()] }));
        let hs = d.inlay.hints.hints_on(0);
        assert_eq!(hs.len(), 1);
        assert_eq!((hs[0].kind, hs[0].col, hs[0].width()), (HintKind::Ghost, 9, 7), "full-width cells");
        // An inlay answer for the line keeps the ghost (the server knows nothing about it).
        d.inlay.apply(0, 1, vec![], 2);
        assert_eq!(d.inlay.hints.ghost().map(|h| h.label.as_str()), Some("; // ok"));
        // The caret sits before the ghost text; text after it moves right by its width.
        assert_eq!(d.inlay.hints.vcol(0, "let x = 1", 9, 4, false), 9);
        d.set_ghost(Some(GhostView { line: 1, col: 3, first: "()".into(), rest: vec![] }));
        assert!(d.inlay.hints.hints_on(0).is_empty(), "moved, not duplicated");
        assert_eq!(d.inlay.hints.hints_on(1).len(), 1);
        d.ghost_rows = Some((1, 1));
        d.set_ghost(None);
        assert!(d.inlay.hints.is_empty() && d.ghost.is_none() && d.ghost_rows.is_none());
    }
}
