//! `textDocument/documentHighlight`: pair a tag with the other tags of its block.

use serde_json::{Value, json};

use super::conv::span_json;
use super::docs::Doc;
use crate::ast::{Node, Tag};

/// All tags of the block (opener, branches, closer) a node stands for.
fn block_tags(n: &Node) -> Vec<&Tag> {
    let mut out = Vec::new();
    match n {
        Node::If(x) => {
            out.push(&x.open);
            out.extend(x.branches.iter().map(|b| &b.tag));
            out.extend(x.end.as_ref());
        }
        Node::Switch(x) => {
            out.push(&x.open);
            out.extend(x.cases.iter().map(|b| &b.tag));
            out.extend(x.end.as_ref());
        }
        Node::For(x) => {
            out.push(&x.open);
            out.extend(x.end.as_ref());
        }
        _ => {}
    }
    out
}

/// The tags to highlight for a cursor on a tag head; the innermost block wins.
pub fn paired_tags(doc: &Doc, offset: usize) -> Vec<&Tag> {
    let tree = &doc.parse.tree;
    let hit = tree.iter().map(block_tags).filter(|tags| tags.iter().any(|t| t.head.touches(offset))).last();
    match hit {
        Some(tags) => tags,
        None => tree.tags().into_iter().filter(|t| t.head.touches(offset)).collect(),
    }
}

pub fn highlights(doc: &Doc, offset: usize) -> Value {
    let tags = paired_tags(doc, offset);
    if tags.is_empty() {
        return Value::Null;
    }
    Value::Array(tags.iter().map(|t| json!({"range": span_json(&doc.index, t.head), "kind": 1})).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str, needle: &str, delta: usize) -> Vec<(u64, u64)> {
        let d = Doc::new(1, text.into());
        let off = text.find(needle).unwrap() + delta;
        let v = highlights(&d, off);
        v.as_array()
            .map(|a| {
                a.iter()
                    .map(|h| {
                        (
                            h["range"]["start"]["line"].as_u64().unwrap(),
                            h["range"]["start"]["character"].as_u64().unwrap(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn pairs_if_elif_else_endif() {
        let t = "<go:if a/>\n<go:elif b/>\n<go:else/>\n<go:endif/>";
        let want = vec![(0, 0), (1, 0), (2, 0), (3, 0)];
        assert_eq!(lines(t, "go:elif", 2), want);
        assert_eq!(lines(t, "go:endif", 0), want);
        assert_eq!(lines(t, "go:if", 1), want);
    }

    #[test]
    fn innermost_block_wins() {
        let t = "<go:for x/>\n<go:if a/>\n<go:endif/>\n<go:endfor/>";
        assert_eq!(lines(t, "go:if", 1), [(1, 0), (2, 0)]);
        assert_eq!(lines(t, "go:endfor", 1), [(0, 0), (3, 0)]);
    }

    #[test]
    fn switch_cases_and_stray_tags() {
        let t = "<go:switch v/>\n<go:case 1/>\n<go:default/>\n<go:endswitch/>";
        assert_eq!(lines(t, "go:case", 1).len(), 4);
        assert_eq!(lines("<go:endif/>", "go:endif", 1), [(0, 0)]);
    }

    #[test]
    fn nothing_off_tags() {
        let d = Doc::new(1, "<go:if a/>text<go:endif/>".into());
        assert_eq!(highlights(&d, 12), Value::Null);
        // Inside the Go expression the cursor belongs to gopls, not to tag pairing.
        assert_eq!(highlights(&d, 7), Value::Null);
    }
}
