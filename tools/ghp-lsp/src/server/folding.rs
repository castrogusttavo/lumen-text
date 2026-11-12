//! `textDocument/foldingRange`: blocks, branches and multi-line tags.

use serde_json::{Value, json};

use super::conv::trim_end;
use super::docs::Doc;
use crate::ast::{Branch, Node, Span};

pub fn folding_ranges(doc: &Doc) -> Value {
    let mut out: Vec<(u32, u32)> = Vec::new();
    walk(doc, &doc.parse.tree.nodes, &mut out);
    out.sort_unstable();
    out.dedup_by_key(|r| r.0);
    Value::Array(out.into_iter().map(|(s, e)| json!({"startLine": s, "endLine": e, "kind": "region"})).collect())
}

/// Fold the lines after `start` up to the last content before `end`, so a closer stays visible.
fn fold(doc: &Doc, start: usize, end: usize, out: &mut Vec<(u32, u32)>) {
    let s = doc.index.position(start).line;
    let e = doc.index.position(trim_end(&doc.text, end)).line;
    if e > s {
        out.push((s, e));
    }
}

fn branches(doc: &Doc, list: &[Branch], last_end: usize, out: &mut Vec<(u32, u32)>) {
    for (i, b) in list.iter().enumerate() {
        let end = list.get(i + 1).map_or(last_end, |n| n.tag.span.start);
        fold(doc, b.tag.span.start, end, out);
        walk(doc, &b.body, out);
    }
}

fn tag_fold(doc: &Doc, span: Span, out: &mut Vec<(u32, u32)>) {
    let s = doc.index.position(span.start).line;
    let e = doc.index.position(span.end.saturating_sub(1)).line;
    if e > s {
        out.push((s, e));
    }
}

fn walk(doc: &Doc, list: &[Node], out: &mut Vec<(u32, u32)>) {
    for n in list {
        match n {
            Node::If(x) => {
                let last_end = x.end.map_or(x.span.end, |e| e.span.start);
                fold(doc, x.open.span.start, last_end, out);
                walk(doc, &x.then, out);
                branches(doc, &x.branches, last_end, out);
            }
            Node::Switch(x) => {
                let last_end = x.end.map_or(x.span.end, |e| e.span.start);
                fold(doc, x.open.span.start, last_end, out);
                branches(doc, &x.cases, last_end, out);
            }
            Node::For(x) => {
                let last_end = x.end.map_or(x.span.end, |e| e.span.start);
                fold(doc, x.open.span.start, last_end, out);
                walk(doc, &x.body, out);
            }
            Node::Import(i) => tag_fold(doc, i.tag.span, out),
            Node::Statement(t) | Node::Echo(t) => tag_fold(doc, t.span, out),
            Node::Text(_) | Node::Stray(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(text: &str) -> Vec<(u64, u64)> {
        let d = Doc::new(1, text.into());
        folding_ranges(&d)
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["startLine"].as_u64().unwrap(), r["endLine"].as_u64().unwrap()))
            .collect()
    }

    #[test]
    fn for_block_keeps_closer_visible() {
        // Lines: 0 for, 1 body, 2 body, 3 endfor
        assert_eq!(ranges("<go:for a/>\n x\n y\n<go:endfor/>\n"), [(0, 2)]);
    }

    #[test]
    fn if_branches_fold_separately() {
        let r = ranges("<go:if a/>\n x\n<go:elif b/>\n y\n z\n<go:else/>\n w\n<go:endif/>");
        // whole if (0..6), elif (2..4), else (5..6): the whole-if shares the start line with the
        // first branch fold only when there are no branches.
        assert_eq!(r, [(0, 6), (2, 4), (5, 6)]);
    }

    #[test]
    fn multi_line_statement_and_single_line_blocks() {
        assert_eq!(ranges("<go\n a := 1\n/>"), [(0, 2)]);
        assert!(ranges("<go:if a/>x<go:endif/>").is_empty());
        assert!(ranges("<p>\n</p>").is_empty());
    }

    #[test]
    fn unclosed_block_folds_to_last_content() {
        assert_eq!(ranges("<go:for a/>\n x\n y\n\n"), [(0, 2)]);
    }

    #[test]
    fn switch_cases() {
        let r = ranges("<go:switch v/>\n<go:case 1/>\n a\n<go:default/>\n b\n<go:endswitch/>");
        assert_eq!(r, [(0, 4), (1, 2), (3, 4)]);
    }
}
