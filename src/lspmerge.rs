//! Combining the answers of several language servers for one document (pure functions).
//!
//! Rules, in one place:
//! * diagnostics: every server's list is kept, tagged with its source; identical ones (same range and
//!   message) are shown once, the first source (the primary) winning.
//! * hover: the primary's sections come first, each companion adds its own section after a rule; empty
//!   answers are skipped; with a single non-empty answer it is returned untouched.
//! * completion: lists are concatenated, primary first; an item that repeats a label+text already
//!   offered is dropped; companion items remember their server (`__server`) so they resolve there.
//! * code actions: all servers' actions are listed, duplicates by title dropped (see `app_lspx`).
//! * definition, references, rename, formatting, symbols...: the primary only (formatting falls
//!   through to the next server that can format when the primary cannot).

use crate::doc::Diag;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MultiKind {
    Hover,
    Completion,
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
}

impl Agg {
    pub fn new(kind: MultiKind, serial: u64, uri: String, at: usize, expected: usize) -> Agg {
        Agg { kind, serial, uri, at, expected, parts: Vec::new() }
    }

    /// Record one answer (a failed request counts too: pass `Value::Null`). True when all are in.
    pub fn push(&mut self, order: usize, name: &str, v: Value) -> bool {
        self.parts.push((order, name.to_string(), v));
        self.expected = self.expected.saturating_sub(1);
        self.expected == 0
    }

    pub fn merged(&mut self) -> Value {
        self.parts.sort_by_key(|p| p.0);
        match self.kind {
            MultiKind::Hover => merge_hover(&self.parts),
            MultiKind::Completion => merge_completion(&self.parts),
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

pub fn merge_completion(parts: &[(usize, String, Value)]) -> Value {
    let live: Vec<&(usize, String, Value)> = parts.iter().filter(|p| !items_of(&p.2).is_empty()).collect();
    if live.len() <= 1 {
        return live.first().map(|p| p.2.clone()).unwrap_or(Value::Null);
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut incomplete = false;
    for (n, (order, name, v)) in live.iter().map(|p| (p.0, &p.1, &p.2)).enumerate() {
        incomplete |= v["isIncomplete"].as_bool().unwrap_or(false);
        for it in items_of(v) {
            let label = it["label"].as_str().unwrap_or("");
            let text = it["textEdit"]["newText"].as_str().or_else(|| it["insertText"].as_str()).unwrap_or(label);
            if !seen.insert((label.to_string(), text.to_string())) {
                continue;
            }
            let mut it = it.clone();
            if n > 0 || order > 0 {
                it["__server"] = json!(name);
                if it["detail"].as_str().is_none_or(str::is_empty) {
                    it["detail"] = json!(name);
                }
            }
            out.push(it);
        }
    }
    json!({"isIncomplete": incomplete, "items": out})
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
        assert_eq!(merge_completion(&[part(0, "x", only.clone()), part(1, "y", Value::Null)]), only);
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
