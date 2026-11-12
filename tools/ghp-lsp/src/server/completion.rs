//! Completion of GHP tags, with snippets that close their blocks.

use serde_json::{Value, json};

use super::conv::span_json;
use super::docs::Doc;
use super::hover::tag_doc;
use crate::ast::{Node, Span, TagKind};

struct Entry {
    /// Text after `<`, also the completion label.
    head: &'static str,
    detail: &'static str,
    snippet: &'static str,
    kind: TagKind,
}

const ENTRIES: &[Entry] = &[
    Entry {
        head: "go:if",
        detail: "if block",
        snippet: "<go:if ${1:condition}/>\n\t$0\n<go:endif/>",
        kind: TagKind::If,
    },
    Entry {
        head: "go:elif",
        detail: "else-if branch",
        snippet: "<go:elif ${1:condition}/>\n\t$0",
        kind: TagKind::Elif,
    },
    Entry { head: "go:else", detail: "else branch", snippet: "<go:else/>\n\t$0", kind: TagKind::Else },
    Entry { head: "go:endif", detail: "close if", snippet: "<go:endif/>", kind: TagKind::EndIf },
    Entry {
        head: "go:switch",
        detail: "switch block",
        snippet: "<go:switch ${1:value}/>\n\t<go:case ${2:value}/>\n\t\t$0\n\t<go:default/>\n<go:endswitch/>",
        kind: TagKind::Switch,
    },
    Entry { head: "go:case", detail: "switch case", snippet: "<go:case ${1:value}/>\n\t$0", kind: TagKind::Case },
    Entry { head: "go:default", detail: "switch default", snippet: "<go:default/>\n\t$0", kind: TagKind::Default },
    Entry { head: "go:endswitch", detail: "close switch", snippet: "<go:endswitch/>", kind: TagKind::EndSwitch },
    Entry {
        head: "go:for",
        detail: "for block",
        snippet: "<go:for ${1:_, item := range items}/>\n\t$0\n<go:endfor/>",
        kind: TagKind::For,
    },
    Entry { head: "go:endfor", detail: "close for", snippet: "<go:endfor/>", kind: TagKind::EndFor },
    Entry { head: "go=", detail: "output an expression", snippet: "<go= ${1:expression} />", kind: TagKind::Echo },
    Entry { head: "go", detail: "Go statements", snippet: "<go\n\t$0\n/>", kind: TagKind::Statement },
    Entry {
        head: "go:import",
        detail: "import packages",
        snippet: "<go:import (${1:\"fmt\"})/>",
        kind: TagKind::Import,
    },
];

/// The innermost block construct around `offset`: `If`, `Switch` or `For`.
fn enclosing(doc: &Doc, offset: usize) -> Option<TagKind> {
    doc.parse
        .tree
        .iter()
        .filter(|n| n.span().touches(offset))
        .filter_map(|n| match n {
            Node::If(_) => Some(TagKind::If),
            Node::Switch(_) => Some(TagKind::Switch),
            Node::For(_) => Some(TagKind::For),
            _ => None,
        })
        .last()
}

/// Entries that continue or close the enclosing block sort first.
fn relevant(inside: Option<TagKind>, kind: TagKind) -> bool {
    match inside {
        Some(TagKind::If) => matches!(kind, TagKind::Elif | TagKind::Else | TagKind::EndIf),
        Some(TagKind::Switch) => {
            matches!(kind, TagKind::Case | TagKind::Default | TagKind::EndSwitch)
        }
        Some(TagKind::For) => kind == TagKind::EndFor,
        _ => false,
    }
}

/// Completion list for a cursor outside Go regions; `null` when no tag is being typed.
pub fn complete(doc: &Doc, offset: usize) -> Value {
    let line = doc.index.line_of(offset);
    let Some(ls) = doc.index.line_span(line) else { return Value::Null };
    let before = &doc.text[ls.start..offset.clamp(ls.start, ls.end)];
    let Some(lt) = before.rfind('<') else { return Value::Null };
    let typed = &before[lt + 1..];
    if !typed.chars().all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '=') {
        return Value::Null;
    }
    let range = span_json(&doc.index, Span::new(ls.start + lt, offset));
    let inside = enclosing(doc, offset);

    let items: Vec<Value> = ENTRIES
        .iter()
        .filter(|e| e.head.starts_with(typed))
        .map(|e| {
            let rank = if relevant(inside, e.kind) { "0" } else { "1" };
            json!({
                "label": format!("<{}", e.head),
                "kind": 15,
                "detail": e.detail,
                "documentation": {"kind": "markdown", "value": tag_doc(e.kind)},
                "insertTextFormat": 2,
                "filterText": format!("<{}", e.head),
                "sortText": format!("{rank}{}", e.head),
                "textEdit": {"range": range, "newText": e.snippet},
            })
        })
        .collect();
    if items.is_empty() {
        return Value::Null;
    }
    json!({"isIncomplete": false, "items": items})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> (Doc, usize) {
        let off = text.find('|').unwrap();
        (Doc::new(1, text.replace('|', "")), off)
    }

    fn labels(v: &Value) -> Vec<String> {
        v["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().into()).collect()
    }

    #[test]
    fn lists_all_tags_after_lt() {
        let (d, o) = at("<p>\n<|");
        let l = labels(&complete(&d, o));
        assert_eq!(l.len(), ENTRIES.len());
        assert!(l.contains(&"<go:if".to_string()) && l.contains(&"<go=".to_string()));
    }

    #[test]
    fn filters_by_prefix_and_replaces_from_lt() {
        let (d, o) = at("  <go:e|");
        let v = complete(&d, o);
        assert_eq!(labels(&v), ["<go:elif", "<go:else", "<go:endif", "<go:endswitch", "<go:endfor"]);
        let edit = &v["items"][0]["textEdit"];
        assert_eq!(edit["range"]["start"], json!({"line": 0, "character": 2}));
        assert_eq!(edit["range"]["end"], json!({"line": 0, "character": 7}));
    }

    #[test]
    fn if_snippet_closes_the_block() {
        let (d, o) = at("<go:i|");
        let v = complete(&d, o);
        let item = &v["items"][0];
        assert_eq!(item["label"], "<go:if");
        assert_eq!(item["insertTextFormat"], 2);
        assert!(item["textEdit"]["newText"].as_str().unwrap().ends_with("<go:endif/>"));
        let s = complete(&at("<go:sw|").0, 7);
        assert!(s["items"][0]["textEdit"]["newText"].as_str().unwrap().contains("<go:endswitch/>"));
        let f = complete(&at("<go:fo|").0, 7);
        assert!(f["items"][0]["textEdit"]["newText"].as_str().unwrap().contains("<go:endfor/>"));
    }

    #[test]
    fn closers_of_the_enclosing_block_sort_first() {
        let (d, o) = at("<go:if a/>\n  <|\n<go:endif/>");
        let v = complete(&d, o);
        let rank = |label: &str| {
            v["items"].as_array().unwrap().iter().find(|i| i["label"] == label).unwrap()["sortText"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert!(rank("<go:else") < rank("<go:for"));
        assert!(rank("<go:endif") < rank("<go:endfor"));
        assert!(rank("<go:endif").starts_with('0') && rank("<go:endfor").starts_with('1'));
    }

    #[test]
    fn nothing_without_a_tag_start() {
        assert_eq!(complete(&at("<p>hello|").0, 8), Value::Null);
        assert_eq!(complete(&at("<div cl|").0, 8), Value::Null);
        assert_eq!(complete(&at("<xyz|").0, 5), Value::Null);
    }
}
