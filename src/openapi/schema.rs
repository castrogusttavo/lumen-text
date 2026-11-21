//! `$ref` resolution and the schema helpers shared by the viewer, the example generator and the
//! response validator. Everything works on the raw [`Node`] tree: a schema is never copied, a
//! `$ref` is followed on demand (local `#/...` pointers only).

use super::tree::{Node, Val, scalar_text};
use std::collections::HashSet;

/// Longest chain of `$ref`s followed before giving up (cycles).
const MAX_HOPS: usize = 24;

fn unescape(seg: &str) -> String {
    let b = seg.as_bytes();
    let mut raw = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(h) = b.get(i + 1..i + 3)
            && let Ok(h) = std::str::from_utf8(h)
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            raw.push(v);
            i += 3;
            continue;
        }
        raw.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&raw).replace("~1", "/").replace("~0", "~")
}

/// The node a JSON pointer (`/components/schemas/Pet`, without the leading `#`) points at.
pub fn pointer<'a>(root: &'a Node, ptr: &str) -> Option<&'a Node> {
    let mut cur = root;
    for seg in ptr.split('/').skip(1) {
        let seg = unescape(seg);
        cur = match &cur.val {
            Val::Map(_) => cur.get(&seg)?,
            Val::Arr(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Follow `$ref` chains. A broken or external reference returns the node holding it.
pub fn deref<'a>(root: &'a Node, n: &'a Node) -> &'a Node {
    let mut cur = n;
    for _ in 0..MAX_HOPS {
        let Some(r) = cur.gets("$ref") else { return cur };
        let Some(p) = r.strip_prefix('#') else { return cur };
        match pointer(root, p) {
            Some(t) => cur = t,
            None => return cur,
        }
    }
    cur
}

/// Last segment of a `$ref` (`#/components/schemas/Pet` -> `Pet`).
pub fn ref_name(r: &str) -> &str {
    r.rsplit('/').next().unwrap_or(r)
}

/// The `type` of a schema (the first non-null entry of a 3.1 type list).
pub fn type_of(s: &Node) -> String {
    match s.get("type") {
        Some(t) => match &t.val {
            Val::Str(x) => x.clone(),
            Val::Arr(a) => a.iter().filter_map(|n| n.str()).find(|x| *x != "null").unwrap_or("").to_string(),
            _ => String::new(),
        },
        None => String::new(),
    }
}

pub fn is_nullable(s: &Node) -> bool {
    s.get("nullable").and_then(|n| n.bool()).unwrap_or(false)
        || s.get("type").is_some_and(|t| t.arr().iter().any(|n| n.str() == Some("null")))
}

/// Short type name for display: `string`, `array<Pet>`, `Pet`, `oneOf`...
pub fn type_label(root: &Node, s: &Node, depth: usize) -> String {
    let name = s.gets("$ref").map(|r| ref_name(r).to_string());
    let d = deref(root, s);
    let t = type_of(d);
    let base = if t == "array" {
        match d.get("items") {
            Some(it) if depth < 4 => format!("array<{}>", type_label(root, it, depth + 1)),
            _ => "array".to_string(),
        }
    } else if !t.is_empty() {
        t
    } else if d.get("properties").is_some() || d.get("allOf").is_some() {
        "object".to_string()
    } else if d.get("oneOf").is_some() {
        "oneOf".to_string()
    } else if d.get("anyOf").is_some() {
        "anyOf".to_string()
    } else if d.get("enum").is_some() {
        "enum".to_string()
    } else {
        "any".to_string()
    };
    match name {
        Some(n) if base == "object" || base == "any" => n,
        _ => base,
    }
}

/// Properties of an object schema, merging `allOf` members: (name, schema, required).
pub fn properties<'a>(root: &'a Node, s: &'a Node) -> Vec<(String, &'a Node, bool)> {
    let mut props: Vec<(String, &'a Node)> = Vec::new();
    let mut req: HashSet<String> = HashSet::new();
    collect(root, s, &mut props, &mut req, 0);
    props
        .into_iter()
        .map(|(k, v)| {
            let r = req.contains(&k);
            (k, v, r)
        })
        .collect()
}

fn collect<'a>(
    root: &'a Node,
    s: &'a Node,
    props: &mut Vec<(String, &'a Node)>,
    req: &mut HashSet<String>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    let s = deref(root, s);
    if let Some(p) = s.get("properties") {
        for (k, v) in p.entries() {
            if let Some(slot) = props.iter_mut().find(|(n, _)| n == k) {
                slot.1 = v;
            } else {
                props.push((k.clone(), v));
            }
        }
    }
    for r in s.get("required").map(|n| n.arr()).unwrap_or(&[]) {
        if let Some(n) = r.str() {
            req.insert(n.to_string());
        }
    }
    for a in s.get("allOf").map(|n| n.arr()).unwrap_or(&[]) {
        collect(root, a, props, req, depth + 1);
    }
}

/// One line of the schema tree.
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub depth: usize,
    pub name: String,
    pub ty: String,
    pub required: bool,
    pub nullable: bool,
    pub desc: String,
    pub enums: Vec<String>,
    pub format: String,
    /// Constraints and defaults: `min 1`, `default "x"`...
    pub notes: Vec<String>,
    /// Identity for the expand / collapse state.
    pub path: String,
    pub expandable: bool,
    pub open: bool,
    pub circular: bool,
}

/// Expanded by default: the children of the root and the first nested level.
fn default_open(depth: usize) -> bool {
    depth < 1
}

/// Flatten `schema` into rows. `toggled` holds the paths whose state differs from the default.
pub fn rows(root: &Node, schema: &Node, toggled: &HashSet<String>) -> Vec<Row> {
    let mut out = Vec::new();
    let mut stack = Vec::new();
    let top = deref(root, schema);
    let kids = children(root, top);
    if kids.is_empty() {
        out.push(describe(root, schema, 0, "(value)".into(), String::new(), false));
        return out;
    }
    if let Some(r) = schema.gets("$ref") {
        stack.push(r.to_string());
    }
    for (name, sch, req) in kids {
        walk(root, sch, 0, name, String::new(), req, toggled, &mut out, &mut stack);
    }
    out
}

fn describe(root: &Node, schema: &Node, depth: usize, name: String, path: String, required: bool) -> Row {
    let d = deref(root, schema);
    let mut notes = Vec::new();
    for (k, label) in [
        ("minimum", "min"),
        ("maximum", "max"),
        ("minLength", "min length"),
        ("maxLength", "max length"),
        ("minItems", "min items"),
        ("maxItems", "max items"),
        ("pattern", "pattern"),
        ("default", "default"),
        ("example", "example"),
    ] {
        if let Some(v) = d.get(k) {
            notes.push(format!("{label} {}", scalar_text(v)));
        }
    }
    let desc = d.gets("description").or_else(|| schema.gets("description")).unwrap_or("");
    let first = desc.trim().lines().next().unwrap_or("").to_string();
    Row {
        depth,
        name,
        ty: type_label(root, schema, 0),
        required,
        nullable: is_nullable(d),
        desc: first,
        enums: d.get("enum").map(|e| e.arr().iter().map(scalar_text).collect()).unwrap_or_default(),
        format: d.gets("format").unwrap_or("").to_string(),
        notes,
        path,
        ..Default::default()
    }
}

/// Child schemas of an (already dereferenced) schema: (label, schema, required).
fn children<'a>(root: &'a Node, s: &'a Node) -> Vec<(String, &'a Node, bool)> {
    let mut out = properties(root, s);
    if type_of(s) == "array"
        && let Some(it) = s.get("items")
    {
        out.push(("[items]".to_string(), it, false));
    }
    if let Some(ap) = s.get("additionalProperties")
        && ap.is_map()
    {
        out.push(("[key]".to_string(), ap, false));
    }
    for k in ["oneOf", "anyOf"] {
        for (i, a) in s.get(k).map(|n| n.arr()).unwrap_or(&[]).iter().enumerate() {
            out.push((format!("{k} #{}", i + 1), a, false));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn walk(
    root: &Node,
    sch: &Node,
    depth: usize,
    name: String,
    parent: String,
    required: bool,
    toggled: &HashSet<String>,
    out: &mut Vec<Row>,
    stack: &mut Vec<String>,
) {
    let path = format!("{parent}/{name}");
    let mut row = describe(root, sch, depth, name, path.clone(), required);
    let r = sch.gets("$ref").map(|s| s.to_string());
    if let Some(r) = &r
        && stack.contains(r)
    {
        row.circular = true;
        row.ty = format!("{} (circular)", ref_name(r));
        out.push(row);
        return;
    }
    if depth >= 12 {
        out.push(row);
        return;
    }
    let d = deref(root, sch);
    let kids = children(root, d);
    row.expandable = !kids.is_empty();
    row.open = row.expandable && (default_open(depth) != toggled.contains(&path));
    let open = row.open;
    out.push(row);
    if !open {
        return;
    }
    if let Some(r) = &r {
        stack.push(r.clone());
    }
    for (n, c, req) in kids {
        walk(root, c, depth + 1, n, path.clone(), req, toggled, out, stack);
    }
    if r.is_some() {
        stack.pop();
    }
}

/// Names of the properties an object schema declares (allOf merged).
pub fn property_names(root: &Node, s: &Node) -> HashSet<String> {
    properties(root, s).into_iter().map(|(k, _, _)| k).collect()
}

#[cfg(test)]
mod tests {
    use super::super::tree::parse;
    use super::*;

    const DOC: &str = r##"{"components":{"schemas":{
      "Pet":{"type":"object","required":["id"],"properties":{"id":{"type":"integer","format":"int64"},"tag":{"type":"string","enum":["a","b"],"description":"A tag.\nmore"},"owner":{"$ref":"#/components/schemas/Owner"}}},
      "Owner":{"type":"object","properties":{"pets":{"type":"array","items":{"$ref":"#/components/schemas/Pet"}}}},
      "Dog":{"allOf":[{"$ref":"#/components/schemas/Pet"},{"type":"object","required":["bark"],"properties":{"bark":{"type":"boolean"}}}]}
    }}}"##;

    #[test]
    fn refs_resolve_and_names_are_short() {
        let r = parse(DOC).unwrap();
        let n = Node::new(Val::Map(vec![("$ref".into(), Node::s("#/components/schemas/Pet"))]), 0);
        assert_eq!(type_of(deref(&r, &n)), "object");
        assert_eq!(type_label(&r, &n, 0), "Pet");
        let broken = Node::new(Val::Map(vec![("$ref".into(), Node::s("#/nope"))]), 0);
        assert!(deref(&r, &broken).get("$ref").is_some());
    }

    #[test]
    fn all_of_merges_properties_and_required() {
        let r = parse(DOC).unwrap();
        let dog = pointer(&r, "/components/schemas/Dog").unwrap();
        let p = properties(&r, dog);
        let names: Vec<&str> = p.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(names, ["id", "tag", "owner", "bark"]);
        assert!(p[0].2 && p[3].2 && !p[1].2);
    }

    #[test]
    fn rows_expand_and_stop_on_cycles() {
        let r = parse(DOC).unwrap();
        let pet = Node::new(Val::Map(vec![("$ref".into(), Node::s("#/components/schemas/Pet"))]), 0);
        let rows0 = rows(&r, &pet, &HashSet::new());
        assert_eq!(rows0[0].name, "id");
        assert!(rows0[0].required);
        assert_eq!(rows0[1].desc, "A tag.");
        assert_eq!(rows0[1].enums, ["a", "b"]);
        let owner = rows0.iter().position(|x| x.name == "owner").unwrap();
        assert!(rows0[owner].expandable && rows0[owner].open);
        // owner > pets > [items] is Pet again: circular, never expanded.
        let mut t = HashSet::new();
        t.insert("/owner/pets".to_string());
        let deep = rows(&r, &pet, &t);
        let circ = deep.iter().find(|x| x.circular).expect("circular row");
        assert!(circ.ty.contains("circular"));
        t.clear();
        t.insert(rows0[owner].path.clone());
        let closed = rows(&r, &pet, &t);
        assert!(closed.len() < rows0.len());
    }
}
