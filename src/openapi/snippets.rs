//! Code snippets for a request: curl, JavaScript fetch, Python requests.

use super::tree::{Node, Val, num_text, parse, to_json};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Req {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

pub const LANGS: [&str; 3] = ["curl", "JavaScript", "Python"];

pub fn render(lang: usize, r: &Req) -> String {
    match lang {
        0 => curl(r),
        1 => fetch(r),
        _ => python(r),
    }
}

fn sh(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn curl(r: &Req) -> String {
    let mut out = String::from("curl");
    if r.method != "GET" {
        out.push_str(&format!(" -X {}", r.method));
    }
    out.push(' ');
    out.push_str(&sh(&r.url));
    for (k, v) in &r.headers {
        out.push_str(&format!(" \\\n  -H {}", sh(&format!("{k}: {v}"))));
    }
    if let Some(b) = &r.body {
        out.push_str(&format!(" \\\n  -d {}", sh(b)));
    }
    out
}

fn js_str(s: &str) -> String {
    to_json(&Node::s(s), false)
}

pub fn fetch(r: &Req) -> String {
    let mut out = format!("fetch({}, {{\n  method: {},\n", js_str(&r.url), js_str(&r.method));
    if !r.headers.is_empty() {
        out.push_str("  headers: {\n");
        for (k, v) in &r.headers {
            out.push_str(&format!("    {}: {},\n", js_str(k), js_str(v)));
        }
        out.push_str("  },\n");
    }
    if let Some(b) = &r.body {
        match parse(b) {
            Ok(j) if j.is_map() || !j.arr().is_empty() => {
                out.push_str(&format!("  body: JSON.stringify({}),\n", to_json(&j, false)))
            }
            _ => out.push_str(&format!("  body: {},\n", js_str(b))),
        }
    }
    out.push_str("})\n  .then((res) => res.text())\n  .then(console.log);");
    out
}

fn py_lit(n: &Node, ind: usize, out: &mut String) {
    let pad = "    ".repeat(ind + 1);
    let end = "    ".repeat(ind);
    match &n.val {
        Val::Null => out.push_str("None"),
        Val::Bool(true) => out.push_str("True"),
        Val::Bool(false) => out.push_str("False"),
        Val::Num(x) => out.push_str(&num_text(*x)),
        Val::Str(s) => out.push_str(&to_json(&Node::s(s), false)),
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
                out.push_str(&to_json(&Node::s(k), false));
                out.push_str(": ");
                py_lit(v, ind + 1, out);
                out.push_str(",\n");
            }
            out.push_str(&end);
            out.push('}');
        }
    }
}

pub fn python(r: &Req) -> String {
    let mut out = format!(
        "import requests\n\nresponse = requests.request(\n    {},\n    {},\n",
        js_str(&r.method),
        js_str(&r.url)
    );
    if !r.headers.is_empty() {
        out.push_str("    headers={\n");
        for (k, v) in &r.headers {
            out.push_str(&format!("        {}: {},\n", js_str(k), js_str(v)));
        }
        out.push_str("    },\n");
    }
    if let Some(b) = &r.body {
        match parse(b) {
            Ok(j) if j.is_map() || !j.arr().is_empty() => {
                out.push_str("    json=");
                py_lit(&j, 1, &mut out);
                out.push_str(",\n");
            }
            _ => out.push_str(&format!("    data={},\n", js_str(b))),
        }
    }
    out.push_str(")\nprint(response.status_code)\nprint(response.text)");
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

    #[test]
    fn curl_quotes_for_the_shell() {
        let c = curl(&req());
        assert!(c.starts_with("curl -X POST 'https://api.example.com/pets?limit=2' \\\n"));
        assert!(c.contains("-H 'X-Note: it'\\''s'"));
        assert!(c.ends_with("-d '{\"name\":\"Rex\",\"ok\":true,\"n\":null}'"));
        let get = Req { method: "GET".into(), url: "http://h/".into(), ..Default::default() };
        assert_eq!(curl(&get), "curl 'http://h/'");
    }

    #[test]
    fn fetch_and_python() {
        let f = fetch(&req());
        assert!(f.contains("body: JSON.stringify({\"name\":\"Rex\",\"ok\":true,\"n\":null}),"));
        assert!(f.starts_with("fetch(\"https://api.example.com/pets?limit=2\", {\n  method: \"POST\","));
        let p = python(&req());
        assert!(p.contains("json={\n        \"name\": \"Rex\",\n        \"ok\": True,\n        \"n\": None,\n    },"));
        assert!(p.starts_with("import requests"));
        let raw =
            Req { method: "PUT".into(), url: "http://h".into(), body: Some("plain".into()), ..Default::default() };
        assert!(python(&raw).contains("data=\"plain\","));
    }
}
