//! Hover documentation for GHP tags.

use serde_json::{Value, json};

use super::conv::span_json;
use super::docs::Doc;
use crate::ast::TagKind;

/// Markdown documentation of a tag kind.
pub fn tag_doc(kind: TagKind) -> &'static str {
    match kind {
        TagKind::Import => {
            "**`<go:import (...)/>`**\n\nImports Go packages for the template.\n\n```ghp\n<go:import (\"fmt\"\n    m \"example.com/app/models\")/>\n```\n\nEach item is a quoted import path, optionally preceded by an alias."
        }
        TagKind::Statement => {
            "**`<go ... />`**\n\nRuns Go statements. Nothing is written to the output.\n\n```ghp\n<go\n    user := loadUser()\n/>\n```"
        }
        TagKind::Echo => {
            "**`<go= expr />`**\n\nWrites the value of a Go expression to the output.\n\n```ghp\n<title><go= page.Title /></title>\n```"
        }
        TagKind::If => {
            "**`<go:if cond/>`**\n\nRenders the block when the Go condition is true. Closed by `<go:endif/>`; may contain `<go:elif/>` and `<go:else/>`.\n\n```ghp\n<go:if user.Admin/>\n    ...\n<go:endif/>\n```"
        }
        TagKind::Elif => {
            "**`<go:elif cond/>`**\n\nAlternative branch of a `<go:if/>`, tried when the previous conditions are false."
        }
        TagKind::Else => "**`<go:else/>`**\n\nFinal branch of a `<go:if/>`, rendered when no condition matched.",
        TagKind::EndIf => "**`<go:endif/>`**\n\nCloses a `<go:if/>` block.",
        TagKind::Switch => {
            "**`<go:switch value/>`**\n\nSelects one of several `<go:case/>` branches. The value may be omitted. Closed by `<go:endswitch/>`.\n\n```ghp\n<go:switch user.Role/>\n    <go:case \"admin\"/>\n        ...\n    <go:default/>\n        ...\n<go:endswitch/>\n```"
        }
        TagKind::Case => {
            "**`<go:case value/>`**\n\nBranch of a `<go:switch/>` taken when the value matches. Takes a comma separated list of values."
        }
        TagKind::Default => "**`<go:default/>`**\n\nBranch of a `<go:switch/>` taken when no `<go:case/>` matched.",
        TagKind::EndSwitch => "**`<go:endswitch/>`**\n\nCloses a `<go:switch/>` block.",
        TagKind::For => {
            "**`<go:for clause/>`**\n\nRepeats the block. Takes any Go `for` clause. Closed by `<go:endfor/>`.\n\n```ghp\n<go:for i, item := range items/>\n    <li><go= item /></li>\n<go:endfor/>\n```"
        }
        TagKind::EndFor => "**`<go:endfor/>`**\n\nCloses a `<go:for/>` block.",
    }
}

/// Hover for a cursor outside Go regions: documentation of the tag under it.
pub fn hover(doc: &Doc, offset: usize) -> Value {
    let tree = &doc.parse.tree;
    let Some(tag) = tree.tags().into_iter().find(|t| t.span.contains(offset)) else {
        return Value::Null;
    };
    json!({
        "contents": {"kind": "markdown", "value": tag_doc(tag.kind)},
        "range": span_json(&doc.index, tag.head),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(t: &str) -> Doc {
        Doc::new(1, t.to_owned())
    }

    #[test]
    fn hover_on_tag_head() {
        let d = doc("<p/>\n<go:if a/>x<go:endif/>");
        let off = d.text.find("go:if").unwrap() + 1;
        let h = hover(&d, off);
        assert!(h["contents"]["value"].as_str().unwrap().contains("<go:if cond/>"));
        assert_eq!(h["range"]["start"]["line"], 1);
    }

    #[test]
    fn hover_on_closer_and_text() {
        let d = doc("<go:if a/>x<go:endif/>");
        let h = hover(&d, d.text.find("endif").unwrap());
        assert!(h["contents"]["value"].as_str().unwrap().contains("Closes"));
        assert_eq!(hover(&d, d.text.find('x').unwrap()), Value::Null);
    }

    #[test]
    fn every_kind_is_documented() {
        for k in [
            TagKind::Import,
            TagKind::Statement,
            TagKind::Echo,
            TagKind::If,
            TagKind::Elif,
            TagKind::Else,
            TagKind::EndIf,
            TagKind::Switch,
            TagKind::Case,
            TagKind::Default,
            TagKind::EndSwitch,
            TagKind::For,
            TagKind::EndFor,
        ] {
            assert!(tag_doc(k).starts_with("**`<go"));
        }
    }
}
