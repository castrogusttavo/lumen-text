//! Ordered value tree for JSON and YAML documents, with the source line of every node.
//!
//! `serde_json::Value` would sort object keys (no `preserve_order`), which scrambles the order
//! of paths and properties, and it has no line numbers (the lint list jumps to the source). The
//! parser here reads JSON and the YAML subset OpenAPI files use: block maps and sequences,
//! plain / quoted scalars, `|` and `>` block scalars, flow collections, comments, `---`.
//! Anchors, aliases, tags and merge keys are not supported.

use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Node>),
    Map(Vec<(String, Node)>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub val: Val,
    /// 0-based line of the node (of its key, for a map value).
    pub line: usize,
}

impl Node {
    pub fn new(val: Val, line: usize) -> Node {
        Node { val, line }
    }
    #[cfg(test)]
    pub fn s(t: &str) -> Node {
        Node::new(Val::Str(t.to_string()), 0)
    }
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.val {
            Val::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn gets(&self, key: &str) -> Option<&str> {
        self.get(key)?.str()
    }
    pub fn str(&self) -> Option<&str> {
        match &self.val {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn bool(&self) -> Option<bool> {
        match self.val {
            Val::Bool(b) => Some(b),
            _ => None,
        }
    }
    pub fn num(&self) -> Option<f64> {
        match self.val {
            Val::Num(n) => Some(n),
            _ => None,
        }
    }
    pub fn arr(&self) -> &[Node] {
        match &self.val {
            Val::Arr(a) => a,
            _ => &[],
        }
    }
    pub fn entries(&self) -> &[(String, Node)] {
        match &self.val {
            Val::Map(m) => m,
            _ => &[],
        }
    }
    pub fn is_map(&self) -> bool {
        matches!(self.val, Val::Map(_))
    }
}

/// Parse a JSON or YAML document.
pub fn parse(text: &str) -> Result<Node, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let first = text.trim_start().chars().next();
    if matches!(first, Some('{') | Some('[')) {
        let chars: Vec<char> = text.chars().collect();
        let mut f = Flow { s: &chars, i: 0, line: 0 };
        let n = f.value(0)?;
        f.ws();
        if f.i < chars.len() {
            return Err(format!("line {}: unexpected text after the document", f.line + 1));
        }
        return Ok(n);
    }
    let mut y = Yaml { ls: prep(text), p: 0 };
    if y.ls.first().is_some_and(|l| l.text == "---") {
        y.p = 1;
    }
    let n = y.node(0, 0)?;
    y.skip_blank();
    if y.p < y.ls.len() {
        return Err(format!("line {}: unexpected indentation", y.ls[y.p].no + 1));
    }
    Ok(n)
}

// ───────────── scalars ─────────────

fn numeric(s: &str) -> bool {
    let Some(f) = s.chars().next() else { return false };
    if !(f.is_ascii_digit() || matches!(f, '-' | '+' | '.')) {
        return false;
    }
    s.chars().any(|c| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'))
        && s.parse::<f64>().is_ok()
}

fn plain(s: &str) -> Val {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => Val::Null,
        "true" | "True" | "TRUE" => Val::Bool(true),
        "false" | "False" | "FALSE" => Val::Bool(false),
        _ if numeric(s) => Val::Num(s.parse().unwrap_or(0.0)),
        _ => Val::Str(s.to_string()),
    }
}

// ───────────── flow style (and all of JSON) ─────────────

struct Flow<'a> {
    s: &'a [char],
    i: usize,
    line: usize,
}

impl Flow<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while let Some(c) = self.peek() {
            match c {
                '\n' => {
                    self.line += 1;
                    self.i += 1;
                }
                ' ' | '\t' | '\r' => self.i += 1,
                '#' if self.i == 0 || self.s[self.i - 1].is_whitespace() => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn err<T>(&self, m: &str) -> Result<T, String> {
        Err(format!("line {}: {m}", self.line + 1))
    }

    fn value(&mut self, depth: usize) -> Result<Node, String> {
        if depth > 200 {
            return self.err("nesting is too deep");
        }
        self.ws();
        let line = self.line;
        match self.peek() {
            None => self.err("unexpected end of the document"),
            Some('{') => {
                self.i += 1;
                let mut out = Vec::new();
                loop {
                    self.ws();
                    match self.peek() {
                        None => return self.err("unterminated '{'"),
                        Some('}') => {
                            self.i += 1;
                            break;
                        }
                        Some(',') => {
                            self.i += 1;
                            continue;
                        }
                        _ => {}
                    }
                    let kline = self.line;
                    let key = match self.peek() {
                        Some('"') | Some('\'') => self.quoted()?,
                        _ => {
                            let st = self.i;
                            while self.peek().is_some_and(|c| !matches!(c, ':' | ',' | '}' | '\n')) {
                                self.i += 1;
                            }
                            self.s[st..self.i].iter().collect::<String>().trim().to_string()
                        }
                    };
                    self.ws();
                    let mut v = if self.peek() == Some(':') {
                        self.i += 1;
                        self.value(depth + 1)?
                    } else {
                        Node::new(Val::Null, kline)
                    };
                    v.line = kline;
                    out.push((key, v));
                }
                Ok(Node::new(Val::Map(out), line))
            }
            Some('[') => {
                self.i += 1;
                let mut out = Vec::new();
                loop {
                    self.ws();
                    match self.peek() {
                        None => return self.err("unterminated '['"),
                        Some(']') => {
                            self.i += 1;
                            break;
                        }
                        Some(',') => {
                            self.i += 1;
                            continue;
                        }
                        _ => {}
                    }
                    out.push(self.value(depth + 1)?);
                }
                Ok(Node::new(Val::Arr(out), line))
            }
            Some('"') | Some('\'') => Ok(Node::new(Val::Str(self.quoted()?), line)),
            Some(_) => {
                let st = self.i;
                while self.peek().is_some_and(|c| !matches!(c, ',' | ']' | '}' | '\n')) {
                    self.i += 1;
                }
                let t: String = self.s[st..self.i].iter().collect();
                Ok(Node::new(plain(t.trim()), line))
            }
        }
    }

    /// A quoted string at the cursor (either quote style).
    fn quoted(&mut self) -> Result<String, String> {
        let q = self.s[self.i];
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else { return self.err("unterminated string") };
            self.i += 1;
            if c == '\n' {
                self.line += 1;
            }
            if c == q {
                if q == '\'' && self.peek() == Some('\'') {
                    self.i += 1;
                    out.push('\'');
                    continue;
                }
                return Ok(out);
            }
            if c == '\\' && q == '"' {
                let Some(e) = self.peek() else { return self.err("unterminated string") };
                self.i += 1;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    '0' => out.push('\0'),
                    'u' => {
                        let mut cp = self.hex(4);
                        if (0xD800..0xDC00).contains(&cp)
                            && self.peek() == Some('\\')
                            && self.s.get(self.i + 1) == Some(&'u')
                        {
                            self.i += 2;
                            let lo = self.hex(4);
                            cp = 0x10000 + ((cp - 0xD800) << 10) + lo.wrapping_sub(0xDC00);
                        }
                        out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                    }
                    'x' => {
                        let cp = self.hex(2);
                        out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                    }
                    '\n' => self.line += 1,
                    other => out.push(other),
                }
            } else {
                out.push(c);
            }
        }
    }

    fn hex(&mut self, n: usize) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            let d = self.peek().and_then(|c| c.to_digit(16));
            if let Some(d) = d {
                v = v * 16 + d;
                self.i += 1;
            }
        }
        v
    }
}

// ───────────── block style ─────────────

struct Ln {
    ind: usize,
    /// Content without indentation and comment.
    text: String,
    raw: String,
    no: usize,
}

fn strip_comment(s: &str) -> &str {
    let (mut sq, mut dq) = (false, false);
    let mut prev_ws = true;
    let mut last_sig: Option<char> = None;
    let mut esc = false;
    for (i, c) in s.char_indices() {
        if dq {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                dq = false;
            }
        } else if sq {
            if c == '\'' {
                sq = false;
            }
        } else if c == '#' && prev_ws {
            return &s[..i];
        } else if c == '"' && last_sig.is_none_or(|p| matches!(p, ':' | '-' | ',' | '[' | '{')) {
            dq = true;
        } else if c == '\'' && last_sig.is_none_or(|p| matches!(p, ':' | '-' | ',' | '[' | '{')) {
            sq = true;
        }
        prev_ws = c.is_whitespace();
        if !c.is_whitespace() {
            last_sig = Some(c);
        }
    }
    s
}

fn prep(src: &str) -> Vec<Ln> {
    src.lines()
        .enumerate()
        .map(|(no, l)| {
            let t = l.trim_start_matches([' ', '\t']);
            let ind = l.len() - t.len();
            Ln { ind, text: strip_comment(t).trim_end().to_string(), raw: l.to_string(), no }
        })
        .collect()
}

/// `key: rest` -> (key, rest). The colon must be followed by a space or end the line.
fn split_key(t: &str) -> Option<(String, &str)> {
    let b = t.as_bytes();
    if t.starts_with('"') || t.starts_with('\'') {
        let q = b[0];
        let mut i = 1;
        while i < b.len() {
            if q == b'"' && b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == q {
                if q == b'\'' && b.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                break;
            }
            i += 1;
        }
        let end = i;
        if end >= b.len() || b.get(end + 1) != Some(&b':') {
            return None;
        }
        if b.get(end + 2).is_some_and(|c| *c != b' ') {
            return None;
        }
        let chars: Vec<char> = t[..=end].chars().collect();
        let mut f = Flow { s: &chars, i: 0, line: 0 };
        let key = f.quoted().ok()?;
        return Some((key, t[end + 2..].trim_start()));
    }
    if t.starts_with('[') || t.starts_with('{') {
        return None;
    }
    for (i, c) in t.char_indices() {
        if c == ':' && t[i + 1..].chars().next().is_none_or(|n| n == ' ') {
            return Some((t[..i].trim_end().to_string(), t[i + 1..].trim_start()));
        }
    }
    None
}

fn balance(s: &str) -> i32 {
    let (mut d, mut sq, mut dq, mut esc) = (0i32, false, false, false);
    for c in s.chars() {
        if dq {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                dq = false;
            }
        } else if sq {
            if c == '\'' {
                sq = false;
            }
        } else {
            match c {
                '"' => dq = true,
                '\'' => sq = true,
                '[' | '{' => d += 1,
                ']' | '}' => d -= 1,
                _ => {}
            }
        }
    }
    d
}

fn quote_closed(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut f = Flow { s: &chars, i: 0, line: 0 };
    f.quoted().is_ok()
}

struct Yaml {
    ls: Vec<Ln>,
    p: usize,
}

impl Yaml {
    fn skip_blank(&mut self) {
        while self.p < self.ls.len() {
            let t = &self.ls[self.p].text;
            if t == "..." || (t == "---" && self.p > 0) {
                self.p = self.ls.len();
            } else if t.is_empty() {
                self.p += 1;
            } else {
                break;
            }
        }
    }

    fn node(&mut self, min_ind: usize, depth: usize) -> Result<Node, String> {
        if depth > 200 {
            return Err("nesting is too deep".into());
        }
        self.skip_blank();
        if self.p >= self.ls.len() {
            return Ok(Node::new(Val::Null, 0));
        }
        let (ind, text, no) = {
            let l = &self.ls[self.p];
            (l.ind, l.text.clone(), l.no)
        };
        if ind < min_ind {
            return Ok(Node::new(Val::Null, no));
        }
        if text == "-" || text.starts_with("- ") {
            self.seq(ind, depth)
        } else if split_key(&text).is_some() {
            self.map(ind, depth)
        } else {
            self.p += 1;
            self.inline(&text, ind.saturating_sub(1), no)
        }
    }

    fn seq(&mut self, ind: usize, depth: usize) -> Result<Node, String> {
        let first = self.ls[self.p].no;
        let mut items = Vec::new();
        loop {
            self.skip_blank();
            if self.p >= self.ls.len() {
                break;
            }
            let (cind, text, no) = {
                let l = &self.ls[self.p];
                (l.ind, l.text.clone(), l.no)
            };
            if cind != ind || !(text == "-" || text.starts_with("- ")) {
                break;
            }
            if text == "-" {
                self.p += 1;
                let mut n = self.node(ind + 1, depth + 1)?;
                n.line = no;
                items.push(n);
                continue;
            }
            let rest = text[1..].trim_start();
            let off = text.len() - rest.len();
            let rest = rest.to_string();
            self.ls[self.p].ind = cind + off;
            self.ls[self.p].text = rest;
            let mut n = self.node(ind + 1, depth + 1)?;
            n.line = no;
            items.push(n);
        }
        Ok(Node::new(Val::Arr(items), first))
    }

    fn map(&mut self, ind: usize, depth: usize) -> Result<Node, String> {
        let first = self.ls[self.p].no;
        let mut out: Vec<(String, Node)> = Vec::new();
        loop {
            self.skip_blank();
            if self.p >= self.ls.len() {
                break;
            }
            let (lind, text, no) = {
                let l = &self.ls[self.p];
                (l.ind, l.text.clone(), l.no)
            };
            if lind < ind || (lind == ind && (text == "-" || text.starts_with("- "))) {
                break;
            }
            if lind > ind {
                return Err(format!("line {}: unexpected indentation", no + 1));
            }
            let Some((key, rest)) = split_key(&text) else {
                return Err(format!("line {}: expected 'key: value'", no + 1));
            };
            let rest = rest.to_string();
            self.p += 1;
            let mut v = self.value(&rest, ind, no, depth)?;
            v.line = no;
            out.push((key, v));
        }
        Ok(Node::new(Val::Map(out), first))
    }

    /// The value after `key:` (`rest`), possibly continuing on the next lines.
    fn value(&mut self, rest: &str, ind: usize, no: usize, depth: usize) -> Result<Node, String> {
        if rest.is_empty() {
            self.skip_blank();
            if self.p < self.ls.len() {
                let (lind, is_seq) = {
                    let l = &self.ls[self.p];
                    (l.ind, l.text == "-" || l.text.starts_with("- "))
                };
                if lind > ind {
                    return self.node(ind + 1, depth + 1);
                }
                if lind == ind && is_seq {
                    return self.seq(ind, depth + 1);
                }
            }
            return Ok(Node::new(Val::Null, no));
        }
        if rest.starts_with('|') || rest.starts_with('>') {
            return Ok(Node::new(Val::Str(self.block_scalar(rest, ind)), no));
        }
        self.inline(rest, ind, no)
    }

    /// A scalar or flow collection that starts with `rest` (and may go on over the next lines).
    fn inline(&mut self, rest: &str, ind: usize, no: usize) -> Result<Node, String> {
        if rest.starts_with('[') || rest.starts_with('{') {
            let mut s = rest.to_string();
            while balance(&s) > 0 && self.p < self.ls.len() {
                s.push('\n');
                s.push_str(&self.ls[self.p].text);
                self.p += 1;
            }
            let chars: Vec<char> = s.chars().collect();
            let mut f = Flow { s: &chars, i: 0, line: no };
            return f.value(0);
        }
        let mut s = rest.to_string();
        if s.starts_with('"') || s.starts_with('\'') {
            while !quote_closed(&s) && self.p < self.ls.len() {
                s.push(' ');
                s.push_str(self.ls[self.p].raw.trim());
                self.p += 1;
            }
            let chars: Vec<char> = s.chars().collect();
            let mut f = Flow { s: &chars, i: 0, line: no };
            return Ok(Node::new(Val::Str(f.quoted()?), no));
        }
        while self.p < self.ls.len() && self.ls[self.p].ind > ind && !self.ls[self.p].text.is_empty() {
            s.push(' ');
            s.push_str(&self.ls[self.p].text);
            self.p += 1;
        }
        Ok(Node::new(plain(&s), no))
    }

    fn block_scalar(&mut self, header: &str, parent: usize) -> String {
        let folded = header.starts_with('>');
        let chomp = if header.contains('-') {
            '-'
        } else if header.contains('+') {
            '+'
        } else {
            ' '
        };
        let lead = |s: &str| s.len() - s.trim_start_matches(' ').len();
        let mut bi = None;
        let mut q = self.p;
        while q < self.ls.len() {
            let r = &self.ls[q].raw;
            if !r.trim().is_empty() {
                bi = Some(lead(r));
                break;
            }
            q += 1;
        }
        let Some(bi) = bi.filter(|b| *b > parent) else { return String::new() };
        let mut lines: Vec<String> = Vec::new();
        while self.p < self.ls.len() {
            let r = &self.ls[self.p].raw;
            if r.trim().is_empty() {
                lines.push(String::new());
            } else if lead(r) >= bi {
                lines.push(r.get(bi..).unwrap_or("").to_string());
            } else {
                break;
            }
            self.p += 1;
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        let mut out = String::new();
        let mut need_sep = false;
        for l in &lines {
            if folded {
                if l.is_empty() {
                    out.push('\n');
                    need_sep = false;
                } else {
                    if need_sep {
                        out.push(' ');
                    }
                    out.push_str(l);
                    need_sep = true;
                }
            } else {
                out.push_str(l);
                out.push('\n');
            }
        }
        match chomp {
            '-' => out.trim_end_matches('\n').to_string(),
            _ => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out
            }
        }
    }
}

// ───────────── output ─────────────

fn esc(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn num_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { format!("{n}") }
}

/// JSON text of a node (2-space indentation when `pretty`).
pub fn to_json(n: &Node, pretty: bool) -> String {
    let mut out = String::new();
    write_json(n, pretty, 0, &mut out);
    out
}

fn write_json(n: &Node, pretty: bool, d: usize, out: &mut String) {
    let nl = |out: &mut String, d: usize| {
        if pretty {
            out.push('\n');
            for _ in 0..d {
                out.push_str("  ");
            }
        }
    };
    match &n.val {
        Val::Null => out.push_str("null"),
        Val::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Val::Num(x) => out.push_str(&num_text(*x)),
        Val::Str(s) => esc(s, out),
        Val::Arr(a) if a.is_empty() => out.push_str("[]"),
        Val::Map(m) if m.is_empty() => out.push_str("{}"),
        Val::Arr(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, d + 1);
                write_json(v, pretty, d + 1, out);
            }
            nl(out, d);
            out.push(']');
        }
        Val::Map(m) => {
            out.push('{');
            for (i, (k, v)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, d + 1);
                esc(k, out);
                out.push(':');
                if pretty {
                    out.push(' ');
                }
                write_json(v, pretty, d + 1, out);
            }
            nl(out, d);
            out.push('}');
        }
    }
}

/// Short text of a scalar for display (`to_json` of anything else, compact).
pub fn scalar_text(n: &Node) -> String {
    match &n.val {
        Val::Str(s) => s.clone(),
        Val::Num(x) => num_text(*x),
        Val::Bool(b) => b.to_string(),
        Val::Null => "null".into(),
        _ => to_json(n, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = "openapi: 3.0.3\ninfo:\n  title: 'Pet: store'\n  description: |\n    Line one\n    Line two\n  version: \"1.0\"  # comment\npaths:\n  /pets:\n    get:\n      tags: [pets, \"x y\"]\n      parameters:\n      - name: limit\n        in: query\n        required: false\n        schema: {type: integer, maximum: 100}\n      - $ref: '#/components/parameters/Q'\n      responses:\n        '200':\n          description: ok\n          summary: >-\n            folded\n            text\n";

    #[test]
    fn yaml_blocks_flow_and_scalars() {
        let n = parse(YAML).unwrap();
        assert_eq!(n.gets("openapi"), Some("3.0.3"));
        assert_eq!(n.get("info").unwrap().gets("title"), Some("Pet: store"));
        assert_eq!(n.get("info").unwrap().gets("description"), Some("Line one\nLine two\n"));
        assert_eq!(n.get("info").unwrap().gets("version"), Some("1.0"));
        let get = n.get("paths").unwrap().get("/pets").unwrap().get("get").unwrap();
        assert_eq!(get.get("tags").unwrap().arr().len(), 2);
        let ps = get.get("parameters").unwrap().arr();
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].get("schema").unwrap().get("maximum").unwrap().num(), Some(100.0));
        assert_eq!(ps[1].gets("$ref"), Some("#/components/parameters/Q"));
        let r = get.get("responses").unwrap().get("200").unwrap();
        assert_eq!(r.gets("summary"), Some("folded text"));
        assert_eq!(get.line, 9, "the line of the key");
    }

    #[test]
    fn json_keeps_order_and_lines() {
        let n = parse("{\n \"b\": 1,\n \"a\": [true, null, \"\\u00e9\\n\"],\n \"c\": {}\n}").unwrap();
        let keys: Vec<&str> = n.entries().iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["b", "a", "c"]);
        assert_eq!(n.get("a").unwrap().line, 2);
        assert_eq!(n.get("a").unwrap().arr()[2].str(), Some("é\n"));
        assert_eq!(to_json(&n, false), "{\"b\":1,\"a\":[true,null,\"é\\n\"],\"c\":{}}");
        assert!(parse("{\"a\": ").is_err());
    }

    #[test]
    fn sequences_at_the_key_indent_and_nested_items() {
        let n = parse("a:\n- x: 1\n  y: 2\n- - 1\n  - 2\nb: z\n").unwrap();
        let a = n.get("a").unwrap().arr();
        assert_eq!(a[0].get("y").unwrap().num(), Some(2.0));
        assert_eq!(a[1].arr().len(), 2);
        assert_eq!(n.gets("b"), Some("z"));
    }
}
