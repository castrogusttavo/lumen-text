//! Validate a JSON value (a received response body) against a schema: types, `required`,
//! `enum`, formats, string / number / array limits, `additionalProperties`, `allOf` / `oneOf` /
//! `anyOf`. Divergences are reported with their JSON path (`$.items[0].id`).

use super::schema::{deref, is_nullable, properties, property_names};
use super::tree::{Node, Val, scalar_text, to_json};

const MAX_ISSUES: usize = 200;
const MAX_SCHEMA_DEPTH: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub path: String,
    pub msg: String,
}

/// All divergences of `value` from `schema` (empty = valid).
pub fn validate(root: &Node, schema: &Node, value: &Node) -> Vec<Issue> {
    let mut out = Vec::new();
    check(root, schema, value, "$", 0, &mut out);
    // `allOf` members and the merged object view can report the same divergence twice.
    let mut seen = std::collections::HashSet::new();
    out.retain(|i| seen.insert((i.path.clone(), i.msg.clone())));
    out
}

fn kind(v: &Val) -> &'static str {
    match v {
        Val::Null => "null",
        Val::Bool(_) => "boolean",
        Val::Num(x) if x.fract() == 0.0 => "integer",
        Val::Num(_) => "number",
        Val::Str(_) => "string",
        Val::Arr(_) => "array",
        Val::Map(_) => "object",
    }
}

fn type_ok(t: &str, v: &Val) -> bool {
    match t {
        "string" => matches!(v, Val::Str(_)),
        "integer" => matches!(v, Val::Num(x) if x.fract() == 0.0),
        "number" => matches!(v, Val::Num(_)),
        "boolean" => matches!(v, Val::Bool(_)),
        "array" => matches!(v, Val::Arr(_)),
        "object" => matches!(v, Val::Map(_)),
        "null" => matches!(v, Val::Null),
        _ => true,
    }
}

fn types(s: &Node) -> Vec<String> {
    match s.get("type") {
        Some(t) => match &t.val {
            Val::Str(x) => vec![x.clone()],
            Val::Arr(a) => a.iter().filter_map(|x| x.str().map(String::from)).collect(),
            _ => vec![],
        },
        None => vec![],
    }
}

fn push(out: &mut Vec<Issue>, path: &str, msg: String) {
    if out.len() < MAX_ISSUES {
        out.push(Issue { path: path.to_string(), msg });
    }
}

fn check(root: &Node, schema: &Node, v: &Node, path: &str, sd: usize, out: &mut Vec<Issue>) {
    if sd > MAX_SCHEMA_DEPTH || out.len() >= MAX_ISSUES {
        return;
    }
    let s = deref(root, schema);
    if matches!(v.val, Val::Null) && is_nullable(s) {
        return;
    }
    for a in s.get("allOf").map(|a| a.arr()).unwrap_or(&[]) {
        check(root, a, v, path, sd + 1, out);
    }
    for (key, one) in [("oneOf", true), ("anyOf", false)] {
        let alts = s.get(key).map(|a| a.arr()).unwrap_or(&[]);
        if alts.is_empty() {
            continue;
        }
        let ok = alts
            .iter()
            .filter(|a| {
                let mut tmp = Vec::new();
                check(root, a, v, path, sd + 1, &mut tmp);
                tmp.is_empty()
            })
            .count();
        if ok == 0 {
            push(out, path, format!("does not match any of the {key} schemas"));
        } else if one && ok > 1 {
            push(out, path, format!("matches {ok} of the oneOf schemas (exactly one is allowed)"));
        }
    }
    let ts = types(s);
    if !ts.is_empty() && !ts.iter().any(|t| type_ok(t, &v.val)) {
        push(out, path, format!("expected {}, got {}", ts.join(" | "), kind(&v.val)));
        return;
    }
    if let Some(e) = s.get("enum")
        && !e.arr().is_empty()
    {
        let me = to_json(v, false);
        if !e.arr().iter().any(|x| to_json(x, false) == me) {
            let list: Vec<String> = e.arr().iter().map(scalar_text).collect();
            push(out, path, format!("{} is not one of [{}]", scalar_text(v), list.join(", ")));
        }
    }
    match &v.val {
        Val::Str(t) => check_string(s, t, path, out),
        Val::Num(x) => check_number(s, *x, path, out),
        Val::Arr(items) => {
            let len = items.len() as f64;
            if let Some(m) = s.get("minItems").and_then(|m| m.num())
                && len < m
            {
                push(out, path, format!("has {len} items, at least {m} required"));
            }
            if let Some(m) = s.get("maxItems").and_then(|m| m.num())
                && len > m
            {
                push(out, path, format!("has {len} items, at most {m} allowed"));
            }
            if let Some(it) = s.get("items") {
                for (i, x) in items.iter().enumerate() {
                    check(root, it, x, &format!("{path}[{i}]"), sd + 1, out);
                }
            }
        }
        Val::Map(entries) => check_object(root, s, entries, path, sd, out),
        _ => {}
    }
}

fn check_object(root: &Node, s: &Node, entries: &[(String, Node)], path: &str, sd: usize, out: &mut Vec<Issue>) {
    let props = properties(root, s);
    for (name, _, req) in &props {
        if *req && !entries.iter().any(|(k, _)| k == name) {
            push(out, &format!("{path}.{name}"), "required property is missing".to_string());
        }
    }
    let known = property_names(root, s);
    for (k, val) in entries {
        let here = format!("{path}.{k}");
        if let Some((_, sch, _)) = props.iter().find(|(n, _, _)| n == k) {
            check(root, sch, val, &here, sd + 1, out);
        } else if let Some(ap) = s.get("additionalProperties") {
            match &ap.val {
                Val::Bool(false) if !known.contains(k) => {
                    push(out, &here, "additional property is not allowed".to_string())
                }
                Val::Map(_) => check(root, ap, val, &here, sd + 1, out),
                _ => {}
            }
        }
    }
}

fn check_string(s: &Node, t: &str, path: &str, out: &mut Vec<Issue>) {
    let len = t.chars().count() as f64;
    if let Some(m) = s.get("minLength").and_then(|m| m.num())
        && len < m
    {
        push(out, path, format!("is {len} characters long, at least {m} required"));
    }
    if let Some(m) = s.get("maxLength").and_then(|m| m.num())
        && len > m
    {
        push(out, path, format!("is {len} characters long, at most {m} allowed"));
    }
    if let Some(p) = s.gets("pattern")
        && let Ok(re) = regex::Regex::new(p)
        && !re.is_match(t)
    {
        push(out, path, format!("does not match the pattern {p}"));
    }
    if let Some(f) = s.gets("format")
        && !format_ok(f, t)
    {
        push(out, path, format!("{t:?} is not a valid {f}"));
    }
}

fn check_number(s: &Node, x: f64, path: &str, out: &mut Vec<Issue>) {
    let excl_min_flag = s.get("exclusiveMinimum").and_then(|b| b.bool()).unwrap_or(false);
    let excl_max_flag = s.get("exclusiveMaximum").and_then(|b| b.bool()).unwrap_or(false);
    if let Some(m) = s.get("minimum").and_then(|m| m.num())
        && (x < m || (excl_min_flag && x <= m))
    {
        push(out, path, format!("{} is below the minimum {}", super::tree::num_text(x), super::tree::num_text(m)));
    }
    if let Some(m) = s.get("maximum").and_then(|m| m.num())
        && (x > m || (excl_max_flag && x >= m))
    {
        push(out, path, format!("{} is above the maximum {}", super::tree::num_text(x), super::tree::num_text(m)));
    }
    if let Some(m) = s.get("exclusiveMinimum").and_then(|m| m.num())
        && x <= m
    {
        push(out, path, format!("{} must be greater than {}", super::tree::num_text(x), super::tree::num_text(m)));
    }
    if let Some(m) = s.get("exclusiveMaximum").and_then(|m| m.num())
        && x >= m
    {
        push(out, path, format!("{} must be less than {}", super::tree::num_text(x), super::tree::num_text(m)));
    }
    match s.gets("format") {
        Some("int32") if x.fract() == 0.0 && (x < i32::MIN as f64 || x > i32::MAX as f64) => {
            push(out, path, "does not fit in an int32".to_string());
        }
        _ => {}
    }
}

fn digits(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit())
}

fn date_ok(d: &str) -> bool {
    let p: Vec<&str> = d.split('-').collect();
    if p.len() != 3 || !digits(p[0], 4) || !digits(p[1], 2) || !digits(p[2], 2) {
        return false;
    }
    let (m, day): (u32, u32) = (p[1].parse().unwrap_or(0), p[2].parse().unwrap_or(0));
    (1..=12).contains(&m) && (1..=31).contains(&day)
}

fn time_ok(t: &str) -> bool {
    // hh:mm:ss[.frac](Z|+hh:mm|-hh:mm)
    let (clock, zone) = match t.find(['Z', 'z', '+', '-']) {
        Some(i) => (&t[..i], &t[i..]),
        None => return false,
    };
    let clock = clock.split('.').next().unwrap_or("");
    let p: Vec<&str> = clock.split(':').collect();
    if p.len() != 3 || !p.iter().all(|x| digits(x, 2)) {
        return false;
    }
    let (h, m, s): (u32, u32, u32) =
        (p[0].parse().unwrap_or(99), p[1].parse().unwrap_or(99), p[2].parse().unwrap_or(99));
    if h > 23 || m > 59 || s > 60 {
        return false;
    }
    zone.eq_ignore_ascii_case("z")
        || (zone.len() == 6 && zone.as_bytes()[3] == b':' && digits(&zone[1..3], 2) && digits(&zone[4..6], 2))
}

pub fn format_ok(f: &str, v: &str) -> bool {
    match f {
        "date-time" => {
            let Some(i) = v.find(['T', 't', ' ']) else { return false };
            date_ok(&v[..i]) && time_ok(&v[i + 1..])
        }
        "date" => date_ok(v),
        "email" => {
            let mut it = v.split('@');
            match (it.next(), it.next(), it.next()) {
                (Some(a), Some(b), None) => {
                    !a.is_empty() && b.contains('.') && !b.starts_with('.') && !b.ends_with('.')
                }
                _ => false,
            }
        }
        "uuid" => {
            let p: Vec<&str> = v.split('-').collect();
            p.len() == 5
                && [8, 4, 4, 4, 12]
                    .iter()
                    .zip(&p)
                    .all(|(n, s)| s.len() == *n && s.bytes().all(|b| b.is_ascii_hexdigit()))
        }
        "uri" | "url" => {
            let Some(i) = v.find(':') else { return false };
            i > 0
                && v[..i].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
                && v.len() > i + 1
        }
        "ipv4" => {
            let p: Vec<&str> = v.split('.').collect();
            p.len() == 4 && p.iter().all(|x| !x.is_empty() && x.len() <= 3 && x.parse::<u8>().is_ok())
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::super::spec::{PETSTORE, Spec};
    use super::super::tree::parse;
    use super::*;

    fn pet_schema(s: &Spec) -> &Node {
        s.ops[1].responses[0].media[0].schema.as_ref().unwrap()
    }

    #[test]
    fn a_conforming_body_passes() {
        let s = Spec::parse(PETSTORE).unwrap();
        let v = parse("{\"id\": 1, \"name\": \"Rex\", \"tag\": \"dog\"}").unwrap();
        assert_eq!(validate(&s.root, pet_schema(&s), &v), vec![]);
    }

    #[test]
    fn divergences_are_listed_by_path() {
        let s = Spec::parse(PETSTORE).unwrap();
        let v = parse("{\"id\": \"7\", \"tag\": \"fish\"}").unwrap();
        let issues = validate(&s.root, pet_schema(&s), &v);
        let got: Vec<(&str, &str)> = issues.iter().map(|i| (i.path.as_str(), i.msg.as_str())).collect();
        assert!(got.contains(&("$.name", "required property is missing")), "{got:?}");
        assert!(got.contains(&("$.id", "expected integer, got string")), "{got:?}");
        assert!(got.iter().any(|(p, m)| *p == "$.tag" && m.contains("is not one of [cat, dog]")), "{got:?}");
    }

    #[test]
    fn arrays_nulls_and_additional_properties() {
        let r = parse("{}").unwrap();
        let sch = parse("{\"type\":\"array\",\"items\":{\"type\":\"object\",\"additionalProperties\":false,\"properties\":{\"n\":{\"type\":\"string\",\"nullable\":true,\"format\":\"email\"}}}}").unwrap();
        let v = parse("[{\"n\": null}, {\"n\": \"nope\", \"x\": 1}]").unwrap();
        let issues = validate(&r, &sch, &v);
        let paths: Vec<&str> = issues.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["$[1].n", "$[1].x"]);
        let wrong = parse("{\"a\":1}").unwrap();
        assert_eq!(validate(&r, &sch, &wrong)[0].msg, "expected array, got object");
    }

    #[test]
    fn composition_and_limits() {
        let r = parse("{}").unwrap();
        let sch =
            parse("{\"oneOf\":[{\"type\":\"string\",\"maxLength\":3},{\"type\":\"integer\",\"minimum\":5}]}").unwrap();
        assert!(validate(&r, &sch, &parse("\"abc\"").unwrap()).is_empty());
        assert!(validate(&r, &sch, &parse("9").unwrap()).is_empty());
        assert_eq!(validate(&r, &sch, &parse("2").unwrap())[0].msg, "does not match any of the oneOf schemas");
    }

    #[test]
    fn formats() {
        assert!(format_ok("date-time", "2024-05-01T10:20:30Z"));
        assert!(format_ok("date-time", "2024-05-01T10:20:30.123+02:00"));
        assert!(!format_ok("date-time", "2024-05-01"));
        assert!(format_ok("date", "2024-05-01") && !format_ok("date", "2024-13-01"));
        assert!(format_ok("uuid", "3fa85f64-5717-4562-b3fc-2c963f66afa6") && !format_ok("uuid", "abc"));
        assert!(format_ok("email", "a@b.co") && !format_ok("email", "a@b"));
        assert!(format_ok("ipv4", "10.0.0.1") && !format_ok("ipv4", "300.0.0.1"));
        assert!(format_ok("uri", "https://x.y") && !format_ok("uri", "nothing"));
    }
}
