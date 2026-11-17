//! Combining the answers of several language servers for one document (pure functions).
//!
//! Rules, in one place:
//! * diagnostics: every server's list is kept, tagged with its source; identical ones (same range and
//!   message) are shown once, the first source (the primary) winning.
//! * hover: the primary's sections come first, each companion adds its own section after a rule; empty
//!   answers are skipped; with a single non-empty answer it is returned untouched.
//! * completion: lists are concatenated, primary first; an item that repeats a label+text already
//!   offered is dropped; companion items remember their server (`__server`) so they resolve there.
//! * hover and completion never wait for the slowest server: the primary's answer is shown as soon
//!   as it is in (companions get `COMPANION_WAIT` to join the first paint), and a companion that
//!   answers later with something is merged into what is on screen (see [`Agg::due`]).
//! * code actions: all servers' actions are listed, duplicates by title dropped (see `app_lspx`).
//! * document highlights: asked of every server that offers them; the first non-empty answer in
//!   server order wins (the primary's whenever it has one). Linked editing asks the first server
//!   that offers it, primary first (see `app_assist`).
//! * definition, references, rename, formatting, symbols...: the primary only (formatting falls
//!   through to the next server that can format when the primary cannot).

use crate::doc::Diag;
use serde_json::{Value, json};
use std::time::Duration;

/// How long the primary's answer is held back for the companions before it is shown without them.
pub const COMPANION_WAIT: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MultiKind {
    Hover,
    Completion,
}

impl MultiKind {
    /// Each kind has its own aggregate: a hover asked while typing must not replace the completion's.
    pub fn slot(self) -> usize {
        match self {
            MultiKind::Hover => 0,
            MultiKind::Completion => 1,
        }
    }
}

/// Collects the answers of a request sent to several servers; `order` 0 is the primary.
#[derive(Debug)]
pub struct Agg {
    pub kind: MultiKind,
    pub serial: u64,
    pub uri: String,
    pub at: usize,
    pub expected: usize,
    parts: Vec<(usize, String, Value)>,
    /// The primary (order 0) answered.
    primary: bool,
    /// `merged` was taken at least once: something is (or was) on screen.
    painted: bool,
    /// An answer with content arrived since the last `merged`.
    dirty: bool,
}

impl Agg {
    pub fn new(kind: MultiKind, serial: u64, uri: String, at: usize, expected: usize) -> Agg {
        Agg { kind, serial, uri, at, expected, parts: Vec::new(), primary: false, painted: false, dirty: false }
    }

    /// Record one answer (a failed request counts too: pass `Value::Null`). True when all are in.
    pub fn push(&mut self, order: usize, name: &str, v: Value) -> bool {
        self.primary |= order == 0;
        self.dirty |= match self.kind {
            MultiKind::Hover => !contents_text(&v["contents"]).trim().is_empty(),
            MultiKind::Completion => !items_of(&v).is_empty(),
        };
        self.parts.push((order, name.to_string(), v));
        self.expected = self.expected.saturating_sub(1);
        self.expected == 0
    }

    /// Whether `merged` was already taken (the next one updates what is on screen).
    pub fn painted(&self) -> bool {
        self.painted
    }

    /// The primary answered and nothing was shown yet: `due` turns true at `COMPANION_WAIT`.
    pub fn holding(&self) -> bool {
        self.primary && !self.painted
    }

    /// Is there something to show, `waited` after the requests were sent? The first paint needs the
    /// primary's answer and then either everyone's or `COMPANION_WAIT` gone by (an empty answer is
    /// shown too: the caller has its own fallbacks). After it, only an answer with content counts.
    pub fn due(&self, waited: Duration) -> bool {
        if self.painted {
            return self.dirty;
        }
        self.expected == 0 || (self.primary && waited >= COMPANION_WAIT)
    }

    pub fn merged(&mut self) -> Value {
        self.painted = true;
        self.dirty = false;
        self.parts.sort_by_key(|p| p.0);
        match self.kind {
            MultiKind::Hover => merge_hover(&self.parts),
            // Completion lists are big: once everyone answered they are moved out, not copied.
            MultiKind::Completion if self.expected == 0 => merge_completion(std::mem::take(&mut self.parts)),
            MultiKind::Completion => merge_completion(self.parts.clone()),
        }
    }
}

/// Plain text of a hover `contents` field (MarkupContent, MarkedString or an array of them).
fn contents_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Object(o) => match (o.get("value").and_then(Value::as_str), o.get("language").and_then(Value::as_str)) {
            (Some(v), Some(l)) => format!("```{l}\n{v}\n```"),
            (Some(v), None) => v.to_string(),
            _ => String::new(),
        },
        Value::Array(a) => {
            a.iter().map(contents_text).filter(|s| !s.trim().is_empty()).collect::<Vec<_>>().join("\n\n")
        }
        _ => String::new(),
    }
}

pub fn merge_hover(parts: &[(usize, String, Value)]) -> Value {
    let live: Vec<&(usize, String, Value)> =
        parts.iter().filter(|p| !contents_text(&p.2["contents"]).trim().is_empty()).collect();
    match live.len() {
        0 => Value::Null,
        1 => live[0].2.clone(),
        _ => {
            let text = live.iter().map(|p| contents_text(&p.2["contents"])).collect::<Vec<_>>().join("\n\n---\n\n");
            let mut out = json!({"contents": {"kind": "markdown", "value": text}});
            if let Some(r) = live.iter().find(|p| p.2["range"].is_object()) {
                out["range"] = r.2["range"].clone();
            }
            out
        }
    }
}

fn items_of(v: &Value) -> &[Value] {
    v.get("items").unwrap_or(v).as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// The items of a completion answer (a `CompletionList` or a bare array), moved out of it.
pub fn take_items(mut v: Value) -> Vec<Value> {
    let list = v.get_mut("items").map(Value::take);
    match list.unwrap_or(v) {
        Value::Array(a) => a,
        _ => Vec::new(),
    }
}

/// Merge completion answers given in server order (primary first). The items are moved, never
/// copied: a list has thousands of them.
pub fn merge_completion(parts: Vec<(usize, String, Value)>) -> Value {
    let mut live: Vec<(usize, String, Value)> = parts.into_iter().filter(|p| !items_of(&p.2).is_empty()).collect();
    if live.len() <= 1 {
        return live.pop().map_or(Value::Null, |p| p.2);
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut incomplete = false;
    for (n, (order, name, v)) in live.into_iter().enumerate() {
        incomplete |= v["isIncomplete"].as_bool().unwrap_or(false);
        for mut it in take_items(v) {
            let label = it["label"].as_str().unwrap_or("");
            let text = it["textEdit"]["newText"].as_str().or_else(|| it["insertText"].as_str()).unwrap_or(label);
            if !seen.insert((label.to_string(), text.to_string())) {
                continue;
            }
            if n > 0 || order > 0 {
                if it["detail"].as_str().is_none_or(str::is_empty) {
                    it["detail"] = json!(name);
                }
                it["__server"] = json!(name);
            }
            out.push(it);
        }
    }
    json!({"isIncomplete": incomplete, "items": out})
}

/// Several servers' `documentHighlight` answers (order, name, answer): the first non-empty one by
/// order (0 = primary), else `Null`. Occurrences of one symbol are not unioned across servers: two
/// servers rarely agree on what a symbol is, and a mix would light unrelated words.
pub fn merge_highlights(parts: &[(usize, String, Value)]) -> Value {
    let mut sorted: Vec<&(usize, String, Value)> = parts.iter().collect();
    sorted.sort_by_key(|p| p.0);
    sorted.iter().find(|p| p.2.as_array().is_some_and(|a| !a.is_empty())).map_or(Value::Null, |p| p.2.clone())
}

/// Several servers' diagnostics for one document, deduplicated: same range and message once, first wins.
pub fn dedupe_diags(sources: &[&[Diag]]) -> Vec<Diag> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for list in sources {
        for d in *list {
            if seen.insert((d.sl, d.sc, d.el, d.ec, d.message.clone())) {
                out.push(d.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(order: usize, name: &str, v: Value) -> (usize, String, Value) {
        (order, name.into(), v)
    }

    fn diag(l: usize, msg: &str, src: &str) -> Diag {
        Diag { sl: l, sc: 0, el: l, ec: 3, severity: 1, message: msg.into(), code: String::new(), source: src.into() }
    }

    #[test]
    fn hover_sections_are_joined_primary_first() {
        let a = json!({"contents": {"kind": "markdown", "value": "fn main()"}, "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 4}}});
        let b = json!({"contents": "color: red"});
        let empty = json!({"contents": ""});
        let m = merge_hover(&[part(0, "ra", a.clone()), part(1, "tw", b), part(2, "x", empty)]);
        assert_eq!(m["contents"]["value"], "fn main()\n\n---\n\ncolor: red");
        assert_eq!(m["range"]["end"]["character"], 4);
        // one live answer is returned untouched, none gives null
        assert_eq!(merge_hover(&[part(0, "ra", a.clone()), part(1, "tw", Value::Null)]), a);
        assert!(merge_hover(&[part(0, "ra", Value::Null)]).is_null());
        // marked-string arrays
        let arr = json!({"contents": [{"language": "rust", "value": "x: u8"}, "docs"]});
        assert_eq!(contents_text(&arr["contents"]), "```rust\nx: u8\n```\n\ndocs");
    }

    #[test]
    fn completions_merge_dedupe_and_tag_companions() {
        let a = json!([{"label": "flex", "detail": "css"}, {"label": "grid"}]);
        let b = json!({"isIncomplete": true, "items": [{"label": "flex"}, {"label": "p-4", "insertText": "p-4"}]});
        let mut agg = Agg::new(MultiKind::Completion, 1, "u".into(), 0, 2);
        assert!(!agg.push(1, "tailwind", b));
        assert!(agg.push(0, "css", a));
        let m = agg.merged();
        let labels: Vec<_> = m["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
        assert_eq!(labels, vec!["flex", "grid", "p-4"]);
        assert_eq!(m["isIncomplete"], true);
        let items = m["items"].as_array().unwrap();
        assert!(items[0].get("__server").is_none(), "primary items stay untagged");
        assert_eq!(items[2]["__server"], "tailwind");
        assert_eq!(items[2]["detail"], "tailwind");
        // a lone answer is untouched
        let only = json!([{"label": "a"}]);
        assert_eq!(merge_completion(vec![part(0, "x", only.clone()), part(1, "y", Value::Null)]), only);
        // both shapes of an answer give their items; anything else gives none
        assert_eq!(take_items(only.clone()), only.as_array().unwrap().clone());
        assert_eq!(take_items(json!({"isIncomplete": false, "items": [{"label": "a"}]})), vec![json!({"label": "a"})]);
        assert!(take_items(Value::Null).is_empty() && take_items(json!({"items": null})).is_empty());
    }

    #[test]
    fn the_primary_is_shown_without_waiting_for_slow_companions() {
        let ms = Duration::from_millis;
        let items = |l: &str| json!([{"label": l}]);
        let labels = |m: &Value| -> Vec<String> {
            items_of(m).iter().map(|i| i["label"].as_str().unwrap().to_string()).collect()
        };
        // A companion alone never paints; the primary does once the companions had their 150 ms.
        let mut agg = Agg::new(MultiKind::Completion, 1, "u".into(), 0, 3);
        agg.push(2, "tailwind", items("p-4"));
        assert!(!agg.due(ms(2000)) && !agg.holding());
        agg.push(0, "ts", items("push"));
        assert!(agg.holding());
        assert!(!agg.due(ms(20)), "companions get a moment to join the first paint");
        assert!(agg.due(COMPANION_WAIT));
        assert_eq!(labels(&agg.merged()), vec!["push", "p-4"]);
        assert!(agg.painted() && !agg.holding());
        // Nothing new: nothing to repaint. A late answer with content is merged in, an empty one is not.
        assert!(!agg.due(ms(400)));
        assert!(agg.push(1, "eslint", items("rule")));
        assert!(agg.due(ms(900)));
        assert_eq!(labels(&agg.merged()), vec!["push", "rule", "p-4"]);
        let mut agg = Agg::new(MultiKind::Hover, 2, "u".into(), 0, 2);
        agg.push(0, "ts", json!({"contents": "const a: number"}));
        assert!(agg.due(ms(200)));
        agg.merged();
        agg.push(1, "tailwind", Value::Null);
        assert!(!agg.due(ms(300)), "an empty late answer changes nothing");
        // Everyone in before the 150 ms: shown at once, even when all are empty.
        let mut agg = Agg::new(MultiKind::Hover, 3, "u".into(), 0, 2);
        agg.push(1, "tailwind", Value::Null);
        assert!(agg.push(0, "ts", Value::Null));
        assert!(agg.due(ms(3)));
        assert!(agg.merged().is_null());
        assert_ne!(MultiKind::Hover.slot(), MultiKind::Completion.slot());
    }

    #[test]
    fn highlights_take_the_first_non_empty_answer_primary_first() {
        let hl =
            |l: u64| json!([{"range": {"start": {"line": l, "character": 0}, "end": {"line": l, "character": 1}}}]);
        // Answers arrive in any order; the primary's wins when it has one.
        let m = merge_highlights(&[part(2, "c", hl(2)), part(0, "a", hl(0)), part(1, "b", hl(1))]);
        assert_eq!(m, hl(0));
        // An empty or failed primary answer falls through to the next server.
        let m = merge_highlights(&[part(1, "b", hl(1)), part(0, "a", json!([])), part(3, "d", Value::Null)]);
        assert_eq!(m, hl(1));
        assert_eq!(merge_highlights(&[part(0, "a", Value::Null)]), Value::Null);
        assert_eq!(merge_highlights(&[]), Value::Null);
    }

    #[test]
    fn diagnostics_dedupe_keeps_the_first_source() {
        let a = vec![diag(1, "unused", "rustc"), diag(2, "x", "rustc")];
        let b = vec![diag(1, "unused", "clippy"), diag(1, "style", "clippy")];
        let m = dedupe_diags(&[&a, &b]);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].source, "rustc");
        assert_eq!(m.iter().filter(|d| d.message == "unused").count(), 1);
    }
}
