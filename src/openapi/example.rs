//! Example values generated from a schema (`example` > `examples` > `default` > `enum` > by type).

use super::schema::{deref, properties, type_of};
use super::tree::{Node, Val, to_json};

const MAX_DEPTH: usize = 7;

/// An example value for `schema`.
pub fn generate(root: &Node, schema: &Node) -> Node {
    gen_at(root, schema, 0, &mut Vec::new())
}

/// [`generate`] as pretty JSON text.
pub fn example_json(root: &Node, schema: &Node) -> String {
    to_json(&generate(root, schema), true)
}

fn n(v: Val) -> Node {
    Node::new(v, 0)
}

pub fn sample_string(format: &str) -> &'static str {
    match format {
        "date-time" => "2024-01-01T00:00:00Z",
        "date" => "2024-01-01",
        "time" => "12:00:00",
        "email" => "user@example.com",
        "uuid" => "3fa85f64-5717-4562-b3fc-2c963f66afa6",
        "uri" | "url" => "https://example.com",
        "hostname" => "example.com",
        "ipv4" => "192.0.2.1",
        "ipv6" => "2001:db8::1",
        "byte" => "SGVsbG8=",
        "password" => "********",
        _ => "string",
    }
}

fn gen_at(root: &Node, schema: &Node, depth: usize, stack: &mut Vec<String>) -> Node {
    let ref_key = schema.gets("$ref").map(|s| s.to_string());
    if let Some(r) = &ref_key {
        if stack.contains(r) || depth > MAX_DEPTH {
            return n(Val::Null);
        }
        stack.push(r.clone());
    }
    let out = gen_inner(root, deref(root, schema), depth, stack);
    if ref_key.is_some() {
        stack.pop();
    }
    out
}

fn gen_inner(root: &Node, s: &Node, depth: usize, stack: &mut Vec<String>) -> Node {
    if depth > MAX_DEPTH {
        return n(Val::Null);
    }
    if let Some(e) = s.get("example") {
        return e.clone();
    }
    if let Some(e) = s.get("examples").and_then(|e| e.arr().first()) {
        return e.clone();
    }
    if let Some(d) = s.get("default") {
        return d.clone();
    }
    if let Some(e) = s.get("const") {
        return e.clone();
    }
    if let Some(e) = s.get("enum").and_then(|e| e.arr().first()) {
        return e.clone();
    }
    for k in ["oneOf", "anyOf"] {
        if let Some(first) = s.get(k).and_then(|a| a.arr().first()) {
            return gen_at(root, first, depth + 1, stack);
        }
    }
    let t = type_of(s);
    let has_props = s.get("properties").is_some() || s.get("allOf").is_some();
    if t == "object" || (t.is_empty() && has_props) {
        let mut out: Vec<(String, Node)> = Vec::new();
        for (k, v, _) in properties(root, s) {
            let val = gen_at(root, v, depth + 1, stack);
            out.push((k, val));
        }
        if out.is_empty()
            && let Some(ap) = s.get("additionalProperties")
            && ap.is_map()
        {
            out.push(("key".to_string(), gen_at(root, ap, depth + 1, stack)));
        }
        return n(Val::Map(out));
    }
    match t.as_str() {
        "array" => {
            let item = match s.get("items") {
                Some(it) => gen_at(root, it, depth + 1, stack),
                None => n(Val::Null),
            };
            n(Val::Arr(vec![item]))
        }
        "string" => {
            let fmt = s.gets("format").unwrap_or("");
            n(Val::Str(sample_string(fmt).to_string()))
        }
        "integer" => n(Val::Num(s.get("minimum").and_then(|m| m.num()).unwrap_or(0.0).ceil())),
        "number" => n(Val::Num(s.get("minimum").and_then(|m| m.num()).unwrap_or(0.0))),
        "boolean" => n(Val::Bool(true)),
        _ => n(Val::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::super::spec::{PETSTORE, Spec};
    use super::super::tree::parse;
    use super::*;

    #[test]
    fn objects_arrays_enums_and_allof() {
        let s = Spec::parse(PETSTORE).unwrap();
        let ok = s.ops[1].responses[0].media[0].schema.as_ref().unwrap();
        assert_eq!(example_json(&s.root, ok), "{\n  \"name\": \"string\",\n  \"tag\": \"cat\",\n  \"id\": 0\n}");
        let list = s.ops[0].responses[0].media[0].schema.as_ref().unwrap();
        assert!(example_json(&s.root, list).starts_with("[\n  {"));
    }

    #[test]
    fn explicit_examples_and_formats_win() {
        let r = parse("{}").unwrap();
        let sch = parse("{\"type\":\"object\",\"properties\":{\"a\":{\"type\":\"string\",\"format\":\"uuid\"},\"b\":{\"type\":\"integer\",\"example\":7},\"c\":{\"type\":\"string\",\"default\":\"d\"}}}").unwrap();
        assert_eq!(
            to_json(&generate(&r, &sch), false),
            "{\"a\":\"3fa85f64-5717-4562-b3fc-2c963f66afa6\",\"b\":7,\"c\":\"d\"}"
        );
    }

    #[test]
    fn recursive_schemas_terminate() {
        let r = parse("{\"components\":{\"schemas\":{\"T\":{\"type\":\"object\",\"properties\":{\"kids\":{\"type\":\"array\",\"items\":{\"$ref\":\"#/components/schemas/T\"}}}}}}}").unwrap();
        let t = parse("{\"$ref\":\"#/components/schemas/T\"}").unwrap();
        assert_eq!(to_json(&generate(&r, &t), false), "{\"kids\":[null]}");
    }
}
