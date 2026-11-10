use super::*;

pub(super) fn link_target(url: &str) -> Hit {
    Hit::Link(url.to_string())
}

// ───────────────────────────── App integration ─────────────────────────────

pub(super) fn scope_for(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "" | "ts" | "typescript" | "tsx" | "typescriptreact" => "source.ts",
        "js" | "javascript" | "jsx" | "mjs" | "cjs" => "source.js",
        "json" => "source.json",
        "jsonc" => "source.json.comments",
        "css" => "source.css",
        "html" => "text.html.basic",
        "sh" | "bash" | "shell" | "shellscript" | "zsh" => "source.shell",
        _ => return None,
    })
}

pub(super) fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Byte offset of (line, utf16 column) in `text`.
pub fn byte_off(text: &str, line: usize, col16: usize) -> Option<usize> {
    let mut start = 0usize;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    let mut n = 0;
    let mut off = start;
    for ch in text[start..].chars() {
        if n >= col16 || ch == '\n' {
            break;
        }
        n += ch.len_utf16();
        off += ch.len_utf8();
    }
    Some(off)
}

/// Is this file a TypeScript lib file (`lib.dom.d.ts`...), whose types are too big to inline?
pub(super) fn is_lib_dts(path: &std::path::Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("lib.") && n.ends_with(".d.ts"))
}

/// `@scope/name@1.2.3` for a file inside node_modules.
pub(super) fn package_of(path: &std::path::Path) -> Option<String> {
    let s = path.to_string_lossy();
    let i = s.rfind("/node_modules/")?;
    let rest = &s[i + "/node_modules/".len()..];
    let mut it = rest.split('/');
    let first = it.next()?;
    let name = if first.starts_with('@') { format!("{first}/{}", it.next()?) } else { first.to_string() };
    let pj = std::path::Path::new(&s[..i + "/node_modules/".len()]).join(&name).join("package.json");
    let ver = std::fs::read_to_string(pj)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["version"].as_str().map(str::to_string));
    Some(match ver {
        Some(v) => format!("{name}@{v}"),
        None => name,
    })
}

pub(super) fn hover_markdown(v: &Value) -> String {
    fn one(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Object(o) => {
                let val = o.get("value").and_then(Value::as_str).unwrap_or("");
                match o.get("language").and_then(Value::as_str) {
                    Some(l) => format!("```{l}\n{val}\n```"),
                    None => val.to_string(),
                }
            }
            Value::Array(a) => a.iter().map(one).filter(|s| !s.trim().is_empty()).collect::<Vec<_>>().join("\n\n"),
            _ => String::new(),
        }
    }
    one(&v["contents"]).trim().to_string()
}

/// A literal (string / number / boolean / null) at char column `col` of `line`: (start, end) columns.
pub(super) fn literal_at(line: &str, col: usize) -> Option<(usize, usize)> {
    let cs: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '"' || c == '\'' || c == '`' {
            let s = i;
            i += 1;
            while i < cs.len() && cs[i] != c {
                if cs[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            let e = (i + 1).min(cs.len());
            if (s..e).contains(&col) {
                return Some((s, e));
            }
            i = e;
        } else if c == '/' && cs.get(i + 1) == Some(&'/') {
            return None;
        } else if is_ident(c) {
            let s = i;
            while i < cs.len() && (is_ident(cs[i]) || (cs[i] == '.' && cs[s].is_ascii_digit())) {
                i += 1;
            }
            let word: String = cs[s..i].iter().collect();
            let lit = matches!(word.as_str(), "true" | "false" | "null")
                || word.chars().next().is_some_and(|c| c.is_ascii_digit());
            if lit && (s..i).contains(&col) {
                return Some((s, i));
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Offset of the name in `interface NAME` / `type NAME` / `class NAME` / `enum NAME` in `text`.
pub(super) fn find_type_decl(text: &str, name: &str) -> Option<(usize, Kind)> {
    let re = regex::Regex::new(&format!(r"(?m)^[ \t]*(?:export\s+)?(?:declare\s+)?(?:default\s+)?(?:abstract\s+)?(?:const\s+)?(?:interface|type|class|enum)\s+({})\b", regex::escape(name))).ok()?;
    let m = re.captures(text)?.get(1)?;
    let k = md::decl_kind_at(text, m.start())
        .filter(|k| matches!(k, Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias))?;
    Some((m.start(), k))
}

pub(super) fn describe_literal(text: &str) -> String {
    let t = text.trim();
    match t.chars().next() {
        Some('"' | '\'') => format!("string · {} caracteres", t.chars().count().saturating_sub(2)),
        Some('`') => format!("template · {} caracteres", t.chars().count().saturating_sub(2)),
        Some(c) if c.is_ascii_digit() => {
            if t.ends_with('n') {
                "bigint".to_string()
            } else {
                "number".to_string()
            }
        }
        _ if t == "null" => "null".to_string(),
        _ => "boolean".to_string(),
    }
}
