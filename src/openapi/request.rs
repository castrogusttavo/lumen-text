//! Turning the "Try it" form into a request, and matching an answer to the documented response.

use super::snippets::Req;
use super::spec::{Operation, Response, SecKind};
use super::tree::Node;
use super::tree::parse;
use super::validate::{Issue, validate};

/// What the user picked in the auth block.
#[derive(Clone, Debug)]
pub struct Auth {
    pub kind: SecKind,
    /// Token / key / password / cookie value.
    pub value: String,
    /// User name (Basic only).
    pub user: String,
}

pub fn pct(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') || (keep_slash && b == b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Build the request. `values` holds (location, name, value) of every parameter field.
pub fn build(
    op: &Operation,
    base: &str,
    values: &[(String, String, String)],
    body: &str,
    content_type: &str,
    auth: Option<&Auth>,
    strict: bool,
) -> Result<Req, String> {
    let base = base.trim();
    // Not strict (code snippets): placeholders instead of errors.
    let base = if base.is_empty() && !strict { "http://localhost" } else { base };
    if base.is_empty() {
        return Err("set the base URL first".into());
    }
    let val = |loc: &str, name: &str| -> String {
        values.iter().find(|(l, n, _)| l == loc && n == name).map(|(_, _, v)| v.trim().to_string()).unwrap_or_default()
    };
    // Path.
    let mut path = String::new();
    let mut rest = op.path.as_str();
    while let Some(i) = rest.find('{') {
        let Some(j) = rest[i..].find('}') else { break };
        let name = &rest[i + 1..i + j];
        let v = val("path", name);
        if v.is_empty() && strict {
            return Err(format!("path parameter {name} is empty"));
        }
        path.push_str(&rest[..i]);
        if v.is_empty() {
            path.push_str(&format!("{{{name}}}"));
        } else {
            path.push_str(&pct(&v, false));
        }
        rest = &rest[i + j + 1..];
    }
    path.push_str(rest);
    // Query, headers, cookies.
    let mut query: Vec<String> = Vec::new();
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut cookies: Vec<String> = Vec::new();
    for p in &op.params {
        let v = val(&p.loc, &p.name);
        if v.is_empty() {
            if p.required && p.loc != "path" && strict {
                return Err(format!("{} parameter {} is required", p.loc, p.name));
            }
            continue;
        }
        match p.loc.as_str() {
            "query" => query.push(format!("{}={}", pct(&p.name, false), pct(&v, false))),
            "header" => headers.push((p.name.clone(), v)),
            "cookie" => cookies.push(format!("{}={}", p.name, v)),
            _ => {}
        }
    }
    // Auth.
    if let Some(a) = auth.filter(|a| !a.value.trim().is_empty() || !a.user.trim().is_empty()) {
        let v = a.value.trim();
        match &a.kind {
            SecKind::Bearer | SecKind::OAuth2 => {
                let tok = v.strip_prefix("Bearer ").unwrap_or(v);
                headers.push(("Authorization".into(), format!("Bearer {tok}")));
            }
            SecKind::Basic => {
                headers.push((
                    "Authorization".into(),
                    format!("Basic {}", base64(format!("{}:{}", a.user.trim(), v).as_bytes())),
                ));
            }
            SecKind::ApiKey { name, loc } => match loc.as_str() {
                "query" => query.push(format!("{}={}", pct(name, false), pct(v, false))),
                "cookie" => cookies.push(if v.contains('=') { v.to_string() } else { format!("{name}={v}") }),
                _ => headers.push((name.clone(), v.to_string())),
            },
            SecKind::Other => {}
        }
    }
    if !cookies.is_empty() {
        headers.push(("Cookie".into(), cookies.join("; ")));
    }
    let body = body.trim();
    let body = if body.is_empty() { None } else { Some(body.to_string()) };
    if body.is_some() && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
        let ct = if content_type.is_empty() { "application/json" } else { content_type };
        headers.push(("Content-Type".into(), ct.to_string()));
    }
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept"))
        && op.responses.iter().any(|r| r.media.iter().any(|m| m.content_type.contains("json")))
    {
        headers.push(("Accept".into(), "application/json".into()));
    }
    let mut url = format!("{}{}", base.trim_end_matches('/'), path);
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    Ok(Req { method: op.method.to_uppercase(), url, headers, body })
}

/// The documented response for `status`: exact code, then `2XX`, then `default`.
pub fn response_for(op: &Operation, status: u16) -> Option<&Response> {
    let exact = status.to_string();
    let class = format!("{}XX", status / 100);
    op.responses
        .iter()
        .find(|r| r.code == exact)
        .or_else(|| op.responses.iter().find(|r| r.code.eq_ignore_ascii_case(&class)))
        .or_else(|| op.responses.iter().find(|r| r.code == "default"))
}

/// The schema of the JSON media type of a response.
pub fn json_schema(r: &Response) -> Option<&Node> {
    r.media.iter().find(|m| m.content_type.contains("json")).and_then(|m| m.schema.as_ref())
}

/// Outcome of checking an answer against the documented response.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    Pass,
    Fail(Vec<Issue>),
    /// Nothing to compare with (the reason).
    Skip(String),
}

/// Check a received answer: the status is documented, the content type is one of the documented
/// media types and the JSON body conforms to the response schema.
pub fn check_response(root: &Node, op: &Operation, status: u16, content_type: Option<&str>, body: &str) -> Verdict {
    let Some(r) = response_for(op, status) else {
        let codes: Vec<&str> = op.responses.iter().map(|r| r.code.as_str()).collect();
        let msg = format!("status {status} is not documented (documented: {})", codes.join(", "));
        return Verdict::Fail(vec![Issue { path: "status".into(), msg }]);
    };
    let Some(schema) = json_schema(r) else {
        return Verdict::Skip(format!("response {} documents no JSON schema", r.code));
    };
    let mut issues = Vec::new();
    if let Some(ct) = content_type {
        let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let known = r.media.iter().any(|m| m.content_type.to_ascii_lowercase() == base || m.content_type.contains('*'));
        if !known {
            let list: Vec<&str> = r.media.iter().map(|m| m.content_type.as_str()).collect();
            issues.push(Issue {
                path: "Content-Type".into(),
                msg: format!("{base} is not documented (documented: {})", list.join(", ")),
            });
        }
    }
    match parse(body) {
        Ok(v) => issues.extend(validate(root, schema, &v)),
        Err(e) => issues.push(Issue { path: "$".into(), msg: format!("the body is not valid JSON ({e})") }),
    }
    if issues.is_empty() { Verdict::Pass } else { Verdict::Fail(issues) }
}

#[cfg(test)]
mod tests {
    use super::super::spec::{PETSTORE, Spec};
    use super::*;

    fn v(loc: &str, n: &str, x: &str) -> (String, String, String) {
        (loc.to_string(), n.to_string(), x.to_string())
    }

    #[test]
    fn builds_path_query_headers_and_auth() {
        let s = Spec::parse(PETSTORE).unwrap();
        let show = &s.ops[2];
        let bearer = Auth { kind: SecKind::Bearer, value: "tok".into(), user: String::new() };
        let r = build(show, "https://h/v1/", &[v("path", "petId", "a b/c")], "", "", Some(&bearer), true).unwrap();
        assert_eq!(r.url, "https://h/v1/pets/a%20b%2Fc");
        assert_eq!(r.method, "GET");
        assert!(r.headers.contains(&("Authorization".into(), "Bearer tok".into())));
        assert!(r.headers.contains(&("Accept".into(), "application/json".into())));
        assert!(build(show, "https://h", &[], "", "", None, true).unwrap_err().contains("petId"));
        assert!(build(show, "", &[], "", "", None, true).is_err());
        let lenient = build(show, "", &[], "", "", None, false).unwrap();
        assert_eq!(lenient.url, "http://localhost/pets/{petId}");
    }

    #[test]
    fn query_body_cookie_and_basic() {
        let s = Spec::parse(PETSTORE).unwrap();
        let list = &s.ops[0];
        let cookie = Auth {
            kind: SecKind::ApiKey { name: "sid".into(), loc: "cookie".into() },
            value: "abc".into(),
            user: String::new(),
        };
        let r = build(list, "http://h", &[v("query", "limit", "5")], "", "", Some(&cookie), true).unwrap();
        assert_eq!(r.url, "http://h/pets?limit=5");
        assert!(r.headers.contains(&("Cookie".into(), "sid=abc".into())));
        let pasted = Auth { value: "sid=zzz; other=1".into(), ..cookie };
        let r = build(list, "http://h", &[], "", "", Some(&pasted), true).unwrap();
        assert!(r.headers.contains(&("Cookie".into(), "sid=zzz; other=1".into())));
        let post = &s.ops[1];
        let basic = Auth { kind: SecKind::Basic, value: "pw".into(), user: "u".into() };
        let r = build(post, "http://h", &[], " {\"name\":\"x\"} ", "application/json", Some(&basic), true).unwrap();
        assert_eq!(r.body.as_deref(), Some("{\"name\":\"x\"}"));
        assert!(r.headers.contains(&("Content-Type".into(), "application/json".into())));
        assert!(r.headers.contains(&("Authorization".into(), "Basic dTpwdw==".into())));
    }

    #[test]
    fn checks_answers_against_the_documented_response() {
        let s = Spec::parse(PETSTORE).unwrap();
        let create = &s.ops[1];
        let ok = "{\"id\":1,\"name\":\"Rex\"}";
        assert_eq!(check_response(&s.root, create, 201, Some("application/json; charset=utf-8"), ok), Verdict::Pass);
        let Verdict::Fail(i) = check_response(&s.root, create, 201, Some("text/html"), "{\"id\":\"x\"}") else {
            panic!()
        };
        assert!(
            i.iter().any(|x| x.path == "Content-Type")
                && i.iter().any(|x| x.path == "$.id")
                && i.iter().any(|x| x.path == "$.name")
        );
        assert!(matches!(check_response(&s.root, create, 500, None, "{}"), Verdict::Skip(_)), "default has no schema");
        let Verdict::Fail(i) = check_response(&s.root, &s.ops[0], 404, None, "{}") else { panic!() };
        assert!(i[0].msg.contains("not documented"));
    }

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }

    #[test]
    fn responses_match_by_code_class_then_default() {
        let s = Spec::parse(PETSTORE).unwrap();
        let create = &s.ops[1];
        assert_eq!(response_for(create, 201).unwrap().code, "201");
        assert_eq!(response_for(create, 500).unwrap().code, "default");
        assert!(response_for(&s.ops[0], 404).is_none());
        assert!(json_schema(response_for(create, 201).unwrap()).is_some());
    }
}
