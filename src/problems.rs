//! Project-wide problems: a bounded store of diagnostics for files that are *not* open, the
//! streaming parser of LSP 3.17 `workspace/diagnostic` reports, and the grouping the Problems
//! panel shows.
//!
//! * Open documents keep their diagnostics in `Doc::diags` (positions converted against the
//!   buffer). Everything a server says about other files (pushed `publishDiagnostics` for closed
//!   files, `relatedDocuments` of a pull, `workspace/diagnostic` reports) lands in
//!   [`ProjectDiags`], keyed by file and by the server that said it.
//! * Memory is bounded: at most [`FILE_CAP`] diagnostics per file and server (the most severe
//!   win), [`TOTAL_CAP`] in all, only the first line of each message, and at most [`MAX_FILES`]
//!   result ids per server. What does not fit is counted, never kept.
//! * Workspace reports are parsed on the language server's reader thread ([`WsSeed`], used by
//!   `lsp_stream`): even a report of many megabytes never becomes a `serde_json::Value`, and the
//!   UI thread only moves the already capped lists into the store.

use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
};

/// Diagnostics kept per file and server.
pub const FILE_CAP: usize = 100;
/// Diagnostics kept in the whole store.
pub const TOTAL_CAP: usize = 20_000;
/// File reports taken from one workspace report, and result ids remembered per server.
pub const MAX_FILES: usize = 20_000;
/// Characters kept of a message (its first line).
pub const MSG_CAP: usize = 300;
/// Problem rows the panel lists (the picker shows at most 2000 rows; files take some).
pub const ROWS_SHOWN: usize = 1500;
/// `partialResultToken` prefix of our `workspace/diagnostic` requests; `$/progress` notifications
/// carrying it are partial reports, parsed like a response.
pub const WS_TOKEN_PREFIX: &str = "lumen-wsdiag:";
/// Store key of the diagnostics a document had when it was closed (replaced by whatever a server
/// says about the file next).
pub const LAST_OPEN: &str = "\u{0}last-open";

/// One diagnostic of a file that is not open. Line and column are the server's (0-based, UTF-16
/// column): good enough to place the cursor once the file opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PDiag {
    pub line: u32,
    pub col: u32,
    /// 1 error, 2 warning, 3 info, 4 hint.
    pub severity: u8,
    pub message: String,
    pub source: String,
}

/// First line of `s`, at most [`MSG_CAP`] characters.
pub fn clip_msg(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim_end();
    match line.char_indices().nth(MSG_CAP) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_string(),
    }
}

/// One LSP `Diagnostic` object, or `None` when it has no usable range.
pub fn pdiag_from(v: &Value) -> Option<PDiag> {
    let s = &v["range"]["start"];
    let msg = match &v["message"] {
        Value::String(s) => s.as_str(),
        m => m["value"].as_str().unwrap_or(""),
    };
    Some(PDiag {
        line: u32::try_from(s["line"].as_u64()?).unwrap_or(u32::MAX),
        col: u32::try_from(s["character"].as_u64()?).unwrap_or(u32::MAX),
        severity: v["severity"].as_u64().map_or(1, |n| n.clamp(1, 4) as u8),
        message: clip_msg(msg),
        source: v["source"].as_str().map(clip_msg).unwrap_or_default(),
    })
}

/// Add `d` to `v` keeping at most `cap` entries: when full, it replaces the least severe entry if
/// it is more severe than that one. True when something was left out (`d` or the replaced one).
pub fn keep_worst(v: &mut Vec<PDiag>, d: PDiag, cap: usize) -> bool {
    if v.len() < cap {
        v.push(d);
        return false;
    }
    if let Some((i, w)) = v.iter().enumerate().max_by_key(|(_, x)| x.severity)
        && d.severity < w.severity
    {
        v[i] = d;
    }
    true
}

/// The `diagnostics` array of a `publishDiagnostics` (or the `items` of a document report),
/// capped at `cap`; returns the kept ones and how many were left out.
pub fn pdiags_from(items: &Value, cap: usize) -> (Vec<PDiag>, usize) {
    let mut out = Vec::new();
    let mut dropped = 0;
    for it in items.as_array().into_iter().flatten() {
        if let Some(d) = pdiag_from(it) {
            dropped += keep_worst(&mut out, d, cap) as usize;
        }
    }
    (out, dropped)
}

// ───────────────────────────── store ─────────────────────────────

/// Diagnostics of files that are not open, per file and per server (see the module docs).
#[derive(Default)]
pub struct ProjectDiags {
    files: HashMap<PathBuf, Vec<(String, Vec<PDiag>)>>,
    total: usize,
    /// Diagnostics left out because of the caps (since the last time it was reset).
    pub dropped: usize,
    /// Bumped on every change: the open panel rebuilds its rows when it moves.
    pub generation: u64,
    /// `resultId` per server and URI (sent back as `previousResultIds`).
    result_ids: HashMap<String, HashMap<String, String>>,
}

impl ProjectDiags {
    /// Replace what `source` says about `path` (an empty list clears it). Any report about the file
    /// also replaces the diagnostics it had when it was closed ([`LAST_OPEN`]).
    pub fn set(&mut self, path: &Path, source: &str, mut diags: Vec<PDiag>) {
        diags.sort_by_key(|d| (d.severity, d.line, d.col));
        if diags.len() > FILE_CAP {
            self.dropped += diags.len() - FILE_CAP;
            diags.truncate(FILE_CAP);
        }
        let entry = self.files.entry(path.to_path_buf()).or_default();
        let mut freed = 0;
        entry.retain(|(s, v)| {
            let gone = s == source || (source != LAST_OPEN && s == LAST_OPEN);
            if gone {
                freed += v.len();
            }
            !gone
        });
        self.total -= freed;
        let room = TOTAL_CAP.saturating_sub(self.total);
        if diags.len() > room {
            self.dropped += diags.len() - room;
            diags.truncate(room);
        }
        if !diags.is_empty() {
            self.total += diags.len();
            entry.push((source.to_string(), diags));
        }
        if entry.is_empty() {
            self.files.remove(path);
        }
        if freed > 0 || self.files.contains_key(path) {
            self.generation += 1;
        }
    }

    /// Forget everything `source` said (its server stopped).
    pub fn clear_source(&mut self, source: &str) {
        let mut freed = 0;
        self.files.retain(|_, entry| {
            entry.retain(|(s, v)| {
                if s == source {
                    freed += v.len();
                }
                s != source
            });
            !entry.is_empty()
        });
        self.result_ids.remove(source);
        if freed > 0 {
            self.total -= freed;
            self.generation += 1;
        }
    }

    /// Every file with diagnostics, with all of its servers' lists.
    pub fn iter(&self) -> impl Iterator<Item = (&PathBuf, impl Iterator<Item = &PDiag>)> {
        self.files.iter().map(|(p, e)| (p, e.iter().flat_map(|(_, v)| v.iter())))
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Remember the `resultId` `source` gave for `uri` (`None` forgets it).
    pub fn set_result_id(&mut self, source: &str, uri: &str, id: Option<String>) {
        let m = self.result_ids.entry(source.to_string()).or_default();
        match id {
            Some(id) if m.len() < MAX_FILES || m.contains_key(uri) => {
                m.insert(uri.to_string(), id);
            }
            Some(_) => {}
            None => {
                m.remove(uri);
            }
        }
    }

    /// `previousResultIds` for a `workspace/diagnostic` request to `source`.
    pub fn previous_ids(&self, source: &str) -> Vec<Value> {
        self.result_ids
            .get(source)
            .map(|m| m.iter().map(|(u, v)| serde_json::json!({"uri": u, "value": v})).collect())
            .unwrap_or_default()
    }
}

// ───────────────────────────── workspace reports ─────────────────────────────

/// A `WorkspaceDiagnosticReport` (or one partial result of it), already capped.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WsReport {
    pub files: Vec<FileReport>,
    /// Diagnostics (and whole file reports past [`MAX_FILES`]) left out.
    pub dropped: usize,
    kept: usize,
}

/// One `WorkspaceDocumentDiagnosticReport`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FileReport {
    pub uri: String,
    /// `kind: "full"` (else `"unchanged"`: keep what we have).
    pub full: bool,
    pub result_id: Option<String>,
    pub diags: Vec<PDiag>,
}

impl WsReport {
    /// Parse a report that is already a `Value` (small responses, partial results).
    pub fn from_value(v: Value) -> WsReport {
        WsSeed.deserialize(v).unwrap_or_default()
    }
}

/// Streaming parser of a workspace diagnostic report (see the module docs).
pub struct WsSeed;

impl<'de> DeserializeSeed<'de> for WsSeed {
    type Value = WsReport;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<WsReport, D::Error> {
        d.deserialize_any(WsVisitor)
    }
}

struct WsVisitor;

impl<'de> Visitor<'de> for WsVisitor {
    type Value = WsReport;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a workspace diagnostic report")
    }
    fn visit_unit<E: de::Error>(self) -> Result<WsReport, E> {
        Ok(WsReport::default())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<WsReport, A::Error> {
        let mut out = WsReport::default();
        while let Some(key) = map.next_key::<String>()? {
            if key == "items" {
                map.next_value_seed(FilesSeed { out: &mut out })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(out)
    }
}

struct FilesSeed<'a> {
    out: &'a mut WsReport,
}

impl<'de> DeserializeSeed<'de> for FilesSeed<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for FilesSeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of document reports")
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(FileSeed { out: &mut *self.out })?.is_some() {}
        Ok(())
    }
}

struct FileSeed<'a> {
    out: &'a mut WsReport,
}

impl<'de> DeserializeSeed<'de> for FileSeed<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for FileSeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a document report")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let out = self.out;
        let mut r = FileReport::default();
        let room = if out.files.len() >= MAX_FILES { 0 } else { FILE_CAP.min(TOTAL_CAP - out.kept) };
        let mut dropped = 0;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "uri" => r.uri = map.next_value()?,
                "kind" => r.full = map.next_value::<String>()? == "full",
                "resultId" => r.result_id = map.next_value()?,
                "items" => {
                    let (diags, d) = map.next_value_seed(DiagsSeed { cap: room })?;
                    r.diags = diags;
                    dropped += d;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        out.dropped += dropped;
        if out.files.len() >= MAX_FILES || r.uri.is_empty() {
            out.dropped += (out.files.len() >= MAX_FILES) as usize;
            return Ok(());
        }
        out.kept += r.diags.len();
        out.files.push(r);
        Ok(())
    }
}

struct DiagsSeed {
    cap: usize,
}

impl<'de> DeserializeSeed<'de> for DiagsSeed {
    type Value = (Vec<PDiag>, usize);
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for DiagsSeed {
    type Value = (Vec<PDiag>, usize);
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of diagnostics")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok((Vec::new(), 0))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out = Vec::new();
        let mut dropped = 0;
        // One diagnostic at a time: each is small, the list can be huge.
        while let Some(v) = seq.next_element::<Value>()? {
            match pdiag_from(&v) {
                Some(_) if self.cap == 0 => dropped += 1,
                Some(d) => dropped += keep_worst(&mut out, d, self.cap) as usize,
                None => {}
            }
        }
        Ok((out, dropped))
    }
}

// ───────────────────────────── panel rows ─────────────────────────────

/// A problem as the panel lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    pub col: usize,
    pub severity: u8,
    pub message: String,
    pub source: String,
}

/// One file of the panel: its counts and its problems, most severe first.
#[derive(Debug, PartialEq, Eq)]
pub struct FileGroup {
    pub path: PathBuf,
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    pub rows: Vec<Row>,
}

impl FileGroup {
    /// "2 errors, 1 warning, 3 infos".
    pub fn summary(&self) -> String {
        let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
        let mut parts = Vec::new();
        if self.errors > 0 {
            parts.push(n(self.errors, "error", "errors"));
        }
        if self.warnings > 0 {
            parts.push(n(self.warnings, "warning", "warnings"));
        }
        if self.infos > 0 {
            parts.push(n(self.infos, "info", "infos"));
        }
        parts.join(", ")
    }
}

/// Group problems by file: files with the most errors (then warnings) first, then by path; inside
/// a file by severity, line and column. Hints (severity 4) are left out, repeats collapsed.
pub fn group(rows: impl IntoIterator<Item = (PathBuf, Row)>) -> Vec<FileGroup> {
    let mut by: HashMap<PathBuf, Vec<Row>> = HashMap::new();
    for (p, r) in rows {
        if r.severity <= 3 {
            by.entry(p).or_default().push(r);
        }
    }
    let mut out: Vec<FileGroup> = by
        .into_iter()
        .map(|(path, mut rows)| {
            rows.sort_by(|a, b| (a.severity, a.line, a.col, &a.message).cmp(&(b.severity, b.line, b.col, &b.message)));
            rows.dedup_by(|a, b| (a.line, a.col, &a.message) == (b.line, b.col, &b.message));
            let count = |s: u8| rows.iter().filter(|r| r.severity == s).count();
            FileGroup { errors: count(1), warnings: count(2), infos: count(3), path, rows }
        })
        .collect();
    out.sort_by(|a, b| b.errors.cmp(&a.errors).then(b.warnings.cmp(&a.warnings)).then(a.path.cmp(&b.path)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(line: u32, sev: u8, msg: &str) -> PDiag {
        PDiag { line, col: 0, severity: sev, message: msg.into(), source: String::new() }
    }

    fn lsp_diag(line: u32, sev: u8, msg: &str) -> Value {
        json!({"range": {"start": {"line": line, "character": 2}, "end": {"line": line, "character": 5}}, "severity": sev, "message": msg, "source": "x"})
    }

    #[test]
    fn messages_are_one_clipped_line() {
        assert_eq!(clip_msg("first\nsecond"), "first");
        let long = "é".repeat(MSG_CAP + 10);
        let c = clip_msg(&long);
        assert_eq!(c.chars().count(), MSG_CAP + 1);
        assert!(c.ends_with('…'));
        let p = pdiag_from(&lsp_diag(3, 2, "warn\nmore")).unwrap();
        assert_eq!(p, PDiag { line: 3, col: 2, severity: 2, message: "warn".into(), source: "x".into() });
        assert!(pdiag_from(&json!({"message": "no range"})).is_none());
        // Severity missing = error (as VS Code reads it).
        assert_eq!(pdiag_from(&json!({"range": {"start": {"line": 0, "character": 0}}})).unwrap().severity, 1);
    }

    #[test]
    fn keep_worst_prefers_errors_when_full() {
        let mut v = vec![d(0, 2, "w0"), d(1, 3, "i1")];
        assert!(keep_worst(&mut v, d(2, 1, "e2"), 2));
        assert_eq!(v.iter().map(|x| x.message.as_str()).collect::<Vec<_>>(), ["w0", "e2"]);
        assert!(keep_worst(&mut v, d(3, 3, "i3"), 2), "dropped, not kept");
        assert_eq!(v.len(), 2);
        let (kept, dropped) = pdiags_from(&json!([lsp_diag(0, 2, "a"), lsp_diag(1, 1, "b"), lsp_diag(2, 2, "c")]), 2);
        assert_eq!(dropped, 1);
        assert!(kept.iter().any(|x| x.message == "b"));
    }

    #[test]
    fn store_is_bounded_per_file_and_in_total() {
        let mut s = ProjectDiags::default();
        let many: Vec<PDiag> = (0..FILE_CAP as u32 + 30).map(|i| d(i, if i == 120 { 1 } else { 2 }, "m")).collect();
        s.set(Path::new("/p/a.rs"), "ra", many);
        assert_eq!(s.total(), FILE_CAP);
        assert_eq!(s.dropped, 30);
        // The error past the cap survives: the most severe win.
        assert!(s.iter().flat_map(|(_, it)| it).any(|x| x.severity == 1));
        for f in 0..(TOTAL_CAP / FILE_CAP + 5) {
            s.set(&PathBuf::from(format!("/p/f{f}.rs")), "ra", (0..FILE_CAP as u32).map(|i| d(i, 2, "m")).collect());
        }
        assert_eq!(s.total(), TOTAL_CAP, "never above the total cap");
        // Replacing a list frees its room first; clearing a server frees everything it said.
        let g = s.generation;
        s.set(Path::new("/p/a.rs"), "ra", vec![d(0, 1, "only")]);
        assert!(s.generation > g);
        assert!(s.total() <= TOTAL_CAP);
        s.clear_source("ra");
        assert_eq!((s.total(), s.file_count()), (0, 0));
    }

    #[test]
    fn sources_are_kept_apart_and_last_open_is_replaced() {
        let mut s = ProjectDiags::default();
        let p = Path::new("/p/a.py");
        s.set(p, LAST_OPEN, vec![d(0, 1, "stale")]);
        s.set(p, "ruff", vec![d(1, 2, "ruff says")]);
        let msgs: Vec<String> = s.iter().flat_map(|(_, it)| it.map(|x| x.message.clone())).collect();
        assert_eq!(msgs, ["ruff says"], "a fresh report replaces what the file had when it closed");
        s.set(p, "pyright", vec![d(2, 1, "pyright says")]);
        assert_eq!(s.total(), 2);
        s.set(p, "ruff", Vec::new());
        assert_eq!(s.total(), 1);
        s.set(p, "pyright", Vec::new());
        assert_eq!(s.file_count(), 0, "an empty report removes the file");
    }

    #[test]
    fn result_ids_round_trip_per_server() {
        let mut s = ProjectDiags::default();
        s.set_result_id("ra", "file:///a", Some("1".into()));
        s.set_result_id("ra", "file:///b", Some("2".into()));
        s.set_result_id("ra", "file:///b", None);
        assert_eq!(s.previous_ids("ra"), vec![json!({"uri": "file:///a", "value": "1"})]);
        assert!(s.previous_ids("other").is_empty());
        s.clear_source("ra");
        assert!(s.previous_ids("ra").is_empty());
    }

    #[test]
    fn workspace_report_is_parsed_and_capped() {
        let items: Vec<Value> = (0..FILE_CAP as u32 + 7).map(|i| lsp_diag(i, 2, "w")).collect();
        let v = json!({"items": [
            {"uri": "file:///p/a.rs", "kind": "full", "resultId": "r1", "version": null, "items": items},
            {"uri": "file:///p/b.rs", "kind": "unchanged", "resultId": "r2"},
            {"uri": "file:///p/c.rs", "kind": "full", "items": [lsp_diag(4, 1, "bad\nmore"), {"broken": true}]}
        ]});
        let r = WsReport::from_value(v.clone());
        assert_eq!(r.files.len(), 3);
        assert_eq!(r.files[0].diags.len(), FILE_CAP);
        assert_eq!(r.dropped, 7);
        assert_eq!(r.files[0].result_id.as_deref(), Some("r1"));
        assert!(!r.files[1].full && r.files[1].diags.is_empty());
        assert_eq!(
            r.files[2].diags,
            vec![PDiag { line: 4, col: 2, severity: 1, message: "bad".into(), source: "x".into() }]
        );
        // The streaming path (a reader, as `lsp_stream` uses it) gives the same answer.
        let text = v.to_string();
        let mut de = serde_json::Deserializer::from_reader(text.as_bytes());
        assert_eq!(WsSeed.deserialize(&mut de).unwrap(), r);
        // `null` (and an empty object) are an empty report.
        assert_eq!(WsReport::from_value(Value::Null), WsReport::default());
        assert_eq!(WsReport::from_value(json!({})), WsReport::default());
    }

    #[test]
    fn groups_sort_files_by_errors_and_rows_by_severity() {
        let r = |line, severity, m: &str| Row { line, col: 0, severity, message: m.into(), source: String::new() };
        let g = group(vec![
            (PathBuf::from("/p/b.rs"), r(9, 2, "warn b")),
            (PathBuf::from("/p/a.rs"), r(5, 2, "warn a")),
            (PathBuf::from("/p/c.rs"), r(7, 2, "late warn")),
            (PathBuf::from("/p/c.rs"), r(8, 1, "error c")),
            (PathBuf::from("/p/c.rs"), r(1, 3, "info c")),
            (PathBuf::from("/p/c.rs"), r(1, 3, "info c")),
            (PathBuf::from("/p/c.rs"), r(2, 4, "hint")),
        ]);
        let paths: Vec<&str> = g.iter().map(|f| f.path.to_str().unwrap()).collect();
        assert_eq!(paths, ["/p/c.rs", "/p/a.rs", "/p/b.rs"]);
        let c: Vec<&str> = g[0].rows.iter().map(|x| x.message.as_str()).collect();
        assert_eq!(c, ["error c", "late warn", "info c"], "severity first, repeats and hints gone");
        assert_eq!(g[0].summary(), "1 error, 1 warning, 1 info");
        assert_eq!(g[1].summary(), "1 warning");
    }
}
