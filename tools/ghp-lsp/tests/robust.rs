//! Robustness: the parser must never panic and must always return a lossless, well-formed tree,
//! whatever the input. No external fuzzing crate: truncation at every char boundary plus a small
//! deterministic xorshift generator.

use ghp_lsp::ast::{Node, Segment, Span};
use ghp_lsp::line_index::LineIndex;
use ghp_lsp::parser::parse;

const SAMPLE: &str = "<go:import (\"fmt\"\n  s \"strings\")/>\r\n\
<!doctype html><html><body>\r\n\
<go\n  x := \"é>😀\"\n/>\n\
<go= x + (1 > 0) /> <p>日本語 > <</p>\n\
<go:if (x > 1)/>a<go:elif y/>b<go:else/>c<go:endif/>\n\
<go:switch v/><go:case 1, 2/>one<go:case 3/>three<go:default/>other<go:endswitch/>\n\
<go:for i := range []int{1, 2}/><go= i /><go:endfor/>\n\
</body></html>\n";

const FRAGMENTS: &[&str] = &[
    "<go:if x/>",
    "<go:if/>",
    "<go:elif y/>",
    "<go:else/>",
    "<go:else x/>",
    "<go:endif/>",
    "<go:endif",
    "<go:switch v/>",
    "<go:case 1/>",
    "<go:default/>",
    "<go:endswitch/>",
    "<go:for i := 0; i < n; i++/>",
    "<go:endfor/>",
    "<go= v />",
    "<go=",
    "<go ",
    "<go:import (\"fmt\", s \"os\")/>",
    "<go:import (",
    "<go:nope/>",
    "<",
    ">",
    "/>",
    "/",
    "\"",
    "'",
    "`",
    "(",
    ")",
    "[",
    "\\",
    "é",
    "😀",
    "日本",
    "\r\n",
    "\n",
    "\r",
    " ",
    "text",
    "<div class=\"a\">",
];

/// Everything the editor relies on, checked on arbitrary input.
fn check(src: &str) {
    let p = parse(src);
    let ok = |s: Span| {
        s.start <= s.end && s.end <= src.len() && src.is_char_boundary(s.start) && src.is_char_boundary(s.end)
    };

    // Lossless: text runs and tags tile the source exactly.
    let segs = p.tree.segments();
    let mut at = 0;
    for s in &segs {
        let sp = s.span();
        assert!(ok(sp), "bad segment span {sp:?} in {src:?}");
        assert_eq!(sp.start, at, "gap or overlap before {sp:?} in {src:?}");
        if let Segment::Text(t) = s {
            assert!(!t.is_empty(), "empty text segment in {src:?}");
        }
        at = sp.end;
    }
    assert_eq!(at, src.len(), "tree does not cover the whole source: {src:?}");
    assert_eq!(p.tree.len, src.len());

    // Nodes nest properly.
    for n in p.tree.iter() {
        assert!(ok(n.span()), "bad node span in {src:?}");
        for c in n.children() {
            let (o, i) = (n.span(), c.span());
            assert!(o.start <= i.start && i.end <= o.end, "child {i:?} outside parent {o:?} in {src:?}");
        }
        if let Node::Import(i) = n {
            for s in &i.specs {
                assert!(ok(s.span) && ok(s.path_span));
                assert_eq!(s.path_span.slice(src), s.path);
            }
        }
    }

    // Tags: payload sits inside the tag after the head.
    for t in p.tree.tags() {
        assert!(ok(t.span) && ok(t.head));
        assert!(t.head.start == t.span.start && t.head.end <= t.span.end);
        if let Some(pl) = t.payload {
            assert!(ok(pl) && !pl.is_empty());
            assert!(pl.start >= t.head.end && pl.end <= t.span.end);
            assert_eq!(pl.slice(src), pl.slice(src).trim());
        }
        // The gap between head and closer: where a cursor sits while the payload is being typed.
        assert!(ok(t.inner) && t.inner.start == t.head.end && t.inner.end <= t.span.end, "inner in {src:?}");
        if let Some(pl) = t.payload {
            assert!(t.inner.start <= pl.start && pl.end <= t.inner.end, "payload outside inner in {src:?}");
        } else {
            assert!(t.inner.slice(src).trim().is_empty(), "empty payload but text in inner, {src:?}");
        }
    }

    // Go regions: ordered, disjoint, in bounds.
    let regions = p.tree.expr_regions();
    for w in regions.windows(2) {
        assert!(w[0].span.end <= w[1].span.start, "overlapping regions in {src:?}");
    }
    assert!(regions.iter().all(|r| ok(r.span)));

    // Diagnostics: in bounds, sorted, convertible to LSP ranges.
    let idx = LineIndex::new(src);
    let mut last = 0;
    for d in &p.diagnostics {
        assert!(ok(d.span), "bad diagnostic span {:?} in {src:?}", d.span);
        assert!(d.span.start >= last, "unsorted diagnostics in {src:?}");
        last = d.span.start;
        let r = d.range(&idx);
        assert!(r.start <= r.end);
        assert_eq!(idx.span(r), d.span, "range roundtrip for {src:?}");
    }
}

struct Rng(u64);

impl Rng {
    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.step() % n as u64) as usize
    }
}

#[test]
fn sample_is_clean() {
    let p = parse(SAMPLE);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    check(SAMPLE);
    assert_eq!(p.tree.expr_regions().len(), 9);
}

#[test]
fn every_prefix_of_the_sample() {
    for (end, _) in SAMPLE.char_indices().chain(std::iter::once((SAMPLE.len(), ' '))) {
        check(&SAMPLE[..end]);
    }
}

#[test]
fn every_suffix_of_the_sample() {
    for (start, _) in SAMPLE.char_indices() {
        check(&SAMPLE[start..]);
    }
}

#[test]
fn every_single_char_deletion_of_the_sample() {
    for (i, c) in SAMPLE.char_indices() {
        let mut s = String::with_capacity(SAMPLE.len());
        s.push_str(&SAMPLE[..i]);
        s.push_str(&SAMPLE[i + c.len_utf8()..]);
        check(&s);
    }
}

#[test]
fn random_fragment_soup() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..3000 {
        let mut s = String::new();
        for _ in 0..rng.below(24) {
            s.push_str(FRAGMENTS[rng.below(FRAGMENTS.len())]);
        }
        check(&s);
        // And a random truncation of it, on a char boundary.
        let mut cut = rng.below(s.len() + 1);
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        check(&s[..cut]);
    }
}

#[test]
fn random_truncation_of_valid_documents() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for _ in 0..500 {
        // A random but balanced document, then cut anywhere.
        let mut s = String::new();
        build(&mut rng, &mut s, 0);
        check(&s);
        let mut cut = rng.below(s.len() + 1);
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        check(&s[..cut]);
    }
}

fn build(rng: &mut Rng, out: &mut String, depth: usize) {
    for _ in 0..1 + rng.below(4) {
        match if depth > 4 { 0 } else { rng.below(5) } {
            0 => out.push_str(["<b>é</b>", "😀 ", "<go= a.B /> ", "<go x := 1 />", "\r\n"][rng.below(5)]),
            1 => {
                out.push_str("<go:if (a > b)/>");
                build(rng, out, depth + 1);
                if rng.below(2) == 0 {
                    out.push_str("<go:elif c/>");
                    build(rng, out, depth + 1);
                }
                if rng.below(2) == 0 {
                    out.push_str("<go:else/>");
                    build(rng, out, depth + 1);
                }
                out.push_str("<go:endif/>");
            }
            2 => {
                out.push_str("<go:for _, v := range xs/>");
                build(rng, out, depth + 1);
                out.push_str("<go:endfor/>");
            }
            3 => {
                out.push_str("<go:switch v/><go:case 1, 2/>");
                build(rng, out, depth + 1);
                out.push_str("<go:default/>");
                build(rng, out, depth + 1);
                out.push_str("<go:endswitch/>");
            }
            _ => out.push_str("<go:import (\"fmt\")/>"),
        }
    }
}

#[test]
fn no_panic_on_pathological_inputs() {
    let long_quote = format!("<go= \"{}", "a".repeat(100_000));
    let many_lt = "<".repeat(100_000);
    let many_tags = "<go:endif/>".repeat(20_000);
    let many_unterminated = "<go:if (".repeat(5_000);
    for s in [long_quote.as_str(), many_lt.as_str(), many_tags.as_str(), many_unterminated.as_str(), "\0", "<go:\u{0}>"]
    {
        let p = parse(s);
        assert_eq!(p.tree.len, s.len());
    }
}
