//! Error-tolerant parser. Always yields a [`Tree`]; every problem becomes a [`Diagnostic`].
//!
//! Semantics follow the reference parser (`ghp/src/internal/parser`): `if` / `elif`* / `else`? /
//! `endif`, `switch` / (`case` | `default`)* / `endswitch`, `for` / `endfor`, plus the leaf tags
//! `import`, `<go ... />` and `<go= ... />`. Where the reference stops at the first error, this
//! parser recovers:
//!
//! * a closer or continuation tag with nothing to close becomes [`Node::Stray`];
//! * a tag meant for an enclosing construct (e.g. `<go:endif/>` met inside a `for`) closes the
//!   inner, unclosed constructs implicitly, each reported as unclosed;
//! * nesting deeper than [`MAX_DEPTH`] stops opening new constructs (no stack overflow).

use crate::ast::{Branch, For, If, Import, ImportSpec, Node, Span, Switch, Tag, TagKind, Tree};
use crate::diagnostics::{Diagnostic, code};
use crate::lexer::{self, Token};

/// Maximum nesting of `if` / `switch` / `for`.
pub const MAX_DEPTH: usize = 200;

/// Result of [`parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parse {
    pub tree: Tree,
    /// Sorted by position.
    pub diagnostics: Vec<Diagnostic>,
}

/// Parse a template. Never fails and never panics, whatever the input.
pub fn parse(src: &str) -> Parse {
    let (toks, mut diags) = lexer::lex(src);
    let mut p = Parser { src, toks, i: 0, stack: Vec::new(), diags: Vec::new() };
    let nodes = p.parse_nodes();
    diags.append(&mut p.diags);
    diags.sort_by_key(|d| (d.span.start, d.span.end));
    Parse { tree: Tree { nodes, len: src.len() }, diagnostics: diags }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    If,
    Switch,
    For,
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    i: usize,
    stack: Vec<Ctx>,
    diags: Vec<Diagnostic>,
}

impl Parser<'_> {
    fn err(&mut self, span: Span, code: &'static str, msg: impl Into<String>) {
        self.diags.push(Diagnostic::error(span, code, msg));
    }

    fn peek(&self) -> Option<Token> {
        self.toks.get(self.i).copied()
    }

    /// Whether some open construct consumes a tag of this kind.
    fn accepted(&self, kind: TagKind) -> bool {
        let want = match kind {
            TagKind::Elif | TagKind::Else | TagKind::EndIf => Ctx::If,
            TagKind::Case | TagKind::Default | TagKind::EndSwitch => Ctx::Switch,
            TagKind::EndFor => Ctx::For,
            _ => return false,
        };
        self.stack.contains(&want)
    }

    /// Nodes up to EOF or up to a tag that an enclosing construct handles (left unconsumed).
    fn parse_nodes(&mut self) -> Vec<Node> {
        let mut out = Vec::new();
        while let Some(tok) = self.peek() {
            match tok {
                Token::Text(s) => {
                    self.i += 1;
                    out.push(Node::Text(s));
                }
                Token::Tag(t) => match t.kind {
                    TagKind::Import => {
                        self.i += 1;
                        out.push(Node::Import(self.import(t)));
                    }
                    TagKind::Statement => {
                        self.i += 1;
                        out.push(Node::Statement(t));
                    }
                    TagKind::Echo => {
                        self.i += 1;
                        if t.payload.is_none() {
                            self.err(t.span, code::EMPTY_ECHO, "`<go=/>` requires an expression");
                        }
                        out.push(Node::Echo(t));
                    }
                    TagKind::If | TagKind::Switch | TagKind::For => {
                        self.i += 1;
                        if self.stack.len() >= MAX_DEPTH {
                            self.err(
                                t.span,
                                code::TOO_DEEP,
                                format!("nesting is too deep (more than {MAX_DEPTH} levels)"),
                            );
                            out.push(Node::Stray(t));
                            continue;
                        }
                        out.push(match t.kind {
                            TagKind::If => self.parse_if(t),
                            TagKind::Switch => self.parse_switch(t),
                            _ => self.parse_for(t),
                        });
                    }
                    k => {
                        if self.accepted(k) {
                            break;
                        }
                        self.i += 1;
                        self.stray(&t);
                        out.push(Node::Stray(t));
                    }
                },
            }
        }
        out
    }

    fn stray(&mut self, t: &Tag) {
        let (c, msg) = match t.kind {
            TagKind::Elif => (code::ELIF_OUTSIDE_IF, "`<go:elif/>` outside of `<go:if/>`"),
            TagKind::Else => (code::ELSE_OUTSIDE_IF, "`<go:else/>` outside of `<go:if/>`"),
            TagKind::EndIf => (code::UNMATCHED_ENDIF, "`<go:endif/>` without a matching `<go:if/>`"),
            TagKind::Case => (code::CASE_OUTSIDE_SWITCH, "`<go:case/>` outside of `<go:switch/>`"),
            TagKind::Default => (code::DEFAULT_OUTSIDE_SWITCH, "`<go:default/>` outside of `<go:switch/>`"),
            TagKind::EndSwitch => (code::UNMATCHED_ENDSWITCH, "`<go:endswitch/>` without a matching `<go:switch/>`"),
            _ => (code::UNMATCHED_ENDFOR, "`<go:endfor/>` without a matching `<go:for/>`"),
        };
        self.err(t.span, c, msg);
    }

    /// A closer must stand alone.
    fn check_closer(&mut self, t: &Tag) {
        if let Some(p) = t.payload {
            self.err(p, code::CLOSER_WITH_PAYLOAD, format!("{} does not take a payload", t.kind.name()));
        }
    }

    fn parse_if(&mut self, open: Tag) -> Node {
        if open.payload.is_none() {
            self.err(open.span, code::EMPTY_IF, "`<go:if/>` requires a condition");
        }
        self.stack.push(Ctx::If);
        let then = self.parse_nodes();
        let mut branches: Vec<Branch> = Vec::new();
        let mut seen_else = false;
        let mut end = None;
        loop {
            match self.peek() {
                Some(Token::Tag(t)) if matches!(t.kind, TagKind::Elif | TagKind::Else) => {
                    self.i += 1;
                    if t.kind == TagKind::Elif {
                        if seen_else {
                            self.err(t.span, code::ELIF_AFTER_ELSE, "`<go:elif/>` must come before `<go:else/>`");
                        }
                        if t.payload.is_none() {
                            self.err(t.span, code::EMPTY_ELIF, "`<go:elif/>` requires a condition");
                        }
                    } else {
                        if seen_else {
                            self.err(t.span, code::DUPLICATE_ELSE, "duplicate `<go:else/>`");
                        }
                        seen_else = true;
                        if let Some(p) = t.payload {
                            self.err(
                                p,
                                code::ELSE_WITH_PAYLOAD,
                                "`<go:else/>` does not take a condition; use `<go:elif/>` or a nested `<go:if/>`",
                            );
                        }
                    }
                    let body = self.parse_nodes();
                    branches.push(Branch { tag: t, body });
                }
                Some(Token::Tag(t)) if t.kind == TagKind::EndIf => {
                    self.i += 1;
                    self.check_closer(&t);
                    end = Some(t);
                    break;
                }
                _ => {
                    self.err(open.span, code::UNCLOSED_IF, "`<go:if/>` is never closed; expected `<go:endif/>`");
                    break;
                }
            }
        }
        self.stack.pop();

        let mut stop = open.span.end;
        bump(&mut stop, &then);
        for b in &branches {
            stop = stop.max(b.tag.span.end);
            bump(&mut stop, &b.body);
        }
        if let Some(e) = &end {
            stop = stop.max(e.span.end);
        }
        Node::If(If { open, then, branches, end, span: Span::new(open.span.start, stop) })
    }

    fn parse_switch(&mut self, open: Tag) -> Node {
        self.stack.push(Ctx::Switch);
        let before = self.parse_nodes();
        let stray_content = before.iter().find(|n| match n {
            Node::Text(s) => !s.slice(self.src).trim().is_empty(),
            _ => true,
        });
        if let Some(n) = stray_content {
            let span = n.span();
            self.err(span, code::SWITCH_BODY, "`<go:switch/>` body must start with `<go:case/>` or `<go:default/>`");
        }

        let mut cases: Vec<Branch> = Vec::new();
        let mut seen_default = false;
        let mut end = None;
        loop {
            match self.peek() {
                Some(Token::Tag(t)) if matches!(t.kind, TagKind::Case | TagKind::Default) => {
                    self.i += 1;
                    if t.kind == TagKind::Case {
                        if seen_default {
                            self.err(
                                t.span,
                                code::CASE_AFTER_DEFAULT,
                                "`<go:case/>` after `<go:default/>`; `default` must be last",
                            );
                        }
                        if t.payload.is_none() {
                            self.err(t.span, code::EMPTY_CASE, "`<go:case/>` requires at least one value");
                        }
                    } else {
                        if seen_default {
                            self.err(t.span, code::DUPLICATE_DEFAULT, "duplicate `<go:default/>`");
                        }
                        seen_default = true;
                        if let Some(p) = t.payload {
                            self.err(p, code::DEFAULT_WITH_PAYLOAD, "`<go:default/>` does not take a value");
                        }
                    }
                    let body = self.parse_nodes();
                    cases.push(Branch { tag: t, body });
                }
                Some(Token::Tag(t)) if t.kind == TagKind::EndSwitch => {
                    self.i += 1;
                    self.check_closer(&t);
                    end = Some(t);
                    break;
                }
                _ => {
                    self.err(
                        open.span,
                        code::UNCLOSED_SWITCH,
                        "`<go:switch/>` is never closed; expected `<go:endswitch/>`",
                    );
                    break;
                }
            }
        }
        self.stack.pop();

        let mut stop = open.span.end;
        bump(&mut stop, &before);
        for b in &cases {
            stop = stop.max(b.tag.span.end);
            bump(&mut stop, &b.body);
        }
        if let Some(e) = &end {
            stop = stop.max(e.span.end);
        }
        Node::Switch(Switch { open, before, cases, end, span: Span::new(open.span.start, stop) })
    }

    fn parse_for(&mut self, open: Tag) -> Node {
        if open.payload.is_none() {
            // An empty `for` compiles to `for {}`: an infinite loop.
            self.err(open.span, code::EMPTY_FOR, "`<go:for/>` requires a loop expression");
        }
        self.stack.push(Ctx::For);
        let body = self.parse_nodes();
        let end = match self.peek() {
            Some(Token::Tag(t)) if t.kind == TagKind::EndFor => {
                self.i += 1;
                self.check_closer(&t);
                Some(t)
            }
            _ => {
                self.err(open.span, code::UNCLOSED_FOR, "`<go:for/>` is never closed; expected `<go:endfor/>`");
                None
            }
        };
        self.stack.pop();

        let mut stop = open.span.end;
        bump(&mut stop, &body);
        if let Some(e) = &end {
            stop = stop.max(e.span.end);
        }
        Node::For(For { open, body, end, span: Span::new(open.span.start, stop) })
    }

    fn import(&mut self, tag: Tag) -> Import {
        let mut specs = Vec::new();
        if !self.stack.is_empty() {
            self.err(
                tag.span,
                code::NESTED_IMPORT,
                "`<go:import/>` must be at the top level of the file; Go has no conditional imports",
            );
        }
        let Some(p) = tag.payload else {
            self.err(tag.span, code::IMPORT_EMPTY, "empty `<go:import/>`: expected `(\"path\")`");
            return Import { tag, specs };
        };

        let text = p.slice(self.src);
        let open = text.starts_with('(');
        let close = text.ends_with(')');
        if open != close {
            self.err(p, code::IMPORT_PARENS, "unbalanced parentheses in `<go:import/>`");
        }
        let lo = p.start + usize::from(open);
        let hi = (p.end - usize::from(close)).max(lo);

        let before = self.diags.len();
        let src = self.src;
        let inner = &src[lo..hi];
        let mut seg = 0;
        for (i, ch) in inner.char_indices().chain(std::iter::once((inner.len(), '\n'))) {
            if ch == '\n' || ch == ',' {
                self.import_item(lo + seg, lo + i, &mut specs);
                seg = i + ch.len_utf8();
            }
        }
        if specs.is_empty() && self.diags.len() == before {
            self.err(tag.span, code::IMPORT_EMPTY, "empty `<go:import/>`: expected at least one package");
        }
        Import { tag, specs }
    }

    /// One `[alias] "path"` item living in `src[a..b]`.
    fn import_item(&mut self, a: usize, b: usize, specs: &mut Vec<ImportSpec>) {
        let src = self.src;
        let Some(span) = lexer::trimmed(src, a, b) else { return };
        let item = span.slice(src);
        let base = src.as_ptr() as usize;
        let fields: Vec<(&str, usize)> = item.split_whitespace().map(|f| (f, f.as_ptr() as usize - base)).collect();

        let (alias, (raw, raw_at)) = match fields.as_slice() {
            [one] => (None, *one),
            [al, p] => (Some(*al), *p),
            _ => {
                self.err(
                    span,
                    code::IMPORT_ITEM,
                    format!("`{item}` is not a single import path; use one path per line or separate them with commas"),
                );
                return;
            }
        };
        if let Some((alias, _)) = alias
            && alias.contains(['"', '\''])
        {
            self.err(span, code::IMPORT_ALIAS, format!("import alias `{alias}` cannot be quoted"));
            return;
        }
        let lopsided_quote = raw.starts_with('"') != raw.ends_with('"');
        if lopsided_quote || raw.contains(['\'', '`']) {
            self.err(span, code::IMPORT_PATH, format!("malformed import path `{raw}`; expected \"path\""));
            return;
        }
        let unquoted = raw.trim_matches('"');
        if unquoted.is_empty() {
            self.err(span, code::IMPORT_PATH, "empty import path");
            return;
        }
        let lead = raw.len() - raw.trim_start_matches('"').len();
        specs.push(ImportSpec {
            alias: alias.map(|(a, _)| a.to_string()),
            path: unquoted.to_string(),
            span,
            path_span: Span::new(raw_at + lead, raw_at + lead + unquoted.len()),
        });
    }
}

fn bump(stop: &mut usize, nodes: &[Node]) {
    if let Some(n) = nodes.last() {
        *stop = (*stop).max(n.span().end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::ExprKind;

    fn codes(src: &str) -> Vec<&'static str> {
        parse(src).diagnostics.iter().filter(|d| d.code != code::UNKNOWN_TAG).map(|d| d.code).collect()
    }

    #[test]
    fn diagnostics_table() {
        use code::*;
        let cases: &[(&str, &[&str])] = &[
            // Valid.
            ("", &[]),
            ("plain <b>html</b>", &[]),
            ("<go:if a/>x<go:endif/>", &[]),
            ("<go:if a/>x<go:elif b/>y<go:elif c/>w<go:else/>z<go:endif/>", &[]),
            ("<go:switch v/><go:case 1/>a<go:case 2, 3/>b<go:default/>c<go:endswitch/>", &[]),
            ("<go:switch/>\n<go:case 1/>a<go:endswitch/>", &[]),
            ("<go:for i := range xs/><go= i /><go:endfor/>", &[]),
            ("<go:import (\"fmt\")/>\n<go x := 1 />", &[]),
            ("<go:if a/><go:for b/><go:switch c/><go:case 1/><go= d /><go:endswitch/><go:endfor/><go:endif/>", &[]),
            // Unclosed.
            ("<go:if a/>x", &[UNCLOSED_IF]),
            ("<go:if a/>x<go:else/>y", &[UNCLOSED_IF]),
            ("<go:for x/>", &[UNCLOSED_FOR]),
            ("<go:switch v/>", &[UNCLOSED_SWITCH]),
            ("<go:switch v/><go:case 1/>", &[UNCLOSED_SWITCH]),
            ("<go:for a/><go:if b/>", &[UNCLOSED_FOR, UNCLOSED_IF]),
            ("<go:if a/><go:for b/><go:endif/>", &[UNCLOSED_FOR]),
            ("<go:for a/><go:if b/><go:endfor/>", &[UNCLOSED_IF]),
            ("<go:switch a/><go:case 1/><go:if b/><go:case 2/><go:endswitch/>", &[UNCLOSED_IF]),
            // Unmatched.
            ("x<go:endif/>", &[UNMATCHED_ENDIF]),
            ("<go:endfor/>", &[UNMATCHED_ENDFOR]),
            ("<go:endswitch/>", &[UNMATCHED_ENDSWITCH]),
            ("<go:if a/><go:endif/><go:endif/>", &[UNMATCHED_ENDIF]),
            ("<go:if a/><go:endfor/><go:endif/>", &[UNMATCHED_ENDFOR]),
            ("<go:for a/><go:endswitch/><go:endfor/>", &[UNMATCHED_ENDSWITCH]),
            // Continuations outside their construct.
            ("<go:elif a/>", &[ELIF_OUTSIDE_IF]),
            ("<go:else/>", &[ELSE_OUTSIDE_IF]),
            ("<go:case 1/>", &[CASE_OUTSIDE_SWITCH]),
            ("<go:default/>", &[DEFAULT_OUTSIDE_SWITCH]),
            ("<go:for a/><go:else/><go:endfor/>", &[ELSE_OUTSIDE_IF]),
            ("<go:if a/><go:case 1/><go:endif/>", &[CASE_OUTSIDE_SWITCH]),
            // Ordering inside a construct.
            ("<go:if a/><go:else/><go:elif b/><go:endif/>", &[ELIF_AFTER_ELSE]),
            ("<go:if a/><go:else/><go:else/><go:endif/>", &[DUPLICATE_ELSE]),
            ("<go:switch v/><go:default/><go:case 1/><go:endswitch/>", &[CASE_AFTER_DEFAULT]),
            ("<go:switch v/><go:default/><go:default/><go:endswitch/>", &[DUPLICATE_DEFAULT]),
            ("<go:switch v/>hello<go:case 1/><go:endswitch/>", &[SWITCH_BODY]),
            ("<go:switch v/><go= x /><go:endswitch/>", &[SWITCH_BODY]),
            // Missing / empty expressions.
            ("<go=/>", &[EMPTY_ECHO]),
            ("<go= /><go=\n/>", &[EMPTY_ECHO, EMPTY_ECHO]),
            ("<go:if/><go:endif/>", &[EMPTY_IF]),
            ("<go:if a/><go:elif/><go:endif/>", &[EMPTY_ELIF]),
            ("<go:for/><go:endfor/>", &[EMPTY_FOR]),
            ("<go:switch v/><go:case/><go:endswitch/>", &[EMPTY_CASE]),
            // Payloads that must not be there.
            ("<go:if a/><go:endif x/>", &[CLOSER_WITH_PAYLOAD]),
            ("<go:for a/><go:endfor x/>", &[CLOSER_WITH_PAYLOAD]),
            ("<go:switch a/><go:endswitch x/>", &[CLOSER_WITH_PAYLOAD]),
            ("<go:if a/><go:else x/><go:endif/>", &[ELSE_WITH_PAYLOAD]),
            ("<go:switch v/><go:default x/><go:endswitch/>", &[DEFAULT_WITH_PAYLOAD]),
            // Imports.
            ("<go:import (\"fmt\"\n\"os\")/>", &[]),
            ("<go:import (f \"fmt\", \"os\")/>", &[]),
            ("<go:import \"fmt\"/>", &[]),
            ("<go:import/>", &[IMPORT_EMPTY]),
            ("<go:import ()/>", &[IMPORT_EMPTY]),
            ("<go:import (\n,\n)/>", &[IMPORT_EMPTY]),
            ("<go:import (\"fmt\" \"os\" \"io\")/>", &[IMPORT_ITEM]),
            ("<go:import \"fmt\")/>", &[IMPORT_PARENS]),
            ("<go:import (\"a\" \"fmt\")/>", &[IMPORT_ALIAS]),
            ("<go:import (\"\")/>", &[IMPORT_PATH]),
            ("<go:import (\"fmt)/>", &[UNBALANCED_TAG, IMPORT_PATH]),
            ("<go:if a/><go:import (\"fmt\")/><go:endif/>", &[NESTED_IMPORT]),
            // Broken tags.
            ("<go:if a/>x<go:endif", &[UNTERMINATED_TAG]),
            ("<go:if a", &[UNTERMINATED_TAG, UNCLOSED_IF]),
            ("<go:if a>x<go:endif/>", &[TAG_NOT_CLOSED]),
            ("<go:if (a > b\n<p>x</p>\n<go:endif/>", &[UNTERMINATED_TAG]),
            ("<go= \"abc/>", &[UNBALANCED_TAG]),
        ];
        for (src, want) in cases {
            assert_eq!(codes(src), *want, "src {src:?}");
        }
    }

    #[test]
    fn diagnostic_spans() {
        let src = "<go:if a/>x\n<go:endif/>\n<go:endif/>";
        let p = parse(src);
        assert_eq!(p.diagnostics.len(), 1);
        assert_eq!(p.diagnostics[0].span.slice(src), "<go:endif/>");
        assert_eq!(p.diagnostics[0].span.start, 24);

        let src = "é<go:for 😀/>";
        let p = parse(src);
        assert_eq!(p.diagnostics[0].code, code::UNCLOSED_FOR);
        assert_eq!(p.diagnostics[0].span.slice(src), "<go:for 😀/>");

        let src = "<go:if a/><go:endif zz/>";
        let p = parse(src);
        assert_eq!(p.diagnostics[0].span.slice(src), "zz");
    }

    #[test]
    fn tree_shape() {
        let src = "a<go:if x/>b<go:elif y/>c<go:else/>d<go:endif/>e";
        let p = parse(src);
        assert!(p.diagnostics.is_empty());
        assert_eq!(p.tree.nodes.len(), 3);
        let Node::If(n) = &p.tree.nodes[1] else { panic!() };
        assert_eq!(n.span.slice(src), "<go:if x/>b<go:elif y/>c<go:else/>d<go:endif/>");
        assert_eq!(n.branches.len(), 2);
        assert_eq!(n.branches[0].tag.kind, TagKind::Elif);
        assert_eq!(n.branches[1].tag.kind, TagKind::Else);
        assert!(n.end.is_some());
        // Pre-order: text, if, then-text, elif body, else body, text.
        assert_eq!(p.tree.iter().count(), 6);
    }

    #[test]
    fn unclosed_node_extent() {
        let src = "<go:if x/>body";
        let p = parse(src);
        let Node::If(n) = &p.tree.nodes[0] else { panic!() };
        assert!(n.end.is_none());
        assert_eq!(n.span, Span::new(0, src.len()));
    }

    #[test]
    fn expr_regions_exact() {
        let src = "<go:import (\"fmt\")/>\n<go\n  x := 1\n/>é<go= x + 1 /><go:if x > 0 /> \
                   <go:if (x > 0)/><go:elif y/><go:else/><go:endif/><go:switch k/><go:case 1, 2/><go:default/><go:endswitch/>\
                   <go:for i := 0; i < 3; i++/><go:endfor/>";
        let p = parse(src);
        let got: Vec<(ExprKind, &str)> = p.tree.expr_regions().iter().map(|r| (r.kind, r.span.slice(src))).collect();
        // `<go:if x > 0 />` is cut at the first `>` (reference behaviour): payload `x`, not closed.
        assert!(got.contains(&(ExprKind::Statement, "x := 1")), "{got:?}");
        assert!(got.contains(&(ExprKind::Echo, "x + 1")));
        assert!(got.contains(&(ExprKind::If, "(x > 0)")));
        assert!(got.contains(&(ExprKind::Elif, "y")));
        assert!(got.contains(&(ExprKind::Switch, "k")));
        assert!(got.contains(&(ExprKind::Case, "1, 2")));
        assert!(got.contains(&(ExprKind::For, "i := 0; i < 3; i++")));
        // Imports are not Go regions, and regions come in source order.
        assert!(!got.iter().any(|(_, s)| s.contains("fmt")));
        let starts: Vec<usize> = p.tree.expr_regions().iter().map(|r| r.span.start).collect();
        assert!(starts.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn expr_region_at_cursor() {
        let src = "<go= name />";
        let p = parse(src);
        let r = p.tree.expr_region_at(8).unwrap();
        assert_eq!(r.span.slice(src), "name");
        assert!(p.tree.expr_region_at(9).is_some()); // right after the last byte
        assert!(p.tree.expr_region_at(0).is_none());
    }

    #[test]
    fn stray_tags_are_not_regions() {
        let p = parse("<go:elif a/><go:case b/>");
        assert!(p.tree.expr_regions().is_empty());
    }

    #[test]
    fn import_specs() {
        let src = "<go:import (\n  \"fmt\"\r\n  s \"strings\", \"os\"\n)/>";
        let p = parse(src);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        let specs = p.tree.imports();
        let got: Vec<(Option<&str>, &str, &str)> =
            specs.iter().map(|s| (s.alias.as_deref(), s.path.as_str(), s.path_span.slice(src))).collect();
        assert_eq!(got, [(None, "fmt", "fmt"), (Some("s"), "strings", "strings"), (None, "os", "os")]);
        assert_eq!(specs[1].span.slice(src), "s \"strings\"");
    }

    #[test]
    fn utf8_crlf_spans() {
        let src = "<p>é😀</p>\r\n<go:if ü == \"😀\" />\r\n<go= 日本 />\r\n<go:endif/>";
        let p = parse(src);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        let regions: Vec<&str> = p.tree.expr_regions().iter().map(|r| r.span.slice(src)).collect();
        assert_eq!(regions, ["ü == \"😀\"", "日本"]);
    }

    #[test]
    fn node_at_finds_deepest() {
        let src = "<go:if a/><go:for b/>x<go:endfor/><go:endif/>";
        let p = parse(src);
        let at = src.find('x').unwrap();
        assert!(matches!(p.tree.node_at(at), Some(Node::Text(_))));
        assert!(matches!(p.tree.node_at(1), Some(Node::If(_))));
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let src = "<go:if x/>".repeat(50_000);
        let p = parse(&src);
        assert!(p.diagnostics.iter().any(|d| d.code == code::TOO_DEEP));
        let src = "<go:for x/>".repeat(1000) + &"<go:endfor/>".repeat(1000);
        let p = parse(&src);
        assert!(p.diagnostics.iter().any(|d| d.code == code::TOO_DEEP));
    }
}
