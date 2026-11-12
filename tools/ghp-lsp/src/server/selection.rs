//! `textDocument/selectionRange`: grow the selection word -> Go expression -> tag -> enclosing
//! blocks -> document.

use serde_json::{Value, json};

use super::conv::{pos_from, span_json};
use super::docs::Doc;
use crate::ast::{Node, Span};

fn is_word(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// The identifier-like word touching `offset`, if any.
fn word_at(text: &str, offset: usize) -> Option<Span> {
    let offset = offset.min(text.len());
    let start = text[..offset].char_indices().rev().take_while(|&(_, c)| is_word(c)).last().map_or(offset, |(i, _)| i);
    let len: usize = text[offset..].chars().take_while(|&c| is_word(c)).map(char::len_utf8).sum();
    (start < offset + len).then(|| Span::new(start, offset + len))
}

fn node_spans(list: &[&Node], offset: usize, out: &mut Vec<Span>) {
    for n in list {
        if n.span().touches(offset) {
            out.push(n.span());
            node_spans(&n.children(), offset, out);
        }
    }
}

/// Strictly growing chain of spans around `offset`, innermost first, ending with the document.
pub fn chain(doc: &Doc, offset: usize) -> Vec<Span> {
    let tree = &doc.parse.tree;
    let mut cands = vec![Span::new(offset, offset)];
    cands.extend(word_at(&doc.text, offset));
    if let Some(r) = tree.expr_region_at(offset) {
        cands.push(r.span);
    }
    cands.extend(tree.tags().into_iter().map(|t| t.span));
    node_spans(&tree.nodes.iter().collect::<Vec<_>>(), offset, &mut cands);
    cands.push(Span::new(0, doc.text.len()));
    cands.retain(|s| s.start <= offset && offset <= s.end);
    cands.sort_by_key(|s| (s.len(), s.start));

    let mut out: Vec<Span> = Vec::new();
    for s in cands {
        match out.last() {
            None => out.push(s),
            Some(l) if s != *l && s.start <= l.start && l.end <= s.end => out.push(s),
            _ => {}
        }
    }
    out
}

pub fn selection_ranges(doc: &Doc, params: &Value) -> Value {
    let positions = params.get("positions").and_then(Value::as_array);
    let result: Vec<Value> = positions
        .into_iter()
        .flatten()
        .filter_map(pos_from)
        .map(|p| {
            let spans = chain(doc, doc.offset(p));
            spans.iter().rev().fold(Value::Null, |parent, s| {
                let mut node = json!({"range": span_json(&doc.index, *s)});
                if !parent.is_null() {
                    node["parent"] = parent;
                }
                node
            })
        })
        .collect();
    Value::Array(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_boundaries() {
        assert_eq!(word_at("ab cd", 1), Some(Span::new(0, 2)));
        assert_eq!(word_at("ab cd", 2), Some(Span::new(0, 2)));
        assert_eq!(word_at("ab cd", 3), Some(Span::new(3, 5)));
        assert_eq!(word_at("a  b", 2), None);
        assert_eq!(word_at("", 0), None);
    }

    #[test]
    fn grows_outward() {
        let text = "<div>\n<go:if user.Admin/>\n  <b>x</b>\n<go:endif/>\n</div>";
        let d = Doc::new(1, text.into());
        let off = text.find("Admin").unwrap() + 2;
        let c = chain(&d, off);
        let s = |sp: Span| sp.slice(text);
        assert_eq!(s(c[0]), "");
        assert_eq!(s(c[1]), "Admin");
        assert_eq!(s(c[2]), "user.Admin");
        assert_eq!(s(c[3]), "<go:if user.Admin/>");
        assert!(s(c[4]).starts_with("<go:if") && s(c[4]).ends_with("<go:endif/>"));
        assert_eq!(*c.last().unwrap(), Span::new(0, text.len()));
        for w in c.windows(2) {
            assert!(w[1].start <= w[0].start && w[0].end <= w[1].end && w[0] != w[1]);
        }
    }

    #[test]
    fn nested_json_shape() {
        let d = Doc::new(1, "<go:for xs/>a<go:endfor/>".into());
        let v =
            selection_ranges(&d, &json!({"positions": [{"line": 0, "character": 12}, {"line": 9, "character": 0}]}));
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let mut depth = 0;
        let mut cur = &arr[0];
        while let Some(p) = cur.get("parent") {
            depth += 1;
            cur = p;
        }
        assert!(depth >= 2);
        assert_eq!(cur["range"]["start"], json!({"line": 0, "character": 0}));
    }
}
