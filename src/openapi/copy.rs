//! Text for the "Copy" buttons of the API explorer: parameters, headers, bodies, validator and
//! lint results, and the short "Copied" flash.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};

use super::{
    lint::{Finding, Severity},
    tree::{parse, to_json},
    validate::Issue,
};

/// How long a button says "Copied".
pub const FLASH: Duration = Duration::from_millis(1400);

/// `name: value`.
pub fn pair(name: &str, value: &str) -> String {
    format!("{name}: {value}")
}

/// Every parameter with a value, one per line: `[query] limit: 5`. Rows are (location, name,
/// value); empty values are skipped.
pub fn params(rows: &[(String, String, String)]) -> String {
    rows.iter()
        .filter(|(_, _, v)| !v.trim().is_empty())
        .map(|(l, n, v)| format!("[{l}] {}", pair(n, v.trim())))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Header lines, `Name: value`.
pub fn headers(hs: &[(String, String)]) -> String {
    hs.iter().map(|(k, v)| pair(k, v)).collect::<Vec<_>>().join("\n")
}

/// JSON text pretty-printed; anything else is returned as it is.
pub fn pretty(text: &str) -> String {
    let t = text.trim();
    if t.starts_with(['{', '['])
        && let Ok(n) = parse(t)
    {
        return to_json(&n, true);
    }
    text.to_string()
}

/// Validator issues: `path: message`.
pub fn issues(list: &[Issue]) -> String {
    list.iter().map(|i| pair(&i.path, &i.msg)).collect::<Vec<_>>().join("\n")
}

/// One lint finding: `warning [code] (line N): message`.
pub fn finding(f: &Finding) -> String {
    let sev = match f.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "note",
    };
    format!("{sev} [{}] (line {}): {}", f.code, f.line + 1, f.msg)
}

pub fn findings(list: &[Finding]) -> String {
    list.iter().map(finding).collect::<Vec<_>>().join("\n")
}

/// A whole answer: status line, headers, blank line, body.
pub fn response(status: &str, hs: &[(String, String)], body: &str) -> String {
    let mut out = status.to_string();
    if !hs.is_empty() {
        out.push('\n');
        out.push_str(&headers(hs));
    }
    if !body.is_empty() {
        out.push_str("\n\n");
        out.push_str(body);
    }
    out
}

/// Identity of a copy button: the hash of what it copies.
pub fn key_of(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// The last copy: which button (by `key_of`) and when.
#[derive(Clone, Copy, Debug)]
pub struct Flash {
    pub key: u64,
    pub at: Instant,
}

impl Flash {
    pub fn new(key: u64) -> Flash {
        Flash { key, at: Instant::now() }
    }
    /// Is the button `key` still showing "Copied"?
    pub fn on(&self, key: u64) -> bool {
        self.key == key && self.at.elapsed() < FLASH
    }
    pub fn alive(&self) -> bool {
        self.at.elapsed() < FLASH
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(l: &str, n: &str, v: &str) -> (String, String, String) {
        (l.into(), n.into(), v.into())
    }

    #[test]
    fn parameters_skip_empty_values() {
        let rows = [row("query", "limit", " 5 "), row("path", "id", ""), row("header", "X-A", "é \"q\"")];
        assert_eq!(params(&rows), "[query] limit: 5\n[header] X-A: é \"q\"");
        assert_eq!(params(&[]), "");
    }

    #[test]
    fn headers_pairs_and_responses() {
        let hs = vec![("A".to_string(), "1".to_string()), ("B".to_string(), "".to_string())];
        assert_eq!(headers(&hs), "A: 1\nB: ");
        assert_eq!(pair("k", "v"), "k: v");
        assert_eq!(response("200 OK", &hs, "{}"), "200 OK\nA: 1\nB: \n\n{}");
        assert_eq!(response("204 No Content", &[], ""), "204 No Content");
    }

    #[test]
    fn json_is_formatted_other_text_is_kept() {
        assert_eq!(pretty("{\"a\":[1,2]}"), "{\n  \"a\": [\n    1,\n    2\n  ]\n}");
        assert_eq!(pretty("plain  text"), "plain  text");
        assert_eq!(pretty("{broken"), "{broken");
    }

    #[test]
    fn validator_and_lint_text() {
        let i = vec![Issue { path: "$.id".into(), msg: "expected integer".into() }];
        assert_eq!(issues(&i), "$.id: expected integer");
        let f = Finding { severity: Severity::Warning, code: "no-summary", msg: "missing".into(), line: 6 };
        assert_eq!(finding(&f), "warning [no-summary] (line 7): missing");
        assert_eq!(findings(&[f]), "warning [no-summary] (line 7): missing");
    }

    #[test]
    fn flash_identifies_one_button() {
        let k = key_of("abc");
        assert_eq!(k, key_of("abc"));
        assert_ne!(k, key_of("abd"));
        let f = Flash::new(k);
        assert!(f.on(k) && f.alive());
        assert!(!f.on(key_of("x")));
        let old = Flash { key: k, at: Instant::now() - FLASH - Duration::from_millis(5) };
        assert!(!old.on(k) && !old.alive());
    }
}
