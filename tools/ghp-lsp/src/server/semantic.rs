//! `textDocument/semanticTokens/full` for the GHP syntax itself (tag heads, `/>`, import paths).
//! Go code inside the tags is left to the Go side.

use serde_json::{Value, json};

use super::docs::Doc;
use crate::ast::{Node, Span};

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

pub fn full(doc: &Doc) -> Value {
    let mut data: Vec<u32> = Vec::new();
    let (mut pl, mut pc) = (0u32, 0u32);
    for (span, ty) in collect(doc) {
        let a = doc.index.position(span.start);
        let b = doc.index.position(span.end);
        if a.line != b.line || b.character <= a.character {
            continue;
        }
        let dl = a.line - pl;
        let dc = if dl == 0 { a.character - pc } else { a.character };
        data.extend([dl, dc, b.character - a.character, ty, 0]);
        (pl, pc) = (a.line, a.character);
    }
    json!({"data": data})
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

    #[test]
    fn unclosed_tags_still_get_a_keyword() {
        assert_eq!(tokens("<go:if a")[0], [0, 0, 6, 0, 0]);
    }
}
