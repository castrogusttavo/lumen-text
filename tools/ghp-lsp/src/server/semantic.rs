//! `textDocument/semanticTokens/full` for the GHP syntax itself (tag heads, `/>`, import paths).
//! Go code inside the tags is left to the Go side.

use serde_json::{Value, json};

use super::docs::Doc;
use crate::ast::{Node, Span};
use crate::line_index::Position;

pub const TYPES: &[&str] = &["keyword", "operator", "string", "namespace"];
const KEYWORD: u32 = 0;
const OPERATOR: u32 = 1;
const STRING: u32 = 2;
const NAMESPACE: u32 = 3;

pub fn legend() -> Value {
    json!({"tokenTypes": TYPES, "tokenModifiers": []})
}

/// Absolute tokens `(span, type)` sorted by position.
fn collect(doc: &Doc) -> Vec<(Span, u32)> {
    let text = &doc.text;
    let tree = &doc.parse.tree;
    let mut toks: Vec<(Span, u32)> = Vec::new();
    for t in tree.tags() {
        toks.push((t.head, KEYWORD));
        if t.closed && t.span.len() >= 2 && text[t.span.start..t.span.end].ends_with("/>") {
            toks.push((Span::new(t.span.end - 2, t.span.end), OPERATOR));
        }
    }
    for n in tree.iter() {
        let Node::Import(i) = n else { continue };
        for s in &i.specs {
            if let Some(a) = &s.alias {
                toks.push((Span::new(s.span.start, s.span.start + a.len()), NAMESPACE));
            }
            let (ps, pe) = (s.path_span.start, s.path_span.end);
            let quoted = ps > 0 && text.as_bytes().get(ps - 1) == Some(&b'"') && text.as_bytes().get(pe) == Some(&b'"');
            let span = if quoted { Span::new(ps - 1, pe + 1) } else { s.path_span };
            toks.push((span, STRING));
        }
    }
    toks.sort_by_key(|(s, _)| (s.start, s.end));
    toks.dedup_by_key(|(s, _)| s.start);
    toks
}

/// The LSP token array (`[deltaLine, deltaStart, length, type, modifiers, ...]`) of the whole
/// document, or of the tokens that start inside `range` (`(start, end)` positions, end exclusive).
/// Deltas are always relative to the start of the document, as the protocol wants.
pub fn data(doc: &Doc, range: Option<(Position, Position)>) -> Vec<u32> {
    let mut data: Vec<u32> = Vec::new();
    let (mut pl, mut pc) = (0u32, 0u32);
    for (span, ty) in collect(doc) {
        let a = doc.index.position(span.start);
        let b = doc.index.position(span.end);
        if a.line != b.line || b.character <= a.character {
            continue;
        }
        if let Some((from, to)) = range
            && (a < from || a >= to)
        {
            continue;
        }
        let dl = a.line - pl;
        let dc = if dl == 0 { a.character - pc } else { a.character };
        data.extend([dl, dc, b.character - a.character, ty, 0]);
        (pl, pc) = (a.line, a.character);
    }
    data
}

pub fn full(doc: &Doc) -> Value {
    json!({"data": data(doc, None)})
}

/// `textDocument/semanticTokens/range`.
pub fn range(doc: &Doc, from: Position, to: Position) -> Value {
    json!({"data": data(doc, Some((from, to)))})
}

/// The single edit that turns `old` into `new` (common prefix and suffix kept), as the
/// `edits` of a `textDocument/semanticTokens/full/delta` result. Empty when nothing changed.
pub fn delta(old: &[u32], new: &[u32]) -> Vec<Value> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old.iter().rev().zip(new.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    let (del, ins) = (&old[prefix..old.len() - suffix], &new[prefix..new.len() - suffix]);
    if del.is_empty() && ins.is_empty() {
        return Vec::new();
    }
    vec![json!({"start": prefix, "deleteCount": del.len(), "data": ins})]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(text: &str) -> Vec<[u32; 5]> {
        let d = Doc::new(1, text.into());
        full(&d)["data"]
            .as_array()
            .unwrap()
            .chunks(5)
            .map(|c| [0, 1, 2, 3, 4].map(|i| c[i].as_u64().unwrap() as u32))
            .collect()
    }

    #[test]
    fn heads_and_closers() {
        // `<go:if` (0..6) keyword, `/>` operator; then line 1 `<go:endif` keyword + `/>`.
        assert_eq!(
            tokens("<go:if a/>\n<go:endif/>"),
            [[0, 0, 6, 0, 0], [0, 8, 2, 1, 0], [1, 0, 9, 0, 0], [0, 9, 2, 1, 0]]
        );
    }

    #[test]
    fn import_alias_and_path() {
        let t = tokens("<go:import (m \"x/y\")/>");
        // keyword, alias (namespace), quoted path (string), closer
        assert_eq!(t.iter().map(|x| x[3]).collect::<Vec<_>>(), [0, 3, 2, 1]);
        assert_eq!(t[2][2], 5);
    }

    #[test]
    fn utf16_columns() {
        let t = tokens("😀<go:if a/>");
        assert_eq!(t[0], [0, 2, 6, 0, 0]);
    }

    fn apply(old: &[u32], edits: &[Value]) -> Vec<u32> {
        let mut out = old.to_vec();
        for e in edits.iter().rev() {
            let (s, d) = (e["start"].as_u64().unwrap() as usize, e["deleteCount"].as_u64().unwrap() as usize);
            let ins: Vec<u32> = e["data"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect();
            out.splice(s..s + d, ins);
        }
        out
    }

    #[test]
    fn range_keeps_document_relative_deltas() {
        let d = Doc::new(1, "<go:if a/>\n<go:endif/>\n<go:for x/>\n<go:endfor/>".into());
        let p = |line, character| Position { line, character };
        let all = data(&d, None);
        let tail = data(&d, Some((p(2, 0), p(4, 0))));
        // The `for` tokens start on line 2: the first delta is relative to the document start.
        assert_eq!(&tail[..5], &[2, 0, 7, 0, 0]);
        assert!(tail.len() < all.len());
        assert_eq!(data(&d, Some((p(0, 0), p(9, 0)))), all);
        assert!(data(&d, Some((p(8, 0), p(9, 0)))).is_empty());
    }

    #[test]
    fn delta_edits_reproduce_the_new_tokens() {
        let old = data(&Doc::new(1, "<go:if a/>\n<go:endif/>".into()), None);
        for text in ["<go:if a/>\n<go:endif/>", "x\n<go:if a/>\n<go:endif/>", "<go:for a/>\n<go:endfor/>", ""] {
            let new = data(&Doc::new(2, text.into()), None);
            let edits = delta(&old, &new);
            assert_eq!(apply(&old, &edits), new, "{text:?}");
            assert_eq!(edits.is_empty(), old == new);
        }
    }

    #[test]
    fn unclosed_tags_still_get_a_keyword() {
        assert_eq!(tokens("<go:if a")[0], [0, 0, 6, 0, 0]);
    }
}
