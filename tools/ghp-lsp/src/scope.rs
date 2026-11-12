//! Scope analysis of the Go text of a page, without gopls.
//!
//! This is the fallback behind [`crate::router::Router`] when gopls is missing or has given up:
//! it knows which local names exist where, so completion, hover, go-to-definition and references
//! of *local identifiers* keep working, and it flags unused imports. It cannot know types, so
//! member completion (`user.`) is left to gopls.
//!
//! What is understood:
//!
//! * `<go ... />` statements: `a, b := expr`, `var x T`, `var x = 1`, `var ( ... )`, `const`.
//!   A name is visible from the end of its tag to the end of the enclosing block (the body of
//!   the `if` branch / `for` / `case` it sits in, or the rest of the page).
//! * Headers: `<go:for i, v := range xs/>`, `<go:for i := 0; i < n; i++/>`,
//!   `<go:if x := f(); x > 0/>`, `<go:switch v := x.(type)/>` declare names visible in the whole
//!   construct.
//! * Imports: the unaliased and aliased `<go:import/>` specs, plus the packages the generated
//!   handler always imports (`net/http`, `fmt`, `html`, `io`) and its parameters `w` and `r`.
//!
//! The tokenizer is deliberately small: identifiers, literals (strings, raw strings, runes,
//! numbers), punctuation and newlines; comments are dropped.

use crate::ast::{Branch, Node, Span, Tag, TagKind, Tree};
use crate::diagnostics::Diagnostic;
use crate::gopls::adapter::live_tags;

/// Diagnostic code of an import that no expression of the page uses.
pub const UNUSED_IMPORT: &str = "unused-import";

/// Packages the generated handler imports by itself and always uses.
const MANAGED: &[(&str, &str)] = &[("http", "net/http"), ("fmt", "fmt"), ("html", "html"), ("io", "io")];

const KEYWORDS: &[&str] = &[
    "if",
    "else",
    "for",
    "switch",
    "select",
    "func",
    "go",
    "defer",
    "return",
    "type",
    "import",
    "case",
    "default",
    "goto",
    "break",
    "continue",
    "fallthrough",
    "range",
    "package",
    "struct",
    "interface",
    "map",
    "chan",
];

// ---------------------------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokKind {
    Ident,
    /// String, raw string, rune or number.
    Literal,
    Punct,
    Newline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tok {
    pub kind: TokKind,
    /// Absolute byte span in the page.
    pub span: Span,
}

/// Tokenize the Go text `src[region]`. Spans are absolute offsets into `src`. Never panics, also
/// on unterminated strings or comments.
pub fn tokenize(src: &str, region: Span) -> Vec<Tok> {
    let text = &src[region.start..region.end];
    let b = text.as_bytes();
    let base = region.start;
    let mut out = Vec::new();
    let tok = |kind, a: usize, z: usize| Tok { kind, span: Span::new(base + a, base + z.min(b.len()).max(a)) };
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => {
                out.push(tok(TokKind::Newline, i, i + 1));
                i += 1;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = match text[i + 2..].find("*/") {
                    Some(p) => i + p + 4,
                    None => b.len(),
                };
            }
            b'"' | b'\'' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != c && b[i] != b'\n' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i < b.len() && b[i] == c {
                    i += 1;
                }
                out.push(tok(TokKind::Literal, start, i));
            }
            b'`' => {
                let end = text[i + 1..].find('`').map_or(b.len(), |p| i + p + 2);
                out.push(tok(TokKind::Literal, i, end));
                i = end;
            }
            b':' if b.get(i + 1) == Some(&b'=') => {
                out.push(tok(TokKind::Punct, i, i + 2));
                i += 2;
            }
            _ if c.is_ascii_digit() => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                    i += 1;
                }
                out.push(tok(TokKind::Literal, start, i));
            }
            _ => {
                let ch = text[i..].chars().next().unwrap_or(' ');
                if ch == '_' || ch.is_alphabetic() {
                    let start = i;
                    while let Some(ch) = text[i..].chars().next()
                        && (ch == '_' || ch.is_alphanumeric())
                    {
                        i += ch.len_utf8();
                    }
                    out.push(tok(TokKind::Ident, start, i));
                } else {
                    out.push(tok(TokKind::Punct, i, i + ch.len_utf8()));
                    i += ch.len_utf8();
                }
            }
        }
    }
    out
}

fn text<'a>(src: &'a str, t: &Tok) -> &'a str {
    t.span.slice(src)
}

fn is_punct(src: &str, t: &Tok, p: &str) -> bool {
    t.kind == TokKind::Punct && text(src, t) == p
}

fn depth_delta(src: &str, t: &Tok) -> isize {
    if t.kind != TokKind::Punct {
        return 0;
    }
    match text(src, t) {
        "(" | "[" | "{" => 1,
        ")" | "]" | "}" => -1,
        _ => 0,
    }
}

/// Index of the first top-level (bracket depth 0) token satisfying `pred`.
fn top_level(src: &str, toks: &[Tok], pred: impl Fn(&Tok) -> bool) -> Option<usize> {
    let mut depth = 0isize;
    for (i, t) in toks.iter().enumerate() {
        if depth <= 0 && pred(t) {
            return Some(i);
        }
        depth += depth_delta(src, t);
    }
    None
}

/// Statements: split at top-level `;` and newlines.
fn split_statements<'t>(src: &str, toks: &'t [Tok]) -> Vec<&'t [Tok]> {
    let mut out = Vec::new();
    let mut rest = toks;
    while !rest.is_empty() {
        let cut = top_level(src, rest, |t| t.kind == TokKind::Newline || is_punct(src, t, ";"));
        let (head, tail) = match cut {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        if !head.is_empty() {
            out.push(head);
        }
        rest = tail;
    }
    out
}

/// Collapse whitespace and cap the length, for hover text.
fn brief(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 120 {
        let cut: String = one.chars().take(117).collect();
        format!("{cut}...")
    } else {
        one
    }
}

// ---------------------------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Variable,
    Constant,
    Import,
}

/// A name and where it is visible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// The identifier at its declaration; empty for names that have no place in the page
    /// (`w`, `r`, the managed imports).
    pub decl: Span,
    /// Offsets (`start <= offset <= end`) at which the name resolves.
    pub visible: Span,
    /// One-line description for hover.
    pub detail: String,
}

/// The names of a page.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pub symbols: Vec<Symbol>,
    /// Every token of every Go region, in source order.
    pub tokens: Vec<Tok>,
    imports: Vec<ImportInfo>,
}

#[derive(Clone, Debug)]
struct ImportInfo {
    name: String,
    path: String,
    span: Span,
}

/// Name a package is referred to by when imported without alias: the last path element, minus a
/// major-version suffix (`.../v2`) and a `go-` prefix.
pub fn import_name(path: &str) -> String {
    let mut parts = path.rsplit('/').filter(|p| !p.is_empty());
    let mut last = parts.next().unwrap_or(path);
    if last.len() > 1 && last.starts_with('v') && last[1..].bytes().all(|b| b.is_ascii_digit()) {
        last = parts.next().unwrap_or(last);
    }
    // `gopkg.in/yaml.v3`: the version rides on the last element.
    if let Some((head, ver)) = last.rsplit_once(".v")
        && !ver.is_empty()
        && ver.bytes().all(|b| b.is_ascii_digit())
    {
        last = head;
    }
    let last = last.strip_prefix("go-").unwrap_or(last);
    last.replace(['-', '.'], "_")
}

impl Scope {
    /// Analyze a parsed page.
    pub fn analyze(src: &str, tree: &Tree) -> Scope {
        let mut scope = Scope::default();

        // Always-available names of the generated handler.
        let all = Span::new(0, src.len());
        for (name, detail) in [("w", "w http.ResponseWriter"), ("r", "r *http.Request")] {
            scope.symbols.push(Symbol {
                name: name.into(),
                kind: SymbolKind::Variable,
                decl: Span::default(),
                visible: all,
                detail: detail.into(),
            });
        }

        // Imports: user specs first so an alias or a re-import wins over the managed names.
        for spec in tree.imports() {
            let name = spec.alias.clone().unwrap_or_else(|| import_name(&spec.path));
            let detail = match &spec.alias {
                Some(a) => format!("import {a} \"{}\"", spec.path),
                None => format!("import \"{}\"", spec.path),
            };
            if name != "_" && name != "." {
                scope.symbols.push(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Import,
                    decl: spec.span,
                    visible: all,
                    detail,
                });
            }
            scope.imports.push(ImportInfo { name, path: spec.path.clone(), span: spec.span });
        }
        for (name, path) in MANAGED {
            if !scope.symbols.iter().any(|s| s.name == *name) {
                scope.symbols.push(Symbol {
                    name: (*name).into(),
                    kind: SymbolKind::Import,
                    decl: Span::default(),
                    visible: all,
                    detail: format!("import \"{path}\" (added by the GHP compiler)"),
                });
            }
        }

        // Tokens of every Go region.
        for t in live_tags(tree).into_iter().filter(|t| t.kind != TagKind::Import) {
            if let Some(p) = t.payload {
                scope.tokens.extend(tokenize(src, p));
            }
        }
        scope.tokens.sort_by_key(|t| t.span.start);

        walk(src, &tree.nodes, src.len(), &mut scope.symbols);
        scope
    }

    /// The symbol `name` refers to at `offset`: the most recently declared visible one.
    pub fn resolve(&self, name: &str, offset: usize) -> Option<&Symbol> {
        self.symbols
            .iter()
            .filter(|s| s.name == name && s.visible.touches(offset))
            .max_by_key(|s| (s.visible.start, s.decl.start))
    }

    /// Names visible at `offset`, innermost declaration winning over shadowed ones.
    pub fn visible_at(&self, offset: usize) -> Vec<&Symbol> {
        let mut out: Vec<&Symbol> = Vec::new();
        for s in self.symbols.iter().filter(|s| s.visible.touches(offset)) {
            match out.iter().position(|o| o.name == s.name) {
                Some(i) if (s.visible.start, s.decl.start) > (out[i].visible.start, out[i].decl.start) => out[i] = s,
                Some(_) => {}
                None => out.push(s),
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// The identifier token at `offset` (a cursor right after it counts) and whether it is a
    /// member selector (`x.name`), which scope analysis cannot resolve.
    pub fn ident_at(&self, src: &str, offset: usize) -> Option<(Tok, bool)> {
        let i = self.tokens.iter().position(|t| t.kind == TokKind::Ident && t.span.touches(offset))?;
        let after_dot = i > 0
            && is_punct(src, &self.tokens[i - 1], ".")
            && self.tokens[i - 1].span.end == self.tokens[i].span.start;
        Some((self.tokens[i], after_dot))
    }

    /// What is being completed at `offset` inside the Go region `site`: the prefix typed so far
    /// (its span) and whether it follows a `.` (member completion, which needs gopls).
    pub fn completion_prefix(&self, src: &str, offset: usize, site: Span) -> (Span, bool) {
        let inside: Vec<&Tok> =
            self.tokens.iter().filter(|t| site.touches(t.span.start) && t.span.start < offset).collect();
        let (word, before) = match inside.last() {
            Some(t) if t.kind == TokKind::Ident && t.span.end >= offset => (t.span, inside.len().checked_sub(2)),
            _ => (Span::new(offset, offset), inside.len().checked_sub(1)),
        };
        let member = before.is_some_and(|i| is_punct(src, inside[i], "."));
        (Span::new(word.start, offset.min(word.end).max(word.start)), member)
    }

    /// Every occurrence of the identifier that resolves to `target`, declaration included.
    pub fn references(&self, src: &str, target: &Symbol) -> Vec<Span> {
        let mut out = Vec::new();
        if !target.decl.is_empty() {
            out.push(target.decl);
        }
        for (i, t) in self.tokens.iter().enumerate() {
            if t.kind != TokKind::Ident || text(src, t) != target.name {
                continue;
            }
            // Declarations are not uses: they would otherwise resolve to an outer symbol.
            if self.symbols.iter().any(|s| s.decl == t.span) {
                continue;
            }
            let member =
                i > 0 && is_punct(src, &self.tokens[i - 1], ".") && self.tokens[i - 1].span.end == t.span.start;
            if member {
                continue;
            }
            if self.resolve(&target.name, t.span.start).is_some_and(|s| s == target) {
                out.push(t.span);
            }
        }
        out.sort_by_key(|s| s.start);
        out.dedup();
        out
    }

    /// `"path" imported and not used` warnings. Packages the compiler imports itself are never
    /// reported, nor are blank (`_`) and dot imports.
    pub fn unused_imports(&self, src: &str) -> Vec<Diagnostic> {
        let mut used = std::collections::HashSet::new();
        for (i, t) in self.tokens.iter().enumerate() {
            if t.kind != TokKind::Ident {
                continue;
            }
            let selects = self.tokens.get(i + 1).is_some_and(|n| is_punct(src, n, ".") && n.span.start == t.span.end);
            let member =
                i > 0 && is_punct(src, &self.tokens[i - 1], ".") && self.tokens[i - 1].span.end == t.span.start;
            if selects && !member {
                used.insert(text(src, t));
            }
        }
        self.imports
            .iter()
            .filter(|imp| imp.name != "_" && imp.name != "." && !MANAGED.iter().any(|(_, p)| *p == imp.path))
            .filter(|imp| !used.contains(imp.name.as_str()))
            .map(|imp| Diagnostic::warning(imp.span, UNUSED_IMPORT, format!("\"{}\" imported and not used", imp.path)))
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Declarations
// ---------------------------------------------------------------------------------------------

fn vis(start: usize, end: usize) -> Span {
    Span::new(start, end.max(start))
}

/// Walk `nodes`; names declared by statements are visible until `block_end`.
fn walk(src: &str, nodes: &[Node], block_end: usize, out: &mut Vec<Symbol>) {
    for n in nodes {
        match n {
            Node::Statement(t) => {
                if let Some(p) = t.payload {
                    let toks = tokenize(src, p);
                    for st in split_statements(src, &toks) {
                        declare(src, st, vis(t.span.end, block_end), out);
                    }
                }
            }
            Node::If(i) => {
                let end = i.span.end;
                let body_end = i.end.map_or(end, |t| t.span.start);
                header(src, &i.open, true, vis(i.open.span.end, body_end), out);
                walk(src, &i.then, branch_end(&i.branches, 0, body_end), out);
                for (k, b) in i.branches.iter().enumerate() {
                    if b.tag.kind == TagKind::Elif {
                        header(src, &b.tag, true, vis(b.tag.span.end, body_end), out);
                    }
                    walk(src, &b.body, branch_end(&i.branches, k + 1, body_end), out);
                }
            }
            Node::Switch(s) => {
                let end = s.span.end;
                let body_end = s.end.map_or(end, |t| t.span.start);
                header(src, &s.open, false, vis(s.open.span.end, body_end), out);
                walk(src, &s.before, branch_end(&s.cases, 0, body_end), out);
                for (k, b) in s.cases.iter().enumerate() {
                    walk(src, &b.body, branch_end(&s.cases, k + 1, body_end), out);
                }
            }
            Node::For(f) => {
                let end = f.span.end;
                let body_end = f.end.map_or(end, |t| t.span.start);
                header(src, &f.open, false, vis(f.open.span.end, body_end), out);
                walk(src, &f.body, body_end, out);
            }
            Node::Text(_) | Node::Import(_) | Node::Echo(_) | Node::Stray(_) => {}
        }
    }
}

/// Where the body before branch `k` ends: the start of that branch's tag, or the construct's end.
fn branch_end(branches: &[Branch], k: usize, body_end: usize) -> usize {
    branches.get(k).map_or(body_end, |b| b.tag.span.start)
}

/// Declarations of a construct's header. `need_init`: a `:=` only declares when an init statement
/// is separated by `;` (`if x := f(); x > 0`), as `if a := b` alone is not valid Go.
fn header(src: &str, tag: &Tag, need_init: bool, visible: Span, out: &mut Vec<Symbol>) {
    let Some(p) = tag.payload else { return };
    let toks: Vec<Tok> = tokenize(src, p).into_iter().filter(|t| t.kind != TokKind::Newline).collect();
    let semi = top_level(src, &toks, |t| is_punct(src, t, ";"));
    let init = match (semi, need_init) {
        (Some(i), _) => &toks[..i],
        (None, false) => &toks[..],
        (None, true) => return,
    };
    declare(src, init, visible, out);
}

/// Names declared by one statement.
fn declare(src: &str, st: &[Tok], visible: Span, out: &mut Vec<Symbol>) {
    let Some(first) = st.first() else { return };
    let head = text(src, first);
    if first.kind == TokKind::Ident && (head == "var" || head == "const") {
        let kind = if head == "var" { SymbolKind::Variable } else { SymbolKind::Constant };
        let rest = &st[1..];
        let grouped =
            rest.first().is_some_and(|t| is_punct(src, t, "(")) && rest.last().is_some_and(|t| is_punct(src, t, ")"));
        if grouped && rest.len() >= 2 {
            for spec in split_statements(src, &rest[1..rest.len() - 1]) {
                declare_spec(src, spec, head, kind, visible, out);
            }
        } else {
            declare_spec(src, rest, head, kind, visible, out);
        }
        return;
    }
    if first.kind == TokKind::Ident && KEYWORDS.contains(&head) {
        return;
    }
    let Some(at) = top_level(src, st, |t| is_punct(src, t, ":=")) else { return };
    let names = name_list(src, &st[..at]);
    let detail = brief(&src[first.span.start..st[st.len() - 1].span.end]);
    for n in names {
        out.push(Symbol {
            name: text(src, &n).to_string(),
            kind: SymbolKind::Variable,
            decl: n.span,
            visible,
            detail: detail.clone(),
        });
    }
}

/// `a, b, c` (and nothing else) as identifier tokens, blanks removed; empty when it is not such a list.
fn name_list(src: &str, toks: &[Tok]) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut expect_ident = true;
    for t in toks.iter().filter(|t| t.kind != TokKind::Newline) {
        if expect_ident {
            if t.kind != TokKind::Ident {
                return Vec::new();
            }
            if text(src, t) != "_" {
                out.push(*t);
            }
        } else if !is_punct(src, t, ",") {
            return Vec::new();
        }
        expect_ident = !expect_ident;
    }
    if expect_ident && !toks.is_empty() { Vec::new() } else { out }
}

/// `a, b T` / `a = 1` / `a, b = 1, 2` after `var` / `const`.
fn declare_spec(src: &str, spec: &[Tok], word: &str, kind: SymbolKind, visible: Span, out: &mut Vec<Symbol>) {
    let (Some(first), Some(last)) = (spec.first(), spec.last()) else { return };
    let mut names = Vec::new();
    let mut i = 0;
    while let Some(t) = spec.get(i) {
        if t.kind != TokKind::Ident {
            break;
        }
        names.push(*t);
        if spec.get(i + 1).is_some_and(|c| is_punct(src, c, ",")) {
            i += 2;
        } else {
            break;
        }
    }
    let detail = brief(&format!("{word} {}", &src[first.span.start..last.span.end]));
    for n in names {
        if text(src, &n) != "_" {
            out.push(Symbol { name: text(src, &n).to_string(), kind, decl: n.span, visible, detail: detail.clone() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn analyze(src: &str) -> Scope {
        Scope::analyze(src, &parse(src).tree)
    }

    fn at(src: &str, needle: &str) -> usize {
        src.find(needle).unwrap_or_else(|| panic!("{needle:?} not in {src:?}"))
    }

    fn names(scope: &Scope, src: &str, needle: &str) -> Vec<String> {
        let off = at(src, needle);
        scope
            .visible_at(off)
            .iter()
            .filter(|s| s.decl.start != 0 || s.kind == SymbolKind::Variable)
            .map(|s| s.name.clone())
            .collect()
    }

    #[test]
    fn tokenizer_skips_comments_and_keeps_strings_whole() {
        let src = "a := \"x // y\" /* c */ + `r\naw` // end\nb";
        let toks = tokenize(src, Span::new(0, src.len()));
        let kinds: Vec<_> = toks.iter().map(|t| (t.kind, text(src, t))).collect();
        assert_eq!(
            kinds,
            vec![
                (TokKind::Ident, "a"),
                (TokKind::Punct, ":="),
                (TokKind::Literal, "\"x // y\""),
                (TokKind::Punct, "+"),
                (TokKind::Literal, "`r\naw`"),
                (TokKind::Newline, "\n"),
                (TokKind::Ident, "b"),
            ]
        );
    }

    #[test]
    fn tokenizer_survives_garbage() {
        for src in ["\"unterminated", "`raw", "/* open", "'\\", "é😀 := 1", "x := '\\''", "\\"] {
            let toks = tokenize(src, Span::new(0, src.len()));
            for t in toks {
                assert!(
                    t.span.end <= src.len() && src.is_char_boundary(t.span.start) && src.is_char_boundary(t.span.end)
                );
            }
        }
    }

    #[test]
    fn statement_declarations() {
        let src = "<go\n  user := models.User{Name: \"Ada\"}\n  a, b := 1, 2\n  _, c := f()\n  var d, e int\n  const k = 3\n/><go= user.Name />";
        let s = analyze(src);
        let seen = names(&s, src, "<go= user");
        for n in ["user", "a", "b", "c", "d", "e", "k", "w", "r"] {
            assert!(seen.contains(&n.to_string()), "{n} missing from {seen:?}");
        }
        assert!(!seen.contains(&"_".to_string()));
        let user = s.resolve("user", at(src, "<go= user")).unwrap();
        assert_eq!(user.detail, "user := models.User{Name: \"Ada\"}");
        assert_eq!(user.decl.slice(src), "user");
        assert_eq!(s.resolve("k", at(src, "<go= user")).unwrap().kind, SymbolKind::Constant);
    }

    #[test]
    fn var_block() {
        let src = "<go var (\n  a int\n  b, c = 1, 2\n) /><go= a />";
        let s = analyze(src);
        let seen = names(&s, src, "<go= a");
        for n in ["a", "b", "c"] {
            assert!(seen.contains(&n.to_string()), "{n} missing from {seen:?}");
        }
    }

    #[test]
    fn a_name_is_not_visible_before_or_inside_its_own_declaration() {
        let src = "<go= early /><go x := 1 /><go= x />";
        let s = analyze(src);
        assert!(s.resolve("x", at(src, "<go= early")).is_none());
        assert!(s.resolve("x", at(src, "x := 1")).is_none());
        assert!(s.resolve("x", at(src, "<go= x")).is_some());
    }

    #[test]
    fn for_header_names_live_in_the_loop_only() {
        let src = "<go xs := []int{1}/><go:for i, v := range xs/><go= v /><go:endfor/><go= after />";
        let s = analyze(src);
        assert_eq!(s.resolve("v", at(src, "<go= v")).unwrap().detail, "i, v := range xs");
        assert!(s.resolve("i", at(src, "<go= v")).is_some());
        assert!(s.resolve("v", at(src, "<go= after")).is_none());
        assert!(s.resolve("xs", at(src, "<go= after")).is_some());
    }

    #[test]
    fn three_clause_for_and_if_init() {
        let src = "<go:for n := 0; n < 3; n++/><go= n /><go:endfor/>\
                   <go:if ok := f(); ok/><go= ok /><go:else/><go= ok /><go:endif/><go= ok />";
        let s = analyze(src);
        assert!(s.resolve("n", at(src, "<go= n")).is_some());
        let first_ok = at(src, "<go= ok");
        assert!(s.resolve("ok", first_ok).is_some());
        // Visible in the else branch too, gone after the endif.
        let else_ok = first_ok + src[first_ok + 1..].find("<go= ok").unwrap() + 1;
        assert!(s.resolve("ok", else_ok).is_some());
        let last = src.rfind("<go= ok").unwrap();
        assert!(s.resolve("ok", last).is_none());
    }

    #[test]
    fn block_scoping_and_shadowing() {
        let src = "<go x := 1 /><go:if c/><go x := 2 /><go= x /><go:endif/><go= x />";
        let s = analyze(src);
        let inner = src.find("<go= x").unwrap();
        let outer = src.rfind("<go= x").unwrap();
        assert_eq!(s.resolve("x", inner).unwrap().detail, "x := 2");
        assert_eq!(s.resolve("x", outer).unwrap().detail, "x := 1");
        assert_eq!(s.visible_at(inner).iter().filter(|s| s.name == "x").count(), 1);
    }

    #[test]
    fn case_bodies_are_separate_scopes() {
        let src = "<go:switch v/><go:case 1/><go a := 1 /><go= a /><go:case 2/><go= a /><go:endswitch/>";
        let s = analyze(src);
        assert!(s.resolve("a", src.find("<go= a").unwrap()).is_some());
        assert!(s.resolve("a", src.rfind("<go= a").unwrap()).is_none());
    }

    #[test]
    fn comments_and_keywords_do_not_declare() {
        let src = "<go // c := 1\n  if x := 1; x != 0 { y := 2 }\n  z := 3\n/><go= z />";
        let s = analyze(src);
        let seen = names(&s, src, "<go= z");
        assert!(seen.contains(&"z".to_string()));
        for n in ["c", "x", "y"] {
            assert!(!seen.contains(&n.to_string()), "{n} must not be declared: {seen:?}");
        }
    }

    #[test]
    fn imports_are_symbols_with_alias_and_managed_defaults() {
        let src = "<go:import (\"example.com/lib/v2\"\n  s \"strings\"\n  _ \"embed\")/><go= s.ToUpper(lib.X) />";
        let s = analyze(src);
        let off = at(src, "<go= s");
        assert_eq!(s.resolve("s", off).unwrap().detail, "import s \"strings\"");
        assert_eq!(s.resolve("lib", off).unwrap().detail, "import \"example.com/lib/v2\"");
        assert!(s.resolve("fmt", off).unwrap().detail.contains("added by the GHP compiler"));
        assert!(s.resolve("_", off).is_none());
    }

    #[test]
    fn import_names() {
        assert_eq!(import_name("fmt"), "fmt");
        assert_eq!(import_name("net/http"), "http");
        assert_eq!(import_name("example.com/a/v3"), "a");
        assert_eq!(import_name("github.com/x/go-yaml"), "yaml");
        assert_eq!(import_name("a/b/c-d"), "c_d");
        assert_eq!(import_name("gopkg.in/yaml.v3"), "yaml");
    }

    #[test]
    fn unused_imports_are_flagged_but_not_managed_ones() {
        let src = "<go:import (\"fmt\"\n  \"strings\"\n  m \"example.com/demo/models\"\n  \"os\")/><go= strings.ToUpper(\"a\") /><go= x.os />";
        let s = analyze(src);
        let d = s.unused_imports(src);
        let msgs: Vec<_> = d.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(msgs, vec!["\"example.com/demo/models\" imported and not used", "\"os\" imported and not used"]);
        assert!(d.iter().all(|d| d.code == UNUSED_IMPORT));
        assert_eq!(d[0].span.slice(src), "m \"example.com/demo/models\"");
    }

    #[test]
    fn completion_prefix_and_member_detection() {
        let src = "<go= us />";
        let s = analyze(src);
        let site = Span::new(5, 7);
        let (prefix, member) = s.completion_prefix(src, 7, site);
        assert_eq!((prefix.slice(src), member), ("us", false));
        let (prefix, _) = s.completion_prefix(src, 6, site);
        assert_eq!(prefix.slice(src), "u");

        let src = "<p></p><go= a.b />";
        let s = analyze(src);
        let site = Span::new(12, 15);
        assert!(s.completion_prefix(src, 14, site).1, "after `a.`");
        assert!(!s.completion_prefix(src, 12, site).1, "at `a`");

        // Empty start of a region: nothing from a previous tag leaks in.
        let src = "<go= a. /><go= />";
        let s = analyze(src);
        let (prefix, member) = s.completion_prefix(src, 15, Span::new(15, 15));
        assert!(prefix.is_empty() && !member);
    }

    #[test]
    fn references_follow_scoping() {
        let src = "<go x := 1 /><go= x /><go:if c/><go x := 2 /><go= x /><go:endif/><go= x />";
        let s = analyze(src);
        let outer = s.resolve("x", src.rfind("<go= x").unwrap()).unwrap();
        let refs: Vec<_> = s.references(src, outer).iter().map(|r| r.start).collect();
        let want = vec![at(src, "x := 1"), at(src, "<go= x") + 5, src.rfind("<go= x").unwrap() + 5];
        assert_eq!(refs, want);
    }

    #[test]
    fn ident_at_reports_member_selectors() {
        let src = "<go= a.b />";
        let s = analyze(src);
        let (t, member) = s.ident_at(src, 5).unwrap();
        assert_eq!((text(src, &t), member), ("a", false));
        let (t, member) = s.ident_at(src, 8).unwrap();
        assert_eq!((text(src, &t), member), ("b", true));
        assert!(s.ident_at(src, 0).is_none());
    }
}
