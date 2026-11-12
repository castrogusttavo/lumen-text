//! Minimal tag scanner producing [`ExprRegion`]s straight from `.ghp` text.
//!
//! The real parser (`parser.rs`) is the source of truth for the language server; this scanner
//! exists so the gopls bridge can be developed and tested on its own, and as a tolerant
//! fallback while the user is mid-edit. It recognises `<go ... />` tags only and does not
//! understand a `/>` inside a Go string literal.

use super::virtual_doc::{ExprRegion, RegionKind};

const TAGS: &[(&str, RegionKind)] = &[
    ("<go:endswitch", RegionKind::EndSwitch),
    ("<go:endfor", RegionKind::EndFor),
    ("<go:endif", RegionKind::EndIf),
    ("<go:import", RegionKind::Import),
    ("<go:default", RegionKind::Default),
    ("<go:switch", RegionKind::Switch),
    ("<go:elif", RegionKind::ElseIf),
    ("<go:else", RegionKind::Else),
    ("<go:case", RegionKind::Case),
    ("<go:for", RegionKind::For),
    ("<go:if", RegionKind::If),
    ("<go=", RegionKind::Echo),
];

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Scan `src` for Go tags, in source order.
pub fn scan_regions(src: &str) -> Vec<ExprRegion> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = src[i..].find("<go") {
        let at = i + rel;
        let rest = &src[at..];
        let mut kind = None;
        let mut name_len = 0;
        for (name, k) in TAGS {
            if rest.starts_with(name) {
                let next = bytes.get(at + name.len());
                let boundary = name.ends_with('=') || next.is_none_or(|b| !is_ident(*b));
                if boundary {
                    kind = Some(*k);
                    name_len = name.len();
                    break;
                }
            }
        }
        if kind.is_none() && bytes.get(at + 3).is_some_and(|b| b.is_ascii_whitespace()) {
            kind = Some(RegionKind::Code);
            name_len = 3;
        }
        let Some(kind) = kind else {
            i = at + 3;
            continue;
        };

        let body = at + name_len;
        let close = src[body..].find("/>").map(|p| body + p);
        let limit = close.unwrap_or_else(|| src[body..].find('\n').map_or(src.len(), |p| body + p));
        let mut start = body;
        while start < limit && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
        let mut end = limit;
        while end > start && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        out.push(ExprRegion { start, end, kind });
        i = close.map_or(limit, |c| c + 2).max(at + 3);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str) -> Vec<(RegionKind, &str)> {
        scan_regions(src).iter().map(|r| (r.kind, &src[r.start..r.end])).collect()
    }

    #[test]
    fn scans_every_kind() {
        let src = "<go:import (\"fmt\")/><go\n  x := 1\n/><go= x /><go:if x > 0/><go:elif y/><go:else/><go:endif/>\
                   <go:for i := range 3/><go:endfor/><go:switch x/><go:case 1/><go:default/><go:endswitch/>";
        use RegionKind::*;
        assert_eq!(
            texts(src),
            vec![
                (Import, "(\"fmt\")"),
                (Code, "x := 1"),
                (Echo, "x"),
                (If, "x > 0"),
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
    fn ignores_lookalikes() {
        assert!(scan_regions("<gopher/><go:iffy/><p>go</p>").is_empty());
    }

    #[test]
    fn unterminated_tag_ends_at_line_end() {
        let src = "<go= user.\n<p>";
        assert_eq!(texts(src), vec![(RegionKind::Echo, "user.")]);
    }
}
