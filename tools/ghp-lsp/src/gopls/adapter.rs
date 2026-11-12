//! Adapter from the parser's tree to the bridge's [`ExprRegion`]s.
//!
//! The parser is the single source of truth for where Go text lives in a page: it knows about
//! quoted strings, raw strings and comments (a `/>` inside them does not end a tag) and about
//! unterminated tags. The virtual document additionally needs the structural tags without Go text
//! (`else`, `endif`, ...) as scaffolding, so this walks every tag rather than only the payloads
//! of [`Tree::expr_regions`].

use super::virtual_doc::{ExprRegion, RegionKind};
use crate::ast::{Node, Tag, TagKind, Tree};
use crate::parser::parse;
use std::collections::HashSet;

/// Every tag that belongs to a construct, in source order. Stray tags (a lone `<go:endif/>`, an
/// opener beyond the nesting limit) are left out: they have nothing to be scaffolded against.
pub fn live_tags(tree: &Tree) -> Vec<&Tag> {
    let strays: HashSet<usize> =
        tree.iter().filter_map(|n| if let Node::Stray(t) = n { Some(t.span.start) } else { None }).collect();
    tree.tags().into_iter().filter(|t| !strays.contains(&t.span.start)).collect()
}

fn region_kind(kind: TagKind) -> RegionKind {
    match kind {
        TagKind::Import => RegionKind::Import,
        TagKind::Statement => RegionKind::Code,
        TagKind::Echo => RegionKind::Echo,
        TagKind::If => RegionKind::If,
        TagKind::Elif => RegionKind::ElseIf,
        TagKind::Else => RegionKind::Else,
        TagKind::EndIf => RegionKind::EndIf,
        TagKind::Switch => RegionKind::Switch,
        TagKind::Case => RegionKind::Case,
        TagKind::Default => RegionKind::Default,
        TagKind::EndSwitch => RegionKind::EndSwitch,
        TagKind::For => RegionKind::For,
        TagKind::EndFor => RegionKind::EndFor,
    }
}

/// The regions of a parsed page, in source order, ready for [`super::virtual_doc::build`] and
/// [`super::Bridge::update`].
///
/// * Tags with Go text map to that text's exact span.
/// * Structural tags without text become empty regions at the start of the tag.
/// * A `<go:import/>` without a payload is skipped (`import` alone is not Go).
pub fn regions(tree: &Tree) -> Vec<ExprRegion> {
    let mut out = Vec::new();
    for t in live_tags(tree) {
        let kind = region_kind(t.kind);
        match t.payload {
            Some(p) => out.push(ExprRegion { start: p.start, end: p.end, kind }),
            None if t.kind == TagKind::Import => {}
            // `<go= | />`: Go text is about to be typed. The (possibly empty) gap between the head
            // and the `/>` becomes the region, so a cursor in it maps into the virtual document.
            None if t.kind.expr_kind().is_some() => {
                out.push(ExprRegion { start: t.inner.start, end: t.inner.end, kind })
            }
            None => out.push(ExprRegion { start: t.span.start, end: t.span.start, kind }),
        }
    }
    out
}

/// Parse `src` and return its regions. Convenience for callers that hold only the text.
pub fn regions_of(src: &str) -> Vec<ExprRegion> {
    regions(&parse(src).tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str) -> Vec<(RegionKind, &str)> {
        regions_of(src).iter().map(|r| (r.kind, &src[r.start..r.end])).collect()
    }

    #[test]
    fn every_kind_in_source_order() {
        use RegionKind::*;
        let src = "<go:import (\"fmt\")/><go\n  x := 1\n/><go= x /><go:if x == 0/><go:elif y/><go:else/><go:endif/>\
                   <go:for i := range 3/><go:endfor/><go:switch x/><go:case 1/><go:default/><go:endswitch/>";
        assert_eq!(
            texts(src),
            vec![
                (Import, "(\"fmt\")"),
                (Code, "x := 1"),
                (Echo, "x"),
                (If, "x == 0"),
                (ElseIf, "y"),
                (Else, ""),
                (EndIf, ""),
                (For, "i := range 3"),
                (EndFor, ""),
                (Switch, "x"),
                (Case, "1"),
                (Default, ""),
                (EndSwitch, ""),
            ]
        );
    }

    #[test]
    fn slash_gt_inside_go_strings_and_comments_does_not_end_the_tag() {
        let cases = [
            ("<go= \"a/>b\" />", "\"a/>b\""),
            ("<go= `a/>b` />", "`a/>b`"),
            ("<go= '>' />", "'>'"),
            ("<go= f(\"/>\", `/>`) />", "f(\"/>\", `/>`)"),
            ("<go x := \"/>\" /* /> */ />", "x := \"/>\" /* /> */"),
        ];
        for (src, want) in cases {
            let r = texts(src);
            assert_eq!(r.len(), 1, "{src}: {r:?}");
            assert_eq!(r[0].1, want, "{src}");
        }
    }

    #[test]
    fn stray_tags_are_not_scaffolded() {
        let kinds: Vec<_> = regions_of("<go:endif/><go= x /><go:else/>").iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![RegionKind::Echo]);
    }

    #[test]
    fn structural_tags_are_empty_regions_and_empty_import_is_skipped() {
        let src = "<go:if a/><go:else/><go:endif/><go:import />";
        let r = regions_of(src);
        assert_eq!(r.len(), 3);
        let at = src.find("<go:else").unwrap();
        assert!(r[1].start == at && r[1].end == at);
    }

    #[test]
    fn empty_tags_that_take_go_text_keep_a_slot_between_head_and_closer() {
        let src = "<go= /><go=/><go:if/><go:else/>";
        let r = regions_of(src);
        let got: Vec<(RegionKind, usize, usize)> = r.iter().map(|r| (r.kind, r.start, r.end)).collect();
        assert_eq!(
            got,
            vec![
                (RegionKind::Echo, 4, 5),
                (RegionKind::Echo, 11, 11),
                (RegionKind::If, 19, 19),
                (RegionKind::Else, 21, 21),
            ]
        );
    }

    #[test]
    fn unterminated_tag_keeps_its_text_for_completion() {
        let src = "<go= user.";
        assert_eq!(texts(src), vec![(RegionKind::Echo, "user.")]);
    }
}
