//! `textDocument/documentSymbol`: the block structure and the imports.

use serde_json::{Value, json};

use super::conv::span_json;
use super::docs::Doc;
use crate::ast::{Branch, Import, Node, Span, Tag};

// LSP SymbolKind values.
const MODULE: u32 = 2;
const PACKAGE: u32 = 4;
const ENUM: u32 = 10;
const FUNCTION: u32 = 12;
const ENUM_MEMBER: u32 = 22;
const STRUCT: u32 = 23;
const EVENT: u32 = 24;

pub fn document_symbols(doc: &Doc) -> Value {
    Value::Array(nodes(doc, &doc.parse.tree.nodes))
}

fn nodes(doc: &Doc, list: &[Node]) -> Vec<Value> {
    list.iter().flat_map(|n| node(doc, n)).collect()
}

/// `prefix` plus the tag's payload, whitespace collapsed and shortened.
fn label(doc: &Doc, prefix: &str, tag: &Tag) -> String {
    let payload = tag.payload_text(&doc.text).split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s = if payload.is_empty() { prefix.to_owned() } else { format!("{prefix} {payload}") };
    if s.chars().count() > 60 {
        s = s.chars().take(59).collect::<String>() + "…";
    }
    s
}

fn symbol(doc: &Doc, name: String, kind: u32, range: Span, sel: Span, children: Vec<Value>) -> Value {
    json!({
        "name": name,
        "kind": kind,
        "range": span_json(&doc.index, range),
        "selectionRange": span_json(&doc.index, sel),
        "children": children,
    })
}

fn branch_symbols(
    doc: &Doc,
    branches: &[Branch],
    kind: u32,
    prefix: impl Fn(&Tag) -> &'static str,
    last_end: usize,
) -> Vec<Value> {
    branches
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let end = branches.get(i + 1).map_or(last_end, |next| next.tag.span.start);
            let range = Span::new(b.tag.span.start, end.max(b.tag.span.end));
            symbol(doc, label(doc, prefix(&b.tag), &b.tag), kind, range, b.tag.span, nodes(doc, &b.body))
        })
        .collect()
}

fn node(doc: &Doc, n: &Node) -> Vec<Value> {
    match n {
        Node::Import(i) => vec![import(doc, i)],
        Node::If(x) => {
            let last_end = x.end.map_or(x.span.end, |e| e.span.start);
            let mut kids = nodes(doc, &x.then);
            kids.extend(branch_symbols(
                doc,
                &x.branches,
                EVENT,
                |t| if t.kind == crate::ast::TagKind::Else { "else" } else { "elif" },
                last_end,
            ));
            vec![symbol(doc, label(doc, "if", &x.open), STRUCT, x.span, x.open.span, kids)]
        }
        Node::Switch(x) => {
            let last_end = x.end.map_or(x.span.end, |e| e.span.start);
            let kids = branch_symbols(
                doc,
                &x.cases,
                ENUM_MEMBER,
                |t| if t.kind == crate::ast::TagKind::Default { "default" } else { "case" },
                last_end,
            );
            vec![symbol(doc, label(doc, "switch", &x.open), ENUM, x.span, x.open.span, kids)]
        }
        Node::For(x) => {
            vec![symbol(doc, label(doc, "for", &x.open), FUNCTION, x.span, x.open.span, nodes(doc, &x.body))]
        }
        Node::Text(_) | Node::Statement(_) | Node::Echo(_) | Node::Stray(_) => Vec::new(),
    }
}

fn import(doc: &Doc, i: &Import) -> Value {
    let kids = i
        .specs
        .iter()
        .map(|s| {
            let name = match &s.alias {
                Some(a) => format!("{a} \"{}\"", s.path),
                None => format!("\"{}\"", s.path),
            };
            symbol(doc, name, MODULE, s.span, s.path_span, Vec::new())
        })
        .collect();
    symbol(doc, "import".to_owned(), PACKAGE, i.tag.span, i.tag.span, kids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &Value) -> Vec<String> {
        v.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_owned()).collect()
    }

    #[test]
    fn structure_of_a_page() {
        let d = Doc::new(
            1,
            "<go:import (\"fmt\", m \"x/models\")/>\n<go:if a/>\n<go:for i, v := range xs/>\n<go= v />\n<go:endfor/>\n<go:elif b/>\nx\n<go:else/>\ny\n<go:endif/>\n<go:switch k/>\n<go:case 1/>\n<go:default/>\n<go:endswitch/>\n"
                .into(),
        );
        let v = document_symbols(&d);
        assert_eq!(names(&v), ["import", "if a", "switch k"]);
        assert_eq!(names(&v[0]["children"]), ["\"fmt\"", "m \"x/models\""]);
        let iff = &v[1]["children"];
        assert_eq!(names(iff), ["for i, v := range xs", "elif b", "else"]);
        assert_eq!(names(&v[2]["children"]), ["case 1", "default"]);
        // The `for` sits inside the `if` range.
        assert_eq!(iff[0]["range"]["start"]["line"], 2);
        assert_eq!(v[1]["range"]["end"]["line"], 9);
    }

    #[test]
    fn selection_range_inside_range_even_when_unclosed() {
        let d = Doc::new(1, "<go:if a/>\ntext\n".into());
        let v = document_symbols(&d);
        assert_eq!(names(&v), ["if a"]);
        let r = &v[0]["range"];
        let s = &v[0]["selectionRange"];
        assert!(r["start"] == s["start"]);
    }

    #[test]
    fn long_labels_are_shortened() {
        let d = Doc::new(1, format!("<go:if {}/><go:endif/>", "a && ".repeat(30)));
        let n = names(&document_symbols(&d));
        assert!(n[0].chars().count() <= 60 && n[0].ends_with('…'));
    }
}
