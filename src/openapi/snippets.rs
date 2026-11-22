//! Code snippets for a request in many languages and clients (cURL, HTTPie, wget, PowerShell,
//! JavaScript, Node.js, Python, Go, Rust, PHP, Ruby, Java, C#, Kotlin, Swift, Dart). Every
//! generator reads the same `Req`; the body is classified (JSON, URL-encoded form, multipart,
//! raw text, none) and written with each language's own quoting rules.

use super::tree::{Node, Val, num_text, parse, scalar_text, to_json};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Req {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// A language (group) and one client of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub group: &'static str,
    pub name: &'static str,
}

const fn t(group: &'static str, name: &'static str) -> Target {
    Target { group, name }
}

/// Every target; the index is what `render` takes. The targets of a group are contiguous.
pub const TARGETS: [Target; 22] = [
    t("Shell", "cURL"),
    t("Shell", "HTTPie"),
    t("Shell", "wget"),
    t("PowerShell", "Invoke-RestMethod"),
    t("JavaScript", "fetch"),
    t("JavaScript", "axios"),
    t("JavaScript", "XMLHttpRequest"),
    t("Node.js", "fetch"),
    t("Node.js", "axios"),
    t("Python", "requests"),
    t("Python", "httpx"),
    t("Python", "urllib"),
    t("Go", "net/http"),
    t("Rust", "reqwest"),
    t("PHP", "cURL"),
    t("PHP", "Guzzle"),
    t("Ruby", "Net::HTTP"),
    t("Java", "HttpClient"),
    t("C#", "HttpClient"),
    t("Kotlin", "OkHttp"),
    t("Swift", "URLSession"),
    t("Dart", "http"),
];

pub const CURL: usize = 0;
pub const HTTPIE: usize = 1;

/// The distinct groups, in display order.
pub fn groups() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for tg in TARGETS {
        if !out.contains(&tg.group) {
            out.push(tg.group);
        }
    }
    out
}

/// Indexes of the targets of `group`.
pub fn of_group(group: &str) -> Vec<usize> {
    TARGETS.iter().enumerate().filter(|(_, t)| t.group == group).map(|(i, _)| i).collect()
}

/// "Python · requests".
pub fn label(i: usize) -> String {
    let t = TARGETS.get(i).unwrap_or(&TARGETS[0]);
    format!("{} · {}", t.group, t.name)
}

/// The group of a target.
pub fn group_of(i: usize) -> &'static str {
    TARGETS.get(i).unwrap_or(&TARGETS[0]).group
}

pub fn render(i: usize, r: &Req) -> String {
    let p = Parsed::of(r);
    match i {
        0 => curl_with(r, &p),
        1 => httpie_with(r, &p),
        2 => wget(r, &p),
        3 => powershell(r, &p),
        4 => fetch_js(r, &p, false),
        5 => axios(r, &p, false),
        6 => xhr(r, &p),
        7 => fetch_js(r, &p, true),
        8 => axios(r, &p, true),
        9 => requests(r, &p),
        10 => httpx(r, &p),
        11 => urllib(r, &p),
        12 => go(r, &p),
        13 => reqwest(r, &p),
        14 => php_curl(r, &p),
        15 => guzzle(r, &p),
        16 => ruby(r, &p),
        17 => java(r, &p),
        18 => csharp(r, &p),
        19 => kotlin(r, &p),
        20 => swift(r, &p),
        _ => dart(r, &p),
    }
}

// ── request model ──

/// One field of a form / multipart body. `file` = the value is a path to upload.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub value: String,
    pub file: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    None,
    Json(Node),
    Form(Vec<Field>),
    Multipart(Vec<Field>),
    Raw(String),
}

/// The request as the generators see it: the Content-Type apart from the other headers.
#[derive(Clone, Debug)]
pub struct Parsed {
    /// Content-Type value ("" when none).
    pub ct: String,
    /// Headers without Content-Type.
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match h {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn field_of(name: String, value: String, multipart: bool) -> Field {
    let file = multipart && value.starts_with('@');
    Field { name, value: if file { value[1..].to_string() } else { value }, file }
}

/// Fields of a form body: a JSON object (how the generated example looks), `a=b&c=d`, or, for
/// multipart, one `name=value` per line (`name=@path` uploads a file).
pub fn form_fields(body: &str, multipart: bool) -> Vec<Field> {
    if let Ok(j) = parse(body)
        && j.is_map()
    {
        return j.entries().iter().map(|(k, v)| field_of(k.clone(), scalar_text(v), multipart)).collect();
    }
    let parts: Vec<&str> =
        if multipart && body.contains('\n') { body.lines().collect() } else { body.split('&').collect() };
    parts
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let (k, v) = s.split_once('=').unwrap_or((s, ""));
            if multipart {
                field_of(k.to_string(), v.to_string(), true)
            } else {
                field_of(pct_decode(k), pct_decode(v), false)
            }
        })
        .collect()
}

impl Parsed {
    pub fn of(r: &Req) -> Parsed {
        let ct = r
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_default();
        let headers: Vec<(String, String)> =
            r.headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("content-type")).cloned().collect();
        let lc = ct.to_ascii_lowercase();
        let body = match r.body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
            None => Body::None,
            Some(b) if lc.starts_with("multipart/") => Body::Multipart(form_fields(b, true)),
            Some(b) if lc.contains("x-www-form-urlencoded") => Body::Form(form_fields(b, false)),
            Some(b) => match parse(b) {
                Ok(j) if (lc.is_empty() || lc.contains("json")) && (j.is_map() || !j.arr().is_empty()) => Body::Json(j),
                _ => Body::Raw(b.to_string()),
            },
        };
        Parsed { ct, headers, body }
    }

    /// Headers with the Content-Type put back (for clients that need it spelled out). A
    /// multipart request leaves it out: the client writes it with the boundary.
    fn with_ct(&self) -> Vec<(String, String)> {
        let mut h = self.headers.clone();
        if !self.ct.is_empty() && !matches!(self.body, Body::Multipart(_)) {
            h.push(("Content-Type".into(), self.ct.clone()));
        }
        h
    }

    /// The text to send for a body that is not multipart.
    fn text(&self, r: &Req) -> Option<String> {
        match &self.body {
            Body::None | Body::Multipart(_) => None,
            Body::Json(j) => Some(to_json(j, false)),
            Body::Raw(b) => Some(b.clone()),
            Body::Form(_) => Some(r.body.clone().unwrap_or_default().trim().to_string()),
        }
    }
}

// ── quoting ──

/// Double-quoted string styles.
#[derive(Clone, Copy, PartialEq)]
enum Q {
    /// `\uXXXX` for control characters (JS, Python, Go, Java, C#).
    C,
    /// `\u{X}` (Rust, Swift).
    Brace,
    /// `\uXXXX` and `\$` (Kotlin, Dart).
    Dollar,
}

fn dq(s: &str, q: Q) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '$' if q == Q::Dollar => o.push_str("\\$"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => match q {
                Q::Brace => o.push_str(&format!("\\u{{{:x}}}", c as u32)),
                _ => o.push_str(&format!("\\u{:04x}", c as u32)),
            },
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// POSIX shell single quotes.
fn sh(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// PHP / Ruby single quotes.
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// PowerShell single quotes (typographic quotes count as quotes too).
fn ps(s: &str) -> String {
    let mut o = String::from("'");
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            o.push(c);
        }
        o.push(c);
    }
    o.push('\'');
    o
}

fn js_str(s: &str) -> String {
    dq(s, Q::C)
}

fn base_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

const BOUNDARY: &str = "----LumenFormBoundary7MA4YWxkTrZu0gW";

/// `GET` -> `Get`.
fn title_case(m: &str) -> String {
    let mut c = m.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

fn is_std(m: &str) -> bool {
    matches!(m, "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS")
}

/// `    "K": "v",` lines of a header map.
fn kv_lines(hs: &[(String, String)], pad: &str) -> String {
    hs.iter().map(|(k, v)| format!("{pad}{}: {},\n", js_str(k), js_str(v))).collect()
}

// ── cURL ──

fn curl_with(r: &Req, p: &Parsed) -> String {
    let sep = " \\\n  ";
    let mut out = String::from("curl");
    let has_body = !matches!(p.body, Body::None);
    if r.method == "HEAD" {
        out.push_str(" --head");
    } else if r.method != "GET" || has_body {
        out.push_str(&format!(" -X {}", r.method));
    }
    out.push(' ');
    out.push_str(&sh(&r.url));
    for (k, v) in p.with_ct() {
        out.push_str(&format!("{sep}-H {}", sh(&format!("{k}: {v}"))));
    }
    match &p.body {
        Body::None => {}
        Body::Multipart(f) => {
            for x in f {
                if x.file {
                    out.push_str(&format!("{sep}-F {}", sh(&format!("{}=@{}", x.name, x.value))));
                } else {
                    out.push_str(&format!("{sep}--form-string {}", sh(&format!("{}={}", x.name, x.value))));
                }
            }
        }
        _ => {
            if let Some(b) = p.text(r) {
                out.push_str(&format!("{sep}-d {}", sh(&b)));
            }
        }
    }
    out
}

// ── HTTPie ──

/// Escape a name for an HTTPie request item.
fn hp_name(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        if matches!(c, '\\' | '=' | ':' | '@' | ';' | '/') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

fn httpie_with(r: &Req, p: &Parsed) -> String {
    let sep = " \\\n  ";
    let mut head = String::new();
    let mut opts: Vec<String> = Vec::new();
    let mut items: Vec<String> = Vec::new();
    // Content-Type goes in a header only when HTTPie does not derive it from the body mode.
    let mut ct_header = false;
    match &p.body {
        Body::None => {
            if r.method != "GET" && r.method != "HEAD" {
                opts.push("--ignore-stdin".into());
            }
        }
        Body::Json(j) => {
            let flat = j.is_map() && !j.entries().is_empty() && j.entries().iter().all(|(k, _)| !k.is_empty());
            if flat {
                opts.push("--json".into());
                for (k, v) in j.entries() {
                    match &v.val {
                        Val::Str(s) => items.push(sh(&format!("{}={}", hp_name(k), s))),
                        _ => items.push(sh(&format!("{}:={}", hp_name(k), to_json(v, false)))),
                    }
                }
            } else {
                // Arrays and odd keys: the body goes through stdin.
                head = format!("printf %s {} | ", sh(&to_json(j, false)));
                ct_header = true;
            }
        }
        Body::Form(f) => {
            opts.push("--form".into());
            for x in f {
                items.push(sh(&format!("{}={}", hp_name(&x.name), x.value)));
            }
        }
        Body::Multipart(f) => {
            opts.push("--multipart".into());
            for x in f {
                let sepc = if x.file { '@' } else { '=' };
                items.push(sh(&format!("{}{sepc}{}", hp_name(&x.name), x.value)));
            }
        }
        Body::Raw(b) => {
            opts.push(format!("--raw {}", sh(b)));
            ct_header = true;
        }
    }
    let mut out = format!("{head}http");
    for o in &opts {
        out.push(' ');
        out.push_str(o);
    }
    out.push_str(&format!(" {} {}", r.method, sh(&r.url)));
    let mut hs = p.headers.clone();
    if ct_header && !p.ct.is_empty() {
        hs.push(("Content-Type".into(), p.ct.clone()));
    }
    for (k, v) in &hs {
        let it = if v.is_empty() { format!("{k};") } else { format!("{k}:{v}") };
        out.push_str(&format!("{sep}{}", sh(&it)));
    }
    for it in items {
        out.push_str(&format!("{sep}{it}"));
    }
    out
}

// ── wget ──

fn wget(r: &Req, p: &Parsed) -> String {
    if let Body::Multipart(_) = p.body {
        return format!("# wget cannot send multipart/form-data; the same request with cURL:\n{}", curl_with(r, p));
    }
    let mut out = format!("wget --quiet --method={} -O -", r.method);
    for (k, v) in p.with_ct() {
        out.push_str(&format!(" \\\n  --header={}", sh(&format!("{k}: {v}"))));
    }
    if let Some(b) = p.text(r) {
        out.push_str(&format!(" \\\n  --body-data={}", sh(&b)));
    }
    out.push_str(&format!(" \\\n  {}", sh(&r.url)));
    out
}

// ── PowerShell ──

fn powershell(r: &Req, p: &Parsed) -> String {
    let mut out = String::new();
    if !p.headers.is_empty() {
        out.push_str("$headers = @{\n");
        for (k, v) in &p.headers {
            out.push_str(&format!("    {} = {}\n", ps(k), ps(v)));
        }
        out.push_str("}\n\n");
    }
    out.push_str(&format!("$response = Invoke-RestMethod -Uri {} -Method {}", ps(&r.url), ps(&title_case(&r.method))));
    if !p.headers.is_empty() {
        out.push_str(" -Headers $headers");
    }
    match &p.body {
        Body::Multipart(f) => {
            out.push_str(" -Form @{\n");
            for x in f {
                if x.file {
                    out.push_str(&format!("    {} = Get-Item -Path {}\n", ps(&x.name), ps(&x.value)));
                } else {
                    out.push_str(&format!("    {} = {}\n", ps(&x.name), ps(&x.value)));
                }
            }
            out.push('}');
        }
        Body::None => {}
        _ => {
            if !p.ct.is_empty() {
                out.push_str(&format!(" -ContentType {}", ps(&p.ct)));
            }
            if let Some(b) = p.text(r) {
                out.push_str(&format!(" -Body {}", ps(&b)));
            }
        }
    }
    out.push_str("\n$response | ConvertTo-Json -Depth 10");
    out
}

// ── JavaScript / Node.js ──

fn js_pairs(f: &[Field]) -> String {
    let rows: Vec<String> = f.iter().map(|x| format!("[{}, {}]", js_str(&x.name), js_str(&x.value))).collect();
    format!("[{}]", rows.join(", "))
}

fn js_form_data(f: &[Field]) -> String {
    let mut o = String::from("const form = new FormData();\n");
    for x in f {
        if x.file {
            o.push_str(&format!(
                "form.append({}, new Blob([/* contents of {} */]), {});\n",
                js_str(&x.name),
                x.value.replace("*/", "* /"),
                js_str(&base_name(&x.value))
            ));
        } else {
            o.push_str(&format!("form.append({}, {});\n", js_str(&x.name), js_str(&x.value)));
        }
    }
    o.push('\n');
    o
}

fn js_headers(p: &Parsed) -> String {
    let hs = p.with_ct();
    if hs.is_empty() { String::new() } else { format!("  headers: {{\n{}  }},\n", kv_lines(&hs, "    ")) }
}

fn fetch_js(r: &Req, p: &Parsed, node: bool) -> String {
    let mut out = String::new();
    if let Body::Multipart(f) = &p.body {
        out.push_str(&js_form_data(f));
    }
    let call = if node { "const response = await fetch(" } else { "fetch(" };
    out.push_str(&format!("{call}{}, {{\n  method: {},\n", js_str(&r.url), js_str(&r.method)));
    out.push_str(&js_headers(p));
    match &p.body {
        Body::None => {}
        Body::Json(j) => out.push_str(&format!("  body: JSON.stringify({}),\n", to_json(j, false))),
        Body::Form(f) => out.push_str(&format!("  body: new URLSearchParams({}),\n", js_pairs(f))),
        Body::Multipart(_) => out.push_str("  body: form,\n"),
        Body::Raw(b) => out.push_str(&format!("  body: {},\n", js_str(b))),
    }
    if node {
        out.push_str("});\nconsole.log(response.status);\nconsole.log(await response.text());");
    } else {
        out.push_str("})\n  .then((res) => res.text())\n  .then(console.log);");
    }
    out
}

fn axios(r: &Req, p: &Parsed, node: bool) -> String {
    let mut out = String::new();
    if node {
        out.push_str("const axios = require(\"axios\");\n\n");
    } else {
        out.push_str("import axios from \"axios\";\n\n");
    }
    if let Body::Multipart(f) = &p.body {
        out.push_str(&js_form_data(f));
    }
    out.push_str(&format!(
        "const response = await axios.request({{\n  method: {},\n  url: {},\n",
        js_str(&r.method.to_lowercase()),
        js_str(&r.url)
    ));
    out.push_str(&js_headers(p));
    match &p.body {
        Body::None => {}
        Body::Json(j) => out.push_str(&format!("  data: {},\n", to_json(j, false))),
        Body::Form(f) => out.push_str(&format!("  data: new URLSearchParams({}),\n", js_pairs(f))),
        Body::Multipart(_) => out.push_str("  data: form,\n"),
        Body::Raw(b) => out.push_str(&format!("  data: {},\n", js_str(b))),
    }
    out.push_str("  validateStatus: () => true,\n});\nconsole.log(response.status);\nconsole.log(response.data);");
    out
}

fn xhr(r: &Req, p: &Parsed) -> String {
    let mut out = String::new();
    if let Body::Multipart(f) = &p.body {
        out.push_str(&js_form_data(f));
    }
    out.push_str(&format!("const xhr = new XMLHttpRequest();\nxhr.open({}, {});\n", js_str(&r.method), js_str(&r.url)));
    for (k, v) in p.with_ct() {
        out.push_str(&format!("xhr.setRequestHeader({}, {});\n", js_str(&k), js_str(&v)));
    }
    out.push_str("xhr.onload = () => console.log(xhr.status, xhr.responseText);\n");
    let arg = match &p.body {
        Body::None => String::new(),
        Body::Json(j) => format!("JSON.stringify({})", to_json(j, false)),
        Body::Form(f) => format!("new URLSearchParams({})", js_pairs(f)),
        Body::Multipart(_) => "form".into(),
        Body::Raw(b) => js_str(b),
    };
    out.push_str(&format!("xhr.send({arg});"));
    out
}

// ── Python ──

fn py_lit(n: &Node, ind: usize, out: &mut String) {
    let pad = "    ".repeat(ind + 1);
    let end = "    ".repeat(ind);
    match &n.val {
        Val::Null => out.push_str("None"),
        Val::Bool(true) => out.push_str("True"),
        Val::Bool(false) => out.push_str("False"),
        Val::Num(x) => out.push_str(&num_text(*x)),
        Val::Str(s) => out.push_str(&js_str(s)),
        Val::Arr(a) if a.is_empty() => out.push_str("[]"),
        Val::Map(m) if m.is_empty() => out.push_str("{}"),
        Val::Arr(a) => {
            out.push_str("[\n");
            for v in a {
                out.push_str(&pad);
                py_lit(v, ind + 1, out);
                out.push_str(",\n");
            }
            out.push_str(&end);
            out.push(']');
        }
        Val::Map(m) => {
            out.push_str("{\n");
            for (k, v) in m {
                out.push_str(&pad);
                out.push_str(&js_str(k));
                out.push_str(": ");
                py_lit(v, ind + 1, out);
                out.push_str(",\n");
            }
            out.push_str(&end);
            out.push('}');
        }
    }
}

/// A form as a dict; repeated names become lists.
fn py_dict(f: &[Field]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for x in f {
        if !names.contains(&x.name.as_str()) {
            names.push(&x.name);
        }
    }
    let mut o = String::from("{\n");
    for n in names {
        let vals: Vec<String> = f.iter().filter(|x| x.name == n).map(|x| js_str(&x.value)).collect();
        if vals.len() == 1 {
            o.push_str(&format!("        {}: {},\n", js_str(n), vals[0]));
        } else {
            o.push_str(&format!("        {}: [{}],\n", js_str(n), vals.join(", ")));
        }
    }
    o.push_str("    }");
    o
}

fn py_files(f: &[Field]) -> String {
    let mut o = String::from("[\n");
    for x in f {
        if x.file {
            o.push_str(&format!(
                "        ({}, ({}, open({}, \"rb\"))),\n",
                js_str(&x.name),
                js_str(&base_name(&x.value)),
                js_str(&x.value)
            ));
        } else {
            o.push_str(&format!("        ({}, (None, {})),\n", js_str(&x.name), js_str(&x.value)));
        }
    }
    o.push_str("    ]");
    o
}

fn py_common(lib: &str, r: &Req, p: &Parsed, raw_kw: &str) -> String {
    let mut out =
        format!("import {lib}\n\nresponse = {lib}.request(\n    {},\n    {},\n", js_str(&r.method), js_str(&r.url));
    // `json=` writes the Content-Type itself unless the user set one.
    let hs = p.with_ct();
    if !hs.is_empty() {
        out.push_str(&format!("    headers={{\n{}    }},\n", kv_lines(&hs, "        ")));
    }
    match &p.body {
        Body::None => {}
        Body::Json(j) => {
            out.push_str("    json=");
            py_lit(j, 1, &mut out);
            out.push_str(",\n");
        }
        Body::Form(f) => out.push_str(&format!("    data={},\n", py_dict(f))),
        Body::Multipart(f) => out.push_str(&format!("    files={},\n", py_files(f))),
        Body::Raw(b) => out.push_str(&format!("    {raw_kw}={},\n", js_str(b))),
    }
    out.push_str(")\nprint(response.status_code)\nprint(response.text)");
    out
}

fn requests(r: &Req, p: &Parsed) -> String {
    py_common("requests", r, p, "data")
}

fn httpx(r: &Req, p: &Parsed) -> String {
    py_common("httpx", r, p, "content")
}

fn urllib(r: &Req, p: &Parsed) -> String {
    if let Body::Multipart(_) = p.body {
        return format!("# urllib has no multipart helper; the same request with requests:\n{}", requests(r, p));
    }
    let data = match p.text(r) {
        None => "None".to_string(),
        Some(b) => format!("{}.encode(\"utf-8\")", js_str(&b)),
    };
    let mut out = format!(
        "import urllib.request\n\nreq = urllib.request.Request(\n    {},\n    data={data},\n    method={},\n",
        js_str(&r.url),
        js_str(&r.method)
    );
    let hs = p.with_ct();
    if !hs.is_empty() {
        out.push_str(&format!("    headers={{\n{}    }},\n", kv_lines(&hs, "        ")));
    }
    out.push_str(
        ")\nwith urllib.request.urlopen(req) as res:\n    print(res.status)\n    print(res.read().decode(\"utf-8\"))",
    );
    out
}

// ── Go ──

fn go(r: &Req, p: &Parsed) -> String {
    let mut imports = vec!["\"fmt\"", "\"io\"", "\"net/http\""];
    let mut pre = String::new();
    let mut ct_line = String::new();
    let body_arg = match &p.body {
        Body::Multipart(f) => {
            imports.extend(["\"bytes\"", "\"mime/multipart\""]);
            pre.push_str("\tvar buf bytes.Buffer\n\tw := multipart.NewWriter(&buf)\n");
            for x in f {
                if x.file {
                    if !imports.contains(&"\"os\"") {
                        imports.push("\"os\"");
                    }
                    pre.push_str(&format!(
                        "\t{{\n\t\tfw, _ := w.CreateFormFile({}, {})\n\t\tf, err := os.Open({})\n\t\tif err != nil {{\n\t\t\tpanic(err)\n\t\t}}\n\t\tio.Copy(fw, f)\n\t\tf.Close()\n\t}}\n",
                        js_str(&x.name),
                        js_str(&base_name(&x.value)),
                        js_str(&x.value)
                    ));
                } else {
                    pre.push_str(&format!("\tw.WriteField({}, {})\n", js_str(&x.name), js_str(&x.value)));
                }
            }
            pre.push_str("\tw.Close()\n\n");
            ct_line = "\treq.Header.Set(\"Content-Type\", w.FormDataContentType())\n".into();
            "&buf".to_string()
        }
        _ => match p.text(r) {
            Some(b) => {
                imports.push("\"strings\"");
                format!("strings.NewReader({})", js_str(&b))
            }
            None => "nil".into(),
        },
    };
    imports.sort();
    let mut out = String::from("package main\n\nimport (\n");
    for i in imports {
        out.push_str(&format!("\t{i}\n"));
    }
    out.push_str(")\n\nfunc main() {\n");
    out.push_str(&pre);
    out.push_str(&format!(
        "\treq, err := http.NewRequest({}, {}, {body_arg})\n\tif err != nil {{\n\t\tpanic(err)\n\t}}\n",
        js_str(&r.method),
        js_str(&r.url)
    ));
    for (k, v) in p.with_ct() {
        out.push_str(&format!("\treq.Header.Add({}, {})\n", js_str(&k), js_str(&v)));
    }
    out.push_str(&ct_line);
    out.push_str(
        "\n\tres, err := http.DefaultClient.Do(req)\n\tif err != nil {\n\t\tpanic(err)\n\t}\n\tdefer res.Body.Close()\n\n\tout, _ := io.ReadAll(res.Body)\n\tfmt.Println(res.Status)\n\tfmt.Println(string(out))\n}",
    );
    out
}

// ── Rust ──

fn rs_method(m: &str) -> String {
    if is_std(m) {
        format!("reqwest::Method::{m}")
    } else {
        format!("reqwest::Method::from_bytes({}.as_bytes())?", dq(m, Q::Brace))
    }
}

fn reqwest(r: &Req, p: &Parsed) -> String {
    let mut out = String::from("#[tokio::main]\nasync fn main() -> Result<(), Box<dyn std::error::Error>> {\n");
    if let Body::Multipart(f) = &p.body {
        out.push_str("    let form = reqwest::multipart::Form::new()");
        for x in f {
            if x.file {
                out.push_str(&format!("\n        .file({}, {}).await?", dq(&x.name, Q::Brace), dq(&x.value, Q::Brace)));
            } else {
                out.push_str(&format!("\n        .text({}, {})", dq(&x.name, Q::Brace), dq(&x.value, Q::Brace)));
            }
        }
        out.push_str(";\n");
    }
    out.push_str(&format!(
        "    let client = reqwest::Client::new();\n    let res = client\n        .request({}, {})\n",
        rs_method(&r.method),
        dq(&r.url, Q::Brace)
    ));
    for (k, v) in p.with_ct() {
        out.push_str(&format!("        .header({}, {})\n", dq(&k, Q::Brace), dq(&v, Q::Brace)));
    }
    match &p.body {
        Body::None => {}
        Body::Multipart(_) => out.push_str("        .multipart(form)\n"),
        _ => {
            if let Some(b) = p.text(r) {
                out.push_str(&format!("        .body({})\n", dq(&b, Q::Brace)));
            }
        }
    }
    out.push_str(
        "        .send()\n        .await?;\n    println!(\"{}\", res.status());\n    println!(\"{}\", res.text().await?);\n    Ok(())\n}",
    );
    out
}

// ── PHP ──

fn php_curl(r: &Req, p: &Parsed) -> String {
    let mut out = String::from("<?php\n$ch = curl_init();\ncurl_setopt_array($ch, [\n");
    out.push_str(&format!("    CURLOPT_URL => {},\n    CURLOPT_RETURNTRANSFER => true,\n", sq(&r.url)));
    if r.method == "HEAD" {
        out.push_str("    CURLOPT_NOBODY => true,\n");
    } else {
        out.push_str(&format!("    CURLOPT_CUSTOMREQUEST => {},\n", sq(&r.method)));
    }
    let hs = p.with_ct();
    if !hs.is_empty() {
        out.push_str("    CURLOPT_HTTPHEADER => [\n");
        for (k, v) in &hs {
            out.push_str(&format!("        {},\n", sq(&format!("{k}: {v}"))));
        }
        out.push_str("    ],\n");
    }
    match &p.body {
        Body::None => {}
        Body::Multipart(f) => {
            out.push_str("    CURLOPT_POSTFIELDS => [\n");
            for x in f {
                if x.file {
                    out.push_str(&format!("        {} => new CURLFile({}),\n", sq(&x.name), sq(&x.value)));
                } else {
                    out.push_str(&format!("        {} => {},\n", sq(&x.name), sq(&x.value)));
                }
            }
            out.push_str("    ],\n");
        }
        _ => {
            if let Some(b) = p.text(r) {
                out.push_str(&format!("    CURLOPT_POSTFIELDS => {},\n", sq(&b)));
            }
        }
    }
    out.push_str("]);\n$response = curl_exec($ch);\necho curl_getinfo($ch, CURLINFO_HTTP_CODE), \"\\n\", $response;\ncurl_close($ch);");
    out
}

fn guzzle(r: &Req, p: &Parsed) -> String {
    let mut out = String::from("<?php\nrequire 'vendor/autoload.php';\n\n$client = new GuzzleHttp\\Client();\n");
    out.push_str(&format!(
        "$response = $client->request({}, {}, [\n    'http_errors' => false,\n",
        sq(&r.method),
        sq(&r.url)
    ));
    let hs = p.with_ct();
    if !hs.is_empty() {
        out.push_str("    'headers' => [\n");
        for (k, v) in &hs {
            out.push_str(&format!("        {} => {},\n", sq(k), sq(v)));
        }
        out.push_str("    ],\n");
    }
    match &p.body {
        Body::None => {}
        Body::Multipart(f) => {
            out.push_str("    'multipart' => [\n");
            for x in f {
                if x.file {
                    out.push_str(&format!(
                        "        ['name' => {}, 'contents' => fopen({}, 'r'), 'filename' => {}],\n",
                        sq(&x.name),
                        sq(&x.value),
                        sq(&base_name(&x.value))
                    ));
                } else {
                    out.push_str(&format!("        ['name' => {}, 'contents' => {}],\n", sq(&x.name), sq(&x.value)));
                }
            }
            out.push_str("    ],\n");
        }
        _ => {
            if let Some(b) = p.text(r) {
                out.push_str(&format!("    'body' => {},\n", sq(&b)));
            }
        }
    }
    out.push_str("]);\necho $response->getStatusCode(), \"\\n\", $response->getBody();");
    out
}

// ── Ruby ──

fn ruby(r: &Req, p: &Parsed) -> String {
    let mut out = format!(
        "require 'net/http'\nrequire 'uri'\n\nuri = URI({})\nhttp = Net::HTTP.new(uri.host, uri.port)\nhttp.use_ssl = uri.scheme == 'https'\n\n",
        sq(&r.url)
    );
    if is_std(&r.method) {
        out.push_str(&format!("request = Net::HTTP::{}.new(uri.request_uri)\n", title_case(&r.method)));
    } else {
        out.push_str(&format!(
            "request = Net::HTTPGenericRequest.new({}, true, true, uri.request_uri)\n",
            sq(&r.method)
        ));
    }
    for (k, v) in &p.headers {
        out.push_str(&format!("request[{}] = {}\n", sq(k), sq(v)));
    }
    match &p.body {
        Body::None => {}
        Body::Multipart(f) => {
            let rows: Vec<String> = f
                .iter()
                .map(|x| {
                    if x.file {
                        format!("[{}, File.open({})]", sq(&x.name), sq(&x.value))
                    } else {
                        format!("[{}, {}]", sq(&x.name), sq(&x.value))
                    }
                })
                .collect();
            out.push_str(&format!("request.set_form([{}], 'multipart/form-data')\n", rows.join(", ")));
        }
        _ => {
            if !p.ct.is_empty() {
                out.push_str(&format!("request['Content-Type'] = {}\n", sq(&p.ct)));
            }
            if let Some(b) = p.text(r) {
                out.push_str(&format!("request.body = {}\n", sq(&b)));
            }
        }
    }
    out.push_str("\nresponse = http.request(request)\nputs response.code\nputs response.body");
    out
}

// ── Java ──

fn disposition(name: &str, file: Option<&str>) -> String {
    let name = name.replace('"', "%22").replace(['\r', '\n'], " ");
    match file {
        Some(f) => format!(
            "Content-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\nContent-Type: application/octet-stream\r\n\r\n",
            f.replace('"', "%22")
        ),
        None => format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n"),
    }
}

fn java(r: &Req, p: &Parsed) -> String {
    let mut imports =
        vec!["java.net.URI", "java.net.http.HttpClient", "java.net.http.HttpRequest", "java.net.http.HttpResponse"];
    let mut pre = String::new();
    let mut hs = p.with_ct();
    let publisher = match &p.body {
        Body::Multipart(f) => {
            hs.push(("Content-Type".into(), format!("multipart/form-data; boundary={BOUNDARY}")));
            pre.push_str(&format!(
                "        String boundary = {};\n        StringBuilder body = new StringBuilder();\n",
                dq(BOUNDARY, Q::C)
            ));
            for x in f {
                pre.push_str("        body.append(\"--\").append(boundary).append(\"\\r\\n\")\n");
                if x.file {
                    if !imports.contains(&"java.nio.file.Files") {
                        imports.push("java.nio.file.Files");
                        imports.push("java.nio.file.Path");
                    }
                    pre.push_str(&format!(
                        "            .append({})\n            .append(Files.readString(Path.of({}))).append(\"\\r\\n\");\n",
                        dq(&disposition(&x.name, Some(&base_name(&x.value))), Q::C),
                        dq(&x.value, Q::C)
                    ));
                } else {
                    pre.push_str(&format!(
                        "            .append({})\n            .append({}).append(\"\\r\\n\");\n",
                        dq(&disposition(&x.name, None), Q::C),
                        dq(&x.value, Q::C)
                    ));
                }
            }
            pre.push_str("        body.append(\"--\").append(boundary).append(\"--\\r\\n\");\n\n");
            "HttpRequest.BodyPublishers.ofString(body.toString())".to_string()
        }
        _ => match p.text(r) {
            Some(b) => format!("HttpRequest.BodyPublishers.ofString({})", dq(&b, Q::C)),
            None => "HttpRequest.BodyPublishers.noBody()".into(),
        },
    };
    imports.sort();
    let mut out = String::new();
    for i in imports {
        out.push_str(&format!("import {i};\n"));
    }
    out.push_str("\npublic class Main {\n    public static void main(String[] args) throws Exception {\n");
    out.push_str(&pre);
    out.push_str(&format!(
        "        HttpClient client = HttpClient.newHttpClient();\n        HttpRequest request = HttpRequest.newBuilder()\n            .uri(URI.create({}))\n",
        dq(&r.url, Q::C)
    ));
    for (k, v) in &hs {
        out.push_str(&format!("            .header({}, {})\n", dq(k, Q::C), dq(v, Q::C)));
    }
    out.push_str(&format!(
        "            .method({}, {publisher})\n            .build();\n\n        HttpResponse<String> response = client.send(request, HttpResponse.BodyHandlers.ofString());\n        System.out.println(response.statusCode());\n        System.out.println(response.body());\n    }}\n}}",
        dq(&r.method, Q::C)
    ));
    out
}

// ── C# ──

fn csharp(r: &Req, p: &Parsed) -> String {
    let mut out = String::from(
        "using System;\nusing System.Collections.Generic;\nusing System.IO;\nusing System.Net.Http;\nusing System.Text;\n\n",
    );
    let method = if is_std(&r.method) {
        format!("HttpMethod.{}", title_case(&r.method))
    } else {
        format!("new HttpMethod({})", dq(&r.method, Q::C))
    };
    out.push_str(&format!(
        "using var client = new HttpClient();\nvar request = new HttpRequestMessage({method}, {});\n",
        dq(&r.url, Q::C)
    ));
    for (k, v) in &p.headers {
        out.push_str(&format!("request.Headers.TryAddWithoutValidation({}, {});\n", dq(k, Q::C), dq(v, Q::C)));
    }
    let mime = p.ct.split(';').next().unwrap_or("").trim().to_string();
    match &p.body {
        Body::None => {}
        Body::Form(f) => {
            let rows: Vec<String> = f
                .iter()
                .map(|x| format!("new KeyValuePair<string, string>({}, {})", dq(&x.name, Q::C), dq(&x.value, Q::C)))
                .collect();
            out.push_str(&format!("request.Content = new FormUrlEncodedContent(new[] {{ {} }});\n", rows.join(", ")));
        }
        Body::Multipart(f) => {
            out.push_str("var content = new MultipartFormDataContent();\n");
            for x in f {
                if x.file {
                    out.push_str(&format!(
                        "content.Add(new ByteArrayContent(File.ReadAllBytes({})), {}, {});\n",
                        dq(&x.value, Q::C),
                        dq(&x.name, Q::C),
                        dq(&base_name(&x.value), Q::C)
                    ));
                } else {
                    out.push_str(&format!(
                        "content.Add(new StringContent({}), {});\n",
                        dq(&x.value, Q::C),
                        dq(&x.name, Q::C)
                    ));
                }
            }
            out.push_str("request.Content = content;\n");
        }
        _ => {
            if let Some(b) = p.text(r) {
                if mime.is_empty() {
                    out.push_str(&format!("request.Content = new StringContent({}, Encoding.UTF8);\n", dq(&b, Q::C)));
                } else {
                    out.push_str(&format!(
                        "request.Content = new StringContent({}, Encoding.UTF8, {});\n",
                        dq(&b, Q::C),
                        dq(&mime, Q::C)
                    ));
                }
            }
        }
    }
    out.push_str("using var response = await client.SendAsync(request);\nConsole.WriteLine((int)response.StatusCode);\nConsole.WriteLine(await response.Content.ReadAsStringAsync());");
    out
}

// ── Kotlin ──

fn kotlin(r: &Req, p: &Parsed) -> String {
    let mut imports = vec!["okhttp3.OkHttpClient", "okhttp3.Request"];
    let k = |s: &str| dq(s, Q::Dollar);
    let body = match &p.body {
        Body::None => {
            if matches!(r.method.as_str(), "GET" | "HEAD") {
                "null".to_string()
            } else {
                imports.push("okhttp3.RequestBody.Companion.toRequestBody");
                "ByteArray(0).toRequestBody()".to_string()
            }
        }
        Body::Form(f) => {
            imports.push("okhttp3.FormBody");
            let mut s = String::from("FormBody.Builder()");
            for x in f {
                s.push_str(&format!("\n            .add({}, {})", k(&x.name), k(&x.value)));
            }
            s.push_str("\n            .build()");
            s
        }
        Body::Multipart(f) => {
            imports.push("okhttp3.MultipartBody");
            let mut s = String::from("MultipartBody.Builder()\n            .setType(MultipartBody.FORM)");
            for x in f {
                if x.file {
                    imports.push("okhttp3.RequestBody.Companion.asRequestBody");
                    imports.push("java.io.File");
                    s.push_str(&format!(
                        "\n            .addFormDataPart({}, {}, File({}).asRequestBody())",
                        k(&x.name),
                        k(&base_name(&x.value)),
                        k(&x.value)
                    ));
                } else {
                    s.push_str(&format!("\n            .addFormDataPart({}, {})", k(&x.name), k(&x.value)));
                }
            }
            s.push_str("\n            .build()");
            s
        }
        _ => {
            imports.push("okhttp3.RequestBody.Companion.toRequestBody");
            let b = k(&p.text(r).unwrap_or_default());
            if p.ct.is_empty() {
                format!("{b}.toRequestBody()")
            } else {
                imports.push("okhttp3.MediaType.Companion.toMediaType");
                format!("{b}.toRequestBody({}.toMediaType())", k(&p.ct))
            }
        }
    };
    imports.sort();
    imports.dedup();
    let mut out = String::new();
    for i in imports {
        out.push_str(&format!("import {i}\n"));
    }
    out.push_str(&format!(
        "\nfun main() {{\n    val client = OkHttpClient()\n    val request = Request.Builder()\n        .url({})\n",
        k(&r.url)
    ));
    for (hk, hv) in &p.headers {
        out.push_str(&format!("        .addHeader({}, {})\n", k(hk), k(hv)));
    }
    out.push_str(&format!(
        "        .method({}, {body})\n        .build()\n\n    client.newCall(request).execute().use {{ response ->\n        println(response.code)\n        println(response.body?.string())\n    }}\n}}",
        k(&r.method)
    ));
    out
}

// ── Swift ──

fn swift(r: &Req, p: &Parsed) -> String {
    let s = |x: &str| dq(x, Q::Brace);
    let mut out = format!(
        "import Foundation\n\nvar request = URLRequest(url: URL(string: {})!)\nrequest.httpMethod = {}\n",
        s(&r.url),
        s(&r.method)
    );
    let mut hs = p.with_ct();
    if let Body::Multipart(f) = &p.body {
        hs.push(("Content-Type".into(), format!("multipart/form-data; boundary={BOUNDARY}")));
        for (k, v) in &hs {
            out.push_str(&format!("request.setValue({}, forHTTPHeaderField: {})\n", s(v), s(k)));
        }
        out.push_str(&format!("\nlet boundary = {}\nvar body = Data()\n", s(BOUNDARY)));
        for x in f {
            out.push_str("body.append(\"--\\(boundary)\\r\\n\".data(using: .utf8)!)\n");
            if x.file {
                out.push_str(&format!(
                    "body.append({}.data(using: .utf8)!)\nbody.append(try Data(contentsOf: URL(fileURLWithPath: {})))\nbody.append(\"\\r\\n\".data(using: .utf8)!)\n",
                    s(&disposition(&x.name, Some(&base_name(&x.value)))),
                    s(&x.value)
                ));
            } else {
                let part = format!("{}{}\r\n", disposition(&x.name, None), x.value);
                out.push_str(&format!("body.append({}.data(using: .utf8)!)\n", s(&part)));
            }
        }
        out.push_str("body.append(\"--\\(boundary)--\\r\\n\".data(using: .utf8)!)\nrequest.httpBody = body\n");
    } else {
        for (k, v) in &hs {
            out.push_str(&format!("request.setValue({}, forHTTPHeaderField: {})\n", s(v), s(k)));
        }
        if let Some(b) = p.text(r) {
            out.push_str(&format!("request.httpBody = {}.data(using: .utf8)\n", s(&b)));
        }
    }
    out.push_str("\nlet (data, response) = try await URLSession.shared.data(for: request)\nprint((response as! HTTPURLResponse).statusCode)\nprint(String(decoding: data, as: UTF8.self))");
    out
}

// ── Dart ──

fn dart(r: &Req, p: &Parsed) -> String {
    let d = |x: &str| dq(x, Q::Dollar);
    let mut out = String::from("import 'package:http/http.dart' as http;\n\nFuture<void> main() async {\n");
    let uri = format!("Uri.parse({})", d(&r.url));
    if let Body::Multipart(f) = &p.body {
        out.push_str(&format!("  final request = http.MultipartRequest({}, {uri});\n", d(&r.method)));
        for (k, v) in &p.headers {
            out.push_str(&format!("  request.headers[{}] = {};\n", d(k), d(v)));
        }
        for x in f {
            if x.file {
                out.push_str(&format!(
                    "  request.files.add(await http.MultipartFile.fromPath({}, {}));\n",
                    d(&x.name),
                    d(&x.value)
                ));
            } else {
                out.push_str(&format!("  request.fields[{}] = {};\n", d(&x.name), d(&x.value)));
            }
        }
    } else {
        out.push_str(&format!("  final request = http.Request({}, {uri});\n", d(&r.method)));
        for (k, v) in p.with_ct() {
            out.push_str(&format!("  request.headers[{}] = {};\n", d(&k), d(&v)));
        }
        if let Some(b) = p.text(r) {
            out.push_str(&format!("  request.body = {};\n", d(&b)));
        }
    }
    out.push_str("\n  final streamed = await request.send();\n  final response = await http.Response.fromStream(streamed);\n  print(response.statusCode);\n  print(response.body);\n}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> Req {
        Req {
            method: "POST".into(),
            url: "https://api.example.com/pets?limit=2".into(),
            headers: vec![("Content-Type".into(), "application/json".into()), ("X-Note".into(), "it's".into())],
            body: Some("{\"name\":\"Rex\",\"ok\":true,\"n\":null}".into()),
        }
    }

    fn with(ct: &str, body: &str) -> Req {
        Req {
            method: "POST".into(),
            url: "http://h/p?a=1&a=2".into(),
            headers: vec![("Content-Type".into(), ct.into())],
            body: Some(body.into()),
        }
    }

    fn all(r: &Req) -> Vec<String> {
        (0..TARGETS.len()).map(|i| render(i, r)).collect()
    }

    #[test]
    fn targets_are_grouped_and_labelled() {
        assert_eq!(groups().len(), 14);
        assert_eq!(of_group("Python"), vec![9, 10, 11]);
        assert_eq!(label(CURL), "Shell · cURL");
        assert_eq!(label(HTTPIE), "Shell · HTTPie");
        assert_eq!(group_of(HTTPIE), "Shell");
        // Contiguous groups.
        let mut seen: Vec<&str> = Vec::new();
        for t in TARGETS {
            if seen.last() != Some(&t.group) {
                assert!(!seen.contains(&t.group), "{} is split", t.group);
                seen.push(t.group);
            }
        }
    }

    #[test]
    fn every_target_renders_every_body_kind() {
        let reqs = [
            req(),
            with("multipart/form-data", "title=Hi\nfile=@/tmp/a b.png"),
            with("application/x-www-form-urlencoded", "a=1&a=2&b=x%20y"),
            with("text/plain", "plain text"),
            Req { method: "GET".into(), url: "http://h/".into(), ..Default::default() },
            Req { method: "DELETE".into(), url: "http://h/".into(), body: Some("".into()), ..Default::default() },
        ];
        for r in &reqs {
            for (i, s) in all(r).iter().enumerate() {
                assert!(!s.is_empty() && s.contains("http"), "{} for {:?}", label(i), r.body);
            }
        }
    }

    #[test]
    fn curl_quotes_for_the_shell() {
        let c = render(CURL, &req());
        assert!(c.starts_with("curl -X POST 'https://api.example.com/pets?limit=2' \\\n"));
        assert!(c.contains("-H 'X-Note: it'\\''s'"));
        assert!(c.contains("-H 'Content-Type: application/json'"));
        assert!(c.ends_with("-d '{\"name\":\"Rex\",\"ok\":true,\"n\":null}'"));
        let get = Req { method: "GET".into(), url: "http://h/".into(), ..Default::default() };
        assert_eq!(render(CURL, &get), "curl 'http://h/'");
        let empty =
            Req { method: "POST".into(), url: "http://h/".into(), body: Some("  ".into()), ..Default::default() };
        assert_eq!(render(CURL, &empty), "curl -X POST 'http://h/'");
        let head = Req { method: "HEAD".into(), url: "http://h/".into(), ..Default::default() };
        assert_eq!(render(CURL, &head), "curl --head 'http://h/'");
    }

    #[test]
    fn curl_unicode_repeated_query_and_multipart() {
        let r = with("application/json", "{\"nome\":\"João ☃ \\\"q\\\"\"}");
        let c = render(CURL, &r);
        assert!(c.contains("'http://h/p?a=1&a=2'"));
        assert!(c.contains("João ☃ \\\"q\\\""));
        let m = render(CURL, &with("multipart/form-data", "title=it's\nfile=@/tmp/a.png"));
        assert!(m.contains("--form-string 'title=it'\\''s'"));
        assert!(m.contains("-F 'file=@/tmp/a.png'"));
        assert!(!m.contains("Content-Type"), "the boundary is curl's job");
        let f = render(CURL, &with("application/x-www-form-urlencoded", "a=1&a=2"));
        assert!(f.contains("-H 'Content-Type: application/x-www-form-urlencoded'") && f.ends_with("-d 'a=1&a=2'"));
    }

    #[test]
    fn httpie_picks_json_form_raw_or_stdin() {
        let h = render(HTTPIE, &req());
        assert!(h.starts_with("http --json POST 'https://api.example.com/pets?limit=2' \\\n"), "{h}");
        assert!(h.contains("'X-Note:it'\\''s'"));
        assert!(h.contains("'name=Rex'") && h.contains("'ok:=true'") && h.contains("'n:=null'"));
        assert!(!h.contains("Content-Type"), "--json implies it");
        let arr = render(HTTPIE, &with("application/json", "[1,2]"));
        assert!(arr.starts_with("printf %s '[1,2]' | http POST 'http://h/p?a=1&a=2'"), "{arr}");
        assert!(arr.contains("'Content-Type:application/json'"));
        let raw = render(HTTPIE, &with("text/plain", "it's raw"));
        assert!(raw.starts_with("http --raw 'it'\\''s raw' POST"), "{raw}");
        let form = render(HTTPIE, &with("application/x-www-form-urlencoded", "a=1&b=x%20y&c=%C3%A9"));
        assert!(form.starts_with("http --form POST") && form.contains("'b=x y'") && form.contains("'c=é'"), "{form}");
        let mp = render(HTTPIE, &with("multipart/form-data", "n=v\nfile=@/tmp/x.png"));
        assert!(
            mp.starts_with("http --multipart POST") && mp.contains("'n=v'") && mp.contains("'file@/tmp/x.png'"),
            "{mp}"
        );
        let nobody = Req { method: "DELETE".into(), url: "http://h/x".into(), ..Default::default() };
        assert_eq!(render(HTTPIE, &nobody), "http --ignore-stdin DELETE 'http://h/x'");
        let get = Req {
            method: "GET".into(),
            url: "http://h/x".into(),
            headers: vec![("X-Empty".into(), "".into())],
            body: None,
        };
        assert_eq!(render(HTTPIE, &get), "http GET 'http://h/x' \\\n  'X-Empty;'");
        let odd = render(HTTPIE, &with("application/json", "{\"a:b\":1,\"c=d\":\"é\"}"));
        assert!(odd.contains("'a\\:b:=1'") && odd.contains("'c\\=d=é'"), "{odd}");
    }

    #[test]
    fn wget_and_powershell() {
        let w = render(2, &req());
        assert!(w.starts_with("wget --quiet --method=POST -O -"));
        assert!(w.contains("--header='X-Note: it'\\''s'") && w.contains("--body-data='{\"name\""));
        assert!(w.ends_with("'https://api.example.com/pets?limit=2'"));
        assert!(render(2, &with("multipart/form-data", "a=b")).starts_with("# wget cannot send multipart"));
        let p = render(3, &req());
        assert!(p.contains("'X-Note' = 'it''s'"), "{p}");
        assert!(
            p.contains("-Method 'Post'")
                && p.contains("-ContentType 'application/json'")
                && p.contains("-Body '{\"name\"")
        );
        let q = render(3, &with("application/json", "{\"s\":\"‘x’ y\"}"));
        assert!(q.contains("‘‘x’’ y"), "typographic quotes are doubled: {q}");
        let m = render(3, &with("multipart/form-data", "a=b\nf=@C:\\x\\y.txt"));
        assert!(
            m.contains("-Form @{") && m.contains("'a' = 'b'") && m.contains("'f' = Get-Item -Path 'C:\\x\\y.txt'"),
            "{m}"
        );
        assert!(!m.contains("-ContentType"));
    }

    #[test]
    fn javascript_family() {
        let f = render(4, &req());
        assert!(f.contains("body: JSON.stringify({\"name\":\"Rex\",\"ok\":true,\"n\":null}),"));
        assert!(f.starts_with("fetch(\"https://api.example.com/pets?limit=2\", {\n  method: \"POST\","));
        assert!(f.contains("\"Content-Type\": \"application/json\""));
        let n = render(7, &req());
        assert!(n.starts_with("const response = await fetch(") && n.contains("console.log(await response.text());"));
        let a = render(5, &req());
        assert!(
            a.contains("method: \"post\"") && a.contains("data: {\"name\":\"Rex\"") && a.starts_with("import axios")
        );
        assert!(render(8, &req()).starts_with("const axios = require(\"axios\");"));
        let form = render(4, &with("application/x-www-form-urlencoded", "a=1&a=2"));
        assert!(form.contains("new URLSearchParams([[\"a\", \"1\"], [\"a\", \"2\"]])"), "{form}");
        let mp = render(4, &with("multipart/form-data", "t=\"q\"\nf=@/x/y.png"));
        assert!(mp.starts_with("const form = new FormData();\nform.append(\"t\", \"\\\"q\\\"\");"), "{mp}");
        assert!(
            mp.contains("form.append(\"f\", new Blob([/* contents of /x/y.png */]), \"y.png\");")
                && mp.contains("body: form,")
        );
        assert!(!mp.contains("Content-Type"));
        let uni = render(4, &with("text/plain", "olá\n\u{1}\"x\"\\"));
        assert!(uni.contains("body: \"olá\\n\\u0001\\\"x\\\"\\\\\","), "{uni}");
        let x = render(6, &req());
        assert!(
            x.contains("xhr.setRequestHeader(\"X-Note\", \"it's\");")
                && x.ends_with("xhr.send(JSON.stringify({\"name\":\"Rex\",\"ok\":true,\"n\":null}));")
        );
        let none = render(4, &Req { method: "GET".into(), url: "http://h".into(), ..Default::default() });
        assert!(!none.contains("body:") && !none.contains("headers"));
    }

    #[test]
    fn python_family() {
        let p = render(9, &req());
        assert!(p.contains("json={\n        \"name\": \"Rex\",\n        \"ok\": True,\n        \"n\": None,\n    },"));
        assert!(p.starts_with("import requests"));
        let raw =
            Req { method: "PUT".into(), url: "http://h".into(), body: Some("plain".into()), ..Default::default() };
        assert!(render(9, &raw).contains("data=\"plain\","));
        let hx = render(10, &raw);
        assert!(
            hx.starts_with("import httpx") && hx.contains("httpx.request(") && hx.contains("content=\"plain\","),
            "{hx}"
        );
        let form = render(9, &with("application/x-www-form-urlencoded", "a=1&a=2&b=%C3%A9"));
        assert!(form.contains("\"a\": [\"1\", \"2\"],") && form.contains("\"b\": \"é\","), "{form}");
        let mp = render(9, &with("multipart/form-data", "t=v\nf=@/x/y.png"));
        assert!(
            mp.contains("(\"t\", (None, \"v\")),") && mp.contains("(\"f\", (\"y.png\", open(\"/x/y.png\", \"rb\"))),"),
            "{mp}"
        );
        assert!(!mp.contains("Content-Type"));
        let u = render(11, &req());
        assert!(u.contains("data=\"{\\\"name\\\":\\\"Rex\\\"") && u.contains("method=\"POST\""), "{u}");
        assert!(render(11, &with("multipart/form-data", "a=b")).starts_with("# urllib has no multipart"));
        let empty = render(9, &Req { method: "POST".into(), url: "http://h".into(), ..Default::default() });
        assert!(!empty.contains("json=") && !empty.contains("data="));
    }

    #[test]
    fn go_and_rust() {
        let g = render(12, &req());
        assert!(
            g.starts_with("package main\n\nimport (\n\t\"fmt\"\n\t\"io\"\n\t\"net/http\"\n\t\"strings\"\n)"),
            "{g}"
        );
        assert!(g.contains("http.NewRequest(\"POST\", \"https://api.example.com/pets?limit=2\", strings.NewReader("));
        assert!(g.contains("req.Header.Add(\"X-Note\", \"it's\")"));
        let g0 = render(12, &Req { method: "GET".into(), url: "http://h".into(), ..Default::default() });
        assert!(g0.contains(", nil)") && !g0.contains("strings"));
        let gm = render(12, &with("multipart/form-data", "a=b\nf=@/x/y.png"));
        assert!(
            gm.contains("multipart.NewWriter(&buf)")
                && gm.contains("w.WriteField(\"a\", \"b\")")
                && gm.contains("\"os\""),
            "{gm}"
        );
        assert!(gm.contains("req.Header.Set(\"Content-Type\", w.FormDataContentType())"));
        let r = render(13, &req());
        assert!(r.contains(".request(reqwest::Method::POST, \"https://api.example.com/pets?limit=2\")"), "{r}");
        assert!(r.contains(".header(\"X-Note\", \"it's\")") && r.contains(".body(\"{\\\"name\\\""));
        let rm = render(13, &with("multipart/form-data", "a=b\nf=@/x/y.png"));
        assert!(
            rm.contains(".text(\"a\", \"b\")")
                && rm.contains(".file(\"f\", \"/x/y.png\").await?")
                && rm.contains(".multipart(form)")
        );
        let ru = render(13, &with("text/plain", "\u{1}é"));
        assert!(ru.contains("\\u{1}é"), "{ru}");
        let custom = render(13, &Req { method: "PURGE".into(), url: "http://h".into(), ..Default::default() });
        assert!(custom.contains("from_bytes(\"PURGE\".as_bytes())?"));
    }

    #[test]
    fn php_and_ruby() {
        let c = render(14, &req());
        assert!(c.starts_with("<?php\n$ch = curl_init();"));
        assert!(
            c.contains("CURLOPT_HTTPHEADER => [\n        'X-Note: it\\'s',\n        'Content-Type: application/json',"),
            "{c}"
        );
        assert!(c.contains("CURLOPT_POSTFIELDS => '{\"name\":\"Rex\""));
        let cm = render(14, &with("multipart/form-data", "a=b\nf=@/x/y.png"));
        assert!(
            cm.contains("'a' => 'b',")
                && cm.contains("'f' => new CURLFile('/x/y.png'),")
                && !cm.contains("Content-Type"),
            "{cm}"
        );
        let slash = render(14, &with("text/plain", "C:\\path\\'x'"));
        assert!(slash.contains("'C:\\\\path\\\\\\'x\\''"), "{slash}");
        let g = render(15, &req());
        assert!(
            g.contains("$client->request('POST', 'https://api.example.com/pets?limit=2', [")
                && g.contains("'http_errors' => false,")
        );
        assert!(g.contains("'X-Note' => 'it\\'s',") && g.contains("'body' => '{\"name\""));
        let gm = render(15, &with("multipart/form-data", "a=b\nf=@/x/y.png"));
        assert!(gm.contains("['name' => 'a', 'contents' => 'b'],") && gm.contains("fopen('/x/y.png', 'r')"), "{gm}");
        let rb = render(16, &req());
        assert!(
            rb.contains("request = Net::HTTP::Post.new(uri.request_uri)")
                && rb.contains("request['X-Note'] = 'it\\'s'"),
            "{rb}"
        );
        assert!(
            rb.contains("request['Content-Type'] = 'application/json'") && rb.contains("request.body = '{\"name\"")
        );
        let rbi = render(16, &with("text/plain", "#{x} #$y"));
        assert!(rbi.contains("request.body = '#{x} #$y'"), "single quotes never interpolate: {rbi}");
        let rbm = render(16, &with("multipart/form-data", "a=b\nf=@/x/y.png"));
        assert!(
            rbm.contains("request.set_form([['a', 'b'], ['f', File.open('/x/y.png')]], 'multipart/form-data')"),
            "{rbm}"
        );
        let custom = render(16, &Req { method: "PURGE".into(), url: "http://h/x".into(), ..Default::default() });
        assert!(custom.contains("Net::HTTPGenericRequest.new('PURGE', true, true, uri.request_uri)"));
    }

    #[test]
    fn java_csharp_kotlin() {
        let j = render(17, &req());
        assert!(
            j.contains("import java.net.http.HttpClient;")
                && j.contains(".uri(URI.create(\"https://api.example.com/pets?limit=2\"))"),
            "{j}"
        );
        assert!(
            j.contains(".header(\"X-Note\", \"it's\")")
                && j.contains(".method(\"POST\", HttpRequest.BodyPublishers.ofString(\"{\\\"name\\\"")
        );
        let j0 = render(17, &Req { method: "GET".into(), url: "http://h".into(), ..Default::default() });
        assert!(j0.contains("BodyPublishers.noBody()"));
        let jm = render(17, &with("multipart/form-data", "a=b\nf=@/x/y.png"));
        assert!(
            jm.contains("multipart/form-data; boundary=")
                && jm.contains("Files.readString(Path.of(\"/x/y.png\"))")
                && jm.contains("name=\\\"a\\\"\\r\\n\\r\\n"),
            "{jm}"
        );
        let c = render(18, &req());
        assert!(c.contains("new HttpRequestMessage(HttpMethod.Post, \"https://api.example.com/pets?limit=2\")"), "{c}");
        assert!(
            c.contains("TryAddWithoutValidation(\"X-Note\", \"it's\")")
                && c.contains("Encoding.UTF8, \"application/json\")")
        );
        let cf = render(18, &with("application/x-www-form-urlencoded", "a=1&a=2"));
        assert!(cf.contains("new FormUrlEncodedContent(new[] { new KeyValuePair<string, string>(\"a\", \"1\"), new KeyValuePair<string, string>(\"a\", \"2\") })"), "{cf}");
        let cm = render(18, &with("multipart/form-data", "a=b\nf=@/x/y.png"));
        assert!(
            cm.contains("content.Add(new StringContent(\"b\"), \"a\");")
                && cm.contains("File.ReadAllBytes(\"/x/y.png\")"),
            "{cm}"
        );
        let k = render(19, &req());
        assert!(
            k.contains("import okhttp3.MediaType.Companion.toMediaType")
                && k.contains(".addHeader(\"X-Note\", \"it's\")"),
            "{k}"
        );
        assert!(
            k.contains(".method(\"POST\", \"{\\\"name\\\"")
                && k.contains(".toRequestBody(\"application/json\".toMediaType())")
        );
        let kd = render(19, &with("text/plain", "cost: $5 ${x}"));
        assert!(kd.contains("\"cost: \\$5 \\${x}\""), "{kd}");
        let kg = render(19, &Req { method: "GET".into(), url: "http://h".into(), ..Default::default() });
        assert!(kg.contains(".method(\"GET\", null)"));
        let kp = render(19, &Req { method: "POST".into(), url: "http://h".into(), ..Default::default() });
        assert!(kp.contains("ByteArray(0).toRequestBody()"));
        let kf = render(19, &with("application/x-www-form-urlencoded", "a=1"));
        assert!(kf.contains("FormBody.Builder()") && kf.contains(".add(\"a\", \"1\")"));
        let km = render(19, &with("multipart/form-data", "a=b\nf=@/x/y.png"));
        assert!(
            km.contains("MultipartBody.FORM")
                && km.contains(".addFormDataPart(\"f\", \"y.png\", File(\"/x/y.png\").asRequestBody())"),
            "{km}"
        );
    }

    #[test]
    fn swift_and_dart() {
        let s = render(20, &req());
        assert!(s.contains("URLRequest(url: URL(string: \"https://api.example.com/pets?limit=2\")!)"), "{s}");
        assert!(
            s.contains("request.setValue(\"it's\", forHTTPHeaderField: \"X-Note\")")
                && s.contains("request.httpBody = \"{\\\"name\\\"")
        );
        let su = render(20, &with("text/plain", "\u{1}é\\(x)"));
        assert!(su.contains("\\u{1}é\\\\(x)"), "interpolation is never injected: {su}");
        let sm = render(20, &with("multipart/form-data", "a=b\nf=@/x/y.png"));
        assert!(
            sm.contains("let boundary =")
                && sm.contains("Data(contentsOf: URL(fileURLWithPath: \"/x/y.png\"))")
                && sm.contains("multipart/form-data; boundary="),
            "{sm}"
        );
        let d = render(21, &req());
        assert!(d.contains("http.Request(\"POST\", Uri.parse(\"https://api.example.com/pets?limit=2\"))"), "{d}");
        assert!(d.contains("request.headers[\"X-Note\"] = \"it's\";") && d.contains("request.body = \"{\\\"name\\\""));
        let dd = render(21, &with("text/plain", "$x ${y}"));
        assert!(dd.contains("request.body = \"\\$x \\${y}\";"), "{dd}");
        let dm = render(21, &with("multipart/form-data", "a=b\nf=@/x/y.png"));
        assert!(
            dm.contains("http.MultipartRequest(\"POST\"")
                && dm.contains("request.fields[\"a\"] = \"b\";")
                && dm.contains("MultipartFile.fromPath(\"f\", \"/x/y.png\")"),
            "{dm}"
        );
    }

    #[test]
    fn body_classification() {
        let p = Parsed::of(&req());
        assert!(matches!(p.body, Body::Json(_)));
        assert_eq!(p.ct, "application/json");
        assert!(p.headers.iter().all(|(k, _)| k != "Content-Type"));
        assert!(matches!(Parsed::of(&with("application/json", "not json")).body, Body::Raw(_)));
        assert!(matches!(Parsed::of(&with("application/json", "{}")).body, Body::Json(_)));
        assert_eq!(Parsed::of(&with("text/plain", "")).body, Body::None);
        let nct = Req { method: "POST".into(), url: "u".into(), body: Some("{\"a\":1}".into()), ..Default::default() };
        assert!(matches!(Parsed::of(&nct).body, Body::Json(_)));
    }

    #[test]
    fn form_fields_parse_every_notation() {
        let f = form_fields("a=1&b=x+y&c=%E2%98%83&c=2&d", false);
        let v: Vec<(&str, &str)> = f.iter().map(|x| (x.name.as_str(), x.value.as_str())).collect();
        assert_eq!(v, vec![("a", "1"), ("b", "x y"), ("c", "☃"), ("c", "2"), ("d", "")]);
        let m = form_fields("a=1\nf=@/p/q.txt", true);
        assert!(!m[0].file && m[1].file && m[1].value == "/p/q.txt");
        let j = form_fields("{\"a\":1,\"f\":\"@x.bin\"}", true);
        assert_eq!(j[0].value, "1");
        assert!(j[1].file && j[1].value == "x.bin");
        assert_eq!(pct_decode("100%"), "100%");
        assert_eq!(pct_decode("%zz"), "%zz");
    }

    #[test]
    fn quoting_helpers() {
        assert_eq!(dq("a\"b\\c\n\u{7}", Q::C), "\"a\\\"b\\\\c\\n\\u0007\"");
        assert_eq!(dq("\u{7}", Q::Brace), "\"\\u{7}\"");
        assert_eq!(dq("$x", Q::Dollar), "\"\\$x\"");
        assert_eq!(dq("$x", Q::C), "\"$x\"");
        assert_eq!(sh("a'b"), "'a'\\''b'");
        assert_eq!(sq("a\\b'c"), "'a\\\\b\\'c'");
        assert_eq!(ps("a'b"), "'a''b'");
        assert_eq!(base_name("C:\\x\\y.txt"), "y.txt");
        assert_eq!(base_name("/a/b.c"), "b.c");
    }
}
