//! `textDocument/codeAction`: quick fixes for the parser's diagnostics.

use serde_json::{Value, json};

use super::conv::{diag_json, line_indent, range_from, span_json, trim_end};
use super::docs::Doc;
use crate::ast::{Node, Span, Tag};
use crate::diagnostics::{Diagnostic, code};

struct Edit {
    span: Span,
    text: String,
}

fn find_tag(doc: &Doc, inside: Span) -> Option<&Tag> {
    doc.parse.tree.tags().into_iter().find(|t| t.span.start <= inside.start && inside.end <= t.span.end)
}

/// A tag that has no block to belong to: delete it (and its line when nothing else is on it).
fn remove_tag(doc: &Doc, tag: &Tag) -> Edit {
    let idx = &doc.index;
    let line = idx.line_of(tag.span.start);
    let ls = idx.line_span(line).unwrap_or_default();
    let alone = doc.text[ls.start..tag.span.start].trim().is_empty()
        && tag.span.end <= ls.end
        && doc.text[tag.span.end..ls.end].trim().is_empty();
    if !alone {
        return Edit { span: tag.span, text: String::new() };
    }
    let rest = &doc.text[ls.end..];
    let term = if rest.starts_with("\r\n") { 2 } else { usize::from(rest.starts_with(['\n', '\r'])) };
    Edit { span: Span::new(ls.start, ls.end + term), text: String::new() }
}

/// Insert the missing closer after the last line that is still indented deeper than the opener
/// (branch tags of the same block count as part of it), so it lands before a closing `</div>`.
fn insert_closer(doc: &Doc, opener: Span, closer: &str) -> Option<Edit> {
    let (end, branches): (usize, Vec<usize>) = doc.parse.tree.iter().find_map(|n| match n {
        Node::If(x) if x.end.is_none() && x.open.span == opener => {
            Some((x.span.end, x.branches.iter().map(|b| b.tag.span.start).collect()))
        }
        Node::Switch(x) if x.end.is_none() && x.open.span == opener => {
            Some((x.span.end, x.cases.iter().map(|b| b.tag.span.start).collect()))
        }
        Node::For(x) if x.end.is_none() && x.open.span == opener => Some((x.span.end, Vec::new())),
        _ => None,
    })?;
    let idx = &doc.index;
    let indent = line_indent(&doc.text, idx, opener.start);
    let limit = trim_end(&doc.text, end).max(opener.end);
    let mut at = opener.end;
    for l in idx.line_of(opener.end) + 1..idx.line_count() {
        let sp = idx.line_span(l)?;
        if sp.start >= limit {
            break;
        }
        let line = &doc.text[sp.start..sp.end];
        let rest = line.trim_start();
        if rest.is_empty() {
            continue;
        }
        let first = sp.start + (line.len() - rest.len());
        if line.len() - rest.len() <= indent.len() && !branches.contains(&first) {
            break;
        }
        at = sp.end.min(limit);
    }
    Some(Edit { span: Span::new(at, at), text: format!("\n{indent}{closer}") })
}

fn fix(doc: &Doc, d: &Diagnostic) -> Option<(String, Edit)> {
    let closer = |c: &str| insert_closer(doc, d.span, c);
    match d.code {
        code::UNCLOSED_IF => Some(("Insert missing `<go:endif/>`".into(), closer("<go:endif/>")?)),
        code::UNCLOSED_SWITCH => Some(("Insert missing `<go:endswitch/>`".into(), closer("<go:endswitch/>")?)),
        code::UNCLOSED_FOR => Some(("Insert missing `<go:endfor/>`".into(), closer("<go:endfor/>")?)),
        code::UNMATCHED_ENDIF
        | code::UNMATCHED_ENDFOR
        | code::UNMATCHED_ENDSWITCH
        | code::ELIF_OUTSIDE_IF
        | code::ELSE_OUTSIDE_IF
        | code::CASE_OUTSIDE_SWITCH
        | code::DEFAULT_OUTSIDE_SWITCH => {
            let tag = find_tag(doc, d.span)?;
            let name = tag.head.slice(&doc.text);
            Some((format!("Remove stray `{name}/>`"), remove_tag(doc, tag)))
        }
        code::CLOSER_WITH_PAYLOAD | code::ELSE_WITH_PAYLOAD | code::DEFAULT_WITH_PAYLOAD => {
            let tag = find_tag(doc, d.span)?;
            let start = doc.text[..d.span.start].trim_end().len().max(tag.head.end);
            Some((
                "Remove the unexpected expression".into(),
                Edit { span: Span::new(start, d.span.end), text: String::new() },
            ))
        }
        code::TAG_NOT_CLOSED | code::UNTERMINATED_TAG => {
            let tag = find_tag(doc, d.span)?;
            let src = tag.span.slice(&doc.text);
            if src.ends_with("/>") {
                return None;
            }
            if src.ends_with('>') {
                let at = tag.span.end - 1;
                Some(("Close the tag with `/>`".into(), Edit { span: Span::new(at, at), text: "/".into() }))
            } else {
                let at = tag.span.end;
                Some(("Close the tag with `/>`".into(), Edit { span: Span::new(at, at), text: "/>".into() }))
            }
        }
        _ => None,
    }
}

pub fn code_actions(doc: &Doc, uri: &str, params: &Value) -> Value {
    let want = params.get("range").and_then(range_from).map(|r| doc.index.span(r));
    let mut out = Vec::new();
    for d in &doc.parse.diagnostics {
        if want.is_some_and(|w| !(d.span.start <= w.end && w.start <= d.span.end)) {
            continue;
        }
        let Some((title, edit)) = fix(doc, d) else { continue };
        let mut changes = serde_json::Map::new();
        changes.insert(uri.to_owned(), json!([{"range": span_json(&doc.index, edit.span), "newText": edit.text}]));
        out.push(json!({
            "title": title,
            "kind": "quickfix",
            "diagnostics": [diag_json(d, &doc.index)],
            "isPreferred": true,
            "edit": {"changes": changes},
        }));
    }
    Value::Array(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply the first action whose title contains `title` and return the new text.
    fn run(text: &str, title: &str) -> Option<String> {
        let doc = Doc::new(1, text.into());
        let acts = code_actions(&doc, "file:///a.ghp", &json!({}));
        let act = acts.as_array()?.iter().find(|a| a["title"].as_str().unwrap().contains(title))?;
        let e = &act["edit"]["changes"]["file:///a.ghp"][0];
        let r = range_from(&e["range"])?;
        let s = doc.index.span(r);
        let mut out = text.to_owned();
        out.replace_range(s.start..s.end, e["newText"].as_str()?);
        Some(out)
    }

    fn clean(text: &str) -> bool {
        crate::parser::parse(text).diagnostics.is_empty()
    }

    #[test]
    fn inserts_missing_closers() {
        for (src, title) in [
            ("<div>\n  <go:if a/>\n    <p>x</p>\n</div>", "endif"),
            ("<ul>\n  <go:for x/>\n    <li/>\n</ul>", "endfor"),
            ("<go:switch v/>\n  <go:case 1/>\n  a\n", "endswitch"),
        ] {
            let fixed = run(src, title).unwrap();
            assert!(clean(&fixed), "{fixed:?}");
        }
        let fixed = run("  <go:if a/>\n    x\n", "endif").unwrap();
        assert_eq!(fixed, "  <go:if a/>\n    x\n  <go:endif/>\n");
    }

    #[test]
    fn removes_stray_closers() {
        let fixed = run("<p>\n  <go:endif/>\n</p>", "Remove stray").unwrap();
        assert_eq!(fixed, "<p>\n</p>");
        let fixed = run("<p><go:endfor/></p>", "Remove stray").unwrap();
        assert_eq!(fixed, "<p></p>");
        let fixed = run("<go:else/>\r\nx", "Remove stray").unwrap();
        assert_eq!(fixed, "x");
        assert!(clean(&run("a\n<go:endswitch/>\nb", "Remove stray").unwrap()));
    }

    #[test]
    fn removes_unexpected_payload() {
        assert_eq!(
            run("<go:if a/>x<go:else y/><go:endif/>", "unexpected").unwrap(),
            "<go:if a/>x<go:else/><go:endif/>"
        );
        assert_eq!(run("<go:for a/><go:endfor x/>", "unexpected").unwrap(), "<go:for a/><go:endfor/>");
    }

    #[test]
    fn mismatched_closer_offers_both_fixes() {
        let src = "<go:for a/>\n<go:endif/>\n";
        assert!(run(src, "endfor").is_some());
        assert!(run(src, "Remove stray").is_some());
    }

    #[test]
    fn range_filters_actions() {
        let doc = Doc::new(1, "<go:endif/>\n\n\n<go:endfor/>".into());
        let far = json!({"range": {"start": {"line": 3, "character": 1}, "end": {"line": 3, "character": 1}}});
        let acts = code_actions(&doc, "file:///a.ghp", &far);
        assert_eq!(acts.as_array().unwrap().len(), 1);
        assert!(acts[0]["title"].as_str().unwrap().contains("endfor"));
    }

    #[test]
    fn nothing_for_a_clean_document() {
        let doc = Doc::new(1, "<go:if a/>x<go:endif/>".into());
        assert_eq!(code_actions(&doc, "u", &json!({})), json!([]));
    }
}
