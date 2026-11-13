//! `.http` / `.rest` files (JetBrains HTTP Client and VS Code REST Client syntax): parsing,
//! `{{variable}}` substitution, environment files and response chaining. Pure functions, no UI.
//!
//! ```text
//! @host = https://api.example.com
//! ### login
//! # @name login
//! POST {{host}}/login
//! Content-Type: application/json
//!
//! {"user": "ada"}
//!
//! ###
//! GET {{host}}/me
//! Authorization: Bearer {{login.response.body.$.token}}
//! ```

use crate::httpclient::{Prepared, Resp};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    None,
    Text(String),
    /// `< path` (sent as is) or `<@ path` (variables substituted first).
    File {
        path: String,
        subst: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Req {
    /// `# @name x`: lets later requests read `{{x.response...}}`.
    pub name: Option<String>,
    /// First and last line of the block (between `###` separators).
    pub start: usize,
    pub end: usize,
    /// The line holding `METHOD URL`.
    pub line: usize,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

#[derive(Default, Debug)]
pub struct HttpFile {
    /// `@name = value` lines, in file order (a later definition wins).
    pub vars: Vec<(String, String)>,
    pub reqs: Vec<Req>,
}

fn comment_body(t: &str) -> Option<&str> {
    t.strip_prefix('#').or_else(|| t.strip_prefix("//")).map(str::trim)
}

fn file_var(t: &str) -> Option<(String, String)> {
    let r = t.strip_prefix('@')?;
    let (k, v) = r.split_once('=')?;
    let k = k.trim();
    (!k.is_empty() && !k.contains(char::is_whitespace)).then(|| (k.to_string(), v.trim().to_string()))
}

fn name_tag(c: &str) -> Option<String> {
    let r = c.strip_prefix("@name")?;
    if !r.starts_with(|ch: char| ch.is_whitespace() || ch == '=') {
        return None;
    }
    let n = r.trim_start_matches(|ch: char| ch.is_whitespace() || ch == '=').trim();
    (!n.is_empty()).then(|| n.to_string())
}

fn request_line(t: &str) -> (String, String) {
    let first = t.split_whitespace().next().unwrap_or("");
    let more = t[first.len()..].trim();
    let is_method = first.len() >= 3 && first.chars().all(|c| c.is_ascii_uppercase()) && !more.is_empty();
    let (method, mut rest) = if is_method { (first.to_string(), more) } else { ("GET".to_string(), t) };
    if let Some((head, last)) = rest.rsplit_once(char::is_whitespace)
        && last.starts_with("HTTP/")
    {
        rest = head.trim_end();
    } else if rest.starts_with("HTTP/") {
        rest = "";
    }
    (method, rest.to_string())
}

fn parse_block(lines: &[&str], s: usize, e: usize, out: &mut HttpFile) {
    let (mut i, mut name) = (s, None);
    while i < e {
        let t = lines[i].trim();
        if t.is_empty() {
            i += 1;
        } else if let Some(kv) = file_var(t) {
            out.vars.push(kv);
            i += 1;
        } else if let Some(c) = comment_body(t) {
            if let Some(n) = name_tag(c) {
                name = Some(n);
            }
            i += 1;
        } else {
            break;
        }
    }
    if i >= e {
        return;
    }
    let line = i;
    let (method, mut url) = request_line(lines[i].trim());
    i += 1;
    while i < e {
        let t = lines[i].trim();
        if lines[i].starts_with(char::is_whitespace) && (t.starts_with('?') || t.starts_with('&')) {
            url.push_str(t);
            i += 1;
        } else {
            break;
        }
    }
    let mut headers = Vec::new();
    while i < e {
        let t = lines[i].trim();
        i += 1;
        if t.is_empty() {
            break;
        }
        if comment_body(t).is_some() {
            continue;
        }
        if let Some((k, v)) = t.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let mut body_lines: Vec<&str> = lines[i.min(e)..e].to_vec();
    while body_lines.last().is_some_and(|l| l.trim().is_empty()) {
        body_lines.pop();
    }
    let text = body_lines.join("\n");
    let t = text.trim();
    let body = if t.is_empty() {
        Body::None
    } else if !t.contains('\n')
        && let Some(p) = t.strip_prefix("<@")
    {
        // `<@latin1 ./f` carries an encoding word we do not need: bodies are read as UTF-8.
        let p = p.trim();
        let p = match p.split_once(char::is_whitespace) {
            Some((enc, rest)) if !enc.contains(['/', '.', '\\']) => rest.trim(),
            _ => p,
        };
        Body::File { path: p.to_string(), subst: true }
    } else if !t.contains('\n')
        && let Some(p) = t.strip_prefix("< ")
    {
        Body::File { path: p.trim().to_string(), subst: false }
    } else {
        Body::Text(text)
    };
    out.reqs.push(Req { name, start: s, end: e - 1, line, method, url, headers, body });
}

pub fn parse(text: &str) -> HttpFile {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = HttpFile::default();
    let mut start = 0;
    for i in 0..=lines.len() {
        if i == lines.len() || lines[i].trim_start().starts_with("###") {
            parse_block(&lines, start, i, &mut out);
            start = i + 1;
        }
    }
    out
}

/// The request a cursor on `line` belongs to; on a `###` separator, the next one.
pub fn req_at(f: &HttpFile, line: usize) -> Option<usize> {
    f.reqs.iter().position(|r| line >= r.start && line <= r.end).or_else(|| f.reqs.iter().position(|r| r.start > line))
}

// ───────────── environments ─────────────

pub type Envs = BTreeMap<String, BTreeMap<String, String>>;

fn read_env(path: &Path, into: &mut Envs) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) else { return };
    for (env, vars) in m {
        if let Value::Object(vs) = vars {
            let e = into.entry(env).or_default();
            for (k, v) in vs {
                e.insert(k, if let Value::String(s) = v { s } else { v.to_string() });
            }
        }
    }
}

/// `http-client.env.json` and `http-client.private.env.json` (the latter wins) from the project
/// root, then from the folder of the `.http` file (closest wins).
pub fn load_envs(dir: &Path, root: &Path) -> Envs {
    let mut envs = Envs::new();
    let mut dirs = vec![root];
    if dir != root {
        dirs.push(dir);
    }
    for d in dirs {
        for f in ["http-client.env.json", "http-client.private.env.json"] {
            read_env(&d.join(f), &mut envs);
        }
    }
    envs
}

/// `$shared` plus the selected environment.
pub fn env_vars(envs: &Envs, selected: Option<&str>) -> BTreeMap<String, String> {
    let mut m = envs.get("$shared").cloned().unwrap_or_default();
    if let Some(s) = selected
        && let Some(e) = envs.get(s)
    {
        m.extend(e.clone());
    }
    m
}

/// Environment names to cycle through (`$shared` is not selectable).
pub fn env_names(envs: &Envs) -> Vec<String> {
    envs.keys().filter(|k| *k != "$shared").cloned().collect()
}

// ───────────── variables ─────────────

pub struct Ctx<'a> {
    pub vars: &'a [(String, String)],
    pub env: &'a BTreeMap<String, String>,
    pub responses: &'a HashMap<String, Resp>,
    /// Folder of the `.http` file (relative `< file` bodies and `.env`).
    pub dir: &'a Path,
}

pub fn subst(s: &str, ctx: &Ctx) -> Result<String, String> {
    subst_depth(s, ctx, 0)
}

fn subst_depth(s: &str, ctx: &Ctx, depth: usize) -> Result<String, String> {
    if depth > 8 {
        return Err("variables nest too deeply (a cycle?)".into());
    }
    let (mut out, mut rest) = (String::new(), s);
    while let Some(a) = rest.find("{{") {
        out.push_str(&rest[..a]);
        let after = &rest[a + 2..];
        let Some(b) = after.find("}}") else {
            out.push_str(&rest[a..]);
            return Ok(out);
        };
        out.push_str(&resolve(after[..b].trim(), ctx, depth)?);
        rest = &after[b + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn resolve(expr: &str, ctx: &Ctx, depth: usize) -> Result<String, String> {
    if let Some(sys) = expr.strip_prefix('$') {
        return system_var(sys, ctx);
    }
    if let Some((_, v)) = ctx.vars.iter().rev().find(|(k, _)| k == expr) {
        return subst_depth(v, ctx, depth + 1);
    }
    if let Some(v) = ctx.env.get(expr) {
        return subst_depth(v, ctx, depth + 1);
    }
    if let Some((name, rest)) = expr.split_once(".response.") {
        let r =
            ctx.responses.get(name).ok_or_else(|| format!("request `{name}` has no response yet: send it first"))?;
        return chain_value(r, rest);
    }
    Err(format!("undefined variable `{expr}`"))
}

fn chain_value(r: &Resp, rest: &str) -> Result<String, String> {
    if let Some(h) = rest.strip_prefix("headers.") {
        r.header(h).map(str::to_string).ok_or_else(|| format!("the response has no header `{h}`"))
    } else if let Some(p) = rest.strip_prefix("body.") {
        if p == "*" {
            Ok(r.text())
        } else if p.starts_with('$') {
            json_path(&r.text(), p)
        } else {
            Err("only `body.*` and JSONPath (`body.$.a.b[0]`) can be read from a response body".into())
        }
    } else {
        Err(format!("cannot read `{rest}` from a response (use body.… or headers.…)"))
    }
}

/// Minimal JSONPath: `$`, `.key`, `[0]`, `['key']`.
pub fn json_path(text: &str, path: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(text).map_err(|_| "the response body is not JSON".to_string())?;
    let p = path.strip_prefix('$').ok_or("a JSONPath starts with `$`")?;
    let (b, mut i, mut cur) = (p.as_bytes(), 0, &v);
    while i < b.len() {
        match b[i] {
            b'.' => {
                i += 1;
                let st = i;
                while i < b.len() && b[i] != b'.' && b[i] != b'[' {
                    i += 1;
                }
                let key = &p[st..i];
                cur = cur.get(key).ok_or_else(|| format!("`{key}` not found in the response"))?;
            }
            b'[' => {
                let end = p[i..].find(']').ok_or("unclosed `[` in the JSONPath")? + i;
                let inner = p[i + 1..end].trim();
                i = end + 1;
                cur = match inner.parse::<usize>() {
                    Ok(n) => cur.get(n).ok_or_else(|| format!("index {n} not found in the response"))?,
                    Err(_) => {
                        let k = inner.trim_matches(['\'', '"']);
                        cur.get(k).ok_or_else(|| format!("`{k}` not found in the response"))?
                    }
                };
            }
            _ => return Err(format!("bad JSONPath `{path}`")),
        }
    }
    Ok(match cur {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        o => o.to_string(),
    })
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn rand_u64() -> u64 {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
    let mut z = STATE.fetch_add(seed | 1, Ordering::Relaxed).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

pub fn uuid_v4() -> String {
    let (a, b) = (rand_u64(), rand_u64());
    let mut x = [0u8; 16];
    x[..8].copy_from_slice(&a.to_be_bytes());
    x[8..].copy_from_slice(&b.to_be_bytes());
    x[6] = (x[6] & 0x0f) | 0x40;
    x[8] = (x[8] & 0x3f) | 0x80;
    let h: String = x.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// `-1 d`, `2 h`... after a `$timestamp` / `$datetime`: an offset in seconds.
fn offset(args: &[&str]) -> i64 {
    let mut total = 0;
    for p in args.chunks(2) {
        if let [n, u] = p
            && let Ok(n) = n.parse::<i64>()
        {
            total += n * match *u {
                "s" => 1,
                "m" => 60,
                "h" => 3600,
                "d" => 86400,
                "w" => 604800,
                _ => 0,
            };
        }
    }
    total
}

fn civil(secs: i64) -> (i64, i64, i64, i64, i64, i64, usize) {
    let days = secs.div_euclid(86400);
    let sod = secs.rem_euclid(86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, sod / 3600, sod % 3600 / 60, sod % 60, (days + 4).rem_euclid(7) as usize)
}

fn format_time(secs: i64, fmt: &str) -> String {
    let (y, mo, d, h, mi, s, wd) = civil(secs);
    if fmt.eq_ignore_ascii_case("rfc1123") {
        const WD: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MO: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        format!("{}, {d:02} {} {y} {h:02}:{mi:02}:{s:02} GMT", WD[wd], MO[(mo - 1) as usize])
    } else {
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
    }
}

fn system_var(expr: &str, ctx: &Ctx) -> Result<String, String> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    let (name, args) = (parts.first().copied().unwrap_or(""), parts.get(1..).unwrap_or(&[]));
    match name {
        "guid" | "uuid" => Ok(uuid_v4()),
        "timestamp" => Ok((now_secs() + offset(args)).to_string()),
        "datetime" | "localDatetime" => {
            let fmt = args.first().copied().unwrap_or("iso8601");
            Ok(format_time(now_secs() + offset(args.get(1..).unwrap_or(&[])), fmt))
        }
        "randomInt" => {
            let lo = args.first().and_then(|a| a.parse::<i64>().ok()).unwrap_or(0);
            let hi = args.get(1).and_then(|a| a.parse::<i64>().ok()).unwrap_or(1000);
            let span = (hi - lo).max(1) as u64;
            Ok((lo + (rand_u64() % span) as i64).to_string())
        }
        "processEnv" => {
            let n = args.first().ok_or("$processEnv needs a variable name")?;
            std::env::var(n).map_err(|_| format!("environment variable `{n}` is not set"))
        }
        "dotenv" => {
            let n = args.first().ok_or("$dotenv needs a variable name")?;
            let text = std::fs::read_to_string(ctx.dir.join(".env")).map_err(|_| "no .env next to the .http file")?;
            text.lines()
                .filter_map(|l| l.trim().split_once('='))
                .find(|(k, _)| k.trim() == *n)
                .map(|(_, v)| v.trim().trim_matches(['"', '\'']).to_string())
                .ok_or_else(|| format!("`{n}` is not in .env"))
        }
        _ => Err(format!("unknown system variable `${name}`")),
    }
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for k in 0..4 {
            if k <= c.len() {
                s.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// `Authorization: Basic user pass` (or `user:pass`) becomes the base64 form.
fn auth_value(v: &str) -> String {
    let p: Vec<&str> = v.split_whitespace().collect();
    match p.as_slice() {
        [b, user, pass] if b.eq_ignore_ascii_case("basic") => {
            format!("Basic {}", base64(format!("{user}:{pass}").as_bytes()))
        }
        [b, up] if b.eq_ignore_ascii_case("basic") && up.contains(':') => format!("Basic {}", base64(up.as_bytes())),
        _ => v.to_string(),
    }
}

/// Resolve every variable of `req` and read its body file: ready to send.
pub fn prepare(req: &Req, ctx: &Ctx) -> Result<Prepared, String> {
    let url = subst(&req.url, ctx)?.trim().to_string();
    if url.is_empty() {
        return Err("the request has no URL".into());
    }
    let mut headers = Vec::new();
    for (k, v) in &req.headers {
        let (k, mut v) = (subst(k, ctx)?, subst(v, ctx)?);
        if k.eq_ignore_ascii_case("authorization") {
            v = auth_value(&v);
        }
        headers.push((k, v));
    }
    let body = match &req.body {
        Body::None => Vec::new(),
        Body::Text(t) => subst(t, ctx)?.into_bytes(),
        Body::File { path, subst: s } => {
            let p = subst(path, ctx)?;
            let full = if Path::new(&p).is_absolute() { Path::new(&p).to_path_buf() } else { ctx.dir.join(&p) };
            let bytes = std::fs::read(&full).map_err(|e| format!("cannot read {}: {e}", full.display()))?;
            if *s {
                let text = String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8", full.display()))?;
                subst(&text, ctx)?.into_bytes()
            } else {
                bytes
            }
        }
    };
    Ok(Prepared { method: req.method.to_ascii_uppercase(), url, headers, body })
}

// ───────────── response formatting ─────────────

/// Re-indent JSON keeping key order and number spelling; `None` when `s` is not valid JSON.
pub fn pretty_json(s: &str) -> Option<String> {
    serde_json::from_str::<serde::de::IgnoredAny>(s).ok()?;
    let mut out = String::with_capacity(s.len() * 2);
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    let chars: Vec<char> = s.chars().collect();
    let nl = |out: &mut String, d: usize| {
        out.push('\n');
        for _ in 0..d {
            out.push_str("  ");
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' | '[' => {
                let close = if c == '{' { '}' } else { ']' };
                let next = chars[i..].iter().find(|x| !x.is_whitespace());
                if next == Some(&close) {
                    out.push(c);
                    out.push(close);
                    while chars[i] != close {
                        i += 1;
                    }
                    i += 1;
                } else {
                    out.push(c);
                    depth += 1;
                    nl(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                nl(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(',');
                nl(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    Some(out)
}

/// File extension for saving a response, from its `Content-Type` (and a peek at the body).
pub fn extension(content_type: Option<&str>, body: &str) -> &'static str {
    let ct = content_type.unwrap_or("").to_ascii_lowercase();
    if ct.contains("json") {
        "json"
    } else if ct.contains("html") {
        "html"
    } else if ct.contains("xml") {
        "xml"
    } else if ct.contains("javascript") {
        "js"
    } else if ct.contains("css") {
        "css"
    } else if ct.contains("yaml") {
        "yaml"
    } else if ct.contains("csv") {
        "csv"
    } else if ct.is_empty() && pretty_json(body).is_some() {
        "json"
    } else {
        "txt"
    }
}

pub fn human_size(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1 << 20 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1 << 20) as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "@host = http://h\n@token=abc\n\n### list\n# @name list\nGET {{host}}/items\n    ?page=2\n    &q=x\nAccept: application/json\n\n###\n// @name = make\nPOST {{host}}/items HTTP/1.1\nContent-Type: application/json\n\n{\n  \"a\": 1\n}\n\n###\nPUT /file\n\n< ./body.json\n\n###\nhttp://h/bare\n";

    #[test]
    fn parses_blocks() {
        let f = parse(SRC);
        assert_eq!(f.vars, vec![("host".into(), "http://h".into()), ("token".into(), "abc".into())]);
        assert_eq!(f.reqs.len(), 4);
        let r = &f.reqs[0];
        assert_eq!(
            (r.method.as_str(), r.url.as_str(), r.name.as_deref()),
            ("GET", "{{host}}/items?page=2&q=x", Some("list"))
        );
        assert_eq!(r.headers, vec![("Accept".to_string(), "application/json".to_string())]);
        assert_eq!(r.body, Body::None);
        assert_eq!(r.line, 5);
        let r = &f.reqs[1];
        assert_eq!((r.method.as_str(), r.url.as_str(), r.name.as_deref()), ("POST", "{{host}}/items", Some("make")));
        assert_eq!(r.body, Body::Text("{\n  \"a\": 1\n}".into()));
        assert_eq!(f.reqs[2].body, Body::File { path: "./body.json".into(), subst: false });
        assert_eq!((f.reqs[3].method.as_str(), f.reqs[3].url.as_str()), ("GET", "http://h/bare"));
    }

    #[test]
    fn cursor_maps_to_request() {
        let f = parse(SRC);
        let at = |l| req_at(&f, l);
        assert_eq!(at(5), Some(0));
        assert_eq!(at(9), Some(0));
        // the `###` line itself picks the request below
        assert_eq!(at(f.reqs[1].start - 1), Some(1));
        assert_eq!(at(f.reqs[2].line), Some(2));
        assert_eq!(at(1000), None);
    }

    #[test]
    fn file_body_with_substitution() {
        let f = parse("POST http://x\n\n<@ ./t.json\n");
        assert_eq!(f.reqs[0].body, Body::File { path: "./t.json".into(), subst: true });
        let f = parse("POST http://x\n\n<@latin1 ./t.json\n");
        assert_eq!(f.reqs[0].body, Body::File { path: "./t.json".into(), subst: true });
        // XML is a body, not a file reference
        let f = parse("POST http://x\n\n<a>1</a>\n");
        assert_eq!(f.reqs[0].body, Body::Text("<a>1</a>".into()));
    }

    fn ctx<'a>(
        vars: &'a [(String, String)],
        env: &'a BTreeMap<String, String>,
        rs: &'a HashMap<String, Resp>,
    ) -> Ctx<'a> {
        Ctx { vars, env, responses: rs, dir: Path::new(".") }
    }

    #[test]
    fn variables_env_and_precedence() {
        let vars = vec![("a".to_string(), "{{b}}-1".to_string()), ("b".to_string(), "B".to_string())];
        let env: BTreeMap<String, String> =
            [("b".to_string(), "ENV".to_string()), ("e".to_string(), "E".to_string())].into();
        let rs = HashMap::new();
        let c = ctx(&vars, &env, &rs);
        assert_eq!(subst("{{ a }}/{{e}}/{{b}}", &c).unwrap(), "B-1/E/B");
        assert!(subst("{{nope}}", &c).unwrap_err().contains("undefined variable"));
        assert_eq!(subst("no vars {{ unclosed", &c).unwrap(), "no vars {{ unclosed");
        let cyc = vec![("x".to_string(), "{{x}}".to_string())];
        assert!(subst("{{x}}", &ctx(&cyc, &env, &rs)).unwrap_err().contains("nest"));
    }

    #[test]
    fn system_variables() {
        let (v, e, r) = (vec![], BTreeMap::new(), HashMap::new());
        let c = ctx(&v, &e, &r);
        let g = subst("{{$guid}}", &c).unwrap();
        assert_eq!(g.len(), 36);
        assert_eq!(g.matches('-').count(), 4);
        assert_eq!(&g[14..15], "4");
        assert_ne!(g, subst("{{$uuid}}", &c).unwrap());
        assert!(subst("{{$timestamp}}", &c).unwrap().parse::<i64>().unwrap() > 1_700_000_000);
        let n: i64 = subst("{{$randomInt 5 8}}", &c).unwrap().parse().unwrap();
        assert!((5..8).contains(&n));
        assert!(subst("{{$datetime iso8601}}", &c).unwrap().ends_with('Z'));
        assert!(subst("{{$bogus}}", &c).is_err());
        assert_eq!(format_time(0, "iso8601"), "1970-01-01T00:00:00Z");
        assert_eq!(format_time(1_700_000_000, "rfc1123"), "Tue, 14 Nov 2023 22:13:20 GMT");
        assert_eq!(offset(&["-1", "d", "2", "h"]), -86400 + 7200);
    }

    #[test]
    fn response_chaining() {
        let mut rs = HashMap::new();
        rs.insert(
            "login".to_string(),
            Resp {
                status: 200,
                headers: vec![("X-Id".into(), "7".into())],
                body: br#"{"token":"t0k","user":{"ids":[10,20],"ok":true,"n":null},"a b":1}"#.to_vec(),
                ..Default::default()
            },
        );
        let (v, e) = (vec![], BTreeMap::new());
        let c = ctx(&v, &e, &rs);
        let g = |s: &str| subst(&format!("{{{{{s}}}}}"), &c);
        assert_eq!(g("login.response.body.$.token").unwrap(), "t0k");
        assert_eq!(g("login.response.body.$.user.ids[1]").unwrap(), "20");
        assert_eq!(g("login.response.body.$.user.ok").unwrap(), "true");
        assert_eq!(g("login.response.body.$.user.n").unwrap(), "null");
        assert_eq!(g("login.response.body.$['a b']").unwrap(), "1");
        assert_eq!(g("login.response.body.$.user.ids").unwrap(), "[10,20]");
        assert_eq!(g("login.response.headers.x-id").unwrap(), "7");
        assert!(g("login.response.body.*").unwrap().starts_with('{'));
        assert!(g("login.response.body.$.missing").unwrap_err().contains("not found"));
        assert!(g("other.response.body.$.a").unwrap_err().contains("send it first"));
    }

    #[test]
    fn env_files_merge_shared_and_private() {
        let dir = std::env::temp_dir().join(format!("lumen-http-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("http-client.env.json"),
            r#"{"$shared":{"v":"1","x":"shared"},"dev":{"host":"d","x":"dev","n":5},"prod":{"host":"p"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("http-client.private.env.json"), r#"{"dev":{"key":"secret"}}"#).unwrap();
        let envs = load_envs(&dir, &dir);
        assert_eq!(env_names(&envs), vec!["dev", "prod"]);
        let m = env_vars(&envs, Some("dev"));
        assert_eq!(m["host"], "d");
        assert_eq!(m["x"], "dev");
        assert_eq!(m["key"], "secret");
        assert_eq!(m["n"], "5");
        assert_eq!(m["v"], "1");
        assert_eq!(env_vars(&envs, None).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_resolves_everything() {
        let dir = std::env::temp_dir().join(format!("lumen-http-body-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.txt"), "hi {{who}}").unwrap();
        let f =
            parse("@who = ada\n\nPOST http://h/{{who}}\nAuthorization: Basic user pw\nX-A: {{who}}\n\n<@ ./b.txt\n");
        let (e, r) = (BTreeMap::new(), HashMap::new());
        let c = Ctx { vars: &f.vars, env: &e, responses: &r, dir: &dir };
        let p = prepare(&f.reqs[0], &c).unwrap();
        assert_eq!((p.method.as_str(), p.url.as_str()), ("POST", "http://h/ada"));
        assert_eq!(p.headers[0].1, "Basic dXNlcjpwdw==");
        assert_eq!(p.headers[1].1, "ada");
        assert_eq!(p.body, b"hi ada");
        let f2 = parse("POST http://h\n\n< ./b.txt\n");
        assert_eq!(prepare(&f2.reqs[0], &c).unwrap().body, b"hi {{who}}");
        let f3 = parse("POST http://h\n\n< ./missing.txt\n");
        assert!(prepare(&f3.reqs[0], &c).unwrap_err().contains("cannot read"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_pretty_printing() {
        assert_eq!(
            pretty_json(r#"{"b":[1,2,{}],"a":"x, \"y\": z","c":{"d":[]}}"#).unwrap(),
            "{\n  \"b\": [\n    1,\n    2,\n    {}\n  ],\n  \"a\": \"x, \\\"y\\\": z\",\n  \"c\": {\n    \"d\": []\n  }\n}"
        );
        assert_eq!(pretty_json("[ ]").unwrap(), "[]");
        assert_eq!(pretty_json("1.50").unwrap(), "1.50");
        assert!(pretty_json("{oops").is_none());
        assert!(pretty_json("<html>").is_none());
    }

    #[test]
    fn extensions_and_sizes() {
        assert_eq!(extension(Some("application/json; charset=utf-8"), ""), "json");
        assert_eq!(extension(Some("text/html"), ""), "html");
        assert_eq!(extension(None, "{\"a\":1}"), "json");
        assert_eq!(extension(Some("text/plain"), "{}"), "txt");
        assert_eq!(human_size(12), "12 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(3 << 20), "3.0 MB");
    }
}
