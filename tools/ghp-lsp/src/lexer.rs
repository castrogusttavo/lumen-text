//! Tolerant lexer: splits a template into text runs and `<go...>` tags.
//!
//! Mirrors the reference scanner (`ghp/src/internal/parser`): a `<` only opens a tag when it is
//! followed by one of the GHP heads; anything else (`<div>`, `<google-map>`, `<go:nope/>`) stays
//! text. A tag ends at the first `>` that is outside `()`, `[]` and quoted strings. Unlike the
//! reference it never fails: broken tags are recovered and reported.

use crate::ast::{Span, Tag, TagKind};
use crate::diagnostics::{Diagnostic, code};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Text(Span),
    Tag(Tag),
}

impl Token {
    pub fn span(&self) -> Span {
        match self {
            Token::Text(s) => *s,
            Token::Tag(t) => t.span,
        }
    }
}

/// Heads in the order the reference tries them.
const HEADS: &[(&str, TagKind)] = &[
    ("go:import", TagKind::Import),
    ("go:if", TagKind::If),
    ("go:else", TagKind::Else),
    ("go:elif", TagKind::Elif),
    ("go:switch", TagKind::Switch),
    ("go:case", TagKind::Case),
    ("go:default", TagKind::Default),
    ("go:for", TagKind::For),
    ("go:endif", TagKind::EndIf),
    ("go:endswitch", TagKind::EndSwitch),
    ("go:endfor", TagKind::EndFor),
    ("go=", TagKind::Echo),
    ("go", TagKind::Statement),
];

fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Which tag `rest` (the text right after a `<`) opens, and the head's length in bytes.
pub fn match_head(rest: &str) -> Option<(TagKind, usize)> {
    for &(open, kind) in HEADS {
        let Some(after) = rest.strip_prefix(open) else { continue };
        let next = after.chars().next();
        let ok = match kind {
            // `go=` ends on a hard delimiter: `<go=name/>` is fine.
            TagKind::Echo => true,
            // Bare `go` must not be the start of `go:...`, `go=` or a longer identifier.
            TagKind::Statement => next.is_none_or(|c| !is_ident_char(c) && c != ':' && c != '='),
            _ => next.is_none_or(|c| !is_ident_char(c)),
        };
        if ok {
            return Some((kind, open.len()));
        }
    }
    None
}

/// Outcome of looking for the end of a tag.
enum Scan {
    /// Offset of the closing `>`.
    Close(usize),
    /// A new GHP tag starts here, so the current one was never closed.
    Stop(usize),
    /// Ran off the end of the input.
    Eof,
}

/// Scanning state shared by all the tags of one input.
///
/// # Linear time
///
/// Every tag is scanned on its own, so input made of thousands of broken tags must not make each
/// scan walk to the end of the file. [`Scanner`] keeps the two facts that make repeated scans
/// cheap: from where on no `*/` exists, and which quote openings are known to run off the end.
struct Scanner {
    /// No `*/` at or after this offset (`usize::MAX`: not known to be absent).
    no_comment_end_from: usize,
    /// `(offset of an opening quote, bracket depth)` states from which a scan reaches the end of
    /// the input without finding a close or a new tag. Any later scan entering one stops there.
    dead: HashSet<(usize, usize)>,
}

impl Scanner {
    fn new() -> Scanner {
        Scanner { no_comment_end_from: usize::MAX, dead: HashSet::new() }
    }

    /// Offset of the `/` closing the `/* */` comment opened at `i`, if it is terminated.
    fn comment_end(&mut self, src: &str, i: usize) -> Option<usize> {
        if i + 2 >= self.no_comment_end_from {
            return None;
        }
        match src[i + 2..].find("*/") {
            Some(p) => Some(i + p + 3),
            None => {
                self.no_comment_end_from = i + 2;
                None
            }
        }
    }

    /// Look for the `>` closing a tag whose payload starts at `start`, ignoring any `>` inside
    /// `()`, `[]`, quotes (`"`, `'`, `` ` ``) and Go comments. `{}` are deliberately not tracked (a
    /// `<go>` block may open a brace that a later tag closes). Outside quotes, the start of another
    /// GHP tag ends the search, so a forgotten `/>` does not swallow the rest of the file.
    ///
    /// A `/* ... */` comment is skipped whole (an unterminated one is plain text). A `// ...` comment
    /// runs to the end of the line, but a `/>` or a new GHP tag inside it still ends the search, so
    /// `<go x := 1 // note />` keeps working and an apostrophe in a comment cannot open a quote.
    fn find_close(&mut self, src: &str, start: usize) -> Scan {
        let b = src.as_bytes();
        let mut depth = 0usize;
        let mut quote = 0u8;
        let mut i = start;
        let mut opened: Vec<(usize, usize)> = Vec::new();
        while i < b.len() {
            let c = b[i];
            if quote != 0 {
                if c == b'\\' && quote != b'`' && i + 1 < b.len() {
                    i += 1;
                } else if c == quote {
                    quote = 0;
                }
            } else {
                match c {
                    b'"' | b'\'' | b'`' => {
                        if self.dead.contains(&(i, depth)) {
                            self.dead.extend(opened);
                            return Scan::Eof;
                        }
                        opened.push((i, depth));
                        quote = c;
                    }
                    b'/' if b.get(i + 1) == Some(&b'*') => {
                        if let Some(end) = self.comment_end(src, i) {
                            i = end; // on the closing `/`; the loop step moves past it
                        }
                    }
                    b'/' if b.get(i + 1) == Some(&b'/') => {
                        let mut j = i + 2;
                        while j < b.len() && b[j] != b'\n' {
                            if b[j] == b'>' && b[j - 1] == b'/' && depth == 0 {
                                return Scan::Close(j);
                            }
                            if b[j] == b'<' && match_head(&src[j + 1..]).is_some() {
                                return Scan::Stop(j);
                            }
                            j += 1;
                        }
                        i = j; // at the newline (or the end): the loop step moves past it
                        continue;
                    }
                    b'(' | b'[' => depth += 1,
                    b')' | b']' => depth = depth.saturating_sub(1),
                    b'>' if depth == 0 => return Scan::Close(i),
                    b'<' if match_head(&src[i + 1..]).is_some() => return Scan::Stop(i),
                    _ => {}
                }
            }
            i += 1;
        }
        self.dead.extend(opened);
        Scan::Eof
    }
}

/// Recovery when a quote never closes: the first `/>` before the next GHP tag.
fn find_close_loose(src: &str, start: usize) -> Option<usize> {
    let b = src.as_bytes();
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'>') => return Some(i + 1),
            b'<' if match_head(&src[i + 1..]).is_some() => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// `src[a..b]` trimmed of surrounding whitespace, as an exact span; `None` when nothing is left.
pub(crate) fn trimmed(src: &str, a: usize, b: usize) -> Option<Span> {
    let s = &src[a..b];
    let t = s.trim_start();
    let start = a + (s.len() - t.len());
    let t = t.trim_end();
    if t.is_empty() { None } else { Some(Span::new(start, start + t.len())) }
}

/// Lex `src`. Never fails; problems come back as diagnostics.
pub fn lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let b = src.as_bytes();
    let mut toks = Vec::new();
    let mut diags = Vec::new();
    let mut pos = 0;
    let mut text_start = 0;
    let mut scanner = Scanner::new();

    while pos < b.len() {
        if b[pos] != b'<' {
            pos += 1;
            continue;
        }
        // `<` is ASCII so `pos + 1` is a char boundary.
        let rest = &src[pos + 1..];
        let Some((kind, head_len)) = match_head(rest) else {
            if let Some(after) = rest.strip_prefix("go:") {
                let n: usize = after.chars().take_while(|&c| is_ident_char(c)).map(char::len_utf8).sum();
                if n > 0 {
                    diags.push(Diagnostic::warning(
                        Span::new(pos, pos + 1 + 3 + n),
                        code::UNKNOWN_TAG,
                        format!("unknown tag `<go:{}`; it is treated as plain text", &after[..n]),
                    ));
                }
            }
            pos += 1;
            continue;
        };

        if text_start < pos {
            toks.push(Token::Text(Span::new(text_start, pos)));
        }
        let (tag, end) = lex_tag(src, &mut scanner, pos, kind, head_len, &mut diags);
        toks.push(Token::Tag(tag));
        pos = end;
        text_start = end;
    }
    if text_start < b.len() {
        toks.push(Token::Text(Span::new(text_start, b.len())));
    }
    (toks, diags)
}

/// Lex the tag starting at `pos` (the `<`). Returns it and the offset to resume at.
fn lex_tag(
    src: &str,
    scanner: &mut Scanner,
    pos: usize,
    kind: TagKind,
    head_len: usize,
    diags: &mut Vec<Diagnostic>,
) -> (Tag, usize) {
    let head_end = pos + 1 + head_len;
    let head = Span::new(pos, head_end);

    let (c, balanced) = match scanner.find_close(src, head_end) {
        Scan::Close(c) => (c, true),
        Scan::Stop(end) => return unterminated(src, pos, end, kind, head, diags),
        // Unbalanced quote: fall back to the first `/>` so one typo does not swallow the file.
        Scan::Eof => match find_close_loose(src, head_end) {
            Some(c) => (c, false),
            None => return unterminated(src, pos, src.len(), kind, head, diags),
        },
    };

    let raw = src[head_end..c].trim_end();
    let slash = raw.ends_with('/');
    let payload_end = if slash { head_end + raw.len() - 1 } else { c };
    let tag = Tag {
        kind,
        span: Span::new(pos, c + 1),
        head,
        payload: trimmed(src, head_end, payload_end),
        closed: slash && balanced,
        inner: Span::new(head_end, payload_end),
    };

    if !balanced {
        diags.push(Diagnostic::error(tag.span, code::UNBALANCED_TAG, format!("{} has an unclosed quote", kind.name())));
    } else if !slash {
        diags.push(Diagnostic::error(
            tag.span,
            code::TAG_NOT_CLOSED,
            format!("{} must be closed with `/>`", kind.name()),
        ));
    }
    (tag, c + 1)
}

/// A tag that stops at `end` (end of input, or where the next tag starts) without its `/>`. A
/// trailing `/` is the start of the `/>` still being typed, not part of the payload.
fn unterminated(
    src: &str,
    pos: usize,
    end: usize,
    kind: TagKind,
    head: Span,
    diags: &mut Vec<Diagnostic>,
) -> (Tag, usize) {
    let head_end = head.end;
    let raw = src[head_end..end].trim_end();
    let payload_end = if raw.ends_with('/') { head_end + raw.len() - 1 } else { end };
    let tag = Tag {
        kind,
        span: Span::new(pos, end),
        head,
        payload: trimmed(src, head_end, payload_end),
        closed: false,
        inner: Span::new(head_end, payload_end),
    };
    diags.push(Diagnostic::error(
        tag.span,
        code::UNTERMINATED_TAG,
        format!("unterminated {}: expected `/>`", kind.name()),
    ));
    (tag, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<String> {
        lex(src)
            .0
            .iter()
            .map(|t| match t {
                Token::Text(s) => format!("T:{}", s.slice(src)),
                Token::Tag(g) => format!("{:?}:{}", g.kind, g.payload_text(src)),
            })
            .collect()
    }

    #[test]
    fn heads() {
        let cases: &[(&str, &[&str])] = &[
            ("<go x/>", &["Statement:x"]),
            ("<go\n  expr\n/>", &["Statement:expr"]),
            ("<go= name />", &["Echo:name"]),
            ("<go=name/>", &["Echo:name"]),
            ("<go:if (a > b)/>", &["If:(a > b)"]),
            ("<go:elif x/><go:else/><go:endif/>", &["Elif:x", "Else:", "EndIf:"]),
            (
                "<go:switch v/><go:case 1, 2/><go:default/><go:endswitch/>",
                &["Switch:v", "Case:1, 2", "Default:", "EndSwitch:"],
            ),
            ("<go:for i := range []int{1, 2}/>x<go:endfor/>", &["For:i := range []int{1, 2}", "T:x", "EndFor:"]),
            ("<go:import (\"fmt\")/>", &["Import:(\"fmt\")"]),
            ("<go:if s == \"/>\"/>", &["If:s == \"/>\""]),
            ("<go:if s == '>'/>", &["If:s == '>'"]),
            ("<go:if s == `a>b`/>", &["If:s == `a>b`"]),
            ("<go:if f(\"\\\")\")/>", &["If:f(\"\\\")\")"]),
            ("<go= `a/>b` />", &["Echo:`a/>b`"]),
            ("<go= f('\"', \"/>\") />", &["Echo:f('\"', \"/>\")"]),
            ("<go= \"it's\" + `\"` />", &["Echo:\"it's\" + `\"`"]),
            ("<go x := 1 /* don't > */ />", &["Statement:x := 1 /* don't > */"]),
            ("<go x := 1 /* \" */ y := 2 />", &["Statement:x := 1 /* \" */ y := 2"]),
            ("<go x := 1 // it's a note\n y := 2\n/>", &["Statement:x := 1 // it's a note\n y := 2"]),
            ("<go x := 1 // note />", &["Statement:x := 1 // note"]),
            ("<go= \"http://x\" />", &["Echo:\"http://x\""]),
        ];
        for (src, want) in cases {
            assert_eq!(kinds(src), *want, "src {src:?}");
        }
    }

    #[test]
    fn not_tags() {
        let cases = [
            "<div>",
            "<google-map>",
            "<gopher/>",
            "<go:iffy/>",
            "<go:nope/>",
            "<go_x/>",
            "<goé/>",
            "a < b",
            "<",
            "<g",
            "<go:",
        ];
        for src in cases {
            let (toks, _) = lex(src);
            assert_eq!(toks, vec![Token::Text(Span::new(0, src.len()))], "src {src:?}");
        }
    }

    #[test]
    fn unknown_go_tag_warns() {
        let (_, d) = lex("a <go:nope/> b");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, code::UNKNOWN_TAG);
        assert_eq!(d[0].span, Span::new(2, 10));
        // Plain `<go:` with nothing after it is just text.
        assert!(lex("<go:").1.is_empty());
    }

    #[test]
    fn exact_spans() {
        let src = "é<go:if  x == 1 />😀";
        let (toks, d) = lex(src);
        assert!(d.is_empty());
        assert_eq!(toks.len(), 3);
        let Token::Tag(t) = toks[1] else { panic!() };
        assert_eq!(t.span.slice(src), "<go:if  x == 1 />");
        assert_eq!(t.head.slice(src), "<go:if");
        assert_eq!(t.payload.unwrap().slice(src), "x == 1");
        assert!(t.closed);
        assert_eq!(toks[2], Token::Text(Span::new(t.span.end, src.len())));
    }

    #[test]
    fn missing_slash() {
        let src = "<go:if x>body";
        let (toks, d) = lex(src);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, code::TAG_NOT_CLOSED);
        let Token::Tag(t) = toks[0] else { panic!() };
        assert!(!t.closed);
        assert_eq!(t.payload.unwrap().slice(src), "x");
        assert_eq!(toks[1], Token::Text(Span::new(9, 13)));
    }

    #[test]
    fn unterminated_at_eof() {
        let cases = [
            ("<go:if x ==", "x =="),
            ("<go:if x /", "x"),
            ("<go:if (a > b", "(a > b"),
            ("<go= ", ""),
            ("<go:if \"abc", "\"abc"),
        ];
        for (src, payload) in cases {
            let (toks, d) = lex(src);
            assert_eq!(toks.len(), 1, "src {src:?}");
            let Token::Tag(t) = toks[0] else { panic!() };
            assert_eq!(t.payload_text(src), payload, "src {src:?}");
            assert_eq!(t.span, Span::new(0, src.len()));
            assert!(!t.closed);
            assert_eq!(d.len(), 1, "src {src:?}: {d:?}");
            assert_eq!(d[0].code, code::UNTERMINATED_TAG);
        }
    }

    #[test]
    fn unterminated_stops_at_next_tag() {
        let src = "<go:if (a > b\n<p>x</p>\n<go:endif/>";
        let (toks, d) = lex(src);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, code::UNTERMINATED_TAG);
        assert_eq!(toks.len(), 2);
        let Token::Tag(t) = toks[0] else { panic!() };
        assert_eq!(t.kind, TagKind::If);
        assert_eq!(t.payload_text(src), "(a > b\n<p>x</p>");
        let Token::Tag(e) = toks[1] else { panic!() };
        assert_eq!(e.kind, TagKind::EndIf);
        assert!(e.closed);
    }

    #[test]
    fn unbalanced_quote_recovers_at_next_close() {
        let src = "<go:if s == \"abc/>\n<p>hi</p>";
        let (toks, d) = lex(src);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, code::UNBALANCED_TAG);
        let Token::Tag(t) = toks[0] else { panic!() };
        assert_eq!(t.span.slice(src), "<go:if s == \"abc/>");
        assert_eq!(toks.len(), 2);
    }

    #[test]
    fn crlf_payload_trim() {
        let src = "<go:if\r\n  x\r\n/>";
        let (toks, d) = lex(src);
        assert!(d.is_empty());
        let Token::Tag(t) = toks[0] else { panic!() };
        assert_eq!(t.payload_text(src), "x");
    }

    #[test]
    fn inner_is_the_gap_between_head_and_closer() {
        let cases =
            [("<go= />", 4, 5), ("<go=/>", 4, 4), ("<go:if  x  />", 6, 11), ("<go= ", 4, 5), ("<go=\n/>", 4, 5)];
        for (src, a, b) in cases {
            let Token::Tag(t) = lex(src).0[0] else { panic!("{src:?}") };
            assert_eq!((t.inner.start, t.inner.end), (a, b), "{src:?}");
        }
    }

    /// The scan as it was before [`Scanner`] existed: no memory between tags.
    fn find_close_naive(src: &str, start: usize) -> Scan {
        let b = src.as_bytes();
        let mut depth = 0usize;
        let mut quote = 0u8;
        let mut i = start;
        while i < b.len() {
            let c = b[i];
            if quote != 0 {
                if c == b'\\' && quote != b'`' && i + 1 < b.len() {
                    i += 1;
                } else if c == quote {
                    quote = 0;
                }
            } else {
                match c {
                    b'"' | b'\'' | b'`' => quote = c,
                    b'/' if b.get(i + 1) == Some(&b'*') => {
                        if let Some(p) = src[i + 2..].find("*/") {
                            i += p + 3;
                        }
                    }
                    b'/' if b.get(i + 1) == Some(&b'/') => {
                        let mut j = i + 2;
                        while j < b.len() && b[j] != b'\n' {
                            if b[j] == b'>' && b[j - 1] == b'/' && depth == 0 {
                                return Scan::Close(j);
                            }
                            if b[j] == b'<' && match_head(&src[j + 1..]).is_some() {
                                return Scan::Stop(j);
                            }
                            j += 1;
                        }
                        i = j;
                        continue;
                    }
                    b'(' | b'[' => depth += 1,
                    b')' | b']' => depth = depth.saturating_sub(1),
                    b'>' if depth == 0 => return Scan::Close(i),
                    b'<' if match_head(&src[i + 1..]).is_some() => return Scan::Stop(i),
                    _ => {}
                }
            }
            i += 1;
        }
        Scan::Eof
    }

    fn same(a: &Scan, b: &Scan) -> bool {
        match (a, b) {
            (Scan::Close(x), Scan::Close(y)) | (Scan::Stop(x), Scan::Stop(y)) => x == y,
            (Scan::Eof, Scan::Eof) => true,
            _ => false,
        }
    }

    /// The memoizing scanner answers exactly like the naive one, from every start, in any order
    /// (the memory is facts about the text, not about where a scan began).
    #[test]
    fn scanner_matches_the_naive_scan() {
        const PIECES: &[&str] = &[
            "<go= ", "<go:if ", "<go ", "\"", "'", "`", "(", ")", "[", "]", ">", "/>", "/*", "*/", "//", "\n", "\\",
            "a", " ", "é", "<p>",
        ];
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n as u64) as usize
        };
        for _ in 0..300 {
            let mut src = String::new();
            for _ in 0..next(40) + 1 {
                src.push_str(PIECES[next(PIECES.len())]);
            }
            let mut scanner = Scanner::new();
            let mut starts: Vec<usize> = (0..=src.len()).filter(|&i| src.is_char_boundary(i)).collect();
            for i in (1..starts.len()).rev() {
                starts.swap(i, next(i + 1));
            }
            for st in starts {
                let (a, b) = (scanner.find_close(&src, st), find_close_naive(&src, st));
                assert!(same(&a, &b), "start {st} of {src:?}");
            }
        }
    }
}
