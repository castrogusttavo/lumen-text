//! Lint of the spec itself: broken `$ref`s, duplicate `operationId`s, path parameters that are
//! not declared (or not in the path), operations without a success response, schemas without a
//! type, unknown security schemes. Every finding carries the source line to jump to.

use super::schema::{deref, pointer};
use super::spec::Spec;
use super::tree::{Node, Val};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub severity: Severity,
    pub code: &'static str,
    pub msg: String,
    /// 0-based source line.
    pub line: usize,
}

pub fn counts(f: &[Finding]) -> (usize, usize, usize) {
    let c = |s: Severity| f.iter().filter(|x| x.severity == s).count();
    (c(Severity::Error), c(Severity::Warning), c(Severity::Info))
}

fn path_params(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(i) = rest.find('{') {
        let Some(j) = rest[i..].find('}') else { break };
        out.push(rest[i + 1..i + j].to_string());
        rest = &rest[i + j + 1..];
    }
    out
}

fn refs(n: &Node, out: &mut Vec<(String, usize)>) {
    match &n.val {
        Val::Map(m) => {
            for (k, v) in m {
                if k == "$ref"
                    && let Some(r) = v.str()
                {
                    out.push((r.to_string(), v.line));
                } else {
                    refs(v, out);
                }
            }
        }
        Val::Arr(a) => a.iter().for_each(|v| refs(v, out)),
        _ => {}
    }
}

fn has_shape(s: &Node) -> bool {
    ["type", "$ref", "allOf", "oneOf", "anyOf", "enum", "properties", "items", "not", "const", "additionalProperties"]
        .iter()
        .any(|k| s.get(k).is_some())
}

/// Schemas (and their inline properties / items) that say nothing about their type.
fn untyped(n: &Node, name: String, out: &mut Vec<(String, usize)>, depth: usize) {
    if !n.is_map() || depth > 10 {
        return;
    }
    if !has_shape(n) {
        out.push((name.clone(), n.line));
    }
    for (k, v) in n.get("properties").map(|p| p.entries()).unwrap_or(&[]) {
        untyped(v, format!("{name}.{k}"), out, depth + 1);
    }
    if let Some(it) = n.get("items") {
        untyped(it, format!("{name}[]"), out, depth + 1);
    }
}

pub fn lint(spec: &Spec) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let root = &spec.root;
    let mut add = |severity, code, msg: String, line| out.push(Finding { severity, code, msg, line });

    // $ref targets.
    let mut all = Vec::new();
    refs(root, &mut all);
    for (r, line) in &all {
        match r.strip_prefix('#') {
            Some(p) => {
                if pointer(root, p).is_none() {
                    add(Severity::Error, "broken-ref", format!("$ref {r} does not resolve"), *line);
                }
            }
            None => add(Severity::Info, "external-ref", format!("$ref {r} is external and not followed"), *line),
        }
    }

    // operationId.
    let mut ids: HashMap<&str, usize> = HashMap::new();
    for op in &spec.ops {
        if op.op_id.is_empty() {
            continue;
        }
        if let Some(first) = ids.get(op.op_id.as_str()) {
            let msg = format!("operationId {:?} is already used (line {})", op.op_id, first + 1);
            add(Severity::Error, "duplicate-operation-id", msg, op.line);
        } else {
            ids.insert(&op.op_id, op.line);
        }
    }

    for op in &spec.ops {
        let label = format!("{} {}", op.method.to_uppercase(), op.path);
        // Path parameters.
        let in_path = path_params(&op.path);
        for name in &in_path {
            if !op.params.iter().any(|p| p.loc == "path" && &p.name == name) {
                add(
                    Severity::Error,
                    "undeclared-path-param",
                    format!("{label}: path parameter {{{name}}} is not declared"),
                    op.line,
                );
            }
        }
        for p in op.params.iter().filter(|p| p.loc == "path") {
            if !in_path.contains(&p.name) {
                add(
                    Severity::Warning,
                    "unused-path-param",
                    format!("{label}: parameter {} is not in the path", p.name),
                    op.line,
                );
            } else if !p.required {
                add(
                    Severity::Warning,
                    "path-param-not-required",
                    format!("{label}: path parameter {} must be required", p.name),
                    op.line,
                );
            }
        }
        // Responses.
        if op.responses.is_empty() {
            add(Severity::Error, "no-responses", format!("{label}: no responses documented"), op.line);
        } else if !op.responses.iter().any(|r| r.code.starts_with('2') || r.code == "default") {
            add(Severity::Warning, "no-success-response", format!("{label}: no 2xx response documented"), op.line);
        }
        // Security.
        for s in &op.security {
            if spec.scheme(s).is_none() {
                add(
                    Severity::Error,
                    "unknown-security-scheme",
                    format!("{label}: security scheme {s} is not defined"),
                    op.line,
                );
            }
        }
    }

    // Schemas without a type.
    let defs =
        if spec.swagger2 { root.get("definitions") } else { root.get("components").and_then(|c| c.get("schemas")) };
    let mut bare = Vec::new();
    for (name, s) in defs.map(|d| d.entries()).unwrap_or(&[]) {
        untyped(deref(root, s), name.clone(), &mut bare, 0);
    }
    for (name, line) in bare {
        add(Severity::Warning, "schema-without-type", format!("schema {name} has no type"), line);
    }
    if spec.servers.is_empty() && !spec.swagger2 {
        add(
            Severity::Info,
            "no-servers",
            "no servers declared: set the base URL by hand to try requests".to_string(),
            0,
        );
    }
    out.sort_by_key(|f| (f.severity as u8, f.line));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::spec::PETSTORE;

    #[test]
    fn the_petstore_is_clean() {
        let s = Spec::parse(PETSTORE).unwrap();
        let f = lint(&s);
        assert_eq!(counts(&f), (0, 0, 0), "{f:?}");
    }

    #[test]
    fn finds_the_usual_mistakes() {
        let bad = "openapi: 3.0.0\ninfo: {title: T, version: '1'}\npaths:\n  /a/{id}:\n    get:\n      operationId: dup\n      security: [{nope: []}]\n      responses:\n        '404': {description: x, content: {application/json: {schema: {$ref: '#/components/schemas/Missing'}}}}\n  /b:\n    post:\n      operationId: dup\n      parameters: [{name: z, in: path, required: true, schema: {type: string}}]\ncomponents:\n  schemas:\n    Loose:\n      properties: {a: {description: nothing}}\n";
        let s = Spec::parse(bad).unwrap();
        let f = lint(&s);
        let codes: Vec<&str> = f.iter().map(|x| x.code).collect();
        for want in [
            "broken-ref",
            "duplicate-operation-id",
            "undeclared-path-param",
            "unused-path-param",
            "no-success-response",
            "no-responses",
            "unknown-security-scheme",
            "schema-without-type",
            "no-servers",
        ] {
            assert!(codes.contains(&want), "{want} missing in {codes:?}");
        }
        let dup = f.iter().find(|x| x.code == "duplicate-operation-id").unwrap();
        assert_eq!(dup.line, 10, "points at the second operation");
        let sorted: Vec<u8> = f.iter().map(|x| x.severity as u8).collect();
        assert!(sorted.windows(2).all(|w| w[0] <= w[1]), "errors first");
    }
}
