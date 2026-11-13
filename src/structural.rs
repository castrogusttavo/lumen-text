//! Structural navigation and text objects from tree-sitter syntax trees, with no language server.
//!
//! Nothing is kept alive: a document is parsed only when a structural command runs (a full parse
//! is a few milliseconds for ordinary files, so there is no incremental tree or worker thread to
//! hold in memory) and the tree is dropped as soon as the answer is computed. Idle RAM cost is
//! zero; the only fixed cost is the grammar tables in the binary, which is why the grammar set is
//! small and curated (JS/TS/TSX, Rust, Python, Go, JSON, CSS, HTML, C, Java; TS and TSX share the
//! TSX grammar, C++ is left out because its tables alone are bigger than all the others together).
//!
//! Everything here works on byte offsets of the UTF-8 text. [`TextObject`] queries are meant to be
//! reused by other features (the Vim mode asks for `if`/`af`/`ic`/`aa`...): see [`text_object`].

// The text-object queries (argument, block, comment, inner forms) are API for features that are not
// wired yet (the Vim mode); they are covered by the tests below.
#![allow(dead_code)]

use std::ops::Range;
use std::path::Path;
use tree_sitter::{Node, Parser, Tree};

/// Largest document (bytes) a structural command will parse.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Grammars bundled in the binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Js,
    Tsx,
    Rust,
    Python,
    Go,
    Json,
    Css,
    Html,
    C,
    Java,
}

impl Lang {
    /// Grammar for a file name, from its extension.
    pub fn from_path(path: &Path) -> Option<Lang> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "js" | "mjs" | "cjs" | "jsx" => Lang::Js,
            "ts" | "mts" | "cts" | "tsx" => Lang::Tsx,
            "rs" => Lang::Rust,
            "py" | "pyi" => Lang::Python,
            "go" => Lang::Go,
            "json" => Lang::Json,
            "css" => Lang::Css,
            "html" | "htm" => Lang::Html,
            "c" | "h" => Lang::C,
            "java" => Lang::Java,
            _ => return None,
        })
    }

    fn language(self) -> tree_sitter::Language {
        match self {
            Lang::Js => tree_sitter_javascript::LANGUAGE.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Json => tree_sitter_json::LANGUAGE.into(),
            Lang::Css => tree_sitter_css::LANGUAGE.into(),
            Lang::Html => tree_sitter_html::LANGUAGE.into(),
            Lang::C => tree_sitter_c::LANGUAGE.into(),
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
        }
    }
}

/// Parses `src` (None when too big or the grammar fails to load).
pub fn parse(lang: Lang, src: &str) -> Option<Tree> {
    if src.len() > MAX_BYTES {
        return None;
    }
    let mut p = Parser::new();
    p.set_language(&lang.language()).ok()?;
    p.parse(src, None)
}

/// The kinds of thing a text-object query can find.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextObject {
    /// Function, method, constructor, closure bound to a name.
    Function,
    /// Class, struct, enum, trait, interface, impl, CSS rule.
    Class,
    /// One argument or parameter of a call or declaration.
    Argument,
    /// A braced block, object/array literal, HTML element.
    Block,
    /// A comment (consecutive line comments count as one).
    Comment,
}

const ARG_LISTS: &[&str] = &[
    "arguments",
    "argument_list",
    "formal_parameters",
    "parameters",
    "parameter_list",
    "type_arguments",
    "type_parameters",
    "lambda_parameters",
];

fn is_function_kind(k: &str) -> bool {
    matches!(
        k,
        "function_declaration"
            | "function_expression"
            | "generator_function_declaration"
            | "generator_function"
            | "arrow_function"
            | "method_definition"
            | "function_item"
            | "function_definition"
            | "method_declaration"
            | "func_literal"
            | "constructor_declaration"
            | "lambda_expression"
    )
}

/// Functions that have no name of their own; they only count for navigation when bound to a name.
fn is_anonymous_function(k: &str) -> bool {
    matches!(k, "function_expression" | "generator_function" | "arrow_function" | "func_literal" | "lambda_expression")
}

fn is_class_kind(n: &Node) -> bool {
    match n.kind() {
        "class_declaration"
        | "class"
        | "abstract_class_declaration"
        | "class_definition"
        | "struct_item"
        | "enum_item"
        | "trait_item"
        | "impl_item"
        | "union_item"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration"
        | "type_declaration"
        | "internal_module"
        | "rule_set" => true,
        // `struct foo *p` is a use, only the ones with a body define something.
        "struct_specifier" | "union_specifier" | "enum_specifier" => n.child_by_field_name("body").is_some(),
        _ => false,
    }
}

fn is_block_kind(k: &str) -> bool {
    matches!(
        k,
        "statement_block"
            | "block"
            | "compound_statement"
            | "declaration_list"
            | "class_body"
            | "field_declaration_list"
            | "enum_body"
            | "interface_body"
            | "enum_variant_list"
            | "object"
            | "array"
            | "element"
    )
}

fn is_comment(k: &str) -> bool {
    k.contains("comment")
}

fn matches_object(n: &Node, obj: TextObject) -> bool {
    // Keyword tokens share names with nodes (`class` in Java), only real nodes count.
    if !n.is_named() {
        return false;
    }
    match obj {
        TextObject::Function => is_function_kind(n.kind()),
        TextObject::Class => is_class_kind(n),
        TextObject::Block => is_block_kind(n.kind()),
        TextObject::Comment => is_comment(n.kind()),
        TextObject::Argument => {
            n.is_named() && !is_comment(n.kind()) && n.parent().is_some_and(|p| ARG_LISTS.contains(&p.kind()))
        }
    }
}

/// Range of the node's contents without its braces/brackets, trimmed to whole lines when the
/// delimiters sit on lines of their own (so `{\n  body\n}` yields `  body\n`).
fn inside_delims(src: &str, r: Range<usize>) -> Range<usize> {
    let text = &src[r.clone()];
    let (Some(first), Some(last)) = (text.chars().next(), text.chars().next_back()) else { return r };
    let closing = match first {
        '{' => '}',
        '[' => ']',
        '(' => ')',
        _ => return r,
    };
    if last != closing || text.len() < 2 {
        return r;
    }
    let (s, e) = (r.start + 1, r.end - 1);
    let inner = &src[s..e];
    // Whole-line form: opening delimiter followed only by blanks and a newline, closing one
    // preceded only by blanks after a newline.
    if let Some(nl) = inner.find('\n')
        && inner[..nl].trim().is_empty()
        && let Some(last_nl) = inner.rfind('\n')
        && inner[last_nl + 1..].trim().is_empty()
        && last_nl >= nl
    {
        return s + nl + 1..s + last_nl + 1;
    }
    s..e
}

fn body_range(src: &str, n: &Node) -> Range<usize> {
    match n.child_by_field_name("body") {
        Some(b) => inside_delims(src, b.start_byte()..b.end_byte()),
        None => n.start_byte()..n.end_byte(),
    }
}

/// The comment group around `n`: adjacent line comments (`//`, `#`) on consecutive lines.
fn comment_group(src: &str, n: &Node) -> Range<usize> {
    let marker = |c: &Node| -> Option<&'static str> {
        let t = &src[c.start_byte()..c.end_byte()];
        if t.starts_with("//") {
            Some("//")
        } else if t.starts_with('#') {
            Some("#")
        } else {
            None
        }
    };
    let Some(m) = marker(n) else { return n.start_byte()..n.end_byte() };
    let (mut first, mut last) = (*n, *n);
    while let Some(p) = first.prev_sibling() {
        if is_comment(p.kind()) && marker(&p) == Some(m) && p.end_position().row + 1 == first.start_position().row {
            first = p;
        } else {
            break;
        }
    }
    while let Some(nx) = last.next_sibling() {
        if is_comment(nx.kind()) && marker(&nx) == Some(m) && last.end_position().row + 1 == nx.start_position().row {
            last = nx;
        } else {
            break;
        }
    }
    first.start_byte()..last.end_byte()
}

/// A comment's text without its `//`, `#` or `/* */` markers (first line's marker only).
fn comment_inner(src: &str, r: Range<usize>) -> Range<usize> {
    let t = &src[r.clone()];
    let (mut s, mut e) = (r.start, r.end);
    if t.starts_with("/*") && t.ends_with("*/") && t.len() >= 4 {
        s += 2;
        e -= 2;
        while src[s..e].starts_with('*') {
            s += 1;
        }
    } else if t.starts_with("//") {
        s += t.chars().take_while(|c| *c == '/' || *c == '!').count();
    } else if t.starts_with('#') {
        s += t.chars().take_while(|c| *c == '#').count();
    }
    if src[s..e].starts_with(' ') {
        s += 1;
    }
    s..e
}

/// Range of one argument; the outer form takes the neighbouring comma too.
fn argument_range(src: &str, n: &Node, inner: bool) -> Range<usize> {
    let r = n.start_byte()..n.end_byte();
    if inner {
        return r;
    }
    if let Some(nx) = n.next_sibling()
        && nx.kind() == ","
    {
        // Through the comma and the blanks after it (up to the next token).
        let mut e = nx.end_byte();
        while src[e..].starts_with([' ', '\t']) {
            e += 1;
        }
        return r.start..e;
    }
    if let Some(pv) = n.prev_sibling()
        && pv.kind() == ","
    {
        return pv.start_byte()..r.end;
    }
    r
}

fn object_range(src: &str, n: &Node, obj: TextObject, inner: bool) -> Range<usize> {
    let whole = n.start_byte()..n.end_byte();
    if !inner && obj != TextObject::Argument && obj != TextObject::Comment {
        return whole;
    }
    match obj {
        TextObject::Function | TextObject::Class => body_range(src, n),
        TextObject::Block => inside_delims(src, whole),
        TextObject::Argument => argument_range(src, n, inner),
        TextObject::Comment => {
            let g = comment_group(src, n);
            if inner { comment_inner(src, g) } else { g }
        }
    }
}

/// Byte range to look the starting node up with: a selection as is, a caret as the character it
/// sits on (or the word it just ended), since a zero-width range lands on whichever token touches it.
fn probe(src: &str, sel: &Range<usize>) -> Range<usize> {
    if sel.start != sel.end || sel.start > src.len() {
        return sel.clone();
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let next = src[sel.start..].chars().next();
    let prev = src[..sel.start].chars().next_back();
    match (prev, next) {
        (Some(p), n) if word(p) && !n.is_some_and(word) => sel.start - p.len_utf8()..sel.start,
        (_, Some(n)) => sel.start..sel.start + n.len_utf8(),
        (Some(p), None) => sel.start - p.len_utf8()..sel.start,
        (None, None) => sel.clone(),
    }
}

/// The smallest `obj` around the byte range `sel` that is strictly bigger than `sel`, so calling
/// it again with its own result grows to the next enclosing one. `inner` asks for the contents
/// (a function body without its braces, a comment without its markers) instead of the whole thing.
pub fn text_object(tree: &Tree, src: &str, sel: Range<usize>, obj: TextObject, inner: bool) -> Option<Range<usize>> {
    let p = probe(src, &sel);
    let mut n = Some(tree.root_node().descendant_for_byte_range(p.start, p.end)?);
    while let Some(node) = n {
        if matches_object(&node, obj) {
            let r = object_range(src, &node, obj, inner);
            if r.start <= sel.start && r.end >= sel.end && r != sel && r.start < r.end {
                return Some(r);
            }
        }
        n = node.parent();
    }
    None
}

/// The next syntax node outward: smallest node covering `sel` that is strictly bigger than it.
pub fn expand(tree: &Tree, src: &str, sel: Range<usize>) -> Option<Range<usize>> {
    let p = probe(src, &sel);
    let mut n = Some(tree.root_node().descendant_for_byte_range(p.start, p.end)?);
    while let Some(node) = n {
        let r = node.start_byte()..node.end_byte();
        if r.start <= sel.start && r.end >= sel.end && r != sel {
            return Some(r);
        }
        n = node.parent();
    }
    None
}

/// Start offsets of every function or class in the tree, in document order.
pub fn definitions(tree: &Tree, obj: TextObject) -> Vec<usize> {
    let mut out = Vec::new();
    let mut cur = tree.walk();
    loop {
        let n = cur.node();
        let hit = match obj {
            TextObject::Function => {
                n.is_named()
                    && is_function_kind(n.kind())
                    && (!is_anonymous_function(n.kind())
                        || n.parent().is_some_and(|p| p.kind() == "variable_declarator"))
            }
            _ => matches_object(&n, obj),
        };
        if hit {
            out.push(n.start_byte());
        }
        if cur.goto_first_child() {
            continue;
        }
        loop {
            if cur.goto_next_sibling() {
                break;
            }
            if !cur.goto_parent() {
                return out;
            }
        }
    }
}

/// Start of the next (or previous) function/class strictly after (before) byte `pos`.
pub fn jump(tree: &Tree, pos: usize, obj: TextObject, forward: bool) -> Option<usize> {
    let mut defs = definitions(tree, obj);
    defs.dedup();
    if forward { defs.into_iter().find(|s| *s > pos) } else { defs.into_iter().rev().find(|s| *s < pos) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(lang: Lang, src: &str) -> Tree {
        parse(lang, src).expect("parse")
    }

    /// Byte range of `needle` in `hay`.
    fn at(hay: &str, needle: &str) -> Range<usize> {
        let s = hay.find(needle).unwrap_or_else(|| panic!("{needle} not in source"));
        s..s + needle.len()
    }

    fn caret_in(hay: &str, needle: &str) -> Range<usize> {
        let r = at(hay, needle);
        r.start + 1..r.start + 1
    }

    #[test]
    fn languages_by_extension() {
        assert_eq!(Lang::from_path(Path::new("a/b.RS")), Some(Lang::Rust));
        assert_eq!(Lang::from_path(Path::new("x.ts")), Some(Lang::Tsx));
        assert_eq!(Lang::from_path(Path::new("x.tsx")), Some(Lang::Tsx));
        assert_eq!(Lang::from_path(Path::new("x.mjs")), Some(Lang::Js));
        assert_eq!(Lang::from_path(Path::new("x.h")), Some(Lang::C));
        assert_eq!(Lang::from_path(Path::new("x.cpp")), None);
        assert_eq!(Lang::from_path(Path::new("Makefile")), None);
    }

    #[test]
    fn every_grammar_loads() {
        for (l, src) in [
            (Lang::Js, "let a = 1;"),
            (Lang::Tsx, "let a: number = 1;"),
            (Lang::Rust, "fn a() {}"),
            (Lang::Python, "x = 1\n"),
            (Lang::Go, "package a\n"),
            (Lang::Json, "{\"a\": 1}"),
            (Lang::Css, "a { color: red }"),
            (Lang::Html, "<p>hi</p>"),
            (Lang::C, "int a;"),
            (Lang::Java, "class A {}"),
        ] {
            let t = tree(l, src);
            assert!(!t.root_node().has_error(), "{l:?}");
        }
    }

    #[test]
    fn too_big_is_refused() {
        let big = " ".repeat(MAX_BYTES + 1);
        assert!(parse(Lang::Rust, &big).is_none());
    }

    const RS: &str = "struct S { a: i32 }\n\nimpl S {\n    fn one(&self, x: i32, y: i32) -> i32 {\n        x + y\n    }\n\n    fn two() {\n        // note\n        // more\n        let v = [1, 2];\n    }\n}\n\nfn free() {}\n";

    #[test]
    fn rust_function_object() {
        let t = tree(Lang::Rust, RS);
        let r = text_object(&t, RS, caret_in(RS, "x + y"), TextObject::Function, false).unwrap();
        assert!(RS[r.clone()].starts_with("fn one"));
        assert!(RS[r.clone()].ends_with('}'));
        let inner = text_object(&t, RS, caret_in(RS, "x + y"), TextObject::Function, true).unwrap();
        assert_eq!(&RS[inner], "        x + y\n");
    }

    #[test]
    fn grows_when_repeated() {
        let t = tree(Lang::Rust, RS);
        let inner = text_object(&t, RS, caret_in(RS, "x + y"), TextObject::Function, true).unwrap();
        // Asking again from the inner body yields the whole function.
        let whole = text_object(&t, RS, inner.clone(), TextObject::Function, false).unwrap();
        assert!(RS[whole].starts_with("fn one"));
    }

    #[test]
    fn rust_class_object() {
        let t = tree(Lang::Rust, RS);
        let r = text_object(&t, RS, caret_in(RS, "fn two"), TextObject::Class, false).unwrap();
        assert!(RS[r].starts_with("impl S"));
    }

    #[test]
    fn rust_argument_object() {
        let t = tree(Lang::Rust, RS);
        let inner = text_object(&t, RS, caret_in(RS, "x: i32"), TextObject::Argument, true).unwrap();
        assert_eq!(&RS[inner], "x: i32");
        let outer = text_object(&t, RS, caret_in(RS, "x: i32"), TextObject::Argument, false).unwrap();
        assert_eq!(&RS[outer], "x: i32, ");
        // The last argument takes the comma before it instead.
        let last = text_object(&t, RS, caret_in(RS, "y: i32"), TextObject::Argument, false).unwrap();
        assert_eq!(&RS[last], ", y: i32");
    }

    #[test]
    fn rust_block_and_comment() {
        let t = tree(Lang::Rust, RS);
        let b = text_object(&t, RS, caret_in(RS, "[1, 2]"), TextObject::Block, true);
        // An array expression is not a block kind in Rust; the enclosing fn body is.
        let b = b.unwrap();
        assert!(RS[b].contains("let v"));
        let c = text_object(&t, RS, caret_in(RS, "// note"), TextObject::Comment, false).unwrap();
        assert_eq!(&RS[c.clone()], "// note\n        // more");
        let ci = text_object(&t, RS, caret_in(RS, "note"), TextObject::Comment, true).unwrap();
        assert!(RS[ci].starts_with("note"));
    }

    #[test]
    fn expand_walks_outward() {
        let src = "fn f() { foo(a, b); }\n";
        let t = tree(Lang::Rust, src);
        let a = at(src, "a");
        let mut sel = a.start..a.start;
        let mut seen = Vec::new();
        while let Some(r) = expand(&t, src, sel.clone()) {
            seen.push(src[r.clone()].to_string());
            sel = r;
        }
        assert_eq!(seen.first().map(String::as_str), Some("a"));
        assert!(seen.contains(&"(a, b)".to_string()));
        assert!(seen.contains(&"foo(a, b)".to_string()));
        assert_eq!(seen.last().unwrap().trim_end(), src.trim_end());
        // Strictly growing.
        for w in seen.windows(2) {
            assert!(w[1].len() > w[0].len());
        }
    }

    #[test]
    fn jump_between_functions_and_classes() {
        let t = tree(Lang::Rust, RS);
        let one = RS.find("fn one").unwrap();
        let two = RS.find("fn two").unwrap();
        let free = RS.find("fn free").unwrap();
        assert_eq!(jump(&t, 0, TextObject::Function, true), Some(one));
        assert_eq!(jump(&t, one, TextObject::Function, true), Some(two));
        assert_eq!(jump(&t, two + 3, TextObject::Function, true), Some(free));
        assert_eq!(jump(&t, free, TextObject::Function, true), None);
        assert_eq!(jump(&t, free, TextObject::Function, false), Some(two));
        assert_eq!(jump(&t, one, TextObject::Function, false), None);
        assert_eq!(jump(&t, 1, TextObject::Class, true), Some(RS.find("impl S").unwrap()));
        assert_eq!(jump(&t, RS.len(), TextObject::Class, false), Some(RS.find("impl S").unwrap()));
    }

    #[test]
    fn typescript_functions_and_arrows() {
        let src = "class A {\n  m(x: number) { return x; }\n}\nconst f = (a: number) => a + 1;\nfoo(() => 1);\nfunction g() {}\n";
        let t = tree(Lang::Tsx, src);
        let defs = definitions(&t, TextObject::Function);
        // Method, named arrow and declaration; the callback arrow is not a navigation target.
        assert_eq!(
            defs,
            vec![src.find("m(x").unwrap(), src.find("(a: number)").unwrap(), src.find("function g").unwrap()]
        );
        let c = text_object(&t, src, caret_in(src, "return x"), TextObject::Class, false).unwrap();
        assert!(src[c].starts_with("class A"));
    }

    #[test]
    fn python_function_and_class() {
        let src = "class K:\n    def a(self, x):\n        return x\n\n    def b(self):\n        pass\n";
        let t = tree(Lang::Python, src);
        let f = text_object(&t, src, caret_in(src, "return x"), TextObject::Function, false).unwrap();
        assert_eq!(&src[f], "def a(self, x):\n        return x");
        let body = text_object(&t, src, caret_in(src, "return x"), TextObject::Function, true).unwrap();
        assert_eq!(&src[body], "return x");
        let k = text_object(&t, src, caret_in(src, "pass"), TextObject::Class, false).unwrap();
        assert!(src[k].starts_with("class K"));
        assert_eq!(definitions(&t, TextObject::Function).len(), 2);
    }

    #[test]
    fn go_java_c() {
        let go = "package p\n\nfunc (s S) M() {}\n\nfunc F() {}\n\ntype T struct{ a int }\n";
        let t = tree(Lang::Go, go);
        assert_eq!(
            definitions(&t, TextObject::Function),
            vec![go.find("func (s").unwrap(), go.find("func F").unwrap()]
        );
        assert_eq!(definitions(&t, TextObject::Class), vec![go.find("type T").unwrap()]);

        let java = "class A {\n  A() {}\n  void m(int a, int b) {}\n}\ninterface I {}\n";
        let t = tree(Lang::Java, java);
        assert_eq!(
            definitions(&t, TextObject::Function),
            vec![java.find("A() {}").unwrap(), java.find("void m").unwrap()]
        );
        assert_eq!(definitions(&t, TextObject::Class), vec![0, java.find("interface").unwrap()]);

        let c = "struct P { int x; };\nint f(int a) { return a; }\nstruct P *p;\n";
        let t = tree(Lang::C, c);
        assert_eq!(definitions(&t, TextObject::Function), vec![c.find("int f").unwrap()]);
        // Only the definition with a body counts as a class, not `struct P *p`.
        assert_eq!(definitions(&t, TextObject::Class), vec![0]);
    }

    #[test]
    fn json_css_html_blocks() {
        let json = "{\"a\": [1, 2], \"b\": {\"c\": 3}}";
        let t = tree(Lang::Json, json);
        let b = text_object(&t, json, caret_in(json, "\"c\""), TextObject::Block, false).unwrap();
        assert_eq!(&json[b], "{\"c\": 3}");

        let css = "a { color: red; }\n.b { margin: 0; }\n";
        let t = tree(Lang::Css, css);
        assert_eq!(definitions(&t, TextObject::Class), vec![0, css.find(".b").unwrap()]);

        let html = "<div><p>hi</p></div>";
        let t = tree(Lang::Html, html);
        let e = text_object(&t, html, caret_in(html, "hi"), TextObject::Block, false).unwrap();
        assert_eq!(&html[e], "<p>hi</p>");
    }

    #[test]
    fn utf8_offsets_are_bytes() {
        let src = "fn café() { let s = \"é\"; }\n";
        let t = tree(Lang::Rust, src);
        let r = text_object(&t, src, caret_in(src, "let s"), TextObject::Function, false).unwrap();
        assert_eq!(&src[r], "fn café() { let s = \"é\"; }");
    }

    #[test]
    fn no_object_outside_returns_none() {
        let t = tree(Lang::Rust, RS);
        assert!(text_object(&t, RS, 0..0, TextObject::Function, false).is_none());
    }
}
