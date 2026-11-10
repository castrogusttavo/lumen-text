use super::*;

/// Flatten the two shapes of `textDocument/documentSymbol` results, in document order.
pub fn flatten_symbols(v: &serde_json::Value) -> Vec<Sym> {
    fn walk(v: &serde_json::Value, depth: usize, out: &mut Vec<Sym>) {
        for s in v.as_array().into_iter().flatten() {
            let range = s.get("range").or_else(|| s["location"].get("range"));
            let sel = s.get("selectionRange").or(range);
            let (Some(name), Some(r)) = (s["name"].as_str(), range) else { continue };
            let g = |v: &serde_json::Value, k: &str, k2: &str| v[k][k2].as_u64().unwrap_or(0) as usize;
            out.push(Sym {
                name: name.to_string(),
                kind: s["kind"].as_u64().unwrap_or(0) as u32,
                depth,
                sl: g(r, "start", "line"),
                el: g(r, "end", "line"),
                sel_l: sel.map_or(0, |x| g(x, "start", "line")),
                sel_c: sel.map_or(0, |x| g(x, "start", "character")),
            });
            if let Some(ch) = s.get("children") {
                walk(ch, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(v, 0, &mut out);
    out
}

/// Semantic token classes we recolor (the rest keeps its TextMate color).
pub fn sem_class(name: &str) -> u8 {
    match name {
        "namespace" => 1,
        "class" => 2,
        "enum" => 3,
        "interface" => 4,
        "struct" => 5,
        "typeParameter" => 6,
        "type" => 7,
        "parameter" => 8,
        "variable" => 9,
        "property" => 10,
        "enumMember" => 11,
        "function" => 12,
        "method" => 13,
        "decorator" => 14,
        _ => 0,
    }
}

pub fn sem_scope(class: u8, readonly: bool) -> Option<&'static str> {
    Some(match class {
        1 => "entity.name.namespace",
        2 => "entity.name.type.class",
        3 => "entity.name.type.enum",
        4 => "entity.name.type.interface",
        5 => "entity.name.type.struct",
        6 | 7 => "entity.name.type",
        8 => "variable.parameter",
        9 if readonly => "variable.other.constant",
        10 => "variable.other.property",
        11 => "variable.other.enummember",
        12 | 13 => "entity.name.function",
        14 => "entity.name.function.decorator",
        _ => return None,
    })
}

/// Decode LSP `semanticTokens` (relative encoding) into per-line (utf16 start, utf16 len, class, readonly).
pub fn decode_semantic(
    data: &[u32],
    types: &[String],
    mods: &[String],
) -> std::collections::HashMap<usize, Vec<(usize, usize, u8, bool)>> {
    let ro_bit = mods.iter().position(|m| m == "readonly");
    let classes: Vec<u8> = types.iter().map(|t| sem_class(t)).collect();
    let mut out: std::collections::HashMap<usize, Vec<(usize, usize, u8, bool)>> = std::collections::HashMap::new();
    let (mut line, mut start) = (0usize, 0usize);
    for t in data.as_chunks::<5>().0 {
        let (dl, ds, len, ty, m) = (t[0] as usize, t[1] as usize, t[2] as usize, t[3] as usize, t[4]);
        if dl > 0 {
            line += dl;
            start = ds;
        } else {
            start += ds;
        }
        let class = classes.get(ty).copied().unwrap_or(0);
        if class != 0 {
            let ro = ro_bit.is_some_and(|b| m & (1 << b) != 0);
            out.entry(line).or_default().push((start, len, class, ro));
        }
    }
    out
}

/// Recolor parts of `spans` (byte ranges of `line`) with semantic token colors given as UTF-16 ranges.
pub fn overlay_colors(
    spans: &[(usize, usize, Rgba, u8)],
    line: &str,
    toks: &[(usize, usize, Rgba)],
) -> Vec<(usize, usize, Rgba, u8)> {
    // UTF-16 offset -> byte offset
    let mut map: Vec<usize> = Vec::with_capacity(line.len() + 1);
    for (b, ch) in line.char_indices() {
        for _ in 0..ch.len_utf16() {
            map.push(b);
        }
    }
    map.push(line.len());
    let at = |u: usize| map.get(u).copied().unwrap_or(line.len());
    let mut bt: Vec<(usize, usize, Rgba)> =
        toks.iter().map(|(s, l, c)| (at(*s), at(s + l), *c)).filter(|(a, b, _)| a < b).collect();
    bt.sort_by_key(|t| t.0);
    let mut out = Vec::with_capacity(spans.len() + bt.len() * 2);
    for &(s, e, c, f) in spans {
        let mut cur = s;
        for &(ts, te, tc) in &bt {
            if te <= cur || ts >= e {
                continue;
            }
            let (a, b) = (ts.max(cur), te.min(e));
            if a > cur {
                out.push((cur, a, c, f));
            }
            out.push((a, b, tc, f));
            cur = b;
        }
        if cur < e {
            out.push((cur, e, c, f));
        }
    }
    out
}
