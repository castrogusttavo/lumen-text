//! The debugger's data model, without any UI or process: breakpoint metadata (conditions,
//! logpoints, disabled ones) and its per-project persistence, the Debug Console line encoding,
//! the flattened Variables tree and Call Stack rows, launch-configuration detection and the
//! expression under the pointer for hover-to-evaluate. Everything here is a pure function of its
//! input so the CI tests need no adapter.
#![allow(dead_code)]

use crate::dap::{BpResult, BpSpec, Frame, Session, State, Var};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

// ───────────────────────────── console lines ─────────────────────────────

/// Marks the lines of `Session::output` that are not plain program output.
pub const MARK: char = '\u{1f}';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleKind {
    Stdout,
    Stderr,
    /// What the user typed in the Debug Console.
    Echo,
    Result,
    Error,
}

/// The `Session::output` line for `text` of `kind` (plain output stays as it is).
pub fn console_line(kind: ConsoleKind, text: &str) -> String {
    let tag = match kind {
        ConsoleKind::Stdout => return text.to_string(),
        ConsoleKind::Stderr => 'x',
        ConsoleKind::Echo => 'i',
        ConsoleKind::Result => 'o',
        ConsoleKind::Error => 'e',
    };
    format!("{MARK}{tag}{text}")
}

/// Inverse of [`console_line`].
pub fn classify(line: &str) -> (ConsoleKind, &str) {
    let mut chars = line.chars();
    if chars.next() == Some(MARK)
        && let Some(tag) = chars.next()
    {
        let rest = chars.as_str();
        let kind = match tag {
            'x' => ConsoleKind::Stderr,
            'i' => ConsoleKind::Echo,
            'o' => ConsoleKind::Result,
            'e' => ConsoleKind::Error,
            _ => return (ConsoleKind::Stdout, line),
        };
        return (kind, rest);
    }
    (ConsoleKind::Stdout, line)
}

// ───────────────────────────── breakpoints ─────────────────────────────

/// What a breakpoint carries besides its line.
#[derive(Clone, Debug, PartialEq)]
pub struct BpMeta {
    pub condition: Option<String>,
    /// A logpoint: this message is printed instead of stopping (`{expr}` is interpolated).
    pub log: Option<String>,
    pub enabled: bool,
}

impl Default for BpMeta {
    fn default() -> Self {
        BpMeta { condition: None, log: None, enabled: true }
    }
}

impl BpMeta {
    /// A plain, enabled breakpoint needs no entry.
    pub fn is_plain(&self) -> bool {
        self.condition.is_none() && self.log.is_none() && self.enabled
    }
}

/// 0-based breakpoint lines per file.
pub type Lines = BTreeMap<PathBuf, Vec<usize>>;
/// Metadata of the breakpoints that have some (0-based line -> meta).
pub type Metas = BTreeMap<PathBuf, BTreeMap<usize, BpMeta>>;

/// How a breakpoint is drawn in the gutter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BpGlyph {
    /// Filled dot: set (and verified when a session is running).
    #[default]
    Normal,
    Conditional,
    Log,
    /// The adapter could not place it.
    Unverified,
    Disabled,
}

pub fn glyph(meta: Option<&BpMeta>, verified: Option<bool>) -> BpGlyph {
    if meta.is_some_and(|m| !m.enabled) {
        BpGlyph::Disabled
    } else if verified == Some(false) {
        BpGlyph::Unverified
    } else if meta.is_some_and(|m| m.log.is_some()) {
        BpGlyph::Log
    } else if meta.is_some_and(|m| m.condition.is_some()) {
        BpGlyph::Conditional
    } else {
        BpGlyph::Normal
    }
}

/// The breakpoints to send for a file: enabled ones only, 1-based.
pub fn specs_for(lines: &[usize], meta: Option<&BTreeMap<usize, BpMeta>>) -> Vec<BpSpec> {
    lines
        .iter()
        .filter_map(|&l| {
            let m = meta.and_then(|m| m.get(&l));
            if m.is_some_and(|m| !m.enabled) {
                return None;
            }
            Some(BpSpec {
                line: l + 1,
                condition: m.and_then(|m| m.condition.clone()),
                log_message: m.and_then(|m| m.log.clone()),
            })
        })
        .collect()
}

/// 0-based requested line -> verified, from the adapter's answer for a file.
pub fn verified_by_line(results: &[BpResult]) -> BTreeMap<usize, bool> {
    results.iter().map(|r| (r.requested.saturating_sub(1), r.verified)).collect()
}

/// Add `line` to the sorted `lines`, or remove it. True when it is now set.
pub fn toggle_line(lines: &mut Vec<usize>, line: usize) -> bool {
    match lines.binary_search(&line) {
        Ok(i) => {
            lines.remove(i);
            false
        }
        Err(i) => {
            lines.insert(i, line);
            true
        }
    }
}

// ───────────────────────────── persistence ─────────────────────────────

/// Everything remembered per project between runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DebugState {
    pub lines: Lines,
    pub meta: Metas,
    pub watch: Vec<String>,
    /// The launch configuration chosen for the project (see [`LaunchConfig::id`]).
    pub config: Option<String>,
}

impl DebugState {
    pub fn to_json(&self) -> Value {
        let mut bps = serde_json::Map::new();
        for (path, lines) in &self.lines {
            if lines.is_empty() {
                continue;
            }
            let metas = self.meta.get(path);
            let arr: Vec<Value> = lines
                .iter()
                .map(|&l| {
                    let mut o = json!({"line": l});
                    if let Some(m) = metas.and_then(|m| m.get(&l)) {
                        if let Some(c) = &m.condition {
                            o["condition"] = json!(c);
                        }
                        if let Some(t) = &m.log {
                            o["log"] = json!(t);
                        }
                        if !m.enabled {
                            o["enabled"] = json!(false);
                        }
                    }
                    o
                })
                .collect();
            bps.insert(path.to_string_lossy().into_owned(), Value::Array(arr));
        }
        let mut v = json!({"version": 1, "breakpoints": bps, "watch": self.watch});
        if let Some(c) = &self.config {
            v["config"] = json!(c);
        }
        v
    }

    pub fn from_json(v: &Value) -> DebugState {
        let mut st = DebugState::default();
        if let Some(o) = v["breakpoints"].as_object() {
            for (path, arr) in o {
                let mut lines: Vec<usize> = Vec::new();
                let mut metas: BTreeMap<usize, BpMeta> = BTreeMap::new();
                for e in arr.as_array().into_iter().flatten() {
                    let Some(l) = e["line"].as_u64() else { continue };
                    let l = l as usize;
                    if lines.contains(&l) {
                        continue;
                    }
                    lines.push(l);
                    let text = |k: &str| e[k].as_str().map(str::to_string).filter(|s| !s.trim().is_empty());
                    let m = BpMeta {
                        condition: text("condition"),
                        log: e["log"].as_str().map(str::to_string).filter(|s| !s.is_empty()),
                        enabled: e["enabled"].as_bool().unwrap_or(true),
                    };
                    if !m.is_plain() {
                        metas.insert(l, m);
                    }
                }
                lines.sort_unstable();
                if !lines.is_empty() {
                    st.lines.insert(PathBuf::from(path), lines);
                }
                if !metas.is_empty() {
                    st.meta.insert(PathBuf::from(path), metas);
                }
            }
        }
        st.watch = v["watch"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
            .collect();
        st.config = v["config"].as_str().map(str::to_string).filter(|s| !s.is_empty());
        st
    }
}

/// `<state>/lumen-text/debug/<project key>.json`.
pub fn state_file(root: &Path) -> Option<PathBuf> {
    Some(
        crate::paths::state_home()?
            .join("lumen-text")
            .join("debug")
            .join(format!("{}.json", crate::backup::project_key(root))),
    )
}

/// The remembered state of the project at `root`; breakpoints in files that are gone are dropped.
pub fn load(root: &Path) -> DebugState {
    let Some(text) = state_file(root).and_then(|p| std::fs::read_to_string(p).ok()) else {
        return DebugState::default();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return DebugState::default() };
    let mut st = DebugState::from_json(&v);
    st.lines.retain(|p, _| p.exists());
    st.meta.retain(|p, _| st.lines.contains_key(p));
    st
}

pub fn save(root: &Path, st: &DebugState) {
    let Some(path) = state_file(root) else { return };
    if st.lines.values().all(Vec::is_empty) && st.watch.is_empty() && st.config.is_none() {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, st.to_json().to_string());
}

// ───────────────────────────── variables tree ─────────────────────────────

const MAX_ROWS: usize = 4000;
const MAX_DEPTH: usize = 24;

#[derive(Clone, Debug, PartialEq)]
pub struct TreeRow {
    pub depth: usize,
    /// Path of the row ("Locals/obj/field"); what expansion is remembered by.
    pub key: String,
    pub name: String,
    pub value: String,
    pub type_: String,
    pub vars_ref: i64,
    pub open: bool,
    /// Open, but the children have not arrived yet.
    pub loading: bool,
    /// `Some(expensive)` for a scope row.
    pub scope: Option<bool>,
}

impl TreeRow {
    pub fn expandable(&self) -> bool {
        self.vars_ref > 0
    }
}

/// The Variables view: every scope, and the children of everything the user opened.
pub fn flatten(s: &Session) -> Vec<TreeRow> {
    let mut out = Vec::new();
    for sc in &s.scopes {
        let open = if sc.expensive { s.expanded.contains(&sc.name) } else { !s.collapsed.contains(&sc.name) };
        out.push(TreeRow {
            depth: 0,
            key: sc.name.clone(),
            name: sc.name.clone(),
            value: String::new(),
            type_: String::new(),
            vars_ref: sc.vars_ref,
            open,
            loading: open && sc.vars_ref > 0 && !s.var_children.contains_key(&sc.vars_ref),
            scope: Some(sc.expensive),
        });
        if open {
            walk(s, sc.vars_ref, &sc.name, 1, &mut out);
        }
    }
    out
}

pub fn walk(s: &Session, r: i64, base: &str, depth: usize, out: &mut Vec<TreeRow>) {
    let Some(vars) = s.var_children.get(&r) else { return };
    for v in vars {
        if out.len() >= MAX_ROWS {
            return;
        }
        let key = format!("{base}/{}", v.name);
        let open = v.vars_ref > 0 && s.expanded.contains(&key) && depth < MAX_DEPTH;
        out.push(row_of(v, depth, key.clone(), open, open && !s.var_children.contains_key(&v.vars_ref)));
        if open {
            walk(s, v.vars_ref, &key, depth + 1, out);
        }
    }
}

/// The Watch view: each expression as a root row (key `Watch/<expr>`) and, for structured values
/// the user opened, its children. `error` rows carry the message in `value` and cannot expand.
pub fn flatten_watch(s: &Session) -> Vec<(usize, TreeRow)> {
    let mut out = Vec::new();
    for (i, e) in s.watch.iter().enumerate() {
        let key = format!("Watch/{e}");
        let (value, type_, vars_ref) = match s.watch_results.get(e) {
            Some(Ok((v, t))) => (v.clone(), t.clone(), s.watch_refs.get(e).copied().unwrap_or(0)),
            Some(Err(m)) => (m.clone(), String::new(), 0),
            None => ("not available".to_string(), String::new(), 0),
        };
        let open = vars_ref > 0 && s.expanded.contains(&key);
        out.push((
            i,
            TreeRow {
                depth: 0,
                key: key.clone(),
                name: e.clone(),
                value,
                type_,
                vars_ref,
                open,
                loading: open && !s.var_children.contains_key(&vars_ref),
                scope: None,
            },
        ));
        if open {
            let mut kids = Vec::new();
            walk(s, vars_ref, &key, 1, &mut kids);
            out.extend(kids.into_iter().map(|r| (i, r)));
        }
    }
    out
}

fn row_of(v: &Var, depth: usize, key: String, open: bool, loading: bool) -> TreeRow {
    TreeRow {
        depth,
        key,
        name: v.name.clone(),
        value: v.value.clone(),
        type_: v.type_.clone(),
        vars_ref: v.vars_ref,
        open,
        loading,
        scope: None,
    }
}

// ───────────────────────────── call stack ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum StackRow {
    Thread { id: i64, name: String, open: bool, current: bool },
    Frame { thread: i64, idx: usize, frame: Frame, selected: bool },
}

const MAX_THREADS: usize = 200;

/// The Call Stack view. With a single thread the thread row is left out.
pub fn stack_rows(s: &Session) -> Vec<StackRow> {
    let mut out = Vec::new();
    if !matches!(s.state, State::Stopped { .. }) {
        return out;
    }
    let cur = match s.state {
        State::Stopped { thread } => thread,
        _ => 0,
    };
    let sel = s.selected;
    let push_frames = |out: &mut Vec<StackRow>, tid: i64| {
        for (i, f) in s.thread_frames.get(&tid).into_iter().flatten().enumerate() {
            out.push(StackRow::Frame { thread: tid, idx: i, frame: f.clone(), selected: sel == Some((tid, i)) });
        }
    };
    if s.threads.len() <= 1 {
        push_frames(&mut out, cur);
        return out;
    }
    for t in s.threads.iter().take(MAX_THREADS) {
        let open = s.thread_open.contains(&t.id);
        out.push(StackRow::Thread { id: t.id, name: t.name.clone(), open, current: t.id == cur });
        if open {
            push_frames(&mut out, t.id);
        }
    }
    out
}

/// `main.add  main.go:12`.
pub fn frame_label(f: &Frame) -> String {
    match &f.path {
        Some(p) => format!(
            "{}  {}:{}",
            f.name,
            Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            f.line
        ),
        None => f.name.clone(),
    }
}

// ───────────────────────────── launch configurations ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum LaunchKind {
    /// TypeScript / JavaScript through js-debug.
    Node,
    /// `cargo build`, then gdb.
    Rust,
    /// An adapter of the registry (`dap_adapters.json`), by id.
    Adapter(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaunchConfig {
    /// Stable id remembered per project: `node`, `rust`, `delve`, `ghp`.
    pub id: &'static str,
    pub label: String,
    pub kind: LaunchKind,
    pub file: PathBuf,
}

/// Does this Go source declare `package main`?
pub fn is_go_main(src: &str) -> bool {
    let mut in_block = false;
    for l in src.lines() {
        let t = l.trim();
        if in_block {
            if t.contains("*/") {
                in_block = false;
            }
            continue;
        }
        if t.is_empty() || t.starts_with("//") {
            continue;
        }
        if t.starts_with("/*") {
            in_block = !t.contains("*/");
            continue;
        }
        return t.strip_prefix("package").is_some_and(|r| r.split("//").next().unwrap_or("").trim() == "main");
    }
    false
}

/// The first `.ghp` file of the project (two levels deep, no hidden or vendored folders).
pub fn find_ghp(root: &Path) -> Option<PathBuf> {
    fn scan(dir: &Path, depth: usize) -> Option<PathBuf> {
        let mut dirs = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "target" | "vendor") {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "ghp") {
                return Some(p);
            }
        }
        if depth > 0 {
            for d in dirs {
                if let Some(f) = scan(&d, depth - 1) {
                    return Some(f);
                }
            }
        }
        None
    }
    scan(root, 2)
}

/// The launch configurations that apply: the active file's own language first, then a `.ghp`
/// project's adapter (`ghp_file` is the active file when it is a page, else the project's first).
pub fn detect(file: Option<&Path>, go_main: bool, ghp_file: Option<&Path>) -> Vec<LaunchConfig> {
    let mut out = Vec::new();
    if let Some(f) = file {
        let ext = f.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match ext.as_str() {
            "ts" | "js" | "mjs" | "cjs" | "mts" | "cts" => out.push(LaunchConfig {
                id: "node",
                label: format!("Node: {name}"),
                kind: LaunchKind::Node,
                file: f.to_path_buf(),
            }),
            "rs" => out.push(LaunchConfig {
                id: "rust",
                label: "Rust: cargo build + gdb".into(),
                kind: LaunchKind::Rust,
                file: f.to_path_buf(),
            }),
            "go" if go_main => out.push(LaunchConfig {
                id: "delve",
                label: format!(
                    "Go: {} (delve)",
                    f.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or(name)
                ),
                kind: LaunchKind::Adapter("delve"),
                file: f.to_path_buf(),
            }),
            _ => {}
        }
    }
    if let Some(g) = ghp_file {
        out.push(LaunchConfig {
            id: "ghp",
            label: format!("GHP: {}", g.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            kind: LaunchKind::Adapter("ghp"),
            file: g.to_path_buf(),
        });
    }
    out
}

/// The remembered choice when it still applies, else the first.
pub fn choose<'a>(cfgs: &'a [LaunchConfig], saved: Option<&str>) -> Option<&'a LaunchConfig> {
    saved.and_then(|id| cfgs.iter().find(|c| c.id == id)).or_else(|| cfgs.first())
}

// ───────────────────────────── hover ─────────────────────────────

fn ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The expression under char column `col` of `line`: an identifier with the `a.b.c` / `a->b` chain
/// before it ("user.name" when pointing at `name`). `(start, end)` in chars.
pub fn expr_at(line: &str, col: usize) -> Option<(usize, usize)> {
    let ch: Vec<char> = line.chars().collect();
    let col = col.min(ch.len());
    let (mut s, mut e) = (col, col);
    while s > 0 && ident(ch[s - 1]) {
        s -= 1;
    }
    while e < ch.len() && ident(ch[e]) {
        e += 1;
    }
    if s == e {
        return None;
    }
    // Pull in `obj.` / `obj->` prefixes.
    loop {
        let sep = if s >= 1 && ch[s - 1] == '.' {
            1
        } else if s >= 2 && ch[s - 2] == '-' && ch[s - 1] == '>' {
            2
        } else {
            break;
        };
        let mut p = s - sep;
        let end = p;
        while p > 0 && ident(ch[p - 1]) {
            p -= 1;
        }
        if p == end {
            break;
        }
        s = p;
    }
    if ch[s].is_ascii_digit() {
        return None; // a number literal
    }
    Some((s, e))
}

// ───────────────────────────── tests ─────────────────────────────

// ───────────────────────────── breakpoints follow edits ─────────────────────────────

/// Where the breakpoint on `line` is after the edits `recs`, or `None` when its line was removed.
/// The same bookkeeping as bookmarks (`bookmarks::on_edits`).
pub fn follow_line(line: usize, recs: &[crate::buffer::EditRec]) -> Option<usize> {
    let mut one = vec![line];
    crate::bookmarks::on_edits(&mut one, recs);
    one.first().copied()
}

/// Move a file's breakpoint lines (and their metadata) through `recs`. Returns true when anything moved.
pub fn follow_edits(
    lines: &mut Vec<usize>,
    meta: Option<&mut BTreeMap<usize, BpMeta>>,
    recs: &[crate::buffer::EditRec],
) -> bool {
    let moved: Vec<(usize, Option<usize>)> = lines.iter().map(|&l| (l, follow_line(l, recs))).collect();
    if moved.iter().all(|(a, b)| Some(*a) == *b) {
        return false;
    }
    if let Some(m) = meta {
        let old = std::mem::take(m);
        for (l, mv) in old {
            let to = moved.iter().find(|(a, _)| *a == l).and_then(|(_, b)| *b);
            if let Some(t) = to {
                m.insert(t, mv);
            }
        }
    }
    let mut next: Vec<usize> = moved.into_iter().filter_map(|(_, b)| b).collect();
    next.sort_unstable();
    next.dedup();
    *lines = next;
    true
}

// ───────────────────────────── .lumen/launch.json ─────────────────────────────

/// One entry of `.lumen/launch.json`: overrides for a built-in launch configuration (`delve`, `ghp`).
///
/// ```json
/// { "configurations": [
///   { "config": "ghp", "program": "build/site", "args": ["-addr=:8081"],
///     "env": {"GHP_ENV": "dev"}, "cwd": "site", "adapter": "ghp" } ] }
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaunchOverride {
    /// The launch configuration id it applies to (`ghp`, `delve`); defaults to `adapter`.
    pub config: String,
    /// Debug with another adapter of the registry (`dap_adapters.json`) instead of the config's own.
    pub adapter: Option<String>,
    pub program: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

pub const LAUNCH_FILE: &str = "launch.json";

/// Parse the text of a `launch.json` (whole-line `//` comments are allowed).
pub fn parse_launch(text: &str) -> Result<Vec<LaunchOverride>, String> {
    let clean: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    if clean.trim().is_empty() {
        return Ok(Vec::new());
    }
    let v: Value = serde_json::from_str(&clean).map_err(|e| format!("{LAUNCH_FILE}: {e}"))?;
    let list = match v.get("configurations") {
        Some(Value::Array(a)) => a.as_slice(),
        None => return Ok(Vec::new()),
        Some(_) => return Err(format!("{LAUNCH_FILE}: \"configurations\" must be a list")),
    };
    let text_of = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).map(str::to_string);
    let mut out = Vec::new();
    for (i, c) in list.iter().enumerate() {
        let adapter = text_of(c, "adapter");
        let Some(config) = text_of(c, "config").or_else(|| adapter.clone()) else {
            return Err(format!("{LAUNCH_FILE}: configuration {} needs \"config\" (ghp, delve) or \"adapter\"", i + 1));
        };
        let args = c.get("args").and_then(Value::as_array).map(|a| {
            a.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>()
        });
        let env = c
            .get("env")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        out.push(LaunchOverride { config, adapter, program: text_of(c, "program"), args, env, cwd: text_of(c, "cwd") });
    }
    Ok(out)
}

/// The project's `.lumen/launch.json`: no file is no overrides; a broken one is an error to show.
pub fn load_launch(root: &Path) -> Result<Vec<LaunchOverride>, String> {
    match std::fs::read_to_string(root.join(".lumen").join(LAUNCH_FILE)) {
        Ok(t) => parse_launch(&t),
        Err(_) => Ok(Vec::new()),
    }
}

/// The override that applies to launch configuration `id`.
pub fn launch_override<'a>(list: &'a [LaunchOverride], id: &str) -> Option<&'a LaunchOverride> {
    list.iter().find(|o| o.config == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dap::Scope;

    #[test]
    fn console_lines_round_trip() {
        for k in [ConsoleKind::Stderr, ConsoleKind::Echo, ConsoleKind::Result, ConsoleKind::Error] {
            let l = console_line(k, "a b");
            assert_eq!(classify(&l), (k, "a b"));
        }
        assert_eq!(console_line(ConsoleKind::Stdout, "hi"), "hi");
        assert_eq!(classify("hi"), (ConsoleKind::Stdout, "hi"));
        assert_eq!(classify(""), (ConsoleKind::Stdout, ""));
    }

    #[test]
    fn glyph_priority() {
        let plain = BpMeta::default();
        let cond = BpMeta { condition: Some("i > 3".into()), ..BpMeta::default() };
        let log = BpMeta { log: Some("i={i}".into()), ..BpMeta::default() };
        let off = BpMeta { enabled: false, condition: Some("x".into()), ..BpMeta::default() };
        assert_eq!(glyph(None, None), BpGlyph::Normal);
        assert_eq!(glyph(Some(&plain), Some(true)), BpGlyph::Normal);
        assert_eq!(glyph(Some(&cond), None), BpGlyph::Conditional);
        assert_eq!(glyph(Some(&log), None), BpGlyph::Log);
        assert_eq!(glyph(Some(&off), Some(false)), BpGlyph::Disabled, "disabled wins");
        assert_eq!(glyph(Some(&cond), Some(false)), BpGlyph::Unverified, "not placed wins over the kind");
    }

    #[test]
    fn specs_skip_disabled_and_are_one_based() {
        let mut m = BTreeMap::new();
        m.insert(4, BpMeta { enabled: false, ..BpMeta::default() });
        m.insert(9, BpMeta { condition: Some("n == 2".into()), log: Some("n={n}".into()), enabled: true });
        let specs = specs_for(&[1, 4, 9], Some(&m));
        assert_eq!(
            specs,
            vec![
                BpSpec { line: 2, condition: None, log_message: None },
                BpSpec { line: 10, condition: Some("n == 2".into()), log_message: Some("n={n}".into()) },
            ]
        );
        let results = vec![
            BpResult { requested: 2, line: 2, verified: true, message: String::new() },
            BpResult { requested: 10, line: 11, verified: false, message: String::new() },
        ];
        let v = verified_by_line(&results);
        assert_eq!(v.get(&1), Some(&true));
        assert_eq!(v.get(&9), Some(&false), "keyed by the line that was asked for, not where it landed");
    }

    #[test]
    fn toggle_line_keeps_order() {
        let mut l = vec![2, 8];
        assert!(toggle_line(&mut l, 5));
        assert_eq!(l, [2, 5, 8]);
        assert!(!toggle_line(&mut l, 2));
        assert_eq!(l, [5, 8]);
    }

    #[test]
    fn state_json_round_trips_and_drops_garbage() {
        let mut st = DebugState::default();
        st.lines.insert(PathBuf::from("/p/a.go"), vec![3, 10, 20]);
        st.meta
            .entry(PathBuf::from("/p/a.go"))
            .or_default()
            .insert(10, BpMeta { condition: Some("i == 4".into()), log: None, enabled: true });
        st.meta.entry(PathBuf::from("/p/a.go")).or_default().insert(20, BpMeta { enabled: false, ..BpMeta::default() });
        st.watch = vec!["total".into(), "a+b".into()];
        st.config = Some("delve".into());
        let back = DebugState::from_json(&st.to_json());
        assert_eq!(back, st);

        let junk = json!({"breakpoints": {"/p/b.go": [{"line": 2}, {"line": 2}, {"nope": 1}, {"line": 1, "condition": "  "}], "/p/c.go": []},
            "watch": ["", "x", 5], "config": ""});
        let st = DebugState::from_json(&junk);
        assert_eq!(st.lines.get(Path::new("/p/b.go")), Some(&vec![1, 2]), "sorted, deduplicated");
        assert!(st.meta.is_empty(), "a blank condition is no condition");
        assert!(!st.lines.contains_key(Path::new("/p/c.go")));
        assert_eq!(st.watch, ["x"]);
        assert_eq!(st.config, None);
    }

    #[test]
    fn state_is_saved_per_project_and_missing_files_are_forgotten() {
        let dir = std::env::temp_dir().join(format!("lumen-dbgmodel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("main.go");
        std::fs::write(&real, "package main\n").unwrap();
        let other = dir.join("other-project");
        let g = crate::app_unsaved::testkit::lock();
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.join("state")) };
        let mut st = DebugState::default();
        st.lines.insert(real.clone(), vec![0]);
        st.lines.insert(dir.join("gone.go"), vec![5]);
        st.watch = vec!["x".into()];
        save(&dir, &st);
        let got = load(&dir);
        assert_eq!(got.lines.len(), 1, "gone.go is dropped");
        assert_eq!(got.lines.get(&real), Some(&vec![0]));
        assert_eq!(got.watch, ["x"]);
        assert_eq!(load(&other), DebugState::default(), "another project has its own file");
        save(&dir, &DebugState::default());
        assert!(!state_file(&dir).unwrap().exists(), "an empty state removes the file");
        drop(g);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn session_with_tree() -> Session {
        let mut s = Session::new("x", &[], Path::new("."), false);
        s.state = State::Stopped { thread: 1 };
        s.scopes = vec![
            Scope { name: "Locals".into(), vars_ref: 10, expensive: false },
            Scope { name: "Globals".into(), vars_ref: 11, expensive: true },
        ];
        let v = |n: &str, r: i64| Var { name: n.into(), value: "v".into(), type_: "t".into(), vars_ref: r };
        s.var_children.insert(10, vec![v("a", 0), v("obj", 20)]);
        s.var_children.insert(20, vec![v("f", 0), v("inner", 21)]);
        s
    }

    #[test]
    fn tree_scopes_open_by_default_and_expensive_ones_closed() {
        let s = session_with_tree();
        let rows = flatten(&s);
        let names: Vec<_> = rows.iter().map(|r| (r.depth, r.name.as_str(), r.open)).collect();
        assert_eq!(names, [(0, "Locals", true), (1, "a", false), (1, "obj", false), (0, "Globals", false)]);
        assert!(rows[2].expandable() && !rows[1].expandable());
    }

    #[test]
    fn tree_expansion_follows_the_path_and_shows_loading() {
        let mut s = session_with_tree();
        s.expanded.insert("Locals/obj".into());
        s.expanded.insert("Locals/obj/inner".into());
        let rows = flatten(&s);
        let shown: Vec<_> = rows.iter().map(|r| (r.depth, r.key.as_str(), r.loading)).collect();
        assert_eq!(
            shown,
            [
                (0, "Locals", false),
                (1, "Locals/a", false),
                (1, "Locals/obj", false),
                (2, "Locals/obj/f", false),
                (2, "Locals/obj/inner", true), // children of ref 21 not fetched yet
                (0, "Globals", false),
            ]
        );
        // Folding a scope hides everything below it but remembers the inner expansion.
        s.collapsed.insert("Locals".into());
        assert_eq!(flatten(&s).len(), 2);
        s.collapsed.clear();
        assert_eq!(flatten(&s).len(), 6);
    }

    #[test]
    fn stack_rows_hide_the_thread_row_for_one_thread_and_follow_the_selection() {
        let mut s = Session::new("x", &[], Path::new("."), false);
        s.state = State::Stopped { thread: 1 };
        let fr = |n: &str| Frame { id: 1, name: n.into(), path: Some("/w/main.go".into()), line: 3, col: 1 };
        s.thread_frames.insert(1, vec![fr("main.a"), fr("main.main")]);
        s.selected = Some((1, 1));
        let rows = stack_rows(&s);
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[1], StackRow::Frame { selected: true, idx: 1, .. }));

        s.threads = vec![
            crate::dap::ThreadInfo { id: 1, name: "main".into() },
            crate::dap::ThreadInfo { id: 2, name: "worker".into() },
        ];
        s.thread_open.insert(1);
        let rows = stack_rows(&s);
        assert_eq!(rows.len(), 4, "two thread rows, the open one lists its frames");
        assert!(matches!(&rows[0], StackRow::Thread { id: 1, open: true, current: true, .. }));
        assert!(matches!(&rows[3], StackRow::Thread { id: 2, open: false, current: false, .. }));
        s.state = State::Running;
        assert!(stack_rows(&s).is_empty());
        assert_eq!(frame_label(&fr("main.a")), "main.a  main.go:3");
    }

    #[test]
    fn launch_detection_and_choice() {
        let go = Path::new("/p/cmd/app/main.go");
        let ghp = Path::new("/p/site/index.ghp");
        let c = detect(Some(go), true, Some(ghp));
        assert_eq!(c.iter().map(|c| c.id).collect::<Vec<_>>(), ["delve", "ghp"]);
        assert_eq!(c[0].label, "Go: app (delve)");
        assert_eq!(c[1].kind, LaunchKind::Adapter("ghp"));
        assert!(detect(Some(go), false, None).is_empty(), "a Go file that is not package main has no config");
        assert_eq!(detect(Some(Path::new("/p/a.ts")), false, None)[0].kind, LaunchKind::Node);
        assert_eq!(detect(Some(Path::new("/p/a.rs")), false, None)[0].kind, LaunchKind::Rust);
        assert_eq!(choose(&c, None).unwrap().id, "delve");
        assert_eq!(choose(&c, Some("ghp")).unwrap().id, "ghp");
        assert_eq!(choose(&c, Some("node")).unwrap().id, "delve", "a stale choice falls back");
        assert!(choose(&[], Some("ghp")).is_none());
    }

    #[test]
    fn go_main_detection_skips_comments() {
        assert!(is_go_main("package main\n\nfunc main() {}\n"));
        assert!(is_go_main("/* block\n package lib */\npackage main // entry\n"));
        assert!(is_go_main("// Copyright\n/* block\n package lib */\n\npackage main\n"));
        assert!(!is_go_main("package server\n"));
        assert!(!is_go_main(""));
    }

    #[test]
    fn finds_a_ghp_page_without_entering_hidden_or_vendored_dirs() {
        let dir = std::env::temp_dir().join(format!("lumen-ghpfind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("site/pages")).unwrap();
        std::fs::write(dir.join("node_modules/x/a.ghp"), "").unwrap();
        std::fs::write(dir.join(".git/b.ghp"), "").unwrap();
        assert_eq!(find_ghp(&dir), None);
        std::fs::write(dir.join("site/pages/home.ghp"), "").unwrap();
        assert_eq!(find_ghp(&dir), Some(dir.join("site/pages/home.ghp")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expression_under_the_pointer() {
        let line = "    total = user.profile.name + 42";
        let at = |needle: &str, off: usize| line.find(needle).unwrap() + off;
        let s = |r: Option<(usize, usize)>| r.map(|(a, b)| line.chars().skip(a).take(b - a).collect::<String>());
        assert_eq!(s(expr_at(line, at("total", 2))).as_deref(), Some("total"));
        assert_eq!(s(expr_at(line, at("name", 1))).as_deref(), Some("user.profile.name"), "the chain before it");
        assert_eq!(s(expr_at(line, at("profile", 0))).as_deref(), Some("user.profile"));
        assert_eq!(s(expr_at(line, at("42", 0))), None, "numbers are not evaluated");
        assert_eq!(s(expr_at(line, 0)), None, "blank");
        assert_eq!(expr_at("p->next->val", 10), Some((0, 12)));
    }
    #[test]
    fn breakpoints_follow_line_edits_with_their_metadata() {
        use crate::buffer::{EditRec, EditShape};
        let rec = |line, col, rem, ins| EditRec {
            at: 0,
            removed: 0,
            inserted: 0,
            line,
            col,
            rem_lines: rem,
            ins_lines: ins,
            tail_empty: false,
            shape: EditShape::Splice,
        };
        let mut lines = vec![3, 10];
        let mut meta: BTreeMap<usize, BpMeta> = BTreeMap::new();
        meta.insert(10, BpMeta { condition: Some("x > 1".into()), ..Default::default() });
        // A line break inserted above both.
        assert!(follow_edits(&mut lines, Some(&mut meta), &[rec(1, 0, 0, 1)]));
        assert_eq!(lines, [4, 11]);
        assert_eq!(meta.get(&11).and_then(|m| m.condition.as_deref()), Some("x > 1"));
        assert!(!meta.contains_key(&10));
        // Typing inside a line moves nothing.
        assert!(!follow_edits(&mut lines, Some(&mut meta), &[rec(4, 2, 0, 0)]));
        // The line break before line 11 removed: the breakpoint moves up one.
        assert!(follow_edits(&mut lines, Some(&mut meta), &[rec(9, 3, 1, 0)]));
        assert_eq!(lines, [4, 10]);
    }

    #[test]
    fn launch_json_parses_overrides_and_reports_mistakes() {
        let t = "// comment\n{\"configurations\": [{\"config\": \"ghp\", \"program\": \"out/app\", \"args\": [\"-v\", 3], \"env\": {\"A\": \"1\", \"B\": 2}, \"cwd\": \"site\"}, {\"adapter\": \"delve\"}]}";
        let l = parse_launch(t).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].args.as_deref(), Some(&["-v".to_string(), "3".to_string()][..]));
        assert_eq!(l[0].env["B"], "2");
        assert_eq!(l[1].config, "delve", "config defaults to the adapter");
        assert_eq!(launch_override(&l, "ghp").and_then(|o| o.cwd.as_deref()), Some("site"));
        assert!(launch_override(&l, "node").is_none());
        assert!(parse_launch("").unwrap().is_empty());
        assert!(parse_launch("{}").unwrap().is_empty());
        assert!(parse_launch("{").unwrap_err().contains("launch.json"));
        assert!(parse_launch("{\"configurations\": [{}]}").unwrap_err().contains("needs"));
        assert!(parse_launch("{\"configurations\": 1}").is_err());
    }

    #[test]
    fn watch_rows_expand_structured_values() {
        let mut s = Session::new("x", &[], Path::new("."), false);
        s.state = State::Stopped { thread: 1 };
        s.watch = vec!["obj".into(), "n".into(), "bad".into()];
        s.watch_results.insert("obj".into(), Ok(("{...}".into(), "T".into())));
        s.watch_results.insert("n".into(), Ok(("1".into(), String::new())));
        s.watch_results.insert("bad".into(), Err("nope".into()));
        s.watch_refs.insert("obj".into(), 40);
        let rows = flatten_watch(&s);
        assert_eq!(rows.len(), 3);
        assert!(rows[0].1.expandable() && !rows[0].1.open && !rows[1].1.expandable() && !rows[2].1.expandable());
        s.expanded.insert("Watch/obj".into());
        assert!(flatten_watch(&s)[0].1.loading, "open but the children have not arrived");
        s.var_children.insert(40, vec![Var { name: "x".into(), value: "5".into(), type_: "int".into(), vars_ref: 0 }]);
        let rows = flatten_watch(&s);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            (rows[1].1.depth, rows[1].1.name.as_str(), rows[1].0),
            (1, "x", 0),
            "children belong to their watch"
        );
        assert_eq!(rows[2].0, 1);
    }
}
