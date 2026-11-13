//! The unified "Go to" picker, pure parts: the prefix that picks a mode and the merge/ranking of
//! `workspace/symbol` answers from several language servers (see `lspmerge` for the other merges).
//!
//! * plain text: files (quick open)
//! * `@query`: symbols of the current file
//! * `#query`: symbols of the whole workspace, asked of every running server that offers them
//! * `:line[:column]`: a line of the current file

use crate::fuzzy::fuzzy;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Files,
    DocSymbols,
    WorkspaceSymbols,
    Line,
}

/// The mode a typed text selects and the query that follows the prefix.
pub fn parse(text: &str) -> (Mode, &str) {
    let t = text.trim_start();
    match t.chars().next() {
        Some('@') => (Mode::DocSymbols, &t[1..]),
        Some('#') => (Mode::WorkspaceSymbols, &t[1..]),
        Some(':') => (Mode::Line, &t[1..]),
        _ => (Mode::Files, text),
    }
}

/// `12` or `12:5` (1-based) as typed after `:`; `None` while it is empty or not a number.
pub fn parse_line(rest: &str) -> Option<(usize, Option<usize>)> {
    let mut it = rest.trim().split(':');
    let line = it.next()?.trim().parse::<usize>().ok().filter(|&l| l >= 1)?;
    let col = it.next().and_then(|c| c.trim().parse::<usize>().ok()).filter(|&c| c >= 1);
    Some((line, col))
}

/// Short name of an LSP `SymbolKind`.
pub fn kind_name(k: u32) -> &'static str {
    match k {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type parameter",
        _ => "",
    }
}

/// One symbol of a `workspace/symbol` answer (line and column as the server sent them: 0-based, UTF-16).
#[derive(Clone, Debug, PartialEq)]
pub struct WsSym {
    pub name: String,
    pub container: String,
    pub kind: u32,
    pub uri: String,
    pub line: usize,
    pub col: usize,
    pub score: i32,
}

/// How well `name` answers `query` (higher is better): fuzzy score plus bonuses for an exact or
/// prefix match; a symbol that only matches through its container is ranked lower, one the
/// server returned that does not match at all comes last.
pub fn rank_symbol(query: &str, name: &str, container: &str) -> i32 {
    if query.is_empty() {
        return 0;
    }
    let mut s = match fuzzy(query, name) {
        Some((s, _)) => s,
        None => match fuzzy(query, &format!("{container}.{name}")) {
            Some((s, _)) => s - 30,
            None => -1000,
        },
    };
    let (n, q) = (name.to_lowercase(), query.to_lowercase());
    if n == q {
        s += 200;
    } else if n.starts_with(&q) {
        s += 80;
    } else if n.contains(&q) {
        s += 30;
    }
    s - (name.chars().count() as i32) / 4
}

/// Merge the answers (`order` 0 is the primary) into one list: duplicates (same name and place)
/// are shown once, the first server winning; the rest is ranked by [`rank_symbol`], ties keep
/// server order.
pub fn merge_workspace_symbols(parts: &[(usize, String, Value)], query: &str, limit: usize) -> Vec<WsSym> {
    let mut sorted: Vec<&(usize, String, Value)> = parts.iter().collect();
    sorted.sort_by_key(|p| p.0);
    let mut seen: HashSet<(String, usize, usize, String)> = HashSet::new();
    let mut out: Vec<WsSym> = Vec::new();
    for (_, _, v) in sorted {
        for s in v.as_array().into_iter().flatten() {
            let name = s["name"].as_str().unwrap_or("");
            let loc = &s["location"];
            let Some(uri) = loc["uri"].as_str() else { continue };
            if name.is_empty() {
                continue;
            }
            // A WorkspaceSymbol may come without a range (resolved lazily): the file's start.
            let start = &loc["range"]["start"];
            let line = start["line"].as_u64().unwrap_or(0) as usize;
            let col = start["character"].as_u64().unwrap_or(0) as usize;
            if !seen.insert((uri.to_string(), line, col, name.to_string())) {
                continue;
            }
            let container = s["containerName"].as_str().unwrap_or("").to_string();
            out.push(WsSym {
                score: rank_symbol(query, name, &container),
                name: name.to_string(),
                container,
                kind: s["kind"].as_u64().unwrap_or(0) as u32,
                uri: uri.to_string(),
                line,
                col,
            });
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.score)); // stable: server order breaks ties
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sym(name: &str, uri: &str, line: u64, container: &str) -> Value {
        json!({"name": name, "kind": 12, "containerName": container,
               "location": {"uri": uri, "range": {"start": {"line": line, "character": 2}, "end": {"line": line, "character": 5}}}})
    }

    fn part(order: usize, v: Value) -> (usize, String, Value) {
        (order, format!("s{order}"), v)
    }

    #[test]
    fn prefixes_select_the_mode() {
        assert_eq!(parse("main"), (Mode::Files, "main"));
        assert_eq!(parse(""), (Mode::Files, ""));
        assert_eq!(parse("@open"), (Mode::DocSymbols, "open"));
        assert_eq!(parse("#Pick"), (Mode::WorkspaceSymbols, "Pick"));
        assert_eq!(parse(":12:3"), (Mode::Line, "12:3"));
        assert_eq!(parse("  #x"), (Mode::WorkspaceSymbols, "x"));
        assert_eq!(parse("a#b"), (Mode::Files, "a#b"));
    }

    #[test]
    fn line_and_column_are_parsed() {
        assert_eq!(parse_line("12"), Some((12, None)));
        assert_eq!(parse_line(" 12 : 5 "), Some((12, Some(5))));
        assert_eq!(parse_line("12:"), Some((12, None)));
        assert_eq!(parse_line("0"), None);
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("abc"), None);
        assert_eq!(parse_line("-3"), None);
    }

    #[test]
    fn kinds_have_names() {
        assert_eq!(kind_name(12), "function");
        assert_eq!(kind_name(22), "enum member");
        assert_eq!(kind_name(999), "");
    }

    #[test]
    fn ranking_prefers_exact_then_prefix_then_contained() {
        let exact = rank_symbol("open", "open", "");
        let prefix = rank_symbol("open", "open_file", "");
        let inside = rank_symbol("open", "reopen_all", "");
        assert!(exact > prefix && prefix > inside, "{exact} {prefix} {inside}");
        assert!(rank_symbol("zzz", "open", "") < -500);
        assert_eq!(rank_symbol("", "anything", ""), 0);
        // matching only through the container ranks below a direct match
        assert!(rank_symbol("app", "run", "App") < rank_symbol("app", "app", ""));
    }

    #[test]
    fn merge_dedupes_ranks_and_keeps_server_order_on_ties() {
        let a = json!([sym("openFile", "file:///a.ts", 3, "Ui"), sym("reopen", "file:///b.ts", 1, "")]);
        let b = json!([
            sym("openFile", "file:///a.ts", 3, "Ui"), // same place and name: dropped
            sym("open", "file:///c.rs", 9, "")
        ]);
        let m = merge_workspace_symbols(&[part(1, b), part(0, a)], "open", 10);
        let names: Vec<&str> = m.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["open", "openFile", "reopen"]);
        assert_eq!(m[1].container, "Ui");
        assert_eq!((m[0].line, m[0].col), (9, 2));
        assert_eq!(m[0].kind, 12);
    }

    #[test]
    fn merge_handles_missing_ranges_failures_and_limit() {
        let lazy = json!([
            {"name": "Thing", "kind": 5, "location": {"uri": "file:///t.rs"}},
            {"name": "", "location": {"uri": "x"}},
            {"name": "NoLoc"}
        ]);
        let m = merge_workspace_symbols(&[part(0, lazy), part(1, Value::Null), part(2, json!({"oops": 1}))], "", 10);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].line, m[0].col), (0, 0));
        let many = Value::Array((0..50).map(|i| sym(&format!("s{i}"), "file:///m.rs", i, "")).collect());
        assert_eq!(merge_workspace_symbols(&[part(0, many)], "s", 7).len(), 7);
    }
}
