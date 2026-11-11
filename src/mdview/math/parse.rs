//! TeX math parser: the source of one formula to a node tree (recursive descent over the subset
//! people write in Markdown: KaTeX / GitHub style). Errors carry the byte range to blame.

use super::syms::{self, Class, Font};

/// TeX styles: display, text, script, scriptscript.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    D,
    T,
    S,
    SS,
}

/// Column alignment of an array.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    L,
    C,
    R,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArrKind {
    /// `matrix` & co.: centered columns in text style, 1em apart.
    Matrix,
    /// `smallmatrix`: script style.
    Small,
    /// `cases`: two left-aligned columns.
    Cases,
    /// `aligned` / `align` / `split`: right-left column pairs, display style.
    Aligned,
    /// `gathered` / `gather`, and `\\` at the top level: centered rows.
    Gathered,
    /// `array`: columns from the spec.
    Array,
    /// `\substack`: centered rows in the current (script) style, tight.
    Substack,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// One glyph; `at` is its byte range in the source (blamed when the font lacks it).
    Char {
        c: char,
        class: Class,
        at: (u32, u32),
    },
    /// `\text{..}`: set in the preview's body font.
    Text {
        s: String,
        bold: bool,
    },
    /// Operator names (`\sin`, `\lim`, `\operatorname`): upright letters, class Op. `limits` is the
    /// `\limits` / `\nolimits` override, `dl` = takes limits in display style by default.
    Word {
        s: String,
        limits: Option<bool>,
        dl: bool,
    },
    /// Large operators (`\sum`, `\int`): `integral` ones keep their limits as scripts.
    BigOp {
        c: char,
        limits: Option<bool>,
        integral: bool,
        at: (u32, u32),
    },
    Group(Vec<Node>),
    /// `\mathbin{..}` & co.: a group with a forced class.
    Class(Class, Vec<Node>),
    Scripts {
        base: Box<Node>,
        sup: Option<Vec<Node>>,
        sub: Option<Vec<Node>>,
    },
    /// `\frac` (`rule`), `\binom` (no rule, `delims`); `style` forces display / text (`\dfrac`).
    Frac {
        num: Vec<Node>,
        den: Vec<Node>,
        rule: bool,
        style: Option<Style>,
        delims: Option<(char, char)>,
    },
    Sqrt {
        body: Vec<Node>,
        index: Option<Vec<Node>>,
    },
    /// `\left( .. \right)`; `None` is the null delimiter `.`.
    LeftRight {
        left: Option<char>,
        right: Option<char>,
        body: Vec<Node>,
    },
    Middle(Option<char>),
    /// `\big(` .. `\Bigg(`: `size` 0..=3.
    Big {
        c: Option<char>,
        size: u8,
        class: Class,
    },
    Accent {
        c: char,
        body: Vec<Node>,
        wide: bool,
    },
    Overline(Vec<Node>),
    Underline(Vec<Node>),
    /// `\overbrace` / `\underbrace` (their `^` / `_` become limits).
    Brace {
        body: Vec<Node>,
        over: bool,
    },
    /// `\overset` / `\underset` / `\stackrel`.
    Stack {
        base: Vec<Node>,
        over: Option<Vec<Node>>,
        under: Option<Vec<Node>>,
        class: Class,
    },
    /// `\xrightarrow[under]{over}`.
    XArrow {
        c: char,
        over: Vec<Node>,
        under: Option<Vec<Node>>,
    },
    /// Horizontal space in em (of the current style).
    Space(f32),
    Style(Style),
    Array {
        rows: Vec<Vec<Vec<Node>>>,
        cols: Vec<Align>,
        kind: ArrKind,
    },
    /// `\phantom` (keeps width and height), `\hphantom` (width), `\vphantom` (height).
    Phantom {
        body: Vec<Node>,
        w: bool,
        h: bool,
    },
    Boxed(Vec<Node>),
    /// `\not` over something Unicode has no negated form for.
    Not(Box<Node>),
}

/// A formula that cannot be typeset: what went wrong and the source bytes to blame.
#[derive(Clone, Debug, PartialEq)]
pub struct MathErr {
    pub at: (usize, usize),
    pub msg: String,
}

pub type R<T> = Result<T, MathErr>;

/// What ended a list of atoms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Term {
    Eof,
    Brace,
    Right,
    End,
    Amp,
    Row,
}

/// Nesting limit (braces, environments, `\left`): deeper input is rejected instead of recursing.
const MAX_DEPTH: u32 = 64;

struct P<'a> {
    s: &'a str,
    i: usize,
    depth: u32,
}

/// Parse one formula. `\\` and `&` at the top level make the formula a set of centered (or, with
/// `&`, aligned) rows, as if it were wrapped in `gathered` / `aligned`.
pub fn parse(src: &str) -> R<Vec<Node>> {
    let mut p = P { s: src, i: 0, depth: 0 };
    let mut rows: Vec<Vec<Vec<Node>>> = vec![];
    let mut row: Vec<Vec<Node>> = vec![];
    let mut amp = false;
    loop {
        let mut f = Font::Normal;
        let (cell, t, tat) = p.list(&mut f)?;
        row.push(cell);
        match t {
            Term::Eof => {
                rows.push(row);
                break;
            }
            Term::Amp => amp = true,
            Term::Row => rows.push(std::mem::take(&mut row)),
            Term::Brace => return p.err(tat, tat + 1, "unmatched }"),
            Term::Right => return p.err(tat, p.i, "\\right without \\left"),
            Term::End => return p.err(tat, p.i, "\\end without \\begin"),
        }
    }
    if rows.len() == 1 && rows[0].len() == 1 {
        return Ok(rows.pop().and_then(|mut r| r.pop()).unwrap_or_default());
    }
    Ok(vec![rows_node(rows, amp)])
}

/// Rows of cells as one node: aligned when any row has several cells, else gathered.
fn rows_node(mut rows: Vec<Vec<Vec<Node>>>, amp: bool) -> Node {
    drop_empty_last_row(&mut rows);
    let kind = if amp { ArrKind::Aligned } else { ArrKind::Gathered };
    Node::Array { rows, cols: vec![], kind }
}

/// A trailing `\\` leaves an empty last row: drop it.
fn drop_empty_last_row(rows: &mut Vec<Vec<Vec<Node>>>) {
    if rows.len() > 1 && rows.last().is_some_and(|r| r.iter().all(|c| c.is_empty())) {
        rows.pop();
    }
}

impl<'a> P<'a> {
    fn err<T>(&self, a: usize, b: usize, msg: impl Into<String>) -> R<T> {
        Err(MathErr { at: (a, b.max(a + 1)), msg: msg.into() })
    }

    fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }

    fn skip_ws(&mut self) {
        let b = self.s.as_bytes();
        while self.i < b.len() {
            match b[self.i] {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                b'%' => {
                    while self.i < b.len() && b[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
    }

    /// The control sequence whose backslash is at `self.i`: its name (letters, or one other char)
    /// and the byte offset of the backslash.
    fn command(&mut self) -> (&'a str, usize) {
        let s: &'a str = self.s;
        let start = self.i;
        self.i += 1;
        let rest = &s[self.i..];
        let n = rest.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
        let name = if n > 0 { &rest[..n] } else { rest.chars().next().map_or("", |c| &rest[..c.len_utf8()]) };
        self.i += name.len();
        (name, start)
    }

    fn enter(&mut self, at: usize) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err(at, at + 1, "formula nested too deeply");
        }
        Ok(())
    }

    /// Atoms until a terminator. Returns the list, the terminator (consumed; for `\right` and
    /// `\end` only the command itself) and where it starts.
    fn list(&mut self, font: &mut Font) -> R<(Vec<Node>, Term, usize)> {
        let mut out: Vec<Node> = Vec::new();
        loop {
            self.skip_ws();
            let at = self.i;
            let Some(c) = self.peek() else { return Ok((out, Term::Eof, at)) };
            match c {
                '}' => {
                    self.i += 1;
                    return Ok((out, Term::Brace, at));
                }
                '&' => {
                    self.i += 1;
                    return Ok((out, Term::Amp, at));
                }
                '{' => {
                    let g = self.group(*font)?;
                    out.push(Node::Group(g));
                }
                '^' | '_' => {
                    self.i += 1;
                    let arg = self.arg(*font)?;
                    attach(&mut out, c == '^', arg).or_else(|m| self.err(at, at + 1, m))?;
                }
                '\\' => {
                    let (name, st) = self.command();
                    match name {
                        "\\" | "cr" | "newline" => {
                            // `\\[4pt]`: the extra row space is ignored
                            if self.peek() == Some('[') {
                                match self.s[self.i..].find(']') {
                                    Some(e) => self.i += e + 1,
                                    None => return self.err(self.i, self.s.len(), "missing ]"),
                                }
                            }
                            return Ok((out, Term::Row, at));
                        }
                        "right" => return Ok((out, Term::Right, at)),
                        "end" => return Ok((out, Term::End, at)),
                        "over" | "atop" | "choose" => {
                            self.enter(st)?;
                            let (den, t, tat) = self.list(font)?;
                            self.depth -= 1;
                            let (rule, delims) = match name {
                                "over" => (true, None),
                                "atop" => (false, None),
                                _ => (false, Some(('(', ')'))),
                            };
                            let num = std::mem::take(&mut out);
                            out.push(Node::Frac { num, den, rule, style: None, delims });
                            return Ok((out, t, tat));
                        }
                        "limits" | "nolimits" | "displaylimits" => {
                            let v = match name {
                                "limits" => Some(true),
                                "nolimits" => Some(false),
                                _ => None,
                            };
                            match out.last_mut() {
                                Some(Node::BigOp { limits, .. }) | Some(Node::Word { limits, .. }) => *limits = v,
                                _ => return self.err(st, self.i, format!("\\{name} must follow an operator")),
                            }
                        }
                        _ => {
                            if let Some(f) = syms::font_switch(name) {
                                *font = f;
                            } else if let Some(n) = self.cmd(name, st, *font)? {
                                out.push(n);
                            }
                        }
                    }
                }
                _ => {
                    let n = self.char_atom(c, *font);
                    out.push(n);
                }
            }
        }
    }

    /// A single character atom (letters in the current font; ASCII operators get their class).
    fn char_atom(&mut self, c: char, font: Font) -> Node {
        let a = self.i;
        self.i += c.len_utf8();
        let at = (a as u32, self.i as u32);
        if c == '~' {
            return Node::Space(0.33);
        }
        if c == '\'' {
            return Node::Char { c: '′', class: Class::Ord, at };
        }
        if let Some(integral) = syms::big_op_char(c) {
            return Node::BigOp { c, limits: None, integral, at };
        }
        let (ch, class) = match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' => (syms::styled(c, font), Class::Ord),
            '-' => ('\u{2212}', Class::Bin),
            '*' => ('\u{2217}', Class::Bin),
            '|' | '/' | '.' => (c, Class::Ord),
            _ => (syms::styled(c, font), syms::class_of(c)),
        };
        Node::Char { c: ch, class, at }
    }

    /// One argument: a braced group, or a single char / command (`x^2`, `\frac12`, `x^\alpha`).
    fn arg(&mut self, font: Font) -> R<Vec<Node>> {
        self.skip_ws();
        let at = self.i;
        let Some(c) = self.peek() else {
            return self.err(at.saturating_sub(1), at.max(1), "missing argument");
        };
        match c {
            '{' => self.group(font),
            '}' | '&' | '^' | '_' => self.err(at, at + 1, "missing argument"),
            '\\' => {
                let (name, st) = self.command();
                match name {
                    "\\" | "right" | "end" | "over" | "atop" | "choose" | "cr" => {
                        self.err(st, self.i, "missing argument")
                    }
                    _ => {
                        if syms::font_switch(name).is_some() {
                            return self.err(st, self.i, format!("\\{name} cannot be an argument"));
                        }
                        // `\hat\hat\hat..x` nests without braces
                        self.enter(st)?;
                        let n = self.cmd(name, st, font)?;
                        self.depth -= 1;
                        Ok(n.into_iter().collect())
                    }
                }
            }
            _ => Ok(vec![self.char_atom(c, font)]),
        }
    }

    /// `{ ... }` starting at `self.i`.
    fn group(&mut self, font: Font) -> R<Vec<Node>> {
        let open = self.i;
        self.i += 1;
        self.enter(open)?;
        let mut f = font;
        let (l, t, tat) = self.list(&mut f)?;
        self.depth -= 1;
        match t {
            Term::Brace => Ok(l),
            Term::Eof => self.err(open, self.s.len(), "missing }"),
            Term::Amp => self.err(tat, tat + 1, "& inside braces"),
            Term::Row => self.err(tat, self.i, "\\\\ inside braces"),
            Term::Right => self.err(tat, self.i, "\\right without \\left"),
            Term::End => self.err(tat, self.i, "\\end without \\begin"),
        }
    }

    /// `{name}` (environment names, colors): the trimmed text between the braces.
    fn braced_raw(&mut self) -> R<(&'a str, usize)> {
        self.skip_ws();
        let s: &'a str = self.s;
        let at = self.i;
        if self.peek() != Some('{') {
            return self.err(at, at + 1, "expected {");
        }
        let Some(e) = s[at..].find('}') else { return self.err(at, s.len(), "missing }") };
        self.i = at + e + 1;
        Ok((s[at + 1..at + e].trim(), at))
    }

    /// Text argument of `\text{..}`: braces dropped, escapes resolved, spaces collapsed.
    fn text_arg(&mut self) -> R<String> {
        self.skip_ws();
        let open = self.i;
        let Some(first) = self.peek() else { return self.err(open.saturating_sub(1), open.max(1), "missing argument") };
        if first != '{' {
            self.i += first.len_utf8();
            return Ok(first.to_string());
        }
        self.i += 1;
        let mut depth = 0u32;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else { return self.err(open, self.s.len(), "missing }") };
            self.i += c.len_utf8();
            match c {
                '{' => depth += 1,
                '}' if depth == 0 => break,
                '}' => depth -= 1,
                '\\' => {
                    let s: &'a str = self.s;
                    let rest = &s[self.i..];
                    let n = rest.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
                    if n == 0 {
                        if let Some(e) = rest.chars().next() {
                            self.i += e.len_utf8();
                            match e {
                                ',' | ';' | ':' | ' ' | '!' | '\\' => {
                                    if !out.ends_with(' ') {
                                        out.push(' ')
                                    }
                                }
                                _ => out.push(e),
                            }
                        }
                    } else {
                        let name = &rest[..n];
                        self.i += n;
                        match name {
                            "textbackslash" => out.push('\\'),
                            "quad" | "qquad" | "enspace" => out.push_str("  "),
                            _ => {
                                if let Some((ch, _)) = syms::symbol(name) {
                                    out.push(ch);
                                } else if let Some(ch) = syms::greek(name) {
                                    out.push(ch);
                                }
                            }
                        }
                    }
                }
                '$' => {}
                '~' => out.push('\u{a0}'),
                c if c.is_whitespace() => {
                    if !out.ends_with(' ') {
                        out.push(' ');
                    }
                }
                _ => out.push(c),
            }
        }
        Ok(out)
    }

    /// A delimiter after `\left`, `\right`, `\big` ...: `None` for `.`.
    fn delim(&mut self) -> R<Option<char>> {
        self.skip_ws();
        let at = self.i;
        let Some(c) = self.peek() else { return self.err(at.saturating_sub(1), at.max(1), "missing delimiter") };
        if c == '\\' {
            let (name, st) = self.command();
            let d = match name {
                "{" | "lbrace" => '{',
                "}" | "rbrace" => '}',
                "|" | "Vert" | "lVert" | "rVert" => '‖',
                "vert" | "lvert" | "rvert" | "mid" => '|',
                "langle" => '⟨',
                "rangle" => '⟩',
                "lfloor" => '⌊',
                "rfloor" => '⌋',
                "lceil" => '⌈',
                "rceil" => '⌉',
                "lbrack" => '[',
                "rbrack" => ']',
                "lgroup" => '⟮',
                "rgroup" => '⟯',
                "backslash" => '\\',
                "uparrow" => '↑',
                "downarrow" => '↓',
                "updownarrow" => '↕',
                "Uparrow" => '⇑',
                "Downarrow" => '⇓',
                "Updownarrow" => '⇕',
                _ => return self.err(st, self.i, format!("\\{name} is not a delimiter")),
            };
            return Ok(Some(d));
        }
        self.i += c.len_utf8();
        Ok(Some(match c {
            '.' => return Ok(None),
            '(' | ')' | '[' | ']' | '|' | '/' | '⟨' | '⟩' | '‖' | '⌊' | '⌋' | '⌈' | '⌉' => c,
            '<' => '⟨',
            '>' => '⟩',
            _ => return self.err(at, self.i, format!("{c} is not a delimiter")),
        }))
    }

    /// A dimension (`3mu`, `{1.5em}`, `-2pt`) in em.
    fn dimen(&mut self) -> R<f32> {
        self.skip_ws();
        let braced = self.peek() == Some('{');
        if braced {
            self.i += 1;
            self.skip_ws();
        }
        let at = self.i;
        let s = self.s;
        let num_len = s[at..].bytes().take_while(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+')).count();
        let v: f32 = match s[at..at + num_len].parse() {
            Ok(v) => v,
            Err(_) => return self.err(at, (at + num_len).max(at + 1), "bad dimension"),
        };
        self.i = at + num_len;
        self.skip_ws();
        let unit_len = s[self.i..].bytes().take_while(|b| b.is_ascii_alphabetic()).count().min(2);
        let unit = &s[self.i..self.i + unit_len];
        let em = match unit {
            "em" => 1.0,
            "ex" => 0.43,
            "pt" => 0.1,
            "mu" => 1.0 / 18.0,
            "px" => 1.0 / 16.0,
            "mm" => 0.2845,
            "cm" => 2.845,
            "in" => 7.227,
            "bp" => 0.1004,
            "pc" => 1.2,
            _ => return self.err(at, self.i + unit_len.max(1), "bad dimension unit"),
        };
        self.i += unit_len;
        if braced {
            self.skip_ws();
            if self.peek() != Some('}') {
                return self.err(at, self.i, "missing }");
            }
            self.i += 1;
        }
        Ok(v * em)
    }

    /// Optional `[..]` argument (`\sqrt[3]{x}`), parsed as math.
    fn opt_arg(&mut self, font: Font) -> R<Option<Vec<Node>>> {
        self.skip_ws();
        if self.peek() != Some('[') {
            return Ok(None);
        }
        let open = self.i;
        let s: &'a str = self.s;
        // the matching `]` (brackets inside braces do not count)
        let mut depth = 0i32;
        let mut end = None;
        for (k, ch) in s[open + 1..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                ']' if depth == 0 => {
                    end = Some(open + 1 + k);
                    break;
                }
                _ => {}
            }
        }
        let Some(end) = end else { return self.err(open, s.len(), "missing ]") };
        let mut sub = P { s: &s[..end], i: open + 1, depth: self.depth };
        sub.enter(open)?;
        let mut f = font;
        let (l, t, tat) = sub.list(&mut f)?;
        if t != Term::Eof {
            return self.err(tat, tat + 1, "unexpected token in [..]");
        }
        self.i = end + 1;
        Ok(Some(l))
    }

    /// Rows and cells of an environment body, up to `\end{name}`. Returns the rows and whether
    /// any row has more than one cell.
    fn env_rows(&mut self, name: &str, begin: usize) -> R<(Vec<Vec<Vec<Node>>>, bool)> {
        let mut rows: Vec<Vec<Vec<Node>>> = vec![];
        let mut row: Vec<Vec<Node>> = vec![];
        let mut amp = false;
        loop {
            let mut f = Font::Normal;
            let (cell, t, tat) = self.list(&mut f)?;
            row.push(cell);
            match t {
                Term::Amp => amp = true,
                Term::Row => rows.push(std::mem::take(&mut row)),
                Term::End => {
                    let (n, nat) = self.braced_raw()?;
                    if n != name {
                        return self.err(nat, self.i, format!("\\end{{{n}}} closes \\begin{{{name}}}"));
                    }
                    rows.push(row);
                    break;
                }
                Term::Eof => return self.err(begin, self.s.len(), format!("missing \\end{{{name}}}")),
                Term::Brace => return self.err(tat, tat + 1, "unmatched }"),
                Term::Right => return self.err(tat, self.i, "\\right without \\left"),
            }
        }
        drop_empty_last_row(&mut rows);
        Ok((rows, amp))
    }

    /// `\begin{name} ... \end{name}`.
    fn env(&mut self, begin: usize) -> R<Node> {
        let (name, nat) = self.braced_raw()?;
        self.enter(begin)?;
        let mut cols: Vec<Align> = vec![];
        let kind = match name {
            "matrix" | "pmatrix" | "bmatrix" | "Bmatrix" | "vmatrix" | "Vmatrix" => ArrKind::Matrix,
            "smallmatrix" => ArrKind::Small,
            "cases" | "dcases" | "rcases" => ArrKind::Cases,
            "aligned" | "align" | "align*" | "alignat" | "alignat*" | "alignedat" | "split" | "eqnarray"
            | "eqnarray*" | "flalign" | "flalign*" => ArrKind::Aligned,
            "gathered" | "gather" | "gather*" | "multline" | "multline*" | "equation" | "equation*" => {
                ArrKind::Gathered
            }
            "array" | "darray" | "subarray" => ArrKind::Array,
            _ => return self.err(nat, self.i, format!("unknown environment {name}")),
        };
        if matches!(name, "alignat" | "alignat*" | "alignedat") {
            self.braced_raw()?; // number of column pairs
        }
        if kind == ArrKind::Array {
            let (spec, _) = self.braced_raw()?;
            let mut skip = 0i32;
            for ch in spec.chars() {
                match ch {
                    '{' => skip += 1,
                    '}' => skip -= 1,
                    _ if skip > 0 => {}
                    'l' | 'p' => cols.push(Align::L),
                    'c' => cols.push(Align::C),
                    'r' => cols.push(Align::R),
                    _ => {}
                }
            }
        }
        let (rows, amp) = self.env_rows(name, begin)?;
        self.depth -= 1;
        let arr = if matches!(name, "equation" | "equation*") {
            if rows.len() == 1 && rows[0].len() == 1 {
                Node::Group(rows.into_iter().next().and_then(|r| r.into_iter().next()).unwrap_or_default())
            } else {
                rows_node(rows, amp)
            }
        } else {
            Node::Array { rows, cols, kind }
        };
        let (l, r) = match name {
            "pmatrix" => (Some('('), Some(')')),
            "bmatrix" => (Some('['), Some(']')),
            "Bmatrix" => (Some('{'), Some('}')),
            "vmatrix" => (Some('|'), Some('|')),
            "Vmatrix" => (Some('‖'), Some('‖')),
            "cases" | "dcases" => (Some('{'), None),
            "rcases" => (None, Some('}')),
            _ => return Ok(arr),
        };
        Ok(Node::LeftRight { left: l, right: r, body: vec![arr] })
    }

    /// `\substack{a \\ b}`: centered rows.
    fn substack(&mut self, font: Font) -> R<Node> {
        self.skip_ws();
        let open = self.i;
        if self.peek() != Some('{') {
            return self.err(open, open + 1, "expected {");
        }
        self.i += 1;
        self.enter(open)?;
        let mut rows: Vec<Vec<Vec<Node>>> = vec![];
        loop {
            let mut f = font;
            let (cell, t, tat) = self.list(&mut f)?;
            rows.push(vec![cell]);
            match t {
                Term::Row => {}
                Term::Brace => break,
                Term::Eof => return self.err(open, self.s.len(), "missing }"),
                _ => return self.err(tat, tat + 1, "unexpected token in \\substack"),
            }
        }
        self.depth -= 1;
        drop_empty_last_row(&mut rows);
        Ok(Node::Array { rows, cols: vec![], kind: ArrKind::Substack })
    }

    /// A control sequence (backslash at `st`, name already read). `None` for commands that only
    /// change state or are ignored (`\color`, `\label`).
    fn cmd(&mut self, name: &'a str, st: usize, font: Font) -> R<Option<Node>> {
        let end = self.i;
        let at = (st as u32, end as u32);
        if let Some(c) = syms::greek(name) {
            return Ok(Some(Node::Char { c: syms::styled(c, font), class: Class::Ord, at }));
        }
        if let Some((c, class)) = syms::symbol(name) {
            let c = if class == Class::Ord { syms::styled(c, font) } else { c };
            return Ok(Some(Node::Char { c, class, at }));
        }
        if let Some((c, integral)) = syms::big_op(name) {
            return Ok(Some(Node::BigOp { c, limits: None, integral, at }));
        }
        if let Some((s, dl)) = syms::function(name) {
            return Ok(Some(Node::Word { s: s.to_string(), limits: None, dl }));
        }
        if let Some((c, wide)) = syms::accent(name) {
            let body = self.arg(font)?;
            return Ok(Some(Node::Accent { c, body, wide }));
        }
        if let Some(f) = syms::font_cmd(name) {
            return Ok(Some(Node::Group(self.arg(f)?)));
        }
        let space = |em: f32| -> R<Option<Node>> { Ok(Some(Node::Space(em))) };
        let node = match name {
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let num = self.arg(font)?;
                let den = self.arg(font)?;
                let style = match name {
                    "dfrac" | "cfrac" => Some(Style::D),
                    "tfrac" => Some(Style::T),
                    _ => None,
                };
                Node::Frac { num, den, rule: true, style, delims: None }
            }
            "binom" | "dbinom" | "tbinom" => {
                let num = self.arg(font)?;
                let den = self.arg(font)?;
                let style = match name {
                    "dbinom" => Some(Style::D),
                    "tbinom" => Some(Style::T),
                    _ => None,
                };
                Node::Frac { num, den, rule: false, style, delims: Some(('(', ')')) }
            }
            "sqrt" => {
                let index = self.opt_arg(font)?;
                let body = self.arg(font)?;
                Node::Sqrt { body, index }
            }
            "left" => {
                let left = self.delim()?;
                let after = self.i;
                self.enter(st)?;
                let mut f = font;
                let (body, t, _) = self.list(&mut f)?;
                self.depth -= 1;
                if t != Term::Right {
                    return self.err(st, after, "\\left without \\right");
                }
                let right = self.delim()?;
                Node::LeftRight { left, right, body }
            }
            "middle" => Node::Middle(self.delim()?),
            "big" | "Big" | "bigg" | "Bigg" | "bigl" | "Bigl" | "biggl" | "Biggl" | "bigr" | "Bigr" | "biggr"
            | "Biggr" | "bigm" | "Bigm" | "biggm" | "Biggm" => {
                let size = if name.starts_with("Bigg") {
                    3
                } else if name.starts_with("bigg") {
                    2
                } else if name.starts_with("Big") {
                    1
                } else {
                    0
                };
                let class = match name.as_bytes().last() {
                    Some(b'l') => Class::Open,
                    Some(b'r') => Class::Close,
                    Some(b'm') => Class::Rel,
                    _ => Class::Ord,
                };
                Node::Big { c: self.delim()?, size, class }
            }
            "begin" => self.env(st)?,
            "text" | "textrm" | "textnormal" | "textup" | "mbox" | "hbox" | "textsf" | "texttt" | "textit" | "emph"
            | "textmd" => Node::Text { s: self.text_arg()?, bold: false },
            "textbf" => Node::Text { s: self.text_arg()?, bold: true },
            "operatorname" | "operatornamewithlimits" => {
                let star = self.peek() == Some('*');
                if star {
                    self.i += 1;
                }
                let s = self.text_arg()?;
                Node::Word { s: s.trim().to_string(), limits: None, dl: star || name == "operatornamewithlimits" }
            }
            "mathop" => Node::Class(Class::Op, self.arg(font)?),
            "mathbin" => Node::Class(Class::Bin, self.arg(font)?),
            "mathrel" => Node::Class(Class::Rel, self.arg(font)?),
            "mathord" => Node::Class(Class::Ord, self.arg(font)?),
            "mathopen" => Node::Class(Class::Open, self.arg(font)?),
            "mathclose" => Node::Class(Class::Close, self.arg(font)?),
            "mathpunct" => Node::Class(Class::Punct, self.arg(font)?),
            "mathinner" => Node::Class(Class::Inner, self.arg(font)?),
            "," | "thinspace" => return space(3.0 / 18.0),
            ":" | ">" | "medspace" => return space(4.0 / 18.0),
            ";" | "thickspace" => return space(5.0 / 18.0),
            "!" | "negthinspace" => return space(-3.0 / 18.0),
            "negmedspace" => return space(-4.0 / 18.0),
            "negthickspace" => return space(-5.0 / 18.0),
            " " | "space" | "nobreakspace" => return space(0.33),
            "enspace" => return space(0.5),
            "quad" => return space(1.0),
            "qquad" => return space(2.0),
            "hspace" | "hskip" | "kern" | "mkern" | "mskip" | "mspace" => {
                if self.peek() == Some('*') {
                    self.i += 1;
                }
                return space(self.dimen()?);
            }
            "phantom" => Node::Phantom { body: self.arg(font)?, w: true, h: true },
            "hphantom" => Node::Phantom { body: self.arg(font)?, w: true, h: false },
            "vphantom" => Node::Phantom { body: self.arg(font)?, w: false, h: true },
            "mathstrut" | "strut" => {
                Node::Phantom { body: vec![Node::Char { c: '(', class: Class::Ord, at }], w: false, h: true }
            }
            "boxed" => Node::Boxed(self.arg(font)?),
            "fbox" => Node::Boxed(vec![Node::Text { s: self.text_arg()?, bold: false }]),
            "overline" => Node::Overline(self.arg(font)?),
            "underline" => Node::Underline(self.arg(font)?),
            "overbrace" => Node::Brace { body: self.arg(font)?, over: true },
            "underbrace" => Node::Brace { body: self.arg(font)?, over: false },
            "overset" | "stackrel" => {
                let over = self.arg(font)?;
                let base = self.arg(font)?;
                let class = if name == "stackrel" { Class::Rel } else { class_of(&base) };
                Node::Stack { base, over: Some(over), under: None, class }
            }
            "underset" => {
                let under = self.arg(font)?;
                let base = self.arg(font)?;
                let class = class_of(&base);
                Node::Stack { base, over: None, under: Some(under), class }
            }
            "xrightarrow" | "xleftarrow" | "xRightarrow" | "xLeftarrow" | "xleftrightarrow" | "xLeftrightarrow"
            | "xmapsto" | "xhookrightarrow" | "xhookleftarrow" => {
                let c = match name {
                    "xrightarrow" => '→',
                    "xleftarrow" => '←',
                    "xRightarrow" => '⇒',
                    "xLeftarrow" => '⇐',
                    "xleftrightarrow" => '↔',
                    "xLeftrightarrow" => '⇔',
                    "xmapsto" => '↦',
                    "xhookrightarrow" => '↪',
                    _ => '↩',
                };
                let under = self.opt_arg(font)?;
                let over = self.arg(font)?;
                Node::XArrow { c, over, under }
            }
            "not" => {
                self.skip_ws();
                let save = self.i;
                let src: &'a str = self.s;
                let next = match self.peek() {
                    Some('\\') => self.command().0,
                    Some(c) => {
                        self.i += c.len_utf8();
                        &src[save..self.i]
                    }
                    None => return self.err(st, end, "\\not needs something to negate"),
                };
                if let Some(c) = syms::negated(next) {
                    Node::Char { c, class: Class::Rel, at: (st as u32, self.i as u32) }
                } else {
                    self.i = save;
                    let inner = self.arg(font)?;
                    let n = if inner.len() == 1 {
                        inner.into_iter().next().unwrap_or_default()
                    } else {
                        Node::Group(inner)
                    };
                    Node::Not(Box::new(n))
                }
            }
            "pmod" => {
                let body = self.arg(font)?;
                let mut v = vec![Node::Space(1.0), Node::Char { c: '(', class: Class::Open, at }];
                v.push(Node::Word { s: "mod".into(), limits: None, dl: false });
                v.push(Node::Space(0.33));
                v.extend(body);
                v.push(Node::Char { c: ')', class: Class::Close, at });
                Node::Group(v)
            }
            "bmod" => Node::Class(Class::Bin, vec![Node::Word { s: "mod".into(), limits: None, dl: false }]),
            "mod" => Node::Group(vec![
                Node::Space(0.67),
                Node::Word { s: "mod".into(), limits: None, dl: false },
                Node::Space(0.33),
            ]),
            "substack" => self.substack(font)?,
            "displaystyle" => Node::Style(Style::D),
            "textstyle" => Node::Style(Style::T),
            "scriptstyle" => Node::Style(Style::S),
            "scriptscriptstyle" => Node::Style(Style::SS),
            "color" => {
                self.braced_raw()?;
                return Ok(None);
            }
            "textcolor" | "colorbox" => {
                self.braced_raw()?;
                Node::Group(self.arg(font)?)
            }
            "tag" | "label" | "tag*" => {
                if self.peek() == Some('*') {
                    self.i += 1;
                }
                self.braced_raw()?;
                return Ok(None);
            }
            "nonumber" | "notag" | "hline" | "hdashline" | "relax" | "allowbreak" | "nobreak" => return Ok(None),
            "" => return self.err(st, end, "lone backslash"),
            _ => return self.err(st, end, format!("unknown command \\{name}")),
        };
        Ok(Some(node))
    }
}

/// Attach a script to the last atom of `out` (an empty base when there is none).
fn attach(out: &mut Vec<Node>, sup_side: bool, arg: Vec<Node>) -> Result<(), &'static str> {
    match out.pop() {
        Some(Node::Scripts { base, sup, sub }) => {
            let (sup, sub) = match (sup_side, sup, sub) {
                (true, None, sub) => (Some(arg), sub),
                (false, sup, None) => (sup, Some(arg)),
                (true, Some(_), _) => return Err("double superscript"),
                (false, _, Some(_)) => return Err("double subscript"),
            };
            out.push(Node::Scripts { base, sup, sub });
        }
        b => {
            let base = Box::new(b.unwrap_or(Node::Group(vec![])));
            let (sup, sub) = if sup_side { (Some(arg), None) } else { (None, Some(arg)) };
            out.push(Node::Scripts { base, sup, sub });
        }
    }
    Ok(())
}

impl Default for Node {
    fn default() -> Self {
        Node::Group(vec![])
    }
}

/// Class of a one-atom list (`\overset{!}{=}` is a relation), Ord otherwise.
fn class_of(l: &[Node]) -> Class {
    match l {
        [Node::Char { class, .. }] => *class,
        [Node::BigOp { .. }] | [Node::Word { .. }] => Class::Op,
        _ => Class::Ord,
    }
}
