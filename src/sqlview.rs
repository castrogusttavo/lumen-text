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

/// True when `sql` has a `;` outside of a string or quoted name (a second statement).
fn has_inner_semicolon(sql: &str) -> bool {
    let mut quote: Option<char> = None;
    for c in sql.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`') => quote = Some(c),
            (None, '[') => quote = Some(']'),
            (None, ';') => return true,
            _ => {}
        }
    }
    false
}

/// The statement to run for what the user typed, or why it is refused. Only one read-only
/// statement passes: `SELECT`, `PRAGMA` (without `=`: no setting) or `EXPLAIN`. (`WITH` is out:
/// SQLite allows `WITH ... DELETE`.) The CLI runs with `-readonly` as well: two locks.
pub fn guard(input: &str) -> Result<String, String> {
    let sql = input.trim().trim_end_matches(';').trim();
    if sql.is_empty() {
        return Err("Type a SELECT query".into());
    }
    if sql.contains(['\n', '\r', '\0']) {
        return Err("One line only".into());
    }
    if has_inner_semicolon(sql) {
        return Err("One statement at a time".into());
    }
    let first: String = sql.chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_uppercase();
    match first.as_str() {
        "SELECT" | "EXPLAIN" => Ok(sql.to_string()),
        "PRAGMA" if !sql.contains('=') => Ok(sql.to_string()),
        "PRAGMA" => Err("PRAGMA can only read here".into()),
        _ => Err("Read-only viewer: only SELECT, PRAGMA and EXPLAIN run".into()),
    }
}

/// True when `sql` can be wrapped as a sub-select (so LIMIT / OFFSET apply to it).
pub fn wrappable(sql: &str) -> bool {
    sql.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("select"))
}

/// The statement for page `offset` of a guarded query. A `SELECT` is wrapped with LIMIT/OFFSET
/// (one extra row tells whether a next page exists); PRAGMA / EXPLAIN run as they are and are
/// cut on this side ([`parse_rows`] keeps `skip` / `PAGE` of them).
pub fn page_sql(sql: &str, offset: usize) -> String {
    if wrappable(sql) { format!("SELECT * FROM ({sql}) LIMIT {} OFFSET {offset}", PAGE + 1) } else { sql.to_string() }
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

/// Parse the CLI's `-json` output (empty when the result has no rows). Keeps at most [`PAGE`]
/// rows after skipping `skip`; `more` says whether rows were left over.
pub fn parse_rows(out: &str, skip: usize) -> Result<Rows, String> {
    let out = out.trim();
    if out.is_empty() {
        return Ok(Rows::default());
    }
    let Table0(all) = serde_json::from_str(out).map_err(|e| format!("Unreadable output: {e}"))?;
    let cols = all.first().map(|r| r.0.iter().map(|(k, _)| k.clone()).collect()).unwrap_or_default();
    let total = all.len();
    let rows: Vec<Vec<Cell>> =
        all.into_iter().skip(skip).take(PAGE).map(|r| r.0.iter().map(|(_, v)| cell_of(v)).collect()).collect();
    Ok(Rows { cols, rows, more: total > skip + PAGE })
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
        assert_eq!(page_sql("SELECT 1", 100), "SELECT * FROM (SELECT 1) LIMIT 101 OFFSET 100");
        assert_eq!(page_sql("PRAGMA table_info(t)", 0), "PRAGMA table_info(t)");
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
