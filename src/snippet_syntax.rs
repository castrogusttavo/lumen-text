//! VS Code snippet syntax: tab stops (`$1`, `${1:default}`, `${1|a,b|}`), nested placeholders,
//! mirrors (the same number twice), variables (`$TM_FILENAME`, `${CLIPBOARD:fallback}`…) and
//! regex transforms (`${1/(.*)/${1:/upcase}/g}`), expanded to plain text plus tab-stop ranges.

use regex::Regex;
use std::collections::HashMap;

/// Everything variables can resolve to. Empty strings mean "not available".
#[derive(Clone, Default)]
pub struct Ctx {
    /// Absolute path of the file, its name, its folder and its path relative to the workspace.
    pub filepath: String,
    pub filename: String,
    pub directory: String,
    pub relative: String,
    pub workspace_name: String,
    pub workspace_folder: String,
    pub selection: String,
    pub clipboard: String,
    /// Zero-based line and char column of the insertion point.
    pub line_index: usize,
    pub cursor_index: usize,
    pub current_line: String,
    pub current_word: String,
    pub line_comment: String,
    pub block_start: String,
    pub block_end: String,
    /// Pinned clock for tests; the system clock when `None`.
    pub now: Option<Now>,
}

#[derive(Clone, Copy)]
pub struct Now {
    pub ts: i64,
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    /// Local offset from UTC in minutes.
    pub offset_min: i32,
}

impl Now {
    pub fn system() -> Now {
        let ts =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let (year, month, day, hour, min, sec) = crate::paths::local_time(ts);
        let local = days_from_civil(year, month, day) * 86400 + (hour * 3600 + min * 60 + sec) as i64;
        Now { ts, year, month, day, hour, min, sec, offset_min: ((local - ts) / 60) as i32 }
    }

    fn weekday(&self) -> usize {
        // 1970-01-01 was a Thursday; 0 = Sunday.
        ((days_from_civil(self.year, self.month, self.day) + 4).rem_euclid(7)) as usize
    }
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(if m <= 2 { y - 1 } else { y });
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new().build_hasher().finish()
}

fn uuid_v4() -> String {
    let (a, b) = (rand_u64(), rand_u64());
    let mut x = [0u8; 16];
    x[..8].copy_from_slice(&a.to_le_bytes());
    x[8..].copy_from_slice(&b.to_le_bytes());
    x[6] = (x[6] & 0x0f) | 0x40;
    x[8] = (x[8] & 0x3f) | 0x80;
    let h: Vec<String> = x.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..].concat())
}

impl Ctx {
    /// The value of a built-in variable, or `None` when the name is not one.
    fn var(&self, name: &str) -> Option<String> {
        let now = || self.now.unwrap_or_else(Now::system);
        Some(match name {
            "TM_FILENAME" => self.filename.clone(),
            "TM_FILENAME_BASE" => match self.filename.rfind('.') {
                Some(i) if i > 0 => self.filename[..i].to_string(),
                _ => self.filename.clone(),
            },
            "TM_DIRECTORY" => self.directory.clone(),
            "TM_FILEPATH" => self.filepath.clone(),
            "RELATIVE_FILEPATH" => self.relative.clone(),
            "WORKSPACE_NAME" => self.workspace_name.clone(),
            "WORKSPACE_FOLDER" => self.workspace_folder.clone(),
            "TM_SELECTED_TEXT" | "SELECTION" => self.selection.clone(),
            "CLIPBOARD" => self.clipboard.clone(),
            "TM_LINE_INDEX" => self.line_index.to_string(),
            "TM_LINE_NUMBER" => (self.line_index + 1).to_string(),
            "CURSOR_INDEX" => self.cursor_index.to_string(),
            "CURSOR_NUMBER" => (self.cursor_index + 1).to_string(),
            "TM_CURRENT_LINE" => self.current_line.clone(),
            "TM_CURRENT_WORD" => self.current_word.clone(),
            "LINE_COMMENT" => self.line_comment.clone(),
            "BLOCK_COMMENT_START" => self.block_start.clone(),
            "BLOCK_COMMENT_END" => self.block_end.clone(),
            "CURRENT_YEAR" => format!("{:04}", now().year),
            "CURRENT_YEAR_SHORT" => format!("{:02}", now().year.rem_euclid(100)),
            "CURRENT_MONTH" => format!("{:02}", now().month),
            "CURRENT_DATE" => format!("{:02}", now().day),
            "CURRENT_HOUR" => format!("{:02}", now().hour),
            "CURRENT_MINUTE" => format!("{:02}", now().min),
            "CURRENT_SECOND" => format!("{:02}", now().sec),
            "CURRENT_SECONDS_UNIX" => now().ts.to_string(),
            "CURRENT_MONTH_NAME" => MONTHS[(now().month as usize).clamp(1, 12) - 1].to_string(),
            "CURRENT_MONTH_NAME_SHORT" => MONTHS[(now().month as usize).clamp(1, 12) - 1][..3].to_string(),
            "CURRENT_DAY_NAME" => DAYS[now().weekday()].to_string(),
            "CURRENT_DAY_NAME_SHORT" => DAYS[now().weekday()][..3].to_string(),
            "CURRENT_TIMEZONE_OFFSET" => {
                let o = now().offset_min;
                format!("{}{:02}:{:02}", if o < 0 { '-' } else { '+' }, o.abs() / 60, o.abs() % 60)
            }
            "RANDOM" => format!("{:06}", rand_u64() % 1_000_000),
            "RANDOM_HEX" => format!("{:06x}", rand_u64() % 0x100_0000),
            "UUID" => uuid_v4(),
            _ => return None,
        })
    }
}

// ───────────── regex transforms ─────────────

#[derive(Clone)]
enum Modifier {
    None,
    Upcase,
    Downcase,
    Capitalize,
    Pascal,
    Camel,
    /// `+text`: only when the group matched.
    If(String),
    /// `-text` / plain `text`: when it did not.
    Else(String),
    IfElse(String, String),
}

#[derive(Clone)]
enum Fmt {
    Text(String),
    Group(usize, Modifier),
}

/// `/regex/format/options` applied to the text of a mirror or variable.
#[derive(Clone)]
pub struct Transform {
    re: Regex,
    fmt: Vec<Fmt>,
    global: bool,
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

fn cap(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

impl Transform {
    pub fn apply(&self, s: &str) -> String {
        let mut out = String::new();
        let mut last = 0;
        for caps in self.re.captures_iter(s) {
            let Some(m) = caps.get(0) else { continue };
            out.push_str(&s[last..m.start()]);
            for f in &self.fmt {
                match f {
                    Fmt::Text(t) => out.push_str(t),
                    Fmt::Group(n, md) => {
                        let g = caps.get(*n).map(|m| m.as_str()).filter(|t| !t.is_empty());
                        match (md, g) {
                            (Modifier::None, Some(t)) => out.push_str(t),
                            (Modifier::Upcase, Some(t)) => out.push_str(&t.to_uppercase()),
                            (Modifier::Downcase, Some(t)) => out.push_str(&t.to_lowercase()),
                            (Modifier::Capitalize, Some(t)) => {
                                let mut c = t.chars();
                                if let Some(f) = c.next() {
                                    out.extend(f.to_uppercase());
                                    out.push_str(c.as_str());
                                }
                            }
                            (Modifier::Pascal, Some(t)) => {
                                out.push_str(&words(t).iter().map(|w| cap(w)).collect::<String>())
                            }
                            (Modifier::Camel, Some(t)) => {
                                for (i, w) in words(t).iter().enumerate() {
                                    out.push_str(&if i == 0 { w.to_lowercase() } else { cap(w) });
                                }
                            }
                            (Modifier::If(t), Some(_)) => out.push_str(t),
                            (Modifier::IfElse(t, _), Some(_)) => out.push_str(t),
                            (Modifier::Else(t), None) => out.push_str(t),
                            (Modifier::IfElse(_, e), None) => out.push_str(e),
                            _ => {}
                        }
                    }
                }
            }
            last = m.end();
            if !self.global {
                break;
            }
        }
        out.push_str(&s[last..]);
        out
    }
}

/// Index of the first unescaped `stop` at brace depth 0, from `from`.
fn find_unescaped(c: &[char], from: usize, stop: char) -> Option<usize> {
    let mut i = from;
    let mut depth = 0;
    while i < c.len() {
        match c[i] {
            '\\' => i += 1,
            '$' if c.get(i + 1) == Some(&'{') => {
                depth += 1;
                i += 1;
            }
            '}' if depth > 0 => depth -= 1,
            ch if ch == stop && depth == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

fn parse_format(c: &[char]) -> Vec<Fmt> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    let flush = |text: &mut String, out: &mut Vec<Fmt>| {
        if !text.is_empty() {
            out.push(Fmt::Text(std::mem::take(text)));
        }
    };
    while i < c.len() {
        match c[i] {
            '\\' if i + 1 < c.len() && matches!(c[i + 1], '$' | '/' | '\\' | '}') => {
                text.push(c[i + 1]);
                i += 2;
            }
            '$' if i + 1 < c.len() && c[i + 1].is_ascii_digit() => {
                let mut j = i + 1;
                let mut n = 0usize;
                while j < c.len() && c[j].is_ascii_digit() {
                    n = n * 10 + c[j].to_digit(10).unwrap_or(0) as usize;
                    j += 1;
                }
                flush(&mut text, &mut out);
                out.push(Fmt::Group(n, Modifier::None));
                i = j;
            }
            '$' if c.get(i + 1) == Some(&'{') => {
                let mut j = i + 2;
                let mut n = 0usize;
                let ds = j;
                while j < c.len() && c[j].is_ascii_digit() {
                    n = n * 10 + c[j].to_digit(10).unwrap_or(0) as usize;
                    j += 1;
                }
                let close = (j..c.len()).find(|&k| c[k] == '}' && (k == 0 || c[k - 1] != '\\'));
                match (j > ds, close) {
                    (true, Some(close)) => {
                        flush(&mut text, &mut out);
                        let rest: String = c[j..close].iter().collect();
                        let unesc = |s: &str| s.replace("\\:", ":").replace("\\}", "}");
                        let md = if rest.is_empty() {
                            Modifier::None
                        } else if let Some(m) = rest.strip_prefix(":/") {
                            match m {
                                "upcase" => Modifier::Upcase,
                                "downcase" => Modifier::Downcase,
                                "capitalize" => Modifier::Capitalize,
                                "pascalcase" => Modifier::Pascal,
                                "camelcase" => Modifier::Camel,
                                _ => Modifier::None,
                            }
                        } else if let Some(m) = rest.strip_prefix(":+") {
                            Modifier::If(unesc(m))
                        } else if let Some(m) = rest.strip_prefix(":-") {
                            Modifier::Else(unesc(m))
                        } else if let Some(m) = rest.strip_prefix(":?") {
                            match m.split_once(':') {
                                Some((a, b)) => Modifier::IfElse(unesc(a), unesc(b)),
                                None => Modifier::If(unesc(m)),
                            }
                        } else if let Some(m) = rest.strip_prefix(':') {
                            Modifier::Else(unesc(m))
                        } else {
                            Modifier::None
                        };
                        out.push(Fmt::Group(n, md));
                        i = close + 1;
                    }
                    _ => {
                        text.push('$');
                        i += 1;
                    }
                }
            }
            ch => {
                text.push(ch);
                i += 1;
            }
        }
    }
    flush(&mut text, &mut out);
    out
}

/// Parses the `regex/format/options` part (the chars after the opening `/`). Returns the
/// transform and the index just after the options; `None` when malformed or the regex is invalid.
fn parse_transform(c: &[char], from: usize) -> Option<(Option<Transform>, usize)> {
    let e1 = find_unescaped(c, from, '/')?;
    let e2 = find_unescaped(c, e1 + 1, '/')?;
    let end = (e2 + 1..c.len()).find(|&k| c[k] == '}')?;
    let re_src: String = c[from..e1].iter().collect();
    let opts: String = c[e2 + 1..end].iter().collect();
    let mut flags = String::new();
    if opts.contains('i') {
        flags.push('i');
    }
    if opts.contains('m') {
        flags.push('m');
    }
    if opts.contains('s') {
        flags.push('s');
    }
    let src = if flags.is_empty() { re_src } else { format!("(?{flags}){re_src}") };
    let tf =
        Regex::new(&src).ok().map(|re| Transform { re, fmt: parse_format(&c[e1 + 1..e2]), global: opts.contains('g') });
    Some((tf, end))
}

// ───────────── parsing ─────────────

enum Node {
    Text(String),
    Tab { id: usize, n: u32, kids: Option<Vec<Node>>, tf: Option<Transform> },
    Var { name: String, kids: Option<Vec<Node>>, tf: Option<Transform> },
}

struct Parser {
    c: Vec<char>,
    i: usize,
    next_id: usize,
}

fn ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

impl Parser {
    fn nodes(&mut self, in_placeholder: bool) -> Vec<Node> {
        let mut out = Vec::new();
        let mut text = String::new();
        while self.i < self.c.len() {
            let ch = self.c[self.i];
            if in_placeholder && ch == '}' {
                break;
            }
            match ch {
                '\\' if self.i + 1 < self.c.len() && matches!(self.c[self.i + 1], '$' | '}' | '\\') => {
                    text.push(self.c[self.i + 1]);
                    self.i += 2;
                }
                '$' => {
                    let save = self.i;
                    match self.dollar() {
                        Some(node) => {
                            if !text.is_empty() {
                                out.push(Node::Text(std::mem::take(&mut text)));
                            }
                            out.push(node);
                        }
                        None => {
                            self.i = save + 1;
                            text.push('$');
                        }
                    }
                }
                _ => {
                    text.push(ch);
                    self.i += 1;
                }
            }
        }
        if !text.is_empty() {
            out.push(Node::Text(text));
        }
        out
    }

    fn number(&mut self) -> Option<u32> {
        let s = self.i;
        let mut n = 0u32;
        while self.i < self.c.len() && self.c[self.i].is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add(self.c[self.i].to_digit(10)?);
            self.i += 1;
        }
        (self.i > s).then_some(n)
    }

    fn ident(&mut self) -> Option<String> {
        let s = self.i;
        if !self.c.get(self.i).is_some_and(|c| ident_start(*c)) {
            return None;
        }
        while self.i < self.c.len() && (self.c[self.i].is_ascii_alphanumeric() || self.c[self.i] == '_') {
            self.i += 1;
        }
        Some(self.c[s..self.i].iter().collect())
    }

    fn id(&mut self) -> usize {
        self.next_id += 1;
        self.next_id - 1
    }

    /// At a `$`. `None` leaves it to the caller to treat the `$` as text.
    fn dollar(&mut self) -> Option<Node> {
        self.i += 1;
        let next = *self.c.get(self.i)?;
        if next.is_ascii_digit() {
            let id = self.id();
            let n = self.number()?;
            return Some(Node::Tab { id, n, kids: None, tf: None });
        }
        if ident_start(next) {
            let name = self.ident()?;
            return Some(Node::Var { name, kids: None, tf: None });
        }
        if next != '{' {
            return None;
        }
        self.i += 1;
        if self.c.get(self.i).is_some_and(|c| c.is_ascii_digit()) {
            let id = self.id();
            let n = self.number()?;
            return match self.c.get(self.i)? {
                '}' => {
                    self.i += 1;
                    Some(Node::Tab { id, n, kids: None, tf: None })
                }
                ':' => {
                    self.i += 1;
                    let kids = self.nodes(true);
                    self.expect_close()?;
                    Some(Node::Tab { id, n, kids: Some(kids), tf: None })
                }
                '|' => {
                    self.i += 1;
                    let first = self.choices()?;
                    Some(Node::Tab { id, n, kids: Some(vec![Node::Text(first)]), tf: None })
                }
                '/' => {
                    let (tf, end) = parse_transform(&self.c, self.i + 1)?;
                    self.i = end + 1;
                    Some(Node::Tab { id, n, kids: None, tf })
                }
                _ => None,
            };
        }
        let name = self.ident()?;
        match self.c.get(self.i)? {
            '}' => {
                self.i += 1;
                Some(Node::Var { name, kids: None, tf: None })
            }
            ':' => {
                self.i += 1;
                let kids = self.nodes(true);
                self.expect_close()?;
                Some(Node::Var { name, kids: Some(kids), tf: None })
            }
            '/' => {
                let (tf, end) = parse_transform(&self.c, self.i + 1)?;
                self.i = end + 1;
                Some(Node::Var { name, kids: None, tf })
            }
            _ => None,
        }
    }

    fn expect_close(&mut self) -> Option<()> {
        if self.c.get(self.i) == Some(&'}') {
            self.i += 1;
            Some(())
        } else {
            None
        }
    }

    /// After `${1|`: reads `a,b,c|}` and returns the first choice.
    fn choices(&mut self) -> Option<String> {
        let mut cur = String::new();
        let mut first: Option<String> = None;
        loop {
            let ch = *self.c.get(self.i)?;
            match ch {
                '\\' if self.i + 1 < self.c.len() && matches!(self.c[self.i + 1], '$' | '}' | '\\' | ',' | '|') => {
                    cur.push(self.c[self.i + 1]);
                    self.i += 2;
                }
                ',' => {
                    first.get_or_insert_with(|| std::mem::take(&mut cur));
                    cur.clear();
                    self.i += 1;
                }
                '|' if self.c.get(self.i + 1) == Some(&'}') => {
                    self.i += 2;
                    return Some(first.unwrap_or(cur));
                }
                _ => {
                    cur.push(ch);
                    self.i += 1;
                }
            }
        }
    }
}

// ───────────── rendering ─────────────

/// A copy of a tab stop's text that follows it (same number used again), optionally transformed.
#[derive(Clone)]
pub struct Mirror {
    /// Index into [`Expanded::stops`] of the stop it follows.
    pub stop: usize,
    pub start: usize,
    pub end: usize,
    pub tf: Option<Transform>,
}

pub struct Expanded {
    pub text: String,
    /// Tab stops in visiting order (`$1`, `$2`, … then `$0`), as char ranges into `text`.
    pub stops: Vec<(usize, usize)>,
    pub mirrors: Vec<Mirror>,
}

struct Render<'a> {
    ctx: &'a Ctx,
    indent: &'a str,
    /// tab number -> id of the node that owns the stop
    owners: &'a HashMap<u32, usize>,
    /// tab number -> initial text of its stop (what mirrors start with)
    initial: &'a HashMap<u32, String>,
    flat: bool,
    text: String,
    n: usize,
    stops: Vec<(u32, usize, usize)>,
    mirrors: Vec<(u32, usize, usize, Option<Transform>)>,
}

impl Render<'_> {
    fn raw(&mut self, s: &str) {
        self.n += s.chars().count();
        self.text.push_str(s);
    }

    fn text(&mut self, s: &str) {
        let mut parts = s.split('\n');
        if let Some(p) = parts.next() {
            self.raw(p);
        }
        for p in parts {
            self.raw("\n");
            let ind = self.indent;
            self.raw(ind);
            self.raw(p);
        }
    }

    fn emit(&mut self, nodes: &[Node]) {
        for node in nodes {
            match node {
                Node::Text(t) => self.text(t),
                Node::Var { name, kids, tf } => {
                    let known = self.ctx.var(name);
                    let mut v = known.clone().filter(|v| !v.is_empty());
                    if v.is_none() {
                        match kids {
                            Some(k) => {
                                let mut sub = self.sub();
                                sub.emit(k);
                                v = Some(sub.text);
                            }
                            None if known.is_none() => v = Some(name.clone()),
                            None => {}
                        }
                    }
                    let mut v = v.unwrap_or_default();
                    if let Some(tf) = tf {
                        v = tf.apply(&v);
                    }
                    self.raw(&v);
                }
                Node::Tab { id, n, kids, tf } => {
                    let owner = self.owners.get(n) == Some(id);
                    if self.flat {
                        if let Some(k) = kids {
                            self.emit(k);
                        } else if !owner && let Some(t) = self.initial.get(n) {
                            let t = tf.as_ref().map_or_else(|| t.clone(), |tf| tf.apply(t));
                            self.raw(&t);
                        }
                    } else if owner {
                        let start = self.n;
                        if let Some(k) = kids {
                            self.emit(k);
                        }
                        self.stops.push((*n, start, self.n));
                    } else if *n != 0 {
                        let start = self.n;
                        let t = self.initial.get(n).cloned().unwrap_or_default();
                        let t = tf.as_ref().map_or_else(|| t.clone(), |tf| tf.apply(&t));
                        self.raw(&t);
                        self.mirrors.push((*n, start, self.n, tf.clone()));
                    }
                }
            }
        }
    }

    fn sub(&self) -> Render<'_> {
        Render {
            ctx: self.ctx,
            indent: self.indent,
            owners: self.owners,
            initial: self.initial,
            flat: true,
            text: String::new(),
            n: 0,
            stops: Vec::new(),
            mirrors: Vec::new(),
        }
    }
}

/// Per tab number, the node that owns the stop: the first one with a default, else the first
/// without a transform, else the first.
fn pick_owners(nodes: &[Node], cand: &mut HashMap<u32, (usize, u8)>) {
    for node in nodes {
        match node {
            Node::Tab { id, n, kids, tf } => {
                let rank = if kids.is_some() {
                    3
                } else if tf.is_none() {
                    2
                } else {
                    1
                };
                if cand.get(n).is_none_or(|(_, r)| rank > *r) {
                    cand.insert(*n, (*id, rank));
                }
                if let Some(k) = kids {
                    pick_owners(k, cand);
                }
            }
            Node::Var { kids: Some(k), .. } => pick_owners(k, cand),
            _ => {}
        }
    }
}

fn find_kids(nodes: &[Node], id: usize) -> Option<&Option<Vec<Node>>> {
    for node in nodes {
        match node {
            Node::Tab { id: i, kids, .. } => {
                if *i == id {
                    return Some(kids);
                }
                if let Some(k) = kids
                    && let Some(f) = find_kids(k, id)
                {
                    return Some(f);
                }
            }
            Node::Var { kids: Some(k), .. } => {
                if let Some(f) = find_kids(k, id) {
                    return Some(f);
                }
            }
            _ => {}
        }
    }
    None
}

/// Expand `body`, indenting continuation lines with `indent` and resolving variables from `ctx`.
pub fn expand(body: &str, indent: &str, ctx: &Ctx) -> Expanded {
    let mut p = Parser { c: body.chars().collect(), i: 0, next_id: 0 };
    let nodes = p.nodes(false);

    let mut cand = HashMap::new();
    pick_owners(&nodes, &mut cand);
    let owners: HashMap<u32, usize> = cand.iter().map(|(n, (id, _))| (*n, *id)).collect();

    // Initial text of each stop (flat render of its default), what mirrors start with.
    let empty = HashMap::new();
    let mut initial: HashMap<u32, String> = HashMap::new();
    for (n, id) in &owners {
        if let Some(Some(k)) = find_kids(&nodes, *id) {
            let mut r = Render {
                ctx,
                indent,
                owners: &owners,
                initial: &empty,
                flat: true,
                text: String::new(),
                n: 0,
                stops: Vec::new(),
                mirrors: Vec::new(),
            };
            r.emit(k);
            initial.insert(*n, r.text);
        }
    }

    let mut r = Render {
        ctx,
        indent,
        owners: &owners,
        initial: &initial,
        flat: false,
        text: String::new(),
        n: 0,
        stops: Vec::new(),
        mirrors: Vec::new(),
    };
    r.emit(&nodes);
    let total = r.n;
    let mut stops = r.stops;
    // Visiting order: 1, 2, 3, … and `$0` last (at the end of the text if the body has none).
    stops.sort_by_key(|(num, s, _)| (if *num == 0 { u32::MAX } else { *num }, *s));
    let order: Vec<u32> = stops.iter().map(|(n, _, _)| *n).collect();
    let mut out: Vec<(usize, usize)> = stops.iter().map(|(_, s, e)| (*s, *e)).collect();
    if !order.contains(&0) {
        out.push((total, total));
    }
    let mirrors = r
        .mirrors
        .into_iter()
        .filter_map(|(n, start, end, tf)| Some(Mirror { stop: order.iter().position(|o| *o == n)?, start, end, tf }))
        .collect();
    Expanded { text: r.text, stops: out, mirrors }
}

/// Re-evaluates mirrors after the stop `idx` changed to `value`: returns the replacement text
/// for each mirror of that stop.
pub fn mirror_text(m: &Mirror, value: &str) -> String {
    m.tf.as_ref().map_or_else(|| value.to_string(), |t| t.apply(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(b: &str) -> Expanded {
        expand(b, "", &Ctx::default())
    }

    #[test]
    fn escapes_and_literal_dollars() {
        assert_eq!(ex("a\\$1 \\} \\\\ $ 5").text, "a$1 } \\ $ 5");
        assert_eq!(ex("x}y").text, "x}y");
    }

    #[test]
    fn nested_placeholders_and_choices() {
        let e = ex("${1:foo ${2:bar}} ${3|one,two|}");
        assert_eq!(e.text, "foo bar one");
        assert_eq!(e.stops[0], (0, 7));
        assert_eq!(e.stops[1], (4, 7));
        assert_eq!(e.stops[2], (8, 11), "a choice starts with the first option selected");
    }

    #[test]
    fn mirrors_follow_the_owner() {
        let e = ex("$1 and ${1:name} and $1");
        assert_eq!(e.text, "name and name and name");
        assert_eq!(e.stops[0], (9, 13), "the placeholder with a default owns the stop");
        assert_eq!(e.mirrors.len(), 2);
        assert_eq!((e.mirrors[0].start, e.mirrors[0].end), (0, 4));
        assert_eq!(e.mirrors[0].stop, 0);
    }

    #[test]
    fn transforms_on_mirrors() {
        let e = ex("${1:hello world} ${1/(.*)/${1:/upcase}/}");
        assert_eq!(e.text, "hello world HELLO WORLD");
        let e = ex("${1:foo_bar} ${1/(\\w+)_(\\w+)/$2-$1/}");
        assert_eq!(e.text, "foo_bar bar-foo");
        let m = &e.mirrors[0];
        assert_eq!(mirror_text(m, "a_b"), "b-a");
        let e = ex("${1:x} ${1/(.)/${1:/pascalcase}-${1:/camelcase}/}");
        assert_eq!(e.text, "x X-x");
    }

    #[test]
    fn transform_flags_conditionals_and_bad_regex() {
        let e = ex("${1:a b c} ${1/ /_/g}");
        assert_eq!(e.text, "a b c a_b_c");
        let e = ex("${1:abc} ${1/(b)|(z)/${2:-nope}${1:+!}/}");
        assert_eq!(e.text, "abc anope!c");
        let e = ex("${1:A} ${1/(a)/${1:?yes:no}/i}");
        assert_eq!(e.text, "A yes");
        let e = ex("${1:v} ${1/(/x/}");
        assert!(e.text.starts_with("v "), "an invalid regex must not panic");
        assert_eq!(ex("${1:foo_bar_baz} ${1/(\\w+)/${1:/pascalcase}/}").text, "foo_bar_baz FooBarBaz");
    }

    #[test]
    fn variables_resolve_with_defaults_and_unknown_names() {
        let ctx = Ctx {
            filename: "app.test.ts".into(),
            relative: "src/app.test.ts".into(),
            line_comment: "//".into(),
            selection: "SEL".into(),
            clipboard: "clip".into(),
            now: Some(Now { ts: 0, year: 2026, month: 3, day: 7, hour: 9, min: 5, sec: 1, offset_min: -180 }),
            ..Ctx::default()
        };
        let e = |b: &str| expand(b, "", &ctx).text;
        assert_eq!(e("$TM_FILENAME ${TM_FILENAME_BASE} $RELATIVE_FILEPATH"), "app.test.ts app.test src/app.test.ts");
        assert_eq!(e("$LINE_COMMENT $SELECTION $CLIPBOARD ${TM_SELECTED_TEXT}"), "// SEL clip SEL");
        assert_eq!(
            e("$CURRENT_YEAR-$CURRENT_MONTH-$CURRENT_DATE $CURRENT_HOUR:$CURRENT_MINUTE:$CURRENT_SECOND"),
            "2026-03-07 09:05:01"
        );
        assert_eq!(
            e("$CURRENT_YEAR_SHORT $CURRENT_MONTH_NAME $CURRENT_MONTH_NAME_SHORT $CURRENT_DAY_NAME_SHORT"),
            "26 March Mar Sat"
        );
        assert_eq!(e("$CURRENT_TIMEZONE_OFFSET"), "-03:00");
        assert_eq!(e("${BLOCK_COMMENT_START:fallback}"), "fallback", "empty value takes the default");
        assert_eq!(e("$NOPE ${ALSO_NOPE}"), "NOPE ALSO_NOPE", "unknown variables show their name");
        assert_eq!(e("${TM_FILENAME/(.*)\\..+$/$1/}"), "app.test");
        assert_eq!(e("${TM_FILENAME/[^.]*\\.(.*)/${1:/upcase}/}"), "TEST.TS");
        let u = expand("$UUID", "", &ctx).text;
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_eq!(
            expand("$RANDOM $RANDOM_HEX", "", &ctx).text.split(' ').map(str::len).collect::<Vec<_>>(),
            vec![6, 6]
        );
    }

    #[test]
    fn multiline_variables_and_text_indent() {
        let ctx = Ctx { clipboard: "a\nb".into(), ..Ctx::default() };
        assert_eq!(expand("x\n$CLIPBOARD", "  ", &ctx).text, "x\n  a\nb");
    }

    #[test]
    fn variable_inside_placeholder_default() {
        let ctx = Ctx { filename: "f.rs".into(), ..Ctx::default() };
        let e = expand("${1:$TM_FILENAME}!", "", &ctx);
        assert_eq!(e.text, "f.rs!");
        assert_eq!(e.stops[0], (0, 4));
    }
}
