//! Pure logic behind the rich hover popup (no UI, no LSP): Markdown -> blocks, JSDoc tag
//! grouping, signature parsing and extraction of declaration bodies out of TypeScript source.
//!
//! The language servers (tsgo) only send a one-line summary for `interface` / `class` / `enum`
//! (`interface User`), so the app asks for the definition and cuts the declaration out of the file
//! with the small lexer below (it skips strings, comments and template literals, nothing more).
#![allow(dead_code)]

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use regex::Regex;

// ───────────────────────────── blocks ─────────────────────────────

/// One styled run of inline text.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: Option<String>,
}

impl Span {
    pub fn plain(t: impl Into<String>) -> Span {
        Span { text: t.into(), ..Default::default() }
    }
    pub fn code(t: impl Into<String>) -> Span {
        Span { text: t.into(), code: true, ..Default::default() }
    }
    fn same_style(&self, o: &Span) -> bool {
        self.bold == o.bold && self.italic == o.italic && self.code == o.code && self.strike == o.strike && self.link == o.link
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    pub marker: String,
    pub depth: usize,
    pub spans: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Blk {
    /// Fenced code (highlighted by the TextMate tokenizer of `lang`).
    Code { text: String, lang: String },
    Para(Vec<Span>),
    List(Vec<ListItem>),
    Quote(Vec<Span>),
    Rule,
    /// Small caps section title ("Parâmetros", "Retorna", ...).
    Label(String),
    /// `head` is TypeScript shown highlighted (`name: type`), `body` the prose, `extra` more blocks.
    Item { head: Option<String>, body: Vec<Span>, extra: Vec<Blk> },
    /// A diagnostic: severity 1 error / 2 warning / 3 info / 4 hint.
    Diag { sev: u8, msg: String, code: String, url: Option<String> },
    /// A tinted callout ("Obsoleto — use `add`").
    Note { sev: u8, title: String, body: Vec<Span> },
    /// Dim one-liner ("... mais 12 linhas").
    Dim(Vec<Span>),
    /// "Módulo   ./types"
    Kv { key: String, val: Vec<Span> },
}

fn push_text(spans: &mut Vec<Span>, mut s: Span) {
    if s.text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.same_style(&s) {
            last.text.push_str(&s.text);
            return;
        }
    }
    if spans.is_empty() && !s.code {
        s.text = s.text.trim_start().to_string();
        if s.text.is_empty() {
            return;
        }
    }
    spans.push(s);
}

/// Split plain text so bare `https://...` URLs become links.
fn linkify(base: &Span, text: &str, out: &mut Vec<Span>) {
    let re = url_re();
    let mut last = 0;
    for m in re.find_iter(text) {
        let mut end = m.end();
        while end > m.start() && matches!(text.as_bytes()[end - 1], b'.' | b',' | b';' | b':' | b'!' | b'?') {
            end -= 1;
        }
        if m.start() > last {
            push_text(out, Span { text: text[last..m.start()].to_string(), ..base.clone() });
        }
        push_text(out, Span { text: text[m.start()..end].to_string(), link: Some(text[m.start()..end].to_string()), ..base.clone() });
        last = end;
    }
    if last < text.len() {
        push_text(out, Span { text: text[last..].to_string(), ..base.clone() });
    }
}

fn url_re() -> &'static Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"https?://[^\s<>"'`)\]]+"#).unwrap())
}

fn trim_end_spans(spans: &mut Vec<Span>) {
    while let Some(l) = spans.last_mut() {
        if l.code {
            break;
        }
        let t = l.text.trim_end().to_string();
        if t.is_empty() {
            spans.pop();
        } else {
            l.text = t;
            break;
        }
    }
}

struct ListFrame {
    ordered: bool,
    n: u64,
    items: Vec<ListItem>,
    marker: String,
}

/// Markdown (the dialect language servers send in hovers) to blocks.
pub fn parse_markdown(md: &str) -> Vec<Blk> {
    let mut out: Vec<Blk> = Vec::new();
    let mut cur: Option<Vec<Span>> = None;
    let (mut bold, mut italic, mut strike) = (0u32, 0u32, 0u32);
    let mut links: Vec<String> = Vec::new();
    let mut code: Option<(String, String)> = None;
    let mut lists: Vec<ListFrame> = Vec::new();
    let mut quote: Option<Vec<Span>> = None;
    let mut heading = false;

    fn base(bold: u32, italic: u32, strike: u32, links: &[String]) -> Span {
        Span { bold: bold > 0, italic: italic > 0, strike: strike > 0, link: links.last().cloned(), ..Default::default() }
    }
    fn commit_item(lists: &mut [ListFrame], cur: &mut Option<Vec<Span>>) {
        let depth = lists.len().saturating_sub(1);
        if let (Some(f), Some(mut sp)) = (lists.last_mut(), cur.take()) {
            trim_end_spans(&mut sp);
            if !sp.is_empty() {
                f.items.push(ListItem { marker: std::mem::take(&mut f.marker), depth, spans: sp });
            }
        }
    }

    for ev in Parser::new_ext(md, Options::ENABLE_STRIKETHROUGH) {
        match ev {
            Event::Start(Tag::Paragraph) => match &mut cur {
                None => cur = Some(vec![]),
                Some(v) if !v.is_empty() => v.push(Span::plain("\n")),
                _ => {}
            },
            Event::End(TagEnd::Paragraph) => {
                if lists.is_empty() {
                    if let Some(mut sp) = cur.take() {
                        trim_end_spans(&mut sp);
                        if sp.is_empty() {
                        } else if let Some(q) = &mut quote {
                            if !q.is_empty() {
                                q.push(Span::plain("\n"));
                            }
                            q.extend(sp);
                        } else {
                            out.push(Blk::Para(sp));
                        }
                    }
                }
            }
            Event::Start(Tag::Heading { .. }) => {
                cur = Some(vec![]);
                bold += 1;
                heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                bold = bold.saturating_sub(1);
                heading = false;
                if let Some(mut sp) = cur.take() {
                    trim_end_spans(&mut sp);
                    if !sp.is_empty() {
                        out.push(Blk::Para(sp));
                    }
                }
            }
            Event::Start(Tag::BlockQuote(_)) => quote = Some(vec![]),
            Event::End(TagEnd::BlockQuote(_)) => {
                if let Some(mut q) = quote.take() {
                    trim_end_spans(&mut q);
                    if !q.is_empty() {
                        out.push(Blk::Quote(q));
                    }
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.split(|c: char| c.is_whitespace() || c == ',' || c == '{').next().unwrap_or("").to_lowercase(),
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((lang, String::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((lang, text)) = code.take() {
                    let text = text.trim_end_matches('\n').to_string();
                    if !text.trim().is_empty() {
                        // A code block closes the item text before it.
                        if !lists.is_empty() {
                            commit_item(&mut lists, &mut cur);
                        }
                        out.push(Blk::Code { text, lang });
                    }
                }
            }
            Event::Start(Tag::List(start)) => {
                if !lists.is_empty() {
                    commit_item(&mut lists, &mut cur);
                }
                lists.push(ListFrame { ordered: start.is_some(), n: start.unwrap_or(1), items: vec![], marker: String::new() });
            }
            Event::End(TagEnd::List(_)) => {
                commit_item(&mut lists, &mut cur);
                if let Some(f) = lists.pop() {
                    if let Some(p) = lists.last_mut() {
                        p.items.extend(f.items);
                    } else if !f.items.is_empty() {
                        out.push(Blk::List(f.items));
                    }
                }
            }
            Event::Start(Tag::Item) => {
                let nested = lists.len() > 1;
                if let Some(f) = lists.last_mut() {
                    f.marker = if f.ordered {
                        let m = format!("{}.", f.n);
                        f.n += 1;
                        m
                    } else if nested {
                        "◦".to_string()
                    } else {
                        "•".to_string()
                    };
                }
                cur = Some(vec![]);
            }
            Event::End(TagEnd::Item) => commit_item(&mut lists, &mut cur),
            Event::Start(Tag::Emphasis) => italic += 1,
            Event::End(TagEnd::Emphasis) => italic = italic.saturating_sub(1),
            Event::Start(Tag::Strong) => bold += 1,
            Event::End(TagEnd::Strong) => bold = bold.saturating_sub(1),
            Event::Start(Tag::Strikethrough) => strike += 1,
            Event::End(TagEnd::Strikethrough) => strike = strike.saturating_sub(1),
            Event::Start(Tag::Link { dest_url, .. }) => links.push(dest_url.to_string()),
            Event::End(TagEnd::Link) => {
                links.pop();
            }
            Event::Text(t) => {
                if let Some((_, buf)) = &mut code {
                    buf.push_str(&t);
                } else {
                    let b = base(bold, italic, strike, &links);
                    let v = cur.get_or_insert_with(Vec::new);
                    if links.is_empty() {
                        linkify(&b, &t, v);
                    } else {
                        push_text(v, Span { text: t.to_string(), ..b });
                    }
                }
            }
            Event::Code(t) => {
                let mut b = base(bold, italic, strike, &links);
                b.code = true;
                b.text = t.to_string();
                let v = cur.get_or_insert_with(Vec::new);
                push_text(v, b);
            }
            Event::SoftBreak => {
                let b = base(bold, italic, strike, &links);
                let v = cur.get_or_insert_with(Vec::new);
                if v.last().is_some_and(|l| !l.text.ends_with([' ', '\n'])) {
                    push_text(v, Span { text: " ".into(), ..b });
                }
            }
            Event::HardBreak => {
                let v = cur.get_or_insert_with(Vec::new);
                v.push(Span::plain("\n"));
            }
            Event::Html(h) | Event::InlineHtml(h) => {
                if h.trim_start().to_lowercase().starts_with("<br") {
                    cur.get_or_insert_with(Vec::new).push(Span::plain("\n"));
                }
            }
            Event::Rule => out.push(Blk::Rule),
            _ => {}
        }
    }
    let _ = heading;
    if let Some(mut sp) = cur.take() {
        trim_end_spans(&mut sp);
        if !sp.is_empty() {
            out.push(Blk::Para(sp));
        }
    }
    out
}

pub fn spans_text(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

// ───────────────────────────── JSDoc ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct JsTag {
    /// Without the `@`: "param", "returns", ...
    pub tag: String,
    pub name: Option<String>,
    pub body: Vec<Span>,
    pub extra: Vec<Blk>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct JsDoc {
    pub desc: Vec<Blk>,
    pub tags: Vec<JsTag>,
}

fn strip_sep(spans: &mut Vec<Span>) {
    if let Some(f) = spans.first_mut() {
        if !f.code {
            let t = f.text.trim_start();
            let t = t.strip_prefix('—').or_else(|| t.strip_prefix('–')).or_else(|| t.strip_prefix('-')).unwrap_or(t);
            f.text = t.trim_start().to_string();
            if f.text.is_empty() {
                spans.remove(0);
            }
        }
    }
}

/// `*@param* \`a\` — desc` (tsgo/tsserver) or `@param a - desc` (plain) -> a tag.
fn parse_tag_para(sp: &[Span]) -> Option<JsTag> {
    let first = sp.first()?;
    let (tag, mut rest): (String, Vec<Span>) = if first.italic && first.text.trim_start().starts_with('@') {
        (first.text.trim().trim_start_matches('@').to_string(), sp[1..].to_vec())
    } else if !first.code && first.text.trim_start().starts_with('@') {
        let t = first.text.trim_start();
        let word_end = t.find(char::is_whitespace).unwrap_or(t.len());
        let tag = t[1..word_end].to_string();
        let mut rest = sp.to_vec();
        rest[0].text = t[word_end..].to_string();
        if rest[0].text.is_empty() {
            rest.remove(0);
        }
        (tag, rest)
    } else {
        return None;
    };
    if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    while rest.first().is_some_and(|f| !f.code && f.text.trim().is_empty()) {
        rest.remove(0);
    }
    let mut name = None;
    if matches!(tag.as_str(), "param" | "arg" | "argument" | "template" | "typeParam" | "typeparam" | "prop" | "property") {
        if let Some(f) = rest.first_mut() {
            if f.code {
                name = Some(f.text.clone());
                rest.remove(0);
            } else {
                let t = f.text.trim_start();
                let t = if t.starts_with('{') { t.find('}').map_or(t, |e| t[e + 1..].trim_start()) } else { t };
                let end = t.find(char::is_whitespace).unwrap_or(t.len());
                if end > 0 {
                    name = Some(t[..end].to_string());
                    f.text = t[end..].to_string();
                    if f.text.is_empty() {
                        rest.remove(0);
                    }
                }
            }
        }
    }
    strip_sep(&mut rest);
    Some(JsTag { tag, name, body: rest, extra: vec![] })
}

/// Split the description from the `@tag` paragraphs; blocks after a tag (code of `@example`, more
/// paragraphs of a long `@param`) stay attached to that tag.
pub fn group_jsdoc(blocks: Vec<Blk>) -> JsDoc {
    let mut doc = JsDoc::default();
    for b in blocks {
        if let Blk::Para(sp) = &b {
            if let Some(t) = parse_tag_para(sp) {
                doc.tags.push(t);
                continue;
            }
        }
        match doc.tags.last_mut() {
            Some(t) => t.extra.push(b),
            None => doc.desc.push(b),
        }
    }
    doc
}

// ───────────────────────────── kinds & signatures ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Interface,
    Class,
    Enum,
    TypeAlias,
    Function,
    Method,
    Constructor,
    Variable,
    Property,
    Param,
    EnumMember,
    Accessor,
    Namespace,
    Module,
    TypeParam,
    Other,
}

impl Kind {
    /// The kinds whose declaration body is worth fetching with `textDocument/definition`.
    pub fn wants_def(self) -> bool {
        matches!(self, Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias | Kind::Function | Kind::Method | Kind::Variable | Kind::Constructor | Kind::Module)
    }
    pub fn wants_type_def(self) -> bool {
        matches!(self, Kind::Variable | Kind::Property | Kind::Param | Kind::Accessor)
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::Interface => "interface",
            Kind::Class => "classe",
            Kind::Enum => "enum",
            Kind::TypeAlias => "tipo",
            Kind::Function => "função",
            Kind::Method => "método",
            Kind::Constructor => "construtor",
            Kind::Variable => "variável",
            Kind::Property => "propriedade",
            Kind::Param => "parâmetro",
            Kind::EnumMember => "membro de enum",
            Kind::Accessor => "acessor",
            Kind::Namespace => "namespace",
            Kind::Module => "módulo",
            Kind::TypeParam => "parâmetro de tipo",
            Kind::Other => "",
        }
    }
}

/// What a hover signature describes, plus the declared name.
pub fn detect_kind(sig: &str) -> (Kind, String) {
    let first = sig.lines().next().unwrap_or("").trim();
    let mut s = first;
    let mut changed = true;
    while changed {
        changed = false;
        for p in ["(alias) ", "export ", "declare ", "default "] {
            if let Some(r) = s.strip_prefix(p) {
                s = r;
                changed = true;
            }
        }
    }
    let name_of = |t: &str| member_name(t);
    let tests: [(&str, Kind); 22] = [
        ("interface ", Kind::Interface),
        ("abstract class ", Kind::Class),
        ("class ", Kind::Class),
        ("const enum ", Kind::Enum),
        ("enum ", Kind::Enum),
        ("type ", Kind::TypeAlias),
        ("async function ", Kind::Function),
        ("function ", Kind::Function),
        ("namespace ", Kind::Namespace),
        ("const ", Kind::Variable),
        ("let ", Kind::Variable),
        ("var ", Kind::Variable),
        ("(property) ", Kind::Property),
        ("(method) ", Kind::Method),
        ("(parameter) ", Kind::Param),
        ("(enum member) ", Kind::EnumMember),
        ("(accessor) ", Kind::Accessor),
        ("(getter) ", Kind::Accessor),
        ("(setter) ", Kind::Accessor),
        ("constructor ", Kind::Constructor),
        ("(type parameter) ", Kind::TypeParam),
        ("(local function) ", Kind::Function),
    ];
    for (p, k) in tests {
        if let Some(r) = s.strip_prefix(p) {
            return (k, name_of(r));
        }
    }
    for p in ["(local var) ", "(local const) ", "(local let) ", "(var) ", "(const) ", "(let) "] {
        if let Some(r) = s.strip_prefix(p) {
            return (Kind::Variable, name_of(r));
        }
    }
    if let Some(r) = s.strip_prefix("module ") {
        let t = r.trim();
        return (if t.starts_with('"') || t.starts_with('\'') { Kind::Module } else { Kind::Namespace }, t.trim_matches(['"', '\'']).to_string());
    }
    (Kind::Other, String::new())
}

/// The last identifier of a possibly qualified, possibly generic name (`Repo<T>.items: T[]` -> `items`).
pub fn member_name(t: &str) -> String {
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut last = String::new();
    for c in t.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            c if c.is_alphanumeric() || c == '_' || c == '$' || c == '#' => cur.push(c),
            '.' => {
                if !cur.is_empty() {
                    last = std::mem::take(&mut cur);
                }
            }
            _ => {
                if !cur.is_empty() {
                    last = std::mem::take(&mut cur);
                }
                if matches!(c, ':' | '(' | '=' | '?') || (c.is_whitespace() && !last.is_empty()) {
                    break;
                }
            }
        }
    }
    if !cur.is_empty() {
        last = cur;
    }
    last
}

#[derive(Clone, Debug, PartialEq)]
pub struct SigParam {
    pub name: String,
    pub optional: bool,
    pub ty: String,
    pub default: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sig {
    pub name: String,
    pub params: Vec<SigParam>,
    pub ret: Option<String>,
}

/// Split at depth-0 `sep` of `()[]{}<>` (ignores `=>` and `==`).
fn split_top(s: &str, sep: char) -> Vec<String> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for (i, &c) in cs.iter().enumerate() {
        let prev = if i > 0 { cs[i - 1] } else { ' ' };
        let next = cs.get(i + 1).copied().unwrap_or(' ');
        if let Some(q) = quote {
            cur.push(c);
            if c == q && prev != '\\' {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '>' if prev != '=' => depth -= 1,
            _ => {}
        }
        let is_eq_op = sep == '=' && c == '=' && (next == '>' || next == '=' || matches!(prev, '=' | '!' | '<' | '>'));
        if c == sep && depth == 0 && !is_eq_op {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Parse `function f<T>(a: number, b?: string): void`, `(method) A.f(x: T): U`, `constructor X(...)`.
pub fn parse_sig(sig: &str) -> Option<Sig> {
    let mut s = sig.trim();
    // Only the first statement: hovers add `import x` or overload counters on later lines.
    if let Some(i) = s.find("\nimport ") {
        s = &s[..i];
    }
    let mut changed = true;
    while changed {
        changed = false;
        for p in ["(alias) ", "(method) ", "(local function) ", "export ", "declare ", "async ", "function ", "constructor ", "new ", "static ", "abstract ", "get ", "set "] {
            if let Some(r) = s.strip_prefix(p) {
                s = r.trim_start();
                changed = true;
            }
        }
    }
    // Name (possibly qualified / generic) up to the `(` of the parameter list.
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut depth = 0;
    while i < b.len() {
        match b[i] {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            '(' if depth == 0 => break,
            ':' | '=' | '{' if depth == 0 => return None,
            _ => {}
        }
        i += 1;
    }
    if i >= b.len() {
        return None;
    }
    let head: String = b[..i].iter().collect();
    let name = member_name(&head);
    // Matching `)`.
    let mut d = 0;
    let mut j = i;
    let mut close = None;
    let mut prev = ' ';
    while j < b.len() {
        match b[j] {
            '(' | '[' | '{' => d += 1,
            ')' | ']' | '}' => {
                d -= 1;
                if d == 0 {
                    close = Some(j);
                    break;
                }
            }
            _ => {}
        }
        prev = b[j];
        j += 1;
    }
    let _ = prev;
    let close = close?;
    let inner: String = b[i + 1..close].iter().collect();
    let rest: String = b[close + 1..].iter().collect();
    let ret = rest.trim().strip_prefix(':').or_else(|| rest.trim().strip_prefix("=>")).map(|r| r.trim().to_string()).filter(|r| !r.is_empty());
    let mut params = Vec::new();
    for p in split_top(&inner, ',') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        let (decl, default) = match split_top(p, '=').as_slice() {
            [a, rest @ ..] if !rest.is_empty() && !a.trim_end().ends_with(':') => (a.trim().to_string(), Some(rest.join("=").trim().to_string())),
            _ => (p.to_string(), None),
        };
        let (n, ty) = match decl.find(':') {
            Some(k) if !decl.starts_with('{') && !decl.starts_with('[') => (decl[..k].trim().to_string(), decl[k + 1..].trim().to_string()),
            _ => {
                // destructuring: `{ a, b }: T`
                match split_top(&decl, ':').as_slice() {
                    [a, t] => (a.trim().to_string(), t.trim().to_string()),
                    _ => (decl.clone(), String::new()),
                }
            }
        };
        let optional = n.ends_with('?');
        params.push(SigParam { name: n.trim_end_matches('?').to_string(), optional: optional || default.is_some(), ty, default });
    }
    Some(Sig { name, params, ret })
}

// ───────────────────────────── source lexer ─────────────────────────────

/// Walks the code bytes of TypeScript source, skipping comments, strings and template literals.
struct Lx<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Lx<'a> {
    fn new(text: &'a str, at: usize) -> Self {
        Lx { b: text.as_bytes(), i: at }
    }
    /// The next code byte and its offset.
    fn next(&mut self) -> Option<(usize, u8)> {
        let b = self.b;
        while self.i < b.len() {
            let c = b[self.i];
            match c {
                b'/' if b.get(self.i + 1) == Some(&b'/') => {
                    while self.i < b.len() && b[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                b'/' if b.get(self.i + 1) == Some(&b'*') => {
                    self.i += 2;
                    while self.i < b.len() && !(b[self.i] == b'*' && b.get(self.i + 1) == Some(&b'/')) {
                        self.i += 1;
                    }
                    self.i = (self.i + 2).min(b.len());
                }
                b'"' | b'\'' => {
                    self.i += 1;
                    while self.i < b.len() && b[self.i] != c && b[self.i] != b'\n' {
                        if b[self.i] == b'\\' {
                            self.i += 1;
                        }
                        self.i += 1;
                    }
                    self.i += 1;
                }
                b'`' => {
                    self.i += 1;
                    while self.i < b.len() && b[self.i] != b'`' {
                        if b[self.i] == b'\\' {
                            self.i += 1;
                        }
                        self.i += 1;
                    }
                    self.i += 1;
                }
                _ => {
                    let at = self.i;
                    self.i += 1;
                    return Some((at, c));
                }
            }
        }
        None
    }
}

/// Offset of the `}` matching the `{` at `open`.
pub fn find_close(text: &str, open: usize) -> Option<usize> {
    let mut lx = Lx::new(text, open);
    let mut depth = 0i32;
    while let Some((at, c)) = lx.next() {
        match c {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

fn line_start(text: &str, off: usize) -> usize {
    text[..off.min(text.len())].rfind('\n').map_or(0, |i| i + 1)
}

fn prev_sig(text: &str, upto: usize) -> Option<u8> {
    text.as_bytes()[..upto].iter().rev().copied().find(|c| !c.is_ascii_whitespace())
}

fn next_sig(text: &str, from: usize) -> Option<u8> {
    text.as_bytes().get(from..)?.iter().copied().find(|c| !c.is_ascii_whitespace())
}

const MAX_STMT: usize = 6000;

/// End (exclusive) of the statement beginning at `from`: a depth-0 `;`, or a newline that does not
/// continue the expression. With `fn_header`, a `{` that opens a body ends it (before the brace).
/// The bool is true when the statement ended at a `;` (a signature without body).
fn stmt_end(text: &str, from: usize, fn_header: bool) -> (usize, bool) {
    let mut lx = Lx::new(text, from);
    let mut depth = 0i32;
    let mut limit = (from + MAX_STMT).min(text.len());
    while !text.is_char_boundary(limit) {
        limit -= 1;
    }
    while let Some((at, c)) = lx.next() {
        if at > limit {
            return (limit, false);
        }
        match c {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'{' => {
                if fn_header && depth == 0 {
                    let before = text[..at].trim_end();
                    let is_body = if before.ends_with("=>") { false } else { !matches!(before.as_bytes().last(), Some(b':' | b'|' | b'&' | b'<' | b',' | b'=' | b'(' | b'?')) };
                    if is_body {
                        return (at, false);
                    }
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth < 0 {
                    return (at, false);
                }
            }
            b';' if depth == 0 => return (at + 1, true),
            b'\n' if depth == 0 => {
                let p = prev_sig(text, at);
                let arrow = at >= 2 && text[..at].trim_end().ends_with("=>");
                let n = next_sig(text, at);
                let cont_prev = matches!(p, Some(b'=' | b'|' | b'&' | b'?' | b':' | b',' | b'(' | b'[' | b'<' | b'+' | b'-' | b'*' | b'/' | b'%' | b'.')) || arrow;
                let cont_next = matches!(n, Some(b'|' | b'&' | b'?' | b':' | b'.' | b')' | b']'));
                if !(cont_prev || cont_next) {
                    return (at, false);
                }
            }
            _ => {}
        }
    }
    (text.len(), false)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Located {
    pub start: usize,
    pub end: usize,
    /// Offset of the `{` of the body (interface / class / enum), if any.
    pub open: Option<usize>,
}

/// Keyword kind of the declaration whose name starts at `name_off` (`export interface |User`).
pub fn decl_kind_at(text: &str, name_off: usize) -> Option<Kind> {
    let ls = line_start(text, name_off);
    let before = text.get(ls..name_off)?.trim();
    let mut s = before;
    loop {
        let mut ch = false;
        for p in ["export ", "default ", "declare ", "abstract "] {
            if let Some(r) = s.strip_prefix(p) {
                s = r.trim_start();
                ch = true;
            }
        }
        if !ch {
            break;
        }
    }
    match s {
        "interface" => Some(Kind::Interface),
        "class" => Some(Kind::Class),
        "enum" | "const enum" => Some(Kind::Enum),
        "type" => Some(Kind::TypeAlias),
        "function" | "async function" | "function*" => Some(Kind::Function),
        "const" | "let" | "var" => Some(Kind::Variable),
        _ => None,
    }
}

/// Where the declaration of `kind` whose name is at `name_off` starts and ends.
pub fn decl_extent(text: &str, name_off: usize, kind: Kind) -> Option<Located> {
    let start = line_start(text, name_off);
    match kind {
        Kind::Interface | Kind::Class | Kind::Enum | Kind::Namespace => {
            let mut lx = Lx::new(text, name_off);
            let mut depth = 0i32;
            let mut angle = 0i32;
            while let Some((at, c)) = lx.next() {
                match c {
                    b'<' => angle += 1,
                    b'>' if angle > 0 && !text[..at].ends_with('=') => angle -= 1,
                    b'(' | b'[' => depth += 1,
                    b')' | b']' => depth -= 1,
                    b'{' if depth == 0 && angle <= 0 => {
                        let close = find_close(text, at)?;
                        return Some(Located { start, end: close + 1, open: Some(at) });
                    }
                    b'{' if depth == 0 => {
                        // `{` inside generics (`<T extends { a: 1 }>`): skip the block.
                        let close = find_close(text, at)?;
                        lx.i = close + 1;
                    }
                    b';' if depth == 0 => return None,
                    _ => {}
                }
                if at > name_off + MAX_STMT * 20 {
                    return None;
                }
            }
            None
        }
        Kind::TypeAlias => {
            let (end, _) = stmt_end(text, name_off, false);
            Some(Located { start, end, open: None })
        }
        Kind::Function | Kind::Method | Kind::Constructor => {
            let (end, _) = stmt_end(text, name_off, true);
            Some(Located { start, end, open: None })
        }
        Kind::Variable => {
            let (end, _) = stmt_end(text, name_off, false);
            Some(Located { start, end, open: None })
        }
        _ => None,
    }
}

/// Remove the common indentation.
pub fn dedent(s: &str) -> String {
    let ind = s.lines().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
    s.lines().map(|l| if l.len() >= ind { &l[ind..] } else { l.trim_start() }).collect::<Vec<_>>().join("\n")
}

/// Keep at most `max` lines; the second value is how many were cut.
pub fn truncate_lines(s: &str, max: usize) -> (String, usize) {
    let n = s.lines().count();
    if n <= max {
        return (s.to_string(), 0);
    }
    (s.lines().take(max).collect::<Vec<_>>().join("\n"), n - max)
}

// ───────────────────────────── members ─────────────────────────────

/// Byte ranges of the members of a `{ ... }` body (class, interface, enum, type literal).
fn split_members(body: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut lx = Lx::new(body, 0);
    let (mut brace, mut paren, mut angle) = (0i32, 0i32, 0i32);
    let mut start = 0usize;
    let mut prev = 0u8;
    let push = |s: usize, e: usize, out: &mut Vec<(usize, usize)>| {
        if !body[s..e].trim().is_empty() {
            out.push((s, e));
        }
    };
    while let Some((at, c)) = lx.next() {
        match c {
            b'{' => brace += 1,
            b'}' => {
                brace -= 1;
                if brace == 0 && paren == 0 && angle <= 0 {
                    let n = next_sig(body, at + 1);
                    if !matches!(n, Some(b';' | b',' | b'|' | b'&' | b'=' | b'.' | b'?' | b':' | b')' | b'>')) {
                        push(start, at + 1, &mut out);
                        start = at + 1;
                    }
                }
            }
            b'(' | b'[' => paren += 1,
            b')' | b']' => paren -= 1,
            b'<' if brace == 0 && paren == 0 && (prev.is_ascii_alphanumeric() || prev == b'>' || prev == b'_') => angle += 1,
            b'>' if brace == 0 && paren == 0 && angle > 0 && prev != b'=' => angle -= 1,
            b';' | b',' if brace == 0 && paren == 0 && angle <= 0 => {
                push(start, at + 1, &mut out);
                start = at + 1;
                angle = 0;
            }
            _ => {}
        }
        if !c.is_ascii_whitespace() {
            prev = c;
        }
    }
    if start < body.len() {
        push(start, body.len(), &mut out);
    }
    out
}

/// The first line of a `/** ... */` at the start of `s`, and the rest after the comment.
fn leading_doc(s: &str) -> (Option<String>, &str) {
    let t = s.trim_start();
    if let Some(r) = t.strip_prefix("/**") {
        if let Some(e) = r.find("*/") {
            let line = r[..e].lines().map(|l| l.trim().trim_start_matches('*').trim()).find(|l| !l.is_empty() && !l.starts_with('@')).map(str::to_string);
            return (line, r[e + 2..].trim_start());
        }
    }
    (None, t)
}

fn strip_leading_comments(mut s: &str) -> &str {
    loop {
        let t = s.trim_start();
        if let Some(r) = t.strip_prefix("//") {
            s = r.split_once('\n').map_or("", |x| x.1);
        } else if t.starts_with("/*") {
            match t.find("*/") {
                Some(e) => s = &t[e + 2..],
                None => return "",
            }
        } else {
            return t;
        }
    }
}

fn strip_decorators(mut s: &str) -> &str {
    loop {
        let t = s.trim_start();
        if t.starts_with('@') {
            let mut i = 1;
            let b = t.as_bytes();
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
                i += 1;
            }
            if b.get(i) == Some(&b'(') {
                match find_close(t, i) {
                    Some(c) => i = c + 1,
                    None => return t,
                }
            }
            s = &t[i..];
        } else {
            return t;
        }
    }
}

const MODS: [&str; 11] = ["public", "private", "protected", "static", "readonly", "abstract", "override", "async", "declare", "accessor", "export"];

/// A class body reduced to its public surface: signatures without bodies or initializers.
pub fn class_skeleton(body: &str) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut hidden = 0usize;
    for (s, e) in split_members(body) {
        let m = &body[s..e];
        let (doc, rest) = leading_doc(m);
        let rest = strip_decorators(strip_leading_comments(rest));
        if rest.is_empty() {
            continue;
        }
        // modifiers
        let mut r = rest;
        let mut private = false;
        loop {
            let w: String = r.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
            if MODS.contains(&w.as_str()) && r[w.len()..].starts_with(char::is_whitespace) {
                if w == "private" {
                    private = true;
                }
                r = r[w.len()..].trim_start();
            } else {
                break;
            }
        }
        if private || r.starts_with('#') {
            hidden += 1;
            continue;
        }
        let rest = rest.trim_end().trim_end_matches([';', ',']).trim_end();
        // body of a method / accessor: cut before the opening brace
        let (hend, _) = stmt_end(rest, 0, true);
        let mut head = rest[..hend.min(rest.len())].trim_end().trim_end_matches([';', ',']).to_string();
        // initializer of a property
        if !head.contains('(') || head.find('=').is_some_and(|i| head.find('(').is_none_or(|p| i < p)) {
            let parts = split_top(&head, '=');
            if parts.len() > 1 && !parts[0].trim_end().ends_with(':') {
                let init = parts[1..].join("=");
                let has_ty = parts[0].contains(':');
                head = if has_ty || init.trim().chars().count() > 30 || init.contains('\n') { parts[0].trim_end().to_string() } else { format!("{} = {}", parts[0].trim_end(), init.trim()) };
            }
        }
        let head = head.split_whitespace().collect::<Vec<_>>().join(" ");
        if let Some(d) = doc {
            lines.push(format!("/** {d} */"));
        }
        lines.push(format!("{head};"));
    }
    (lines, hidden)
}

/// `Name`, `Name = expr` members of an enum with the implicit numeric values filled in as comments.
pub fn enum_members(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut next: Option<i64> = Some(0);
    for (s, e) in split_members(body) {
        let m = &body[s..e];
        let (doc, rest) = leading_doc(m);
        let rest = strip_leading_comments(rest).trim_end().trim_end_matches(',').trim().to_string();
        if rest.is_empty() {
            continue;
        }
        if let Some(d) = doc {
            out.push(format!("/** {d} */"));
        }
        match rest.split_once('=') {
            Some((n, v)) => {
                let v = v.trim();
                next = parse_int(v).map(|x| x + 1);
                out.push(format!("{} = {v},", n.trim()));
            }
            None => match next {
                Some(n) => {
                    out.push(format!("{rest}, // = {n}"));
                    next = Some(n + 1);
                }
                None => out.push(format!("{rest},")),
            },
        }
    }
    out
}

fn parse_int(v: &str) -> Option<i64> {
    let v = v.trim();
    let (neg, v) = match v.strip_prefix('-') {
        Some(r) => (true, r.trim()),
        None => (false, v),
    };
    let n = if let Some(h) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) { i64::from_str_radix(&h.replace('_', ""), 16).ok()? } else { v.replace('_', "").parse::<i64>().ok()? };
    Some(if neg { -n } else { n })
}

/// The declaration at `name_off` as the text to show (dedented), and how many lines were cut.
pub fn render_decl(text: &str, name_off: usize, kind: Kind, max_lines: usize) -> Option<(String, usize)> {
    let loc = decl_extent(text, name_off, kind)?;
    let src = &text[loc.start..loc.end];
    let out = match (kind, loc.open) {
        (Kind::Class, Some(open)) => {
            let head = text[loc.start..open].trim_end();
            let body = &text[open + 1..loc.end - 1];
            let (lines, hidden) = class_skeleton(body);
            let mut s = format!("{} {{\n", dedent_first(head));
            for l in &lines {
                s.push_str("  ");
                s.push_str(l);
                s.push('\n');
            }
            if hidden > 0 {
                s.push_str(&format!("  // + {hidden} {} privado{}\n", if hidden == 1 { "membro" } else { "membros" }, if hidden == 1 { "" } else { "s" }));
            }
            s.push('}');
            s
        }
        (Kind::Enum, Some(open)) => {
            let head = text[loc.start..open].trim_end();
            let body = &text[open + 1..loc.end - 1];
            let mut s = format!("{} {{\n", dedent_first(head));
            for l in enum_members(body) {
                s.push_str("  ");
                s.push_str(&l);
                s.push('\n');
            }
            s.push('}');
            s
        }
        (Kind::TypeAlias, _) => pretty_union(&dedent(src.trim_end().trim_end_matches(';').trim_end())),
        _ => dedent(src.trim_end().trim_end_matches(';').trim_end()),
    };
    Some(truncate_lines(&out, max_lines))
}

/// A one-line union with many members is broken into one `| member` per line.
pub fn pretty_union(s: &str) -> String {
    if s.contains('\n') || s.chars().count() < 60 {
        return s.to_string();
    }
    let Some(eq) = split_top(s, '=').first().map(|h| h.len()) else { return s.to_string() };
    if eq >= s.len() {
        return s.to_string();
    }
    let (head, body) = (s[..eq].trim_end(), s[eq + 1..].trim());
    let parts = split_top(body, '|');
    if parts.len() < 3 {
        return s.to_string();
    }
    let mut out = format!("{head} =");
    for p in parts {
        out.push_str("\n  | ");
        out.push_str(p.trim());
    }
    out
}

fn dedent_first(s: &str) -> String {
    s.trim_start().to_string()
}

// ───────────────────────────── overloads, values, imports ─────────────────────────────

/// Signatures of the overload set `name` belongs to (consecutive declarations without body).
pub fn find_overloads(text: &str, name: &str, def_off: usize) -> Vec<String> {
    if name.is_empty() {
        return vec![];
    }
    let pat = format!(r"(?m)^[ \t]*(?:(?:export|default|declare|public|private|protected|static|abstract|async|override)\s+)*(?:function\s+)?{}\s*[<(?]", regex::escape(name));
    let Ok(re) = Regex::new(&pat) else { return vec![] };
    struct M {
        start: usize,
        end: usize,
        sig: bool,
    }
    let mut ms: Vec<M> = Vec::new();
    for m in re.find_iter(text).take(400) {
        let (end, semi) = stmt_end(text, m.start(), true);
        ms.push(M { start: m.start(), end, sig: semi });
    }
    let Some(k) = ms.iter().position(|m| m.start <= def_off && def_off <= m.end) else { return vec![] };
    let gap_ok = |a: usize, b: usize| -> bool {
        if a > b {
            return false;
        }
        let mut lx = Lx::new(&text[a..b], 0);
        while let Some((_, c)) = lx.next() {
            if !c.is_ascii_whitespace() {
                return false;
            }
        }
        true
    };
    let (mut lo, mut hi) = (k, k);
    while lo > 0 && gap_ok(ms[lo - 1].end, ms[lo].start) {
        lo -= 1;
    }
    while hi + 1 < ms.len() && gap_ok(ms[hi].end, ms[hi + 1].start) {
        hi += 1;
    }
    let sigs: Vec<String> = ms[lo..=hi]
        .iter()
        .filter(|m| m.sig)
        .map(|m| {
            let s = text[m.start..m.end].trim().trim_end_matches([';', ',']);
            let mut s = s.split_whitespace().collect::<Vec<_>>().join(" ");
            for p in ["export ", "declare ", "default "] {
                while let Some(r) = s.strip_prefix(p) {
                    s = r.to_string();
                }
            }
            s
        })
        .collect();
    if sigs.len() >= 2 { sigs } else { vec![] }
}

/// Short single-line initializer of `const x = <value>` whose name is at `name_off`.
pub fn short_value(text: &str, name_off: usize) -> Option<String> {
    let loc = decl_extent(text, name_off, Kind::Variable)?;
    let src = &text[name_off..loc.end];
    let parts = split_top_eq(src)?;
    let v = parts.trim().trim_end_matches(';').trim();
    if v.is_empty() || v.contains('\n') || v.chars().count() > 60 {
        return None;
    }
    let low = v.trim_start_matches("async ").trim_start();
    if v.contains("=>") || low.starts_with("function") || low.starts_with("class") {
        return None;
    }
    Some(v.to_string())
}

/// Text after the first top-level `=` (not `=>`, `==`, `>=`, `<=`).
fn split_top_eq(s: &str) -> Option<&str> {
    let mut lx = Lx::new(s, 0);
    let mut depth = 0i32;
    let b = s.as_bytes();
    while let Some((at, c)) = lx.next() {
        match c {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' | b'>' => {
                if c != b'>' || at == 0 || b[at - 1] != b'=' {
                    depth -= 1;
                }
            }
            b'=' if depth <= 0 && b.get(at + 1) != Some(&b'>') && b.get(at + 1) != Some(&b'=') && at > 0 && !matches!(b[at - 1], b'=' | b'!' | b'<' | b'>') => {
                return Some(&s[at + 1..]);
            }
            _ => {}
        }
    }
    None
}

/// The module specifier of the import / export-from / require statement that contains line `idx`:
/// (specifier, line, char column of the first char inside the quotes).
pub fn import_spec(lines: &[String], idx: usize) -> Option<(String, usize, usize)> {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"(?:\bfrom\s*|\bimport\s*\(?\s*|\brequire\s*\(\s*)(['"])([^'"]+)['"]"#).unwrap());
    let find = |l: usize| -> Option<(String, usize, usize)> {
        let line = lines.get(l)?;
        let c = re.captures(line)?;
        let m = c.get(2)?;
        Some((m.as_str().to_string(), l, line[..m.start()].chars().count()))
    };
    if let Some(r) = find(idx) {
        return Some(r);
    }
    // multi-line `import {\n a,\n b\n} from "x"`
    let mut start = None;
    for j in (idx.saturating_sub(60)..idx).rev() {
        let t = lines.get(j)?.trim_start();
        if t.starts_with("import ") || t.starts_with("export ") || t.starts_with("import{") || t.starts_with("export{") {
            start = Some(j);
            break;
        }
        if t.ends_with(';') || t.ends_with('}') && !t.starts_with('}') {
            return None;
        }
    }
    let j = start?;
    if (j..=idx).any(|l| re.is_match(&lines[l])) {
        return None;
    }
    (idx..(idx + 60).min(lines.len())).find_map(find)
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_blocks_inline_and_lists() {
        let md = "```typescript\nfunction f(): void\n```\nSome **bold** and *it* and `code` and [link](http://x.y/z).\n\n- one\n- two\n  - nested\n\n1. a\n2. b\n\n---\n> quote";
        let b = parse_markdown(md);
        assert!(matches!(&b[0], Blk::Code { lang, text } if lang == "typescript" && text == "function f(): void"));
        let Blk::Para(sp) = &b[1] else { panic!("{b:?}") };
        assert!(sp.iter().any(|s| s.bold && s.text == "bold"));
        assert!(sp.iter().any(|s| s.italic && s.text == "it"));
        assert!(sp.iter().any(|s| s.code && s.text == "code"));
        assert!(sp.iter().any(|s| s.link.as_deref() == Some("http://x.y/z") && s.text == "link"));
        let Blk::List(it) = &b[2] else { panic!("{b:?}") };
        assert_eq!(it.len(), 3);
        assert_eq!((it[0].marker.as_str(), it[0].depth), ("•", 0));
        assert_eq!((it[2].marker.as_str(), it[2].depth), ("◦", 1));
        let Blk::List(ol) = &b[3] else { panic!("{b:?}") };
        assert_eq!(ol[1].marker, "2.");
        assert!(matches!(b[4], Blk::Rule));
        assert!(matches!(b[5], Blk::Quote(_)));
    }

    #[test]
    fn bare_urls_become_links() {
        let b = parse_markdown("veja https://example.com/docs. fim");
        let Blk::Para(sp) = &b[0] else { panic!() };
        let l = sp.iter().find(|s| s.link.is_some()).unwrap();
        assert_eq!(l.text, "https://example.com/docs");
    }

    #[test]
    fn jsdoc_tags_from_tsgo_markdown() {
        let md = "Soma dois valores.\n\n*@param* `a` — primeiro valor\n\n*@param* `b` — segundo, veja [Role](file:///x.ts#18,13-18,17)\n\n*@returns* — a soma\n\n*@example*\n```ts\nsum(1, 2)\n```\n\n\n*@deprecated* — use `add`\n\n*@see* — https://example.com/docs";
        let d = group_jsdoc(parse_markdown(md));
        assert_eq!(d.desc.len(), 1);
        let t: Vec<&str> = d.tags.iter().map(|t| t.tag.as_str()).collect();
        assert_eq!(t, ["param", "param", "returns", "example", "deprecated", "see"]);
        assert_eq!(d.tags[0].name.as_deref(), Some("a"));
        assert_eq!(spans_text(&d.tags[0].body), "primeiro valor");
        assert!(d.tags[1].body.iter().any(|s| s.link.as_deref().is_some_and(|l| l.starts_with("file://"))));
        assert_eq!(spans_text(&d.tags[2].body), "a soma");
        assert!(matches!(&d.tags[3].extra[0], Blk::Code { text, .. } if text == "sum(1, 2)"));
        assert_eq!(spans_text(&d.tags[4].body), "use add");
        assert!(d.tags[5].body.iter().any(|s| s.link.is_some()));
    }

    #[test]
    fn jsdoc_plain_text_tags() {
        let d = group_jsdoc(parse_markdown("Desc\n\n@param x - the value\n\n@returns nothing"));
        assert_eq!(d.tags[0].name.as_deref(), Some("x"));
        assert_eq!(spans_text(&d.tags[0].body), "the value");
        assert_eq!(d.tags[1].tag, "returns");
    }

    #[test]
    fn kinds_and_names() {
        assert_eq!(detect_kind("interface User"), (Kind::Interface, "User".into()));
        assert_eq!(detect_kind("(alias) interface User\nimport User"), (Kind::Interface, "User".into()));
        assert_eq!(detect_kind("(property) Repo<T extends Base>.items: T[]"), (Kind::Property, "items".into()));
        assert_eq!(detect_kind("(method) User.greet(greeting: string, loud?: boolean): string"), (Kind::Method, "greet".into()));
        assert_eq!(detect_kind("(enum member) Role.Editor = 5"), (Kind::EnumMember, "Editor".into()));
        assert_eq!(detect_kind("const limit: 42"), (Kind::Variable, "limit".into()));
        assert_eq!(detect_kind("module \"./types\"").0, Kind::Module);
        assert_eq!(detect_kind("(alias) constructor Repo<User>(name: string): Repo<User>").0, Kind::Constructor);
        assert_eq!(detect_kind("type Status = \"a\" | \"b\"").0, Kind::TypeAlias);
        assert_eq!(detect_kind("(parameter) a: any"), (Kind::Param, "a".into()));
    }

    #[test]
    fn signature_parsing() {
        let s = parse_sig("function sum(a: number, b?: string, ...rest: Array<[number, string]>): number").unwrap();
        assert_eq!(s.name, "sum");
        assert_eq!(s.params.len(), 3);
        assert_eq!((s.params[1].name.as_str(), s.params[1].optional, s.params[1].ty.as_str()), ("b", true, "string"));
        assert_eq!(s.params[2].name, "...rest");
        assert_eq!(s.params[2].ty, "Array<[number, string]>");
        assert_eq!(s.ret.as_deref(), Some("number"));
        let m = parse_sig("(method) Repo<User>.add(item: User, cb: (x: number) => void): void").unwrap();
        assert_eq!((m.name.as_str(), m.params.len()), ("add", 2));
        assert_eq!(m.params[1].ty, "(x: number) => void");
        let g = parse_sig("function map<T, U>(xs: T[], f: (x: T) => U = id): U[]").unwrap();
        assert_eq!(g.params[1].default.as_deref(), Some("id"));
        assert!(g.params[1].optional);
        assert!(parse_sig("const f: (a: number) => void").is_none());
        assert!(parse_sig("function f()").unwrap().params.is_empty());
    }

    const SRC: &str = r#"import x from "y";

/** A user. */
export interface User extends Base {
  /** id */
  id: number;
  address: { street: string; city: string };
  greet(greeting: string, loud?: boolean): string;
}

export enum Role {
  Admin,
  Editor = 5,
  Viewer,
  Big = 0x10,
  After,
}

export type Status = "idle" | "loading"
  | "error"
  | { code: number; message: string };

export type Pair<T> = [T, T];

export class Repo<T extends Base> extends Map<string, T> {
  private items: T[] = [];
  #secret = 1;
  static count = 0;
  public readonly name: string;
  cache: Map<string, number> = new Map();
  constructor(name: string, private limit = 10) {
    super();
    this.name = name;
  }
  /** Adds an item. */
  add(item: T): void {
    if (a < b) { this.items.push(item); }
  }
  get size(): number {
    return this.items.length;
  }
}

export function sum(a: number, b: number): number;
export function sum(a: string, b: string): string;
export function sum(a: any, b: any): any {
  return a + b;
}

const limit = 42;
export const label = "hello" + `x${1}`;
const fn = (a: number) => a + 1;
const obj = {
  a: 1,
};
"#;

    fn off(name: &str) -> usize {
        SRC.find(name).unwrap()
    }

    #[test]
    fn interface_declaration_is_extracted_whole() {
        let at = off("User extends") ;
        assert_eq!(decl_kind_at(SRC, at), Some(Kind::Interface));
        let (t, more) = render_decl(SRC, at, Kind::Interface, 40).unwrap();
        assert!(t.starts_with("export interface User extends Base {"), "{t}");
        assert!(t.contains("  address: { street: string; city: string };"));
        assert!(t.ends_with("}"));
        assert_eq!(more, 0);
        let (t2, more2) = render_decl(SRC, at, Kind::Interface, 3).unwrap();
        assert_eq!(t2.lines().count(), 3);
        assert_eq!(more2, 3);
    }

    #[test]
    fn enum_members_get_values() {
        let (t, _) = render_decl(SRC, off("Role {"), Kind::Enum, 40).unwrap();
        assert!(t.contains("Admin, // = 0"), "{t}");
        assert!(t.contains("Editor = 5,"));
        assert!(t.contains("Viewer, // = 6"));
        assert!(t.contains("Big = 0x10,"));
        assert!(t.contains("After, // = 17"), "{t}");
    }

    #[test]
    fn type_alias_multiline_union() {
        let (t, _) = render_decl(SRC, off("Status ="), Kind::TypeAlias, 40).unwrap();
        assert!(t.starts_with("export type Status = \"idle\" | \"loading\""), "{t}");
        assert!(t.contains("| { code: number; message: string }"));
        assert!(!t.contains("Pair"));
        let (p, _) = render_decl(SRC, off("Pair<T>"), Kind::TypeAlias, 40).unwrap();
        assert_eq!(p, "export type Pair<T> = [T, T]");
    }

    #[test]
    fn class_skeleton_hides_bodies_and_private() {
        let (t, _) = render_decl(SRC, off("Repo<T extends Base> extends"), Kind::Class, 40).unwrap();
        assert!(t.starts_with("export class Repo<T extends Base> extends Map<string, T> {"), "{t}");
        assert!(t.contains("  static count = 0;"), "{t}");
        assert!(t.contains("  public readonly name: string;"));
        assert!(t.contains("  cache: Map<string, number>;"), "{t}");
        assert!(t.contains("  constructor(name: string, private limit = 10);"), "{t}");
        assert!(t.contains("  /** Adds an item. */"));
        assert!(t.contains("  add(item: T): void;"));
        assert!(t.contains("  get size(): number;"));
        assert!(!t.contains("items.push") && !t.contains("#secret") && !t.contains("private items"));
        assert!(t.contains("// + 2 membros privados"), "{t}");
    }

    #[test]
    fn overloads_are_collected() {
        let at = off("sum(a: string");
        let o = find_overloads(SRC, "sum", at);
        assert_eq!(o, ["function sum(a: number, b: number): number", "function sum(a: string, b: string): string"]);
        // the implementation (with body) is not listed; the def can sit on it
        let at2 = off("sum(a: any");
        assert_eq!(find_overloads(SRC, "sum", at2).len(), 2);
        assert!(find_overloads(SRC, "limit", off("limit = 42")).is_empty());
    }

    #[test]
    fn short_values() {
        assert_eq!(short_value(SRC, off("limit = 42")).as_deref(), Some("42"));
        assert_eq!(short_value(SRC, off("label =")).as_deref(), Some("\"hello\" + `x${1}`"));
        assert_eq!(short_value(SRC, off("fn =")), None);
        assert_eq!(short_value(SRC, off("obj =")), None, "multi-line object");
    }

    #[test]
    fn non_ascii_sources_never_panic() {
        let t = format!("export interface Usuário {{\n  /** descrição — ação */\n  nome: string; // coração\n  tag: \"é\";\n}}\n\nexport type Longo = {};\n", "\"á\" | ".repeat(2000) + "\"z\"");
        let at = t.find("Usuário").unwrap();
        let (d, _) = render_decl(&t, at, Kind::Interface, 40).unwrap();
        assert!(d.contains("nome: string; // coração"));
        let at = t.find("Longo").unwrap();
        assert!(render_decl(&t, at, Kind::TypeAlias, 40).is_some());
        assert!(find_overloads(&t, "nome", t.find("nome").unwrap()).is_empty());
    }

    #[test]
    fn lexer_ignores_strings_and_comments() {
        let t = "interface A { a: \"}\"; /* } */ b: `}`; // }\n c: 1 }";
        let open = t.find('{').unwrap();
        assert_eq!(find_close(t, open), Some(t.len() - 1));
    }

    #[test]
    fn import_specifiers() {
        let l = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let one = l(&["import { A } from \"./types\";"]);
        assert_eq!(import_spec(&one, 0), Some(("./types".into(), 0, 19)));
        let multi = l(&["import {", "  A,", "  B,", "} from 'pkg/sub';", "const x = 1;"]);
        assert_eq!(import_spec(&multi, 1).unwrap().0, "pkg/sub");
        assert_eq!(import_spec(&multi, 4), None);
        let req = l(&["const fs = require(\"node:fs\");"]);
        assert_eq!(import_spec(&req, 0).unwrap().0, "node:fs");
    }

    #[test]
    fn long_unions_are_broken_up() {
        let s = "export type Status = \"idle\" | \"loading\" | \"error\" | { code: number; message: string }";
        assert_eq!(pretty_union(s), "export type Status =\n  | \"idle\"\n  | \"loading\"\n  | \"error\"\n  | { code: number; message: string }");
        assert_eq!(pretty_union("type A = 1 | 2"), "type A = 1 | 2");
        assert_eq!(pretty_union("export type Fn = (a: number, b: string) => Promise<Array<string | number | boolean>>"), "export type Fn = (a: number, b: string) => Promise<Array<string | number | boolean>>");
    }

    #[test]
    fn truncation_and_dedent() {
        assert_eq!(dedent("    a\n      b\n    c"), "a\n  b\nc");
        assert_eq!(truncate_lines("a\nb\nc", 2), ("a\nb".to_string(), 1));
    }
}
