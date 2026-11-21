//! SQLite viewer, pure model: the SQL guard, the statements the viewer sends to the system
//! `sqlite3` CLI (`-readonly -json`), the parser of its JSON output and the pagination math.
//! No I/O besides [`run`]; drawing and input live in `app_sqlview`.

use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::{fmt, path::Path, process::Command};

/// Rows shown per page.
pub const PAGE: usize = 100;
/// Longest cell text kept for display (chars).
const CELL_MAX: usize = 200;

/// One table or view of the database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub name: String,
    pub view: bool,
    /// Row count; `None` until the counting query came back.
    pub rows: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Null,
    Num,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub kind: CellKind,
}

/// A page of a result: the column names (in the order of the SELECT) and the rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rows {
    pub cols: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    /// More rows exist after this page.
    pub more: bool,
}

/// `"name"` with the quotes inside doubled: safe to splice as an SQL identifier.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A piece of a statement, comments dropped.
#[derive(Debug, PartialEq)]
enum Tok {
    /// A bare word (lower-cased): keyword, name or number.
    Word(String),
    /// A `"name"`, `` `name` `` or `[name]` (lower-cased, quotes off).
    Quoted(String),
    /// A `'string'`.
    Str,
    Punct(char),
}

/// The tokens of `sql` with the byte offset each starts at. Strings, quoted names and comments
/// (`-- ...`, `/* ... */`) are opaque, so a `;` or a keyword inside them means nothing.
fn tokens(sql: &str) -> Vec<(Tok, usize)> {
    let b: Vec<(usize, char)> = sql.char_indices().collect();
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let (at, c) = b[i];
        let next = b.get(i + 1).map(|x| x.1);
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && next == Some('-') {
            while i < b.len() && b[i].1 != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            i += 2;
            while i < b.len() && !(b[i].1 == '*' && b.get(i + 1).map(|x| x.1) == Some('/')) {
                i += 1;
            }
            i += 2;
        } else if matches!(c, '\'' | '"' | '`' | '[') {
            let close = if c == '[' { ']' } else { c };
            let mut j = i + 1;
            let mut name = String::new();
            while j < b.len() && b[j].1 != close {
                name.push(b[j].1);
                j += 1;
            }
            i = j + 1;
            out.push((if c == '\'' { Tok::Str } else { Tok::Quoted(name.to_lowercase()) }, at));
        } else if word(c) {
            let mut w = String::new();
            while i < b.len() && word(b[i].1) {
                w.push(b[i].1);
                i += 1;
            }
            out.push((Tok::Word(w.to_lowercase()), at));
        } else {
            out.push((Tok::Punct(c), at));
            i += 1;
        }
    }
    out
}

/// Functions of the `sqlite3` shell that reach outside the database (write a file, run native
/// code): `-readonly` does not stop them, so the guard does.
const BLOCKED_FNS: [&str; 3] = ["writefile", "edit", "load_extension"];

/// PRAGMAs that take an argument and only read.
const READ_PRAGMAS: [&str; 12] = [
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "foreign_key_check",
    "integrity_check",
    "quick_check",
    "collation_list",
    "function_list",
];

/// The statement to run for what the user typed, or why it is refused. Only one read-only
/// statement passes: `SELECT`, `VALUES`, `WITH ... SELECT`, `PRAGMA` (without `=`: no setting) or
/// `EXPLAIN`. A trailing `;` and comments are fine. The CLI runs with `-readonly` as well: two
/// locks.
pub fn guard(input: &str) -> Result<String, String> {
    let input = input.trim();
    let toks = tokens(input);
    let semi = toks.iter().position(|(t, _)| *t == Tok::Punct(';'));
    let (stmt, rest) = match semi {
        Some(i) => (&toks[..i], &toks[i + 1..]),
        None => (&toks[..], &toks[..0]),
    };
    if stmt.is_empty() {
        return Err("Type a SELECT query".into());
    }
    if rest.iter().any(|(t, _)| *t != Tok::Punct(';')) {
        return Err("One statement at a time".into());
    }
    let sql = match semi {
        Some(i) => input[..toks[i].1].trim(),
        None => input,
    };
    if sql.contains(['\n', '\r', '\0']) {
        return Err("One line only".into());
    }
    let words: Vec<&Tok> = stmt.iter().map(|(t, _)| t).collect();
    // A function of the shell that touches files or loads code.
    for (k, t) in words.iter().enumerate() {
        if let Tok::Word(n) | Tok::Quoted(n) = t
            && BLOCKED_FNS.contains(&n.as_str())
            && words.get(k + 1) == Some(&&Tok::Punct('('))
        {
            return Err(format!("{n}() is not allowed in a read-only viewer"));
        }
    }
    let Tok::Word(first) = words[0] else {
        return Err("Read-only viewer: only SELECT, PRAGMA and EXPLAIN run".into());
    };
    match first.as_str() {
        "select" | "explain" | "values" => Ok(sql.to_string()),
        // SQLite allows `WITH ... DELETE` / `INSERT` / `UPDATE`: let a WITH through only without them.
        "with" => {
            let writes = words.iter().enumerate().any(|(k, t)| match t {
                Tok::Word(w) => match w.as_str() {
                    "delete" | "insert" | "update" => true,
                    "replace" => words.get(k + 1) != Some(&&Tok::Punct('(')),
                    _ => false,
                },
                _ => false,
            });
            if writes { Err("Read-only viewer: that WITH writes".into()) } else { Ok(sql.to_string()) }
        }
        "pragma" => {
            if words.iter().any(|t| **t == Tok::Punct('=')) {
                return Err("PRAGMA can only read here".into());
            }
            // `PRAGMA name(value)` sets as well; only the reading ones take an argument.
            if let Some(p) = words.iter().position(|t| **t == Tok::Punct('(')) {
                let ok = p > 0
                    && matches!(words[p - 1], Tok::Word(n) | Tok::Quoted(n) if READ_PRAGMAS.contains(&n.as_str()));
                if !ok {
                    return Err("PRAGMA can only read here".into());
                }
            }
            Ok(sql.to_string())
        }
        _ => Err("Read-only viewer: only SELECT, PRAGMA and EXPLAIN run".into()),
    }
}

/// True when `sql` can be wrapped as a sub-select (so LIMIT / OFFSET apply to it).
pub fn wrappable(sql: &str) -> bool {
    matches!(tokens(sql).first(), Some((Tok::Word(w), _)) if matches!(w.as_str(), "select" | "with" | "values"))
}

/// `EXPLAIN QUERY PLAN ...`: the CLI prints a tree for it, not JSON.
pub fn is_plan(sql: &str) -> bool {
    let t = tokens(sql);
    let w = |i: usize, s: &str| matches!(t.get(i), Some((Tok::Word(x), _)) if x == s);
    w(0, "explain") && w(1, "query") && w(2, "plan")
}

/// The statement for page `offset` of a guarded query. A `SELECT` is wrapped with LIMIT/OFFSET
/// (one extra row tells whether a next page exists); PRAGMA / EXPLAIN run as they are and are
/// cut on this side ([`parse_rows`] keeps `skip` / `PAGE` of them). The line break before the
/// closing parenthesis keeps a trailing `-- comment` from swallowing it.
pub fn page_sql(sql: &str, offset: usize) -> String {
    if wrappable(sql) { format!("SELECT * FROM ({sql}\n) LIMIT {} OFFSET {offset}", PAGE + 1) } else { sql.to_string() }
}

/// The statement showing page `offset` of a table or view.
pub fn table_sql(name: &str, offset: usize) -> String {
    format!("SELECT * FROM {} LIMIT {} OFFSET {offset}", quote_ident(name), PAGE + 1)
}

/// The statement listing the tables and views (system tables left out).
pub const LIST_SQL: &str = "SELECT name, type FROM sqlite_master WHERE type IN ('table','view') \
     AND name NOT LIKE 'sqlite_%' ORDER BY type DESC, name COLLATE NOCASE";

/// One statement counting the rows of every table (`n`, `c`), or `None` without tables.
pub fn counts_sql(tables: &[Table]) -> Option<String> {
    if tables.is_empty() {
        return None;
    }
    let parts: Vec<String> = tables
        .iter()
        .map(|t| format!("SELECT '{}' AS n, COUNT(*) AS c FROM {}", t.name.replace('\'', "''"), quote_ident(&t.name)))
        .collect();
    Some(parts.join(" UNION ALL "))
}

/// A row as the CLI prints it: the pairs in the order of the SELECT (a plain `Map` would sort them).
struct Row(Vec<(String, Value)>);

impl<'de> serde::Deserialize<'de> for Row {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Row;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Row, A::Error> {
                let mut v = Vec::new();
                while let Some(kv) = m.next_entry::<String, Value>()? {
                    v.push(kv);
                }
                Ok(Row(v))
            }
        }
        d.deserialize_map(V)
    }
}

/// The array of rows, read one by one.
struct Table0(Vec<Row>);

impl<'de> serde::Deserialize<'de> for Table0 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Table0;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an array of rows")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Table0, A::Error> {
                let mut v = Vec::new();
                while let Some(r) = s.next_element::<Row>()? {
                    v.push(r);
                }
                Ok(Table0(v))
            }
        }
        d.deserialize_seq(V)
    }
}

/// Display form of one value: text without line breaks, cut when very long.
pub fn cell_of(v: &Value) -> Cell {
    let (text, kind) = match v {
        Value::Null => ("NULL".to_string(), CellKind::Null),
        Value::Number(n) => (n.to_string(), CellKind::Num),
        Value::Bool(b) => ((*b as u8).to_string(), CellKind::Num),
        Value::String(s) => (s.clone(), CellKind::Text),
        other => (other.to_string(), CellKind::Text),
    };
    let flat: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let text = if flat.chars().count() > CELL_MAX {
        let mut t: String = flat.chars().take(CELL_MAX).collect();
        t.push('…');
        t
    } else {
        flat
    };
    Cell { text, kind }
}

/// `out` with every out-of-range JSON number outside strings (the CLI prints infinity as
/// `9e+999`) replaced by the string "Inf" or "-Inf".
fn quote_infinities(out: &str) -> String {
    let mut res = String::with_capacity(out.len());
    let (mut in_str, mut esc) = (false, false);
    let mut num = String::new();
    let flush = |num: &mut String, res: &mut String| {
        if num.is_empty() {
            return;
        }
        match num.parse::<f64>() {
            Ok(f) if f.is_infinite() => res.push_str(if f < 0.0 { "\"-Inf\"" } else { "\"Inf\"" }),
            _ => res.push_str(num),
        }
        num.clear();
    };
    for ch in out.chars() {
        if in_str {
            res.push(ch);
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
        } else if ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E') {
            num.push(ch);
        } else {
            flush(&mut num, &mut res);
            res.push(ch);
            in_str = ch == '"';
        }
    }
    flush(&mut num, &mut res);
    res
}

/// Parse the CLI's `-json` output (empty when the result has no rows). Keeps at most [`PAGE`]
/// rows after skipping `skip`; `more` says whether rows were left over.
pub fn parse_rows(out: &str, skip: usize) -> Result<Rows, String> {
    let out = out.trim();
    if out.is_empty() {
        return Ok(Rows::default());
    }
    // The CLI prints an infinite float as `9e+999`, which a JSON reader refuses: show it as text.
    let parsed = serde_json::from_str::<Table0>(out).or_else(|e| {
        if out.contains("e+") || out.contains("e-") || out.contains('E') {
            serde_json::from_str(&quote_infinities(out))
        } else {
            Err(e)
        }
    });
    let Table0(all) = parsed.map_err(|e| format!("Unreadable output: {e}"))?;
    let cols = all.first().map(|r| r.0.iter().map(|(k, _)| k.clone()).collect()).unwrap_or_default();
    let total = all.len();
    let rows: Vec<Vec<Cell>> =
        all.into_iter().skip(skip).take(PAGE).map(|r| r.0.iter().map(|(_, v)| cell_of(v)).collect()).collect();
    Ok(Rows { cols, rows, more: total > skip + PAGE })
}

/// The tree `EXPLAIN QUERY PLAN` prints (`QUERY PLAN` then `|--SCAN t` lines): one column named
/// after the first line, one row per line. Same paging as [`parse_rows`].
pub fn parse_plan(out: &str, skip: usize) -> Rows {
    if let Some(r) = parse_plan_json(out, skip) {
        return r;
    }
    let mut lines = out.lines().filter(|l| !l.trim().is_empty());
    let Some(head) = lines.next() else { return Rows::default() };
    let all: Vec<&str> = lines.collect();
    let rows = all
        .iter()
        .skip(skip)
        .take(PAGE)
        .map(|l| vec![Cell { text: l.trim_end().to_string(), kind: CellKind::Text }])
        .collect();
    Rows { cols: vec![head.trim().to_string()], rows, more: all.len() > skip + PAGE }
}

/// Newer `sqlite3` builds print the plan as JSON rows (`id`, `parent`, `detail`): the same tree as
/// text, indented by depth. `None` when `out` is not that.
fn parse_plan_json(out: &str, skip: usize) -> Option<Rows> {
    let v: Value = serde_json::from_str(out.trim()).ok()?;
    let arr = v.as_array()?;
    let mut depth: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut all: Vec<String> = Vec::new();
    for r in arr {
        let detail = r.get("detail")?.as_str()?;
        let id = r.get("id").and_then(Value::as_i64).unwrap_or(0);
        let d = r.get("parent").and_then(Value::as_i64).and_then(|p| depth.get(&p)).map_or(0, |d| d + 1);
        depth.insert(id, d);
        all.push(format!("{}{}", "  ".repeat(d), detail));
    }
    let rows = all.iter().skip(skip).take(PAGE).map(|l| vec![Cell { text: l.clone(), kind: CellKind::Text }]).collect();
    Some(Rows { cols: vec!["QUERY PLAN".to_string()], rows, more: all.len() > skip + PAGE })
}

/// Parse a result for `sql`: plans as plan, a bare EXPLAIN that an older `sqlite3` printed as text
/// like a plan too, anything else as JSON rows.
pub fn parse_result(sql: &str, out: &str, skip: usize) -> Result<Rows, String> {
    let explain = sql.trim_start().get(..7).is_some_and(|s| s.eq_ignore_ascii_case("explain"));
    if is_plan(sql) || (explain && !out.trim_start().starts_with('[')) {
        Ok(parse_plan(out, skip))
    } else {
        parse_rows(out, skip)
    }
}

/// The tables listed by [`LIST_SQL`].
pub fn parse_tables(out: &str) -> Result<Vec<Table>, String> {
    let r = parse_rows(out, 0)?;
    Ok(r.rows
        .iter()
        .filter_map(|row| {
            let name = row.first()?.text.clone();
            let view = row.get(1).is_some_and(|c| c.text == "view");
            Some(Table { name, view, rows: None })
        })
        .collect())
}

/// Row counts from [`counts_sql`] copied into `tables`.
pub fn apply_counts(tables: &mut [Table], out: &str) {
    let Ok(r) = parse_rows(out, 0) else { return };
    for row in &r.rows {
        let (Some(n), Some(c)) = (row.first(), row.get(1)) else { continue };
        if let Some(t) = tables.iter_mut().find(|t| t.name == n.text) {
            t.rows = c.text.parse().ok();
        }
    }
}

/// Run `sql` with the system `sqlite3` in read-only mode and return its JSON text. `Err(None)`
/// means there is no `sqlite3` on this machine; `Err(Some(msg))` is what it complained about.
pub fn run(db: &Path, sql: &str) -> Result<String, Option<String>> {
    let out = Command::new("sqlite3")
        .arg("-readonly")
        .arg("-json")
        .arg(db)
        .arg(sql)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { None } else { Some(e.to_string()) })?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let line = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("sqlite3 failed");
    // "Parse error in 4th command line argument: near ...": the argument position is noise here.
    let line = line.split_once("command line argument: ").map_or(line, |(_, m)| m);
    Err(Some(line.trim().trim_start_matches("Error: ").to_string()))
}

/// Number of pages for `total` rows (at least one).
#[cfg(test)]
pub fn page_count(total: u64) -> u64 {
    total.div_ceil(PAGE as u64).max(1)
}

/// Offset of the page after / before `offset`, `None` at the ends (`next` only when more rows exist).
pub fn next_offset(offset: usize, more: bool) -> Option<usize> {
    more.then_some(offset + PAGE)
}

pub fn prev_offset(offset: usize) -> Option<usize> {
    (offset > 0).then(|| offset.saturating_sub(PAGE))
}

/// 1234567 -> "1,234,567".
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "101–200" (or "0" without rows) for the page starting at `offset` holding `n` rows.
pub fn range_label(offset: usize, n: usize) -> String {
    if n == 0 { "0 rows".into() } else { format!("{}–{}", offset + 1, offset + n) }
}

/// Is `path` a SQLite database by name?
pub fn is_sqlite_name(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "db" | "sqlite" | "sqlite3" | "db3"))
}

/// The 16-byte header every SQLite file starts with.
const MAGIC: &[u8] = b"SQLite format 3\0";

/// A `.db` file is opened as a database only when its bytes say so (other `.db` files stay text).
pub fn is_sqlite_file(path: &Path) -> bool {
    use std::io::Read;
    if !is_sqlite_name(path) {
        return false;
    }
    let mut head = [0u8; 16];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head == MAGIC
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_lets_only_one_read_statement_through() {
        assert_eq!(guard(" select * from t ; ").unwrap(), "select * from t");
        assert!(guard("EXPLAIN QUERY PLAN SELECT 1").is_ok());
        assert!(guard("pragma table_info(t)").is_ok());
        for bad in [
            "DELETE FROM t",
            "update t set a=1",
            "INSERT INTO t VALUES (1)",
            "DROP TABLE t",
            "ATTACH 'x' AS y",
            "WITH a AS (SELECT 1) DELETE FROM t",
            "PRAGMA journal_mode=DELETE",
            ".shell ls",
            "SELECT 1; DROP TABLE t",
            "",
            "   ;",
            "SELECT 1\nDELETE FROM t",
        ] {
            assert!(guard(bad).is_err(), "{bad:?} must be refused");
        }
        // A semicolon inside a literal is fine.
        assert!(guard("SELECT ';' AS x").is_ok());
        assert!(guard("SELECT \"a;b\" FROM t").is_ok());
    }

    #[test]
    fn identifiers_are_quoted() {
        assert_eq!(quote_ident("users"), "\"users\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(table_sql("a b", 200), "SELECT * FROM \"a b\" LIMIT 101 OFFSET 200");
    }

    #[test]
    fn selects_are_wrapped_and_others_run_as_they_are() {
        assert_eq!(page_sql("SELECT 1", 100), "SELECT * FROM (SELECT 1\n) LIMIT 101 OFFSET 100");
        assert_eq!(page_sql("PRAGMA table_info(t)", 0), "PRAGMA table_info(t)");
        assert_eq!(page_sql("EXPLAIN SELECT 1", 0), "EXPLAIN SELECT 1");
        assert!(page_sql("WITH a AS (SELECT 1) SELECT * FROM a", 0).starts_with("SELECT * FROM (WITH "));
        assert!(wrappable("values (1)") && wrappable("select 1") && !wrappable("pragma x"));
        assert!(!wrappable("selection"), "only the word SELECT");
    }

    #[test]
    fn guard_ignores_comments_and_literals_and_trailing_semicolons() {
        assert_eq!(guard("SELECT 1;;").unwrap(), "SELECT 1");
        assert_eq!(guard("SELECT 1; -- done").unwrap(), "SELECT 1");
        assert_eq!(guard("SELECT 1 /* a; b */").unwrap(), "SELECT 1 /* a; b */");
        assert_eq!(guard("SELECT 1 -- a; b").unwrap(), "SELECT 1 -- a; b");
        assert!(guard("SELECT 'it''s; fine' AS x").is_ok());
        assert!(guard("SELECT [a;b] FROM t").is_ok());
        assert!(guard("SELECT 1; SELECT 2").is_err(), "two statements");
        assert!(guard("SELECT 1; /* x */ DROP TABLE t").is_err());
        assert!(guard("SELECT 1 /* ; */; DROP TABLE t").is_err());
        assert_eq!(guard("select * from \"my table\"").unwrap(), "select * from \"my table\"");
        assert_eq!(guard("  SeLeCt 1  ").unwrap(), "SeLeCt 1");
        assert_eq!(guard("SELECT 1\nFROM t").unwrap_err(), "One line only");
    }

    #[test]
    fn guard_lets_reading_with_pragma_and_values_through_and_refuses_writes() {
        assert!(guard("WITH a AS (SELECT 1 x) SELECT * FROM a").is_ok());
        assert!(guard("with a as (select replace('x','x','y') r) select * from a").is_ok(), "replace() is a function");
        for bad in [
            "WITH a AS (SELECT 1) INSERT INTO t SELECT * FROM a",
            "WITH a AS (SELECT 1) UPDATE t SET x = 1",
            "WITH a AS (SELECT 1) REPLACE INTO t VALUES (1)",
            "PRAGMA user_version = 5",
            "PRAGMA user_version(5)",
            "PRAGMA journal_mode(wal)",
            "VACUUM",
            "REINDEX",
            "CREATE TABLE x (a)",
            "REPLACE INTO t VALUES (1)",
            "BEGIN",
            "(SELECT 1)",
            "-- hi",
        ] {
            assert!(guard(bad).is_err(), "{bad:?} must be refused");
        }
        for good in [
            "VALUES (1), (2)",
            "PRAGMA table_info('a b')",
            "PRAGMA main.table_info(users)",
            "PRAGMA foreign_key_list(t)",
        ] {
            assert!(guard(good).is_ok(), "{good:?}");
        }
        assert!(guard("PRAGMA table_info('a=b')").is_ok(), "an = inside a string is not a setting");
        assert!(guard("PRAGMA page_size").is_ok());
    }

    #[test]
    fn guard_blocks_shell_functions_that_reach_outside_the_database() {
        for bad in [
            "SELECT writefile('/tmp/x', 'hi')",
            "SELECT WriteFile ('/tmp/x', 'hi')",
            "SELECT writefile/**/('/tmp/x', 'hi')",
            "SELECT \"writefile\"('/tmp/x', 'hi')",
            "SELECT load_extension('/tmp/x.so')",
            "SELECT edit('a', 'vim')",
            "EXPLAIN SELECT writefile('a', 'b')",
        ] {
            assert!(guard(bad).is_err(), "{bad:?} must be refused");
        }
        // Names that merely look like them are fine.
        assert!(guard("SELECT 'writefile(x)' AS s, edit FROM t").is_ok());
        assert!(guard("SELECT readfile FROM t").is_ok());
    }

    #[test]
    fn plan_output_is_a_tree_not_json() {
        assert!(is_plan("EXPLAIN QUERY PLAN SELECT * FROM t") && is_plan("explain  query plan select 1"));
        assert!(!is_plan("EXPLAIN SELECT 1") && !is_plan("SELECT 1"));
        let r = parse_plan("QUERY PLAN\n|--SCAN users\n`--SEARCH orders USING INDEX i (u=?)\n", 0);
        assert_eq!(r.cols, ["QUERY PLAN"]);
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[1][0].text, "`--SEARCH orders USING INDEX i (u=?)");
        assert!(!r.more);
        assert_eq!(parse_plan("", 0), Rows::default());
    }

    #[test]
    fn infinity_prints_as_text_instead_of_failing_the_page() {
        let r = parse_rows("[{\"a\":1,\"b\":9e+999,\"c\":-9e+999}]", 0).unwrap();
        assert_eq!(r.rows[0][1].text, "Inf");
        assert_eq!(r.rows[0][2].text, "-Inf");
        assert!(parse_rows("[{\"a\":}]", 0).is_err());
    }

    #[test]
    fn odd_cells_are_shown_flat() {
        // NUL / control chars of a blob, unicode and the 64-bit edge stay readable and short.
        let r = parse_rows("[{\"b\":\"\\u0000\\u00ff\\u0041\",\"u\":\"é😀\",\"big\":9223372036854775807}]", 0).unwrap();
        assert_eq!(r.rows[0][0].text, " \u{ff}A");
        assert_eq!(r.rows[0][1].text, "é😀");
        assert_eq!(r.rows[0][2], Cell { text: "9223372036854775807".into(), kind: CellKind::Num });
        let long = cell_of(&Value::String("ç".repeat(500)));
        assert_eq!(long.text.chars().count(), CELL_MAX + 1);
        assert!(long.text.ends_with('…'));
        // Repeated column names keep both columns.
        let r = parse_rows("[{\"a\":1,\"a\":2}]", 0).unwrap();
        assert_eq!(r.cols, ["a", "a"]);
    }

    /// Runs against the real `sqlite3` CLI (skipped where it is not installed).
    #[test]
    fn real_sqlite_end_to_end() {
        let dir = std::env::temp_dir().join(format!("lumen-sqlview-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("t.db");
        let _ = std::fs::remove_file(&db);
        let setup = "CREATE TABLE \"my table\" (id INTEGER PRIMARY KEY, name TEXT, data BLOB, n REAL);\
            INSERT INTO \"my table\" (name, data, n) VALUES ('Zoë 😀', x'00ff41', NULL), (NULL, NULL, 1.5), ('long ' || hex(randomblob(300)), x'', 1e999);\
            WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 250) INSERT INTO \"my table\" (name) SELECT 'row' || x FROM c;\
            CREATE VIEW v AS SELECT id FROM \"my table\" WHERE id < 4;";
        match Command::new("sqlite3").arg(&db).arg(setup).output() {
            Ok(o) if o.status.success() => {}
            _ => return, // no sqlite3 here
        }
        let q = |sql: &str, skip: usize| -> Result<Rows, String> {
            let g = guard(sql)?;
            let o = run(&db, &page_sql(&g, 0)).map_err(|e| e.unwrap_or_default())?;
            parse_result(&g, &o, skip)
        };
        // Tables and views, with a name that has a space.
        let tables = parse_tables(&run(&db, LIST_SQL).unwrap()).unwrap();
        assert_eq!(
            tables.iter().map(|t| (t.name.as_str(), t.view)).collect::<Vec<_>>(),
            [("v", true), ("my table", false)]
        );
        let mut tables = tables;
        let counts = run(&db, &counts_sql(&tables).unwrap()).unwrap();
        apply_counts(&mut tables, &counts);
        assert_eq!(tables[1].rows, Some(253));
        assert_eq!(tables[0].rows, Some(3));
        let first = parse_rows(&run(&db, &table_sql("my table", 0)).unwrap(), 0).unwrap();
        assert_eq!(first.rows.len(), PAGE);
        assert!(first.more);
        let last = parse_rows(&run(&db, &table_sql("my table", 200)).unwrap(), 0).unwrap();
        assert_eq!(last.rows.len(), 53);
        assert!(!last.more);
        // NULL, unicode, long text, an infinite float, an empty and a binary blob.
        let r = q("SELECT name, n, data FROM \"my table\" WHERE id <= 3 ORDER BY id", 0).unwrap();
        assert_eq!(r.rows[0][0].text, "Zoë 😀");
        assert_eq!(r.rows[1][0].kind, CellKind::Null);
        assert_eq!(r.rows[0][1].kind, CellKind::Null);
        assert_eq!(r.rows[1][1].text, "1.5");
        assert_eq!(r.rows[2][1].text, "Inf");
        assert_eq!(r.rows[2][0].text.chars().count(), CELL_MAX + 1);
        // WHERE / JOIN / ORDER / LIMIT, aliases and a trailing comment (which used to break paging).
        let r = q("SELECT a.id AS a_id, b.id AS b_id FROM v a JOIN v b ON b.id = a.id + 1 WHERE a.id > 0 ORDER BY a.id DESC LIMIT 5; -- tail", 0).unwrap();
        assert_eq!(r.cols, ["a_id", "b_id"]);
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0][0].text, "2");
        let r = q("SELECT id FROM v -- trailing comment", 0).unwrap();
        assert_eq!(r.rows.len(), 3);
        // Paging a query (the extra row says there is more).
        let r = q("SELECT id FROM \"my table\"", 0).unwrap();
        assert_eq!((r.rows.len(), r.more), (PAGE, true));
        let o = run(&db, &page_sql("SELECT id FROM \"my table\"", 200)).unwrap();
        let r = parse_rows(&o, 0).unwrap();
        assert_eq!((r.rows.len(), r.more), (53, false));
        // Empty result, no columns to show.
        assert_eq!(q("SELECT * FROM v WHERE id < 0", 0).unwrap(), Rows::default());
        // WITH, VALUES, PRAGMA (paged on this side) and EXPLAIN.
        assert_eq!(q("WITH a AS (SELECT 7 AS x) SELECT x FROM a", 0).unwrap().rows[0][0].text, "7");
        assert_eq!(q("VALUES (1), (2)", 0).unwrap().rows.len(), 2);
        let r = q("PRAGMA table_info('my table')", 0).unwrap();
        assert_eq!(r.rows.len(), 4);
        assert_eq!(r.rows[1][1].text, "name");
        assert!(q("EXPLAIN SELECT * FROM v", 0).unwrap().rows.len() > 3);
        let r = q("EXPLAIN QUERY PLAN SELECT * FROM v", 0).unwrap();
        assert_eq!(r.cols, ["QUERY PLAN"]);
        assert!(r.rows.iter().any(|row| row[0].text.contains("SCAN")));
        // Errors come back as one readable line.
        let e = q("SELECT * FROM nope", 0).unwrap_err();
        assert!(e.contains("no such table"), "{e}");
        let e = q("SELECT 1 +", 0).unwrap_err();
        assert!(e.contains("syntax error"), "{e}");
        // Writes never get to the CLI, and the file really is untouched.
        for w in [
            "INSERT INTO v VALUES (1)",
            "UPDATE \"my table\" SET name = 'x'",
            "DROP TABLE \"my table\"",
            "DELETE FROM \"my table\"",
        ] {
            assert!(q(w, 0).is_err(), "{w}");
        }
        assert_eq!(
            parse_rows(&run(&db, "SELECT COUNT(*) AS c FROM \"my table\"").unwrap(), 0).unwrap().rows[0][0].text,
            "253"
        );
        // Even if one slipped past the guard, the CLI itself is read-only.
        assert!(run(&db, "DELETE FROM \"my table\"").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_rows_keep_the_column_order() {
        let r = parse_rows("[{\"z\":1,\"a\":\"x\",\"m\":null},\n{\"z\":2,\"a\":\"y\",\"m\":3.5}]\n", 0).unwrap();
        assert_eq!(r.cols, ["z", "a", "m"]);
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0][2], Cell { text: "NULL".into(), kind: CellKind::Null });
        assert_eq!(r.rows[1][2], Cell { text: "3.5".into(), kind: CellKind::Num });
        assert_eq!(r.rows[0][1].kind, CellKind::Text);
        assert!(!r.more);
        assert_eq!(parse_rows("", 0).unwrap(), Rows::default());
        assert!(parse_rows("nonsense", 0).is_err());
    }

    #[test]
    fn extra_row_means_next_page() {
        let rows: Vec<String> = (0..=PAGE).map(|i| format!("{{\"n\":{i}}}")).collect();
        let r = parse_rows(&format!("[{}]", rows.join(",")), 0).unwrap();
        assert_eq!(r.rows.len(), PAGE);
        assert!(r.more);
        // Client-side paging (PRAGMA): the second page of 150 rows.
        let rows: Vec<String> = (0..150).map(|i| format!("{{\"n\":{i}}}")).collect();
        let r = parse_rows(&format!("[{}]", rows.join(",")), PAGE).unwrap();
        assert_eq!(r.rows.len(), 50);
        assert!(!r.more);
    }

    #[test]
    fn cells_are_flat_and_short() {
        let c = cell_of(&Value::String("a\nb\tc".into()));
        assert_eq!(c.text, "a b c");
        let long = cell_of(&Value::String("x".repeat(500)));
        assert_eq!(long.text.chars().count(), CELL_MAX + 1);
    }

    #[test]
    fn tables_and_counts() {
        let mut t =
            parse_tables("[{\"name\":\"users\",\"type\":\"table\"},{\"name\":\"v1\",\"type\":\"view\"}]").unwrap();
        assert_eq!(t.len(), 2);
        assert!(t[1].view && !t[0].view);
        let sql = counts_sql(&t).unwrap();
        assert!(sql.contains("UNION ALL") && sql.contains("FROM \"v1\""));
        apply_counts(&mut t, "[{\"n\":\"users\",\"c\":42},{\"n\":\"v1\",\"c\":0}]");
        assert_eq!(t[0].rows, Some(42));
        assert_eq!(t[1].rows, Some(0));
        assert!(counts_sql(&[]).is_none());
    }

    #[test]
    fn pagination_math() {
        assert_eq!(page_count(0), 1);
        assert_eq!(page_count(100), 1);
        assert_eq!(page_count(101), 2);
        assert_eq!(next_offset(0, true), Some(100));
        assert_eq!(next_offset(0, false), None);
        assert_eq!(prev_offset(0), None);
        assert_eq!(prev_offset(200), Some(100));
        assert_eq!(group_digits(1234567), "1,234,567");
        assert_eq!(group_digits(12), "12");
        assert_eq!(range_label(100, 100), "101–200");
        assert_eq!(range_label(0, 0), "0 rows");
    }

    #[test]
    fn names_are_matched_case_insensitively() {
        assert!(is_sqlite_name(Path::new("/a/b.SQLITE3")));
        assert!(is_sqlite_name(Path::new("x.db")));
        assert!(!is_sqlite_name(Path::new("x.txt")));
    }
}
