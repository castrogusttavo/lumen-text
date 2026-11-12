//! Debugging UI glue: breakpoints (gutter, F9, conditions, logpoints, persisted per project),
//! session control (F5/F6/F10/F11, toolbar, restart, launch configuration) and the Debug panel:
//! a toolbar over Call Stack + Breakpoints, Variables + Watch and the Debug Console.
//!
//! Node/TypeScript runs through js-debug (`dap::MultiSession`), Rust through gdb's DAP mode and
//! every adapter of the registry (delve for Go, `ghp` for GHP pages) through `dap::Session`. The
//! model (tree flattening, breakpoint metadata, persistence, launch detection) lives in
//! `debug_model`; this file draws it and routes clicks and keys. Nothing here runs unless a
//! breakpoint is set or the Debug tab is open.

use crate::{
    app::{App, KeyIn},
    app_overlay::{Action, Cmd, Item, Overlay, Prompt, PromptKind, choice_picker},
    app_panel::PanelTab,
    dap::{BpSpec, Dap, MultiSession, Session, State},
    debug_model::{self as dm, BpGlyph, BpMeta, ConsoleKind, DebugState, LaunchConfig, LaunchKind, StackRow},
    field::TextField,
    scrollreg::Reg,
    theme::Rgba,
    ui::{Canvas, Rect, line_h},
};
use std::path::{Path, PathBuf};
use winit::keyboard::{Key, NamedKey};

const TOOLBAR_H: i32 = 30;
const SEC_H: i32 = 24;

// ───────────────────────────── debugger handle ─────────────────────────────

pub enum Debugger {
    Node(Box<MultiSession>),
    Native { dap: Box<Dap>, session: Box<Session> },
}

impl Debugger {
    pub fn state(&self) -> State {
        match self {
            Debugger::Node(m) => m.state(),
            Debugger::Native { session, .. } => session.state.clone(),
        }
    }
    /// The session the panel shows: the stopped one, else the newest.
    pub fn session(&self) -> Option<&Session> {
        match self {
            Debugger::Node(m) => m
                .stopped_child()
                .map(|i| &m.children[i].1)
                .or_else(|| m.children.last().map(|c| &c.1))
                .or(Some(&m.root_session)),
            Debugger::Native { session, .. } => Some(session),
        }
    }
    /// The session that can be asked things (evaluate, expand, select a frame): the stopped one.
    pub fn session_mut(&mut self) -> Option<(&mut Dap, &mut Session)> {
        match self {
            Debugger::Node(m) => {
                let i = m.stopped_child()?;
                let (d, s) = &mut m.children[i];
                Some((d, s))
            }
            Debugger::Native { dap, session } => Some((&mut **dap, &mut **session)),
        }
    }
    pub fn output(&self) -> &[String] {
        match self {
            Debugger::Node(m) => m.output(),
            Debugger::Native { session, .. } => &session.output,
        }
    }
    pub fn resume(&mut self) {
        match self {
            Debugger::Node(m) => m.resume(),
            Debugger::Native { dap, session } => session.resume(dap),
        }
    }
    pub fn step_over(&mut self) {
        match self {
            Debugger::Node(m) => m.step_over(),
            Debugger::Native { dap, session } => session.step_over(dap),
        }
    }
    pub fn step_in(&mut self) {
        match self {
            Debugger::Node(m) => m.step_in(),
            Debugger::Native { dap, session } => session.step_in(dap),
        }
    }
    pub fn step_out(&mut self) {
        match self {
            Debugger::Node(m) => m.step_out(),
            Debugger::Native { dap, session } => session.step_out(dap),
        }
    }
    pub fn pause(&mut self) {
        match self {
            Debugger::Node(m) => m.pause(),
            Debugger::Native { dap, session } => session.pause(dap),
        }
    }
    pub fn set_breakpoint_specs(&mut self, path: &str, specs: &[BpSpec]) {
        match self {
            Debugger::Node(m) => m.set_breakpoint_specs(path, specs),
            Debugger::Native { dap, session } => session.set_breakpoint_specs(path, specs.to_vec(), dap),
        }
    }
    pub fn stop(&mut self) {
        match self {
            Debugger::Node(m) => m.stop(),
            Debugger::Native { dap, .. } => {
                dap.disconnect(true);
            }
        }
    }
    /// Drain adapter messages; true when something changed.
    pub fn pump(&mut self) -> bool {
        match self {
            Debugger::Node(m) => m.handle_all(),
            Debugger::Native { dap, session } => {
                let mut changed = false;
                while let Ok(msg) = dap.rx.try_recv() {
                    session.handle(&msg, dap);
                    changed = true;
                }
                changed
            }
        }
    }
}

fn js_debug_server() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LUMEN_JSDEBUG") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let p = crate::paths::data_home()?.join("lumen-text/js-debug/js-debug/src/dapDebugServer.js");
    p.exists().then_some(p)
}

/// `name = "..."` of the `[package]` table.
fn cargo_package_name(manifest: &str) -> Option<String> {
    let mut in_pkg = false;
    for l in manifest.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_pkg = t == "[package]";
        } else if in_pkg && let Some(rest) = t.strip_prefix("name") {
            let v = rest.trim_start().strip_prefix('=')?.trim().trim_matches('"');
            return Some(v.to_string());
        }
    }
    None
}

// ───────────────────────────── UI state ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Console,
    Watch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// F5: start, or continue when stopped.
    Continue,
    Over,
    Into,
    Out,
    Pause,
    Restart,
    Stop,
}

/// What a clickable spot of the Debug panel does.
#[derive(Clone, Debug, PartialEq)]
pub enum DebugHit {
    Tool(Tool),
    Config,
    Thread(i64),
    Frame(i64, usize),
    Var { key: String, vars_ref: i64, scope: Option<bool> },
    Bp(PathBuf, usize),
    BpToggle(PathBuf, usize),
    BpRemove(PathBuf, usize),
    BpClearAll,
    WatchAdd,
    WatchRemove(usize),
    ConsoleInput,
    WatchInput,
    ConsoleClear,
}

/// What the Debug panel remembers besides the session: breakpoint extras, watches, the inputs.
#[derive(Default)]
pub struct DebugUi {
    pub meta: dm::Metas,
    pub watch: Vec<String>,
    /// Launch configuration chosen for the project (see `debug_model::LaunchConfig::id`).
    pub config: Option<String>,
    pub focus: Option<InputKind>,
    pub console: TextField,
    pub watch_field: TextField,
    pub history: Vec<String>,
    pub hist_at: Option<usize>,
    /// What the last session was started from (Restart).
    pub last: Option<LaunchConfig>,
    /// An evaluate-on-hover in flight: `(expression, anchor start, anchor end, probe position)`.
    pub hover_for: Option<(String, usize, usize, usize)>,
    /// A prelaunch build (`ghp build --debug`) running on a worker thread.
    pub build: Option<PendingBuild>,
    /// Debug Console lines of the build (shown while there is no session).
    pub build_log: Vec<String>,
}

/// A prelaunch build in flight: when it succeeds the session starts from `adapter`/`path`/`root`.
pub struct PendingBuild {
    rx: std::sync::mpsc::Receiver<Result<(), String>>,
    adapter: String,
    path: PathBuf,
    root: PathBuf,
    ov: Option<dm::LaunchOverride>,
}

/// One line of a list in the panel.
struct RowSpec {
    indent: i32,
    icon: Option<(&'static str, Rgba)>,
    /// Leave room for an icon even when there is none (aligns siblings).
    reserve: bool,
    parts: Vec<(String, Rgba)>,
    hit: Option<DebugHit>,
    bg: Option<Rgba>,
    /// Icon buttons at the right end of the row.
    actions: Vec<(&'static str, DebugHit)>,
}

impl RowSpec {
    fn text(parts: Vec<(String, Rgba)>) -> RowSpec {
        RowSpec { indent: 0, icon: None, reserve: false, parts, hit: None, bg: None, actions: Vec::new() }
    }
}

impl App {
    // ───────────── breakpoints ─────────────

    fn active_path(&self) -> Option<PathBuf> {
        self.docs.get(self.active).and_then(|d| d.path()).map(Path::to_path_buf)
    }

    pub fn toggle_breakpoint(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let (line, _) = d.buf.line_col(d.ed.head());
        self.toggle_breakpoint_at(line);
    }

    /// Toggle the breakpoint on `line` (0-based) of the active file (gutter click, F9).
    pub fn toggle_breakpoint_at(&mut self, line: usize) {
        let Some(path) = self.active_path() else { return };
        let lines = self.breakpoints.entry(path.clone()).or_default();
        if !dm::toggle_line(lines, line) {
            if let Some(m) = self.dbgui.meta.get_mut(&path) {
                m.remove(&line);
            }
            if lines.is_empty() {
                self.breakpoints.remove(&path);
            }
        }
        self.bp_changed(&path);
    }

    /// A breakpoint changed: marks, the running session and the saved state follow.
    fn bp_changed(&mut self, path: &Path) {
        if self.dbgui.meta.get(path).is_some_and(|m| m.is_empty()) {
            self.dbgui.meta.remove(path);
        }
        self.sync_breakpoint_marks();
        let specs =
            dm::specs_for(self.breakpoints.get(path).map(Vec::as_slice).unwrap_or(&[]), self.dbgui.meta.get(path));
        if let Some(d) = self.dbg.as_mut() {
            d.set_breakpoint_specs(&path.to_string_lossy(), &specs);
        }
        self.debug_save();
    }

    /// Breakpoints follow the edits of their file like bookmarks do (lines inserted above move them, a
    /// removed line takes its breakpoint along); a running session gets the new lines.
    pub fn follow_breakpoint_edits(&mut self) {
        let mut changed: Vec<PathBuf> = Vec::new();
        for d in &mut self.docs {
            if d.bp_pending.is_empty() {
                continue;
            }
            let recs = std::mem::take(&mut d.bp_pending);
            let Some(path) = d.path().map(Path::to_path_buf) else { continue };
            let Some(lines) = self.breakpoints.get_mut(&path) else { continue };
            if dm::follow_edits(lines, self.dbgui.meta.get_mut(&path), &recs) && !changed.contains(&path) {
                changed.push(path);
            }
        }
        for path in changed {
            if self.breakpoints.get(&path).is_some_and(Vec::is_empty) {
                self.breakpoints.remove(&path);
            }
            self.bp_changed(&path);
        }
    }

    pub fn clear_breakpoints(&mut self) {
        let paths: Vec<PathBuf> = self.breakpoints.keys().cloned().collect();
        self.breakpoints.clear();
        self.dbgui.meta.clear();
        self.sync_breakpoint_marks();
        if let Some(dbg) = self.dbg.as_mut() {
            for p in paths {
                dbg.set_breakpoint_specs(&p.to_string_lossy(), &[]);
            }
        }
        self.debug_save();
    }

    /// Is there a breakpoint on `line` of the active file, and is it enabled?
    pub fn breakpoint_enabled_at(&self, line: usize) -> Option<bool> {
        let path = self.active_path()?;
        if !self.breakpoints.get(&path)?.contains(&line) {
            return None;
        }
        Some(self.dbgui.meta.get(&path).and_then(|m| m.get(&line)).is_none_or(|m| m.enabled))
    }

    fn bp_ensure(&mut self, path: &Path, line: usize) {
        let lines = self.breakpoints.entry(path.to_path_buf()).or_default();
        if let Err(i) = lines.binary_search(&line) {
            lines.insert(i, line);
        }
    }

    fn bp_meta_mut(&mut self, path: &Path, line: usize) -> &mut BpMeta {
        self.dbgui.meta.entry(path.to_path_buf()).or_default().entry(line).or_default()
    }

    /// Drop the metadata entry of a breakpoint that has nothing special left.
    fn bp_tidy(&mut self, path: &Path, line: usize) {
        if let Some(m) = self.dbgui.meta.get_mut(path)
            && m.get(&line).is_some_and(BpMeta::is_plain)
        {
            m.remove(&line);
        }
    }

    /// Condition prompt answered: empty removes the condition. A missing breakpoint is created.
    pub fn bp_set_condition(&mut self, path: &Path, line: usize, text: &str) {
        let t = text.trim();
        self.bp_ensure(path, line);
        self.bp_meta_mut(path, line).condition = (!t.is_empty()).then(|| t.to_string());
        self.bp_tidy(path, line);
        self.bp_changed(path);
        self.say(if t.is_empty() { "condition removed".to_string() } else { format!("breakpoint stops when {t}") });
    }

    /// Logpoint prompt answered: empty turns it back into a plain breakpoint.
    pub fn bp_set_log(&mut self, path: &Path, line: usize, text: &str) {
        let t = text.trim();
        self.bp_ensure(path, line);
        self.bp_meta_mut(path, line).log = (!t.is_empty()).then(|| t.to_string());
        self.bp_tidy(path, line);
        self.bp_changed(path);
        self.say(if t.is_empty() { "logpoint removed".to_string() } else { format!("logpoint: {t}") });
    }

    pub fn bp_toggle_enabled(&mut self, path: &Path, line: usize) {
        if self.breakpoints.get(path).is_none_or(|l| !l.contains(&line)) {
            return;
        }
        let m = self.bp_meta_mut(path, line);
        m.enabled = !m.enabled;
        self.bp_tidy(path, line);
        self.bp_changed(path);
    }

    pub fn bp_remove(&mut self, path: &Path, line: usize) {
        if let Some(lines) = self.breakpoints.get_mut(path) {
            lines.retain(|l| *l != line);
            if lines.is_empty() {
                self.breakpoints.remove(path);
            }
        }
        if let Some(m) = self.dbgui.meta.get_mut(path) {
            m.remove(&line);
        }
        self.bp_changed(path);
    }

    /// The caret line of the active file, for the commands that act on it.
    fn caret_bp(&self) -> Option<(PathBuf, usize)> {
        let d = self.docs.get(self.active)?;
        Some((d.path()?.to_path_buf(), d.buf.line_col(d.ed.head()).0))
    }

    fn prompt_bp(&mut self, log: bool) {
        let Some((path, line)) = self.caret_bp() else {
            return self.say("save the file first: breakpoints need a path");
        };
        let meta = self.dbgui.meta.get(&path).and_then(|m| m.get(&line));
        let (kind, title, cur) = if log {
            (
                PromptKind::BpLog { path: path.clone(), line },
                format!("Logpoint at line {}: message, {{expr}} is evaluated (empty: plain breakpoint)", line + 1),
                meta.and_then(|m| m.log.clone()).unwrap_or_default(),
            )
        } else {
            (
                PromptKind::BpCondition { path: path.clone(), line },
                format!("Breakpoint at line {}: stop only when… (empty: always)", line + 1),
                meta.and_then(|m| m.condition.clone()).unwrap_or_default(),
            )
        };
        self.overlay = Overlay::Prompt(Prompt::new(kind, title, TextField::with(&cur)));
    }

    // ───────────── marks and persistence ─────────────

    /// `path -> (0-based requested line -> verified)` from the running session's answers.
    fn bp_verified(&self) -> std::collections::BTreeMap<PathBuf, std::collections::BTreeMap<usize, bool>> {
        let mut out = std::collections::BTreeMap::new();
        let Some(s) = self.dbg.as_ref().and_then(Debugger::session) else { return out };
        for (path, results) in &s.bp_results {
            out.insert(PathBuf::from(path), dm::verified_by_line(results));
        }
        out
    }

    /// Glyphs for the breakpoints of one file (same order as `lines`).
    pub fn bp_glyphs_for(&self, path: &Path, lines: &[usize]) -> Vec<BpGlyph> {
        let verified = if self.dbg.is_some() { self.bp_verified() } else { Default::default() };
        lines
            .iter()
            .map(|l| {
                let meta = self.dbgui.meta.get(path).and_then(|m| m.get(l));
                dm::glyph(meta, verified.get(path).and_then(|v| v.get(l)).copied())
            })
            .collect()
    }

    pub fn sync_breakpoint_marks(&mut self) {
        let verified = if self.dbg.is_some() { self.bp_verified() } else { Default::default() };
        let (breakpoints, metas) = (&self.breakpoints, &self.dbgui.meta);
        for d in &mut self.docs {
            let Some(path) = d.path().map(Path::to_path_buf) else {
                d.bps.clear();
                d.bp_glyphs.clear();
                continue;
            };
            d.bps = breakpoints.get(&path).cloned().unwrap_or_default();
            d.bp_glyphs = d
                .bps
                .iter()
                .map(|l| {
                    let meta = metas.get(&path).and_then(|m| m.get(l));
                    dm::glyph(meta, verified.get(&path).and_then(|v| v.get(l)).copied())
                })
                .collect();
        }
    }

    /// Load what was remembered for the project at `self.root`.
    pub fn debug_load(&mut self) {
        let st = dm::load(&self.root);
        self.breakpoints = st.lines;
        self.dbgui.meta = st.meta;
        self.dbgui.watch = st.watch;
        self.dbgui.config = st.config;
        self.sync_breakpoint_marks();
    }

    pub fn debug_save(&self) {
        dm::save(
            &self.root,
            &DebugState {
                lines: self.breakpoints.clone(),
                meta: self.dbgui.meta.clone(),
                watch: self.dbgui.watch.clone(),
                config: self.dbgui.config.clone(),
            },
        );
    }

    /// The breakpoints to send at the start of a session.
    pub fn breakpoint_specs(&self) -> Vec<(String, Vec<BpSpec>)> {
        self.breakpoints
            .iter()
            .filter_map(|(p, l)| {
                let specs = dm::specs_for(l, self.dbgui.meta.get(p));
                (!specs.is_empty()).then(|| (p.to_string_lossy().into_owned(), specs))
            })
            .collect()
    }

    // ───────────── launch configurations ─────────────

    /// The configurations that apply to the active file / the project.
    pub fn debug_detect(&self) -> Vec<LaunchConfig> {
        let file = self.active_path();
        let is_go = file.as_ref().is_some_and(|f| f.extension().is_some_and(|e| e == "go"));
        let go_main = is_go
            && self.docs.get(self.active).is_some_and(|d| {
                let head: Vec<String> = (0..d.buf.line_count().min(60)).map(|l| d.buf.line(l)).collect();
                dm::is_go_main(&head.join("\n"))
            });
        let ghp = match &file {
            Some(f) if f.extension().is_some_and(|e| e == "ghp") => Some(f.clone()),
            _ => dm::find_ghp(&self.root),
        };
        dm::detect(file.as_deref(), go_main, ghp.as_deref())
    }

    pub fn debug_select_config(&mut self) {
        let cfgs = self.debug_detect();
        if cfgs.is_empty() {
            return self.say("no launch configuration for this file or project");
        }
        let cur = dm::choose(&cfgs, self.dbgui.config.as_deref()).map(|c| c.id);
        let items: Vec<Item> = cfgs
            .iter()
            .map(|c| Item {
                label: c.label.clone(),
                detail: if Some(c.id) == cur { "selected".into() } else { String::new() },
                action: Action::DebugConfig(c.id.to_string()),
            })
            .collect();
        let sel = cfgs.iter().position(|c| Some(c.id) == cur).unwrap_or(0);
        self.overlay = Overlay::Picker(choice_picker("Debug: launch configuration", items, sel));
    }

    pub fn debug_pick_config(&mut self, id: &str) {
        self.dbgui.config = Some(id.to_string());
        self.debug_save();
        self.say(format!("launch configuration: {id} (F5 starts it)"));
    }

    fn start_config(&mut self, cfg: &LaunchConfig) {
        self.dbgui.last = Some(cfg.clone());
        match &cfg.kind {
            LaunchKind::Node => self.start_node_debug(&cfg.file),
            LaunchKind::Rust => self.start_rust_debug(&cfg.file),
            LaunchKind::Adapter(id) => self.start_adapter_debug(id, &cfg.file),
        }
    }

    // ───────────── session control ─────────────

    /// F5: continue a stopped session, or start debugging with the launch configuration that applies.
    pub fn debug_start_or_continue(&mut self) {
        if self.dbgui.build.is_some() {
            return self.say("building… (the session starts when the build finishes)");
        }
        if let Some(d) = self.dbg.as_mut() {
            match d.state() {
                State::Stopped { .. } => return d.resume(),
                State::Running | State::Idle => return,
                State::Terminated => {}
            }
            self.dbg = None; // finished: start a new one
            self.clear_debug_lines();
        }
        let cfgs = self.debug_detect();
        let Some(cfg) = dm::choose(&cfgs, self.dbgui.config.as_deref()).cloned() else {
            return self.say(
                "no debugger for this file (Go package main via delve, TS/JS via js-debug, Rust via gdb, .ghp pages via the ghp adapter)",
            );
        };
        self.start_config(&cfg);
    }

    /// Install a freshly created debugger: breakpoints are already in it.
    pub fn debug_attach(&mut self, mut d: Debugger, note: &str) {
        if let Some((dap, s)) = d.session_mut() {
            s.set_watch(self.dbgui.watch.clone(), dap);
        }
        self.dbg = Some(d);
        self.dbg_last_stop = None;
        self.panel.dbg_scroll = [0.0; 5];
        self.panel.dbg_follow = true;
        if self.config.debug_open_panel {
            self.panel.open = true;
            self.panel.tab = PanelTab::Debug;
        }
        self.sync_breakpoint_marks();
        self.say(note);
    }

    /// Debug `program` under js-debug with program `args` and node `runtime_args` (used for single tests).
    pub fn start_node_debug_args(&mut self, program: &Path, args: Vec<String>, runtime_args: Vec<String>, cwd: &Path) {
        let Some(server) = js_debug_server() else {
            return self.say("js-debug not installed: download js-debug-dap-*.tar.gz from GitHub (microsoft/vscode-js-debug) into ~/.local/share/lumen-text/js-debug");
        };
        let tweak = |v: &mut serde_json::Value| {
            v["args"] = serde_json::json!(args);
            if !runtime_args.is_empty() {
                let mut rt: Vec<serde_json::Value> = v["runtimeArgs"].as_array().cloned().unwrap_or_default();
                rt.extend(runtime_args.iter().map(|a| serde_json::json!(a)));
                v["runtimeArgs"] = serde_json::Value::Array(rt);
            }
        };
        match MultiSession::start_node_with("node", &server, program, Some(cwd), tweak, self.wake()) {
            Ok(mut ms) => {
                for (p, specs) in self.breakpoint_specs() {
                    ms.set_breakpoint_specs(&p, &specs);
                }
                ms.start();
                self.debug_attach(Debugger::Node(Box::new(ms)), "depurando o teste…");
            }
            Err(e) => self.say(format!("could not start js-debug: {e}")),
        }
    }

    fn start_node_debug(&mut self, path: &Path) {
        let Some(server) = js_debug_server() else {
            return self.say("js-debug not installed: download js-debug-dap-*.tar.gz from GitHub (microsoft/vscode-js-debug) into ~/.local/share/lumen-text/js-debug");
        };
        match MultiSession::start_node("node", &server, path, self.wake()) {
            Ok(mut ms) => {
                for (p, specs) in self.breakpoint_specs() {
                    ms.set_breakpoint_specs(&p, &specs);
                }
                ms.start();
                self.debug_attach(Debugger::Node(Box::new(ms)), "depurando…");
            }
            Err(e) => self.say(format!("could not start js-debug: {e}")),
        }
    }

    fn start_rust_debug(&mut self, path: &Path) {
        let Some(root) = path.ancestors().find(|d| d.join("Cargo.toml").exists()).map(Path::to_path_buf) else {
            return self.say("no Cargo.toml found for this file");
        };
        let name = std::fs::read_to_string(root.join("Cargo.toml")).ok().and_then(|t| cargo_package_name(&t));
        let Some(name) = name else { return self.say("Cargo.toml has no [package] name") };
        self.pending_native = Some(root.join("target/debug").join(name));
        self.run_task("cargo build", "Build for debugging");
    }

    /// An adapter of the registry (`delve`, `ghp`, ...): built first when it has a prelaunch step (on a
    /// worker thread), then started, given the breakpoints and the launch request. The project's
    /// `.lumen/launch.json` may override the program, args, env, cwd and the adapter.
    fn start_adapter_debug(&mut self, id: &str, path: &Path) {
        if self.dbgui.build.is_some() {
            return self.say("a debug build is still running");
        }
        let ovs = dm::load_launch(&self.root).unwrap_or_else(|e| {
            self.say_for(e, 6);
            Vec::new()
        });
        let ov = dm::launch_override(&ovs, id).cloned();
        let adapter_id = ov.as_ref().and_then(|o| o.adapter.clone()).unwrap_or_else(|| id.to_string());
        let reg = crate::dap_adapters::Registry::builtin();
        let Some(adapter) = reg.get(&adapter_id) else {
            return self.say(format!("no debug adapter registered: {adapter_id}"));
        };
        // dlv builds the package itself, from the module root; other adapters run from the project.
        let root = if adapter.languages.iter().any(|l| l == "go") {
            path.ancestors()
                .find(|d| d.join("go.mod").exists())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()))
        } else {
            self.root.clone()
        };
        if adapter.prelaunch.is_none() {
            return self.launch_adapter(&adapter_id, path, &root, ov.as_ref());
        }
        // A build step first (`ghp build --debug` for pages): the program to debug is its output. It runs
        // off the UI thread; `pump_debug_build` starts the session when it finishes.
        let name = if adapter.name.is_empty() { adapter.id.clone() } else { adapter.name.clone() };
        let (tx, rx) = std::sync::mpsc::channel();
        let (a, p, r, o, wake) = (adapter.clone(), path.to_path_buf(), root.clone(), ov.clone(), self.wake());
        std::thread::spawn(move || {
            let res = a.run_prelaunch_with(&p, &r, o.as_ref()).map_err(|e| e.to_string());
            let _ = tx.send(res);
            wake();
        });
        self.dbgui.build = Some(PendingBuild { rx, adapter: adapter_id, path: path.to_path_buf(), root, ov });
        self.dbgui.build_log = vec![dm::console_line(ConsoleKind::Echo, &format!("building for {name}…"))];
        self.open_debug_panel();
        self.say(format!("building for {name}…"));
    }

    /// The build thread finished: start the session, or show why the build failed in the Debug Console.
    fn pump_debug_build(&mut self) {
        let res = match self.dbgui.build.as_ref().map(|b| b.rx.try_recv()) {
            None | Some(Err(std::sync::mpsc::TryRecvError::Empty)) => return,
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => Err("the build stopped unexpectedly".to_string()),
            Some(Ok(r)) => r,
        };
        let Some(b) = self.dbgui.build.take() else { return };
        self.request_redraw();
        match res {
            Ok(()) => {
                self.dbgui.build_log.push(dm::console_line(ConsoleKind::Result, "build finished"));
                self.launch_adapter(&b.adapter, &b.path, &b.root, b.ov.as_ref());
            }
            Err(msg) => {
                for l in msg.lines() {
                    self.dbgui.build_log.push(dm::console_line(ConsoleKind::Error, l));
                }
                self.open_debug_panel();
                self.say_for(format!("debug build failed: {}", msg.lines().next().unwrap_or("")), 6);
            }
        }
    }

    /// Start adapter `id` on `path` (the build, if any, is done) and attach it.
    fn launch_adapter(&mut self, id: &str, path: &Path, root: &Path, ov: Option<&dm::LaunchOverride>) {
        let reg = crate::dap_adapters::Registry::builtin();
        let Some(adapter) = reg.get(id) else { return self.say(format!("no debug adapter registered: {id}")) };
        let mut dap = match adapter.start(root, self.wake()) {
            Ok(d) => d,
            Err(e) => return self.say(format!("could not start {}: {e}", adapter.command)),
        };
        let mut session = adapter.session_with(path, root, &[], ov);
        for (p, specs) in self.breakpoint_specs() {
            session.set_breakpoint_specs(&p, specs, &mut dap);
        }
        session.start(&mut dap);
        let note = format!("debugging with {}…", if adapter.name.is_empty() { &adapter.id } else { &adapter.name });
        self.debug_attach(Debugger::Native { dap: Box::new(dap), session: Box::new(session) }, &note);
    }

    /// Called when `cargo build` finished successfully.
    pub fn start_native_debug(&mut self, program: PathBuf) {
        let cwd = program
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone());
        self.start_native_debug_args(program, Vec::new(), cwd);
    }

    /// gdb on `program` with `args`, started in `cwd` (a single test passes its name and `--exact`).
    pub fn start_native_debug_args(&mut self, program: PathBuf, args: Vec<String>, cwd: PathBuf) {
        if !program.exists() {
            return self.say(format!("binary not found: {}", program.display()));
        }
        let mut dap = match Dap::spawn("gdb", &["--interpreter=dap"], &cwd, self.wake()) {
            Ok(d) => d,
            Err(e) => return self.say(format!("gdb unavailable: {e}")),
        };
        let mut session = Session::new(&program.to_string_lossy(), &args, &cwd, false);
        for (p, specs) in self.breakpoint_specs() {
            session.set_breakpoint_specs(&p, specs, &mut dap);
        }
        session.start(&mut dap);
        self.debug_attach(Debugger::Native { dap: Box::new(dap), session: Box::new(session) }, "debugging with gdb…");
    }

    fn clear_debug_lines(&mut self) {
        for doc in &mut self.docs {
            doc.debug_line = None;
        }
        self.dbg_last_stop = None;
    }

    pub fn debug_stop(&mut self) {
        if self.dbgui.build.take().is_some() {
            // The build process runs to its end; its result is dropped.
            self.dbgui.build_log.push(dm::console_line(ConsoleKind::Error, "build cancelled"));
            return self.say("debug build cancelled");
        }
        if let Some(mut d) = self.dbg.take() {
            d.stop();
        }
        self.clear_debug_lines();
        self.sync_breakpoint_marks();
        self.say("debug session ended");
    }

    pub fn debug_restart(&mut self) {
        let Some(cfg) = self.dbgui.last.clone() else {
            return self.say("nothing to restart: start a session with F5 first");
        };
        if self.dbgui.build.is_some() {
            return;
        }
        if let Some(mut d) = self.dbg.take() {
            d.stop();
        }
        self.clear_debug_lines();
        self.start_config(&cfg);
    }

    pub fn debug_pause(&mut self) {
        if let Some(d) = self.dbg.as_mut()
            && d.state() == State::Running
        {
            d.pause();
        }
    }

    pub fn debug_step(&mut self, kind: char) {
        let Some(d) = self.dbg.as_mut() else { return };
        if !matches!(d.state(), State::Stopped { .. }) {
            return;
        }
        match kind {
            'o' => d.step_over(),
            'i' => d.step_in(),
            _ => d.step_out(),
        }
    }

    /// Can the toolbar button do something right now?
    pub fn tool_enabled(&self, t: Tool) -> bool {
        let state = self.dbg.as_ref().map(Debugger::state);
        let stopped = matches!(state, Some(State::Stopped { .. }));
        let building = self.dbgui.build.is_some();
        match t {
            Tool::Continue => !building && (state.is_none() || stopped || state == Some(State::Terminated)),
            Tool::Over | Tool::Into | Tool::Out => stopped,
            Tool::Pause => state == Some(State::Running),
            Tool::Restart => !building && self.dbgui.last.is_some(),
            Tool::Stop => building || state.is_some_and(|s| s != State::Terminated),
        }
    }

    pub fn debug_tool(&mut self, t: Tool) {
        if !self.tool_enabled(t) {
            return;
        }
        match t {
            Tool::Continue => self.debug_start_or_continue(),
            Tool::Over => self.debug_step('o'),
            Tool::Into => self.debug_step('i'),
            Tool::Out => self.debug_step('u'),
            Tool::Pause => self.debug_pause(),
            Tool::Restart => self.debug_restart(),
            Tool::Stop => self.debug_stop(),
        }
    }

    /// Follow the adapter: when it stops, show the selected frame in the editor.
    pub fn pump_debug(&mut self) {
        self.pump_debug_build();
        self.follow_breakpoint_edits();
        let Some(d) = self.dbg.as_mut() else { return };
        if !d.pump() {
            return;
        }
        let state = d.state();
        let hover = d.session_mut().and_then(|(_, s)| s.hover_result.take());
        let watch_stale = d.session().is_some_and(|s| s.watch != self.dbgui.watch);
        let target = d.session().and_then(|s| s.selected_frame().cloned());
        match state {
            State::Stopped { .. } => {
                if watch_stale {
                    self.debug_sync_watch();
                }
                if let Some(f) = target
                    && let Some(p) = f.path.as_deref()
                {
                    let line = f.line.saturating_sub(1);
                    let key = (p.to_string(), line);
                    if self.dbg_last_stop.as_ref() != Some(&key) {
                        self.dbg_last_stop = Some(key);
                        self.panel.dbg_scroll[0] = 0.0;
                        self.panel.dbg_scroll[1] = 0.0;
                        self.debug_show_line(p, line);
                    }
                }
            }
            State::Terminated => {
                self.clear_debug_lines();
                self.say("execution finished");
            }
            _ => self.clear_debug_lines(),
        }
        if let Some((expr, res)) = hover {
            self.debug_hover_answer(expr, res);
        }
        self.sync_breakpoint_marks();
        self.request_redraw();
    }

    /// Open `path` at 0-based `line` and mark it as the execution line.
    fn debug_show_line(&mut self, path: &str, line: usize) {
        if !Path::new(path).exists() {
            return;
        }
        self.open_path(Path::new(path), Some((line, 0)));
        for doc in &mut self.docs {
            doc.debug_line = None;
        }
        if let Some(i) = self.docs.iter().position(|doc| doc.path() == Some(Path::new(path))) {
            self.docs[i].debug_line = Some(line);
        }
    }

    // ───────────── hover to evaluate ─────────────

    /// While stopped, the pointer resting on an expression asks the adapter for its value.
    /// False when this does not apply (the language server's hover runs instead).
    pub fn debug_hover_request(&mut self, at: usize) -> bool {
        if !self.config.debug_hover_evaluate {
            return false;
        }
        let Some(d) = self.docs.get(self.active) else { return false };
        let (line, col) = d.buf.line_col(at);
        let text = d.buf.line(line);
        let Some((s, e)) = dm::expr_at(text.trim_end_matches(['\n', '\r']), col) else { return false };
        let expr: String = text.chars().skip(s).take(e - s).collect();
        let (a, b) = (d.buf.pos(line, s), d.buf.pos(line, e));
        let Some((dap, sess)) = self.dbg.as_mut().and_then(Debugger::session_mut) else { return false };
        if !matches!(sess.state, State::Stopped { .. }) {
            return false;
        }
        sess.evaluate_hover(&expr, dap);
        self.dbgui.hover_for = Some((expr, a, b, at));
        true
    }

    fn debug_hover_answer(&mut self, expr: String, res: Result<String, String>) {
        let Some((want, a, b, at)) = self.dbgui.hover_for.take() else { return };
        if want != expr {
            return;
        }
        match res {
            Ok(v) => {
                let md = format!("```\n{expr} = {v}\n```");
                self.hover_show_markdown(&md, (a, b));
            }
            // Not a value (a type name, a package): the language server may know it.
            Err(_) => self.request_hover_lsp(at),
        }
    }

    // ───────────── watch and console ─────────────

    fn debug_sync_watch(&mut self) {
        let list = self.dbgui.watch.clone();
        if let Some((dap, s)) = self.dbg.as_mut().and_then(Debugger::session_mut) {
            s.set_watch(list, dap);
        }
    }

    pub fn add_watch(&mut self, expr: &str) {
        let e = expr.trim();
        if e.is_empty() || self.dbgui.watch.iter().any(|w| w == e) {
            return;
        }
        self.dbgui.watch.push(e.to_string());
        self.debug_save();
        self.debug_sync_watch();
    }

    pub fn remove_watch(&mut self, i: usize) {
        if i < self.dbgui.watch.len() {
            self.dbgui.watch.remove(i);
            self.debug_save();
            self.debug_sync_watch();
        }
    }

    fn open_debug_panel(&mut self) {
        self.panel.open = true;
        self.panel.tab = PanelTab::Debug;
    }

    fn debug_submit(&mut self, kind: InputKind) {
        match kind {
            InputKind::Console => {
                let t = std::mem::take(&mut self.dbgui.console.text).trim().to_string();
                self.dbgui.console = TextField::default();
                self.dbgui.hist_at = None;
                if t.is_empty() {
                    return;
                }
                if self.dbgui.history.last() != Some(&t) {
                    self.dbgui.history.push(t.clone());
                }
                match self.dbg.as_mut().and_then(Debugger::session_mut) {
                    Some((dap, s)) => s.evaluate_repl(&t, dap),
                    None => self.say("Debug Console: evaluate works while a program is stopped at a breakpoint"),
                }
                self.panel.dbg_follow = true;
            }
            InputKind::Watch => {
                let t = std::mem::take(&mut self.dbgui.watch_field.text);
                self.dbgui.watch_field = TextField::default();
                self.dbgui.focus = None;
                self.add_watch(&t);
            }
        }
    }

    /// Keys while the Debug Console or the watch input has the focus. Function keys and Ctrl
    /// shortcuts other than paste / select-all fall through to the global bindings.
    pub fn debug_key(&mut self, ev: &KeyIn) -> bool {
        let Some(kind) = self.dbgui.focus else { return false };
        if !self.panel.open || self.panel.tab != PanelTab::Debug {
            self.dbgui.focus = None;
            return false;
        }
        let (ctrl, alt) = (self.mods.control_key(), self.mods.alt_key());
        match &ev.key {
            Key::Named(
                NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::CapsLock,
            ) => return true,
            Key::Named(NamedKey::Escape) => {
                self.dbgui.focus = None;
                return true;
            }
            Key::Named(NamedKey::Enter) => {
                self.debug_submit(kind);
                return true;
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) if kind == InputKind::Console => {
                let up = matches!(&ev.key, Key::Named(NamedKey::ArrowUp));
                self.debug_history(up);
                return true;
            }
            _ => {}
        }
        let mut paste = None;
        if ctrl && let Key::Character(c) = &ev.key {
            match c.to_lowercase().as_str() {
                "v" => paste = Some(self.clip.get()),
                "a" => {
                    self.debug_field(kind).select_all();
                    return true;
                }
                _ => return false,
            }
        }
        if let Key::Named(n) = &ev.key
            && !matches!(
                n,
                NamedKey::Backspace
                    | NamedKey::Delete
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::Home
                    | NamedKey::End
                    | NamedKey::Space
                    | NamedKey::Tab
                    | NamedKey::ArrowUp
                    | NamedKey::ArrowDown
            )
        {
            return false; // F5, F10...: the global bindings
        }
        let key = ev.key.clone();
        let field = self.debug_field(kind);
        match &key {
            Key::Named(NamedKey::Backspace) => {
                if ctrl {
                    field.delete_word();
                } else {
                    field.backspace();
                }
            }
            Key::Named(NamedKey::Delete) => {
                field.delete();
            }
            Key::Named(NamedKey::ArrowLeft) => field.left(),
            Key::Named(NamedKey::ArrowRight) => field.right(),
            Key::Named(NamedKey::Home) => field.home(),
            Key::Named(NamedKey::End) => field.end(),
            _ => {
                if let Some(t) = paste {
                    field.insert(&t.replace('\n', " "));
                } else if !ctrl && !alt {
                    if let Some(t) = &ev.text {
                        field.insert(t);
                    }
                } else {
                    return false;
                }
            }
        }
        true
    }

    fn debug_field(&mut self, kind: InputKind) -> &mut TextField {
        match kind {
            InputKind::Console => &mut self.dbgui.console,
            InputKind::Watch => &mut self.dbgui.watch_field,
        }
    }

    /// Up / Down in the console input: earlier lines.
    fn debug_history(&mut self, up: bool) {
        let n = self.dbgui.history.len();
        if n == 0 {
            return;
        }
        let at = match (self.dbgui.hist_at, up) {
            (None, true) => Some(n - 1),
            (None, false) => None,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) => (i + 1 < n).then_some(i + 1),
        };
        self.dbgui.hist_at = at;
        self.dbgui.console = match at {
            Some(i) => TextField::with(&self.dbgui.history[i]),
            None => TextField::default(),
        };
    }

    // ───────────── clicks ─────────────

    /// Click in the debug panel body.
    pub fn debug_click(&mut self, x: f64, y: f64) -> bool {
        let Some(hit) = self.layout.debug_hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone())
        else {
            return false;
        };
        self.debug_hit(hit);
        true
    }

    pub fn debug_hit(&mut self, hit: DebugHit) {
        match hit {
            DebugHit::Tool(t) => self.debug_tool(t),
            DebugHit::Config => self.debug_select_config(),
            DebugHit::Thread(id) => {
                if let Some((dap, s)) = self.dbg.as_mut().and_then(Debugger::session_mut) {
                    s.toggle_thread(id, dap);
                }
            }
            DebugHit::Frame(t, i) => {
                let mut frame = None;
                if let Some((dap, s)) = self.dbg.as_mut().and_then(Debugger::session_mut) {
                    s.select_frame(t, i, dap);
                    frame = s.thread_frames.get(&t).and_then(|v| v.get(i)).cloned();
                }
                if let Some(f) = frame
                    && let Some(p) = f.path.as_deref()
                {
                    let line = f.line.saturating_sub(1);
                    self.dbg_last_stop = Some((p.to_string(), line));
                    self.debug_show_line(p, line);
                }
            }
            DebugHit::Var { key, vars_ref, scope } => {
                if let Some((dap, s)) = self.dbg.as_mut().and_then(Debugger::session_mut) {
                    match scope {
                        Some(expensive) => s.toggle_scope(&key, vars_ref, expensive, dap),
                        None => s.toggle_var(&key, vars_ref, dap),
                    }
                }
            }
            DebugHit::Bp(path, line) => {
                self.open_path(&path, Some((line, 0)));
            }
            DebugHit::BpToggle(path, line) => self.bp_toggle_enabled(&path, line),
            DebugHit::BpRemove(path, line) => self.bp_remove(&path, line),
            DebugHit::BpClearAll => self.clear_breakpoints(),
            DebugHit::WatchAdd | DebugHit::WatchInput => self.dbgui.focus = Some(InputKind::Watch),
            DebugHit::WatchRemove(i) => self.remove_watch(i),
            DebugHit::ConsoleInput => self.dbgui.focus = Some(InputKind::Console),
            DebugHit::ConsoleClear => {
                if let Some(Debugger::Native { session, .. }) = self.dbg.as_mut() {
                    session.output.clear();
                } else if let Some(Debugger::Node(m)) = self.dbg.as_mut() {
                    m.clear_output();
                }
            }
        }
    }

    /// Hover tint (rect, radius) for the list row under the pointer.
    pub fn debug_hover_target(&self) -> Option<(Rect, i32)> {
        if !self.panel.open || self.panel.tab != PanelTab::Debug || !self.overlay.is_none() {
            return None;
        }
        let (x, y) = self.mouse;
        let (r, h) = self.layout.debug_hits.iter().rev().find(|(r, _)| r.contains(x, y))?;
        match h {
            DebugHit::Tool(_) | DebugHit::ConsoleInput | DebugHit::WatchInput => None,
            _ => Some((*r, 5)),
        }
    }

    /// Toolbar button under (x, y) for the tooltip.
    pub fn debug_tip_at(&self, x: f64, y: f64) -> Option<(Tool, Rect)> {
        if !self.panel.open || self.panel.tab != PanelTab::Debug {
            return None;
        }
        self.layout.debug_hits.iter().find_map(|(r, h)| match h {
            DebugHit::Tool(t) if r.contains(x, y) => Some((*t, *r)),
            _ => None,
        })
    }

    pub fn debug_tip(&self, t: Tool) -> crate::app_tooltip::TipText {
        let (title, id, detail) = match t {
            Tool::Continue if self.dbg.is_none() => {
                ("Start debugging", "DebugStart", "Runs the selected launch configuration")
            }
            Tool::Continue => ("Continue", "DebugStart", "Runs until the next breakpoint"),
            Tool::Over => ("Step over", "StepOver", "Runs the current line without entering calls"),
            Tool::Into => ("Step into", "StepInto", "Enters the call on the current line"),
            Tool::Out => ("Step out", "StepOut", "Runs until the current function returns"),
            Tool::Pause => ("Pause", "DebugPause", "Interrupts the running program"),
            Tool::Restart => ("Restart", "DebugRestart", "Stops the session and starts it again"),
            Tool::Stop => ("Stop", "DebugStop", "Ends the session and the program"),
        };
        crate::app_tooltip::TipText {
            title: title.to_string(),
            keys: self.shortcut_text(id),
            detail: Some(detail.to_string()),
            more: Vec::new(),
        }
    }

    pub fn run_debug_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::DebugStart => self.debug_start_or_continue(),
            Cmd::DebugStop => self.debug_stop(),
            Cmd::ToggleBreakpoint => self.toggle_breakpoint(),
            Cmd::ClearBreakpoints => self.clear_breakpoints(),
            Cmd::StepOver => self.debug_step('o'),
            Cmd::StepInto => self.debug_step('i'),
            Cmd::StepOut => self.debug_step('u'),
            Cmd::DebugPause => self.debug_pause(),
            Cmd::DebugRestart => self.debug_restart(),
            Cmd::DebugSelectConfig => self.debug_select_config(),
            Cmd::DebugAddWatch => {
                self.open_debug_panel();
                self.dbgui.focus = Some(InputKind::Watch);
            }
            Cmd::DebugConsole => {
                self.open_debug_panel();
                self.dbgui.focus = Some(InputKind::Console);
            }
            Cmd::DebugBpCondition => self.prompt_bp(false),
            Cmd::DebugBpLog => self.prompt_bp(true),
            Cmd::DebugBpToggleEnabled => match self.caret_bp() {
                Some((p, l)) => self.bp_toggle_enabled(&p, l),
                None => self.say("no breakpoint here"),
            },
            _ => {}
        }
    }

    // ───────────── drawing ─────────────

    pub fn draw_debug(&mut self, cv: &mut Canvas, body: Rect) {
        self.layout.debug_hits.clear();
        let c = self.theme.chrome;
        let bar = Rect::new(body.x, body.y, body.w, TOOLBAR_H);
        self.draw_debug_toolbar(cv, bar);
        cv.fill(Rect::new(body.x, bar.bottom() - 1, body.w, 1), c.border);
        let content = Rect::new(body.x, bar.bottom(), body.w, (body.bottom() - bar.bottom()).max(0));
        let w3 = content.w / 3;
        let cols = [
            Rect::new(content.x, content.y, w3, content.h),
            Rect::new(content.x + w3, content.y, w3, content.h),
            Rect::new(content.x + 2 * w3, content.y, content.w - 2 * w3, content.h),
        ];
        for col in &cols[1..] {
            cv.fill(Rect::new(col.x, col.y, 1, col.h), c.border);
        }
        let top_h = content.h * 3 / 5;
        let split =
            |col: Rect| (Rect::new(col.x, col.y, col.w, top_h), Rect::new(col.x, col.y + top_h, col.w, col.h - top_h));
        let ((stack_r, bps_r), (vars_r, watch_r)) = (split(cols[0]), split(cols[1]));
        for r in [bps_r, watch_r] {
            cv.fill(Rect::new(r.x, r.y, r.w, 1), c.border);
        }

        let rows = self.debug_stack_rows();
        self.debug_section(cv, stack_r, "CALL STACK", &[], Reg::DebugStack, 0, rows, 0);
        let rows = self.debug_var_rows();
        self.debug_section(cv, vars_r, "VARIABLES", &[], Reg::DebugVars, 1, rows, 0);

        let rows = self.debug_bp_rows();
        let btns: Vec<(&'static str, DebugHit)> =
            if self.breakpoints.is_empty() { Vec::new() } else { vec![("ui/trash", DebugHit::BpClearAll)] };
        self.debug_section(cv, bps_r, "BREAKPOINTS", &btns, Reg::DebugBps, 3, rows, 0);

        let rows = self.debug_watch_rows();
        let foot = if self.dbgui.focus == Some(InputKind::Watch) { line_h() as i32 + 8 } else { 0 };
        let area = self.debug_section(
            cv,
            watch_r,
            "WATCH",
            &[("ui/plus", DebugHit::WatchAdd)],
            Reg::DebugWatch,
            4,
            rows,
            foot,
        );
        if foot > 0 {
            let r = Rect::new(area.x, area.bottom(), area.w, foot);
            let (text, cur) = (self.dbgui.watch_field.text.clone(), self.dbgui.watch_field.cursor);
            self.draw_debug_input(cv, r, &text, cur, true, "Expression to watch", DebugHit::WatchInput);
        }

        let rows = self.debug_console_rows();
        let foot = line_h() as i32 + 10;
        let area = self.debug_section(
            cv,
            cols[2],
            "DEBUG CONSOLE",
            &[("ui/close", DebugHit::ConsoleClear)],
            Reg::DebugOut,
            2,
            rows,
            foot,
        );
        let r = Rect::new(area.x, area.bottom(), area.w, foot);
        let (text, cur) = (self.dbgui.console.text.clone(), self.dbgui.console.cursor);
        let focused = self.dbgui.focus == Some(InputKind::Console);
        self.draw_debug_input(cv, r, &text, cur, focused, "Evaluate an expression", DebugHit::ConsoleInput);
    }

    fn draw_debug_toolbar(&mut self, cv: &mut Canvas, bar: Rect) {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let idle = self.dbg.as_ref().is_none_or(|d| d.state() == State::Terminated);
        let tools = [
            (Tool::Continue, if idle { "ui/play_filled" } else { "ui/debug_continue" }),
            (Tool::Over, "ui/debug_step_over"),
            (Tool::Into, "ui/debug_step_into"),
            (Tool::Out, "ui/debug_step_out"),
            (Tool::Pause, "ui/debug_pause"),
            (Tool::Restart, "ui/rerun"),
            (Tool::Stop, "ui/stop"),
        ];
        let (mx, my) = self.mouse;
        let mut x = bar.x + 8;
        for (t, icon) in tools {
            let on = self.tool_enabled(t);
            let col = if !on {
                dim.with_alpha(0x70)
            } else {
                match t {
                    Tool::Stop => c.error,
                    Tool::Continue => c.git_added,
                    _ => fg,
                }
            };
            let r = Rect::new(x, bar.y + 3, 26, TOOLBAR_H - 6);
            if on && r.contains(mx, my) {
                cv.fill_round(r, 5, fg.with_alpha(0x18));
            }
            self.tx.icon(cv, icon, x + 5, bar.y + 7, 16, col, bar);
            self.layout.debug_hits.push((r, DebugHit::Tool(t)));
            x += 28;
        }
        x += 6;
        cv.fill(Rect::new(x, bar.y + 7, 1, TOOLBAR_H - 14), c.border);
        x += 10;
        // The launch configuration chip.
        let label = match (&self.dbgui.last, &self.dbgui.config) {
            (Some(l), _) => l.label.clone(),
            (None, Some(id)) => format!("{id} (selected)"),
            (None, None) => "Launch: auto-detect".to_string(),
        };
        let w = self.tx.ui_width(&label, false);
        let chip = Rect::new(x - 4, bar.y + 3, w + 34, TOOLBAR_H - 6);
        if chip.contains(mx, my) {
            cv.fill_round(chip, 5, fg.with_alpha(0x18));
        }
        self.tx.draw_ui(cv, x, bar.y + 7, &label, fg, bar, false);
        self.tx.icon(cv, "ui/chevron_down", x + w + 6, bar.y + 9, 12, dim, bar);
        self.layout.debug_hits.push((chip, DebugHit::Config));
        // State at the right.
        let status = self.debug_status_text();
        if !status.is_empty() {
            let sw = self.tx.ui_width(&status, false);
            let sx = (bar.right() - sw - 14).max(chip.right() + 10);
            self.tx.draw_ui(cv, sx, bar.y + 7, &status, dim, bar, false);
        }
    }

    fn debug_status_text(&self) -> String {
        if self.dbgui.build.is_some() {
            return "Building…".into();
        }
        let Some(s) = self.dbg.as_ref().and_then(Debugger::session) else { return String::new() };
        match &s.state {
            State::Idle => "Starting…".into(),
            State::Running => "Running".into(),
            State::Terminated => "Finished".into(),
            State::Stopped { .. } => match s.last_stop.as_ref().map(|l| l.reason.as_str()) {
                Some("breakpoint") => "Paused on breakpoint".into(),
                Some("exception") => "Paused on exception".into(),
                Some("step") => "Paused after step".into(),
                Some("pause") => "Paused".into(),
                Some(r) if !r.is_empty() => format!("Paused: {r}"),
                _ => "Paused".into(),
            },
        }
    }

    fn debug_stack_rows(&self) -> Vec<RowSpec> {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let hint = |t: &str| vec![RowSpec::text(vec![(t.to_string(), dim)])];
        let Some(s) = self.dbg.as_ref().and_then(Debugger::session) else {
            return hint("Not debugging. F5 starts the selected configuration.");
        };
        match s.state {
            State::Running => return hint("Running… F6 pauses."),
            State::Terminated => return hint("The program finished."),
            State::Idle => return hint("Starting…"),
            State::Stopped { .. } => {}
        }
        let multi = s.threads.len() > 1;
        dm::stack_rows(s)
            .into_iter()
            .map(|r| match r {
                StackRow::Thread { id, name, open, current } => RowSpec {
                    indent: 0,
                    icon: Some((if open { "ui/chevron_down" } else { "ui/chevron_right" }, dim)),
                    reserve: true,
                    parts: vec![(format!("{name} #{id}"), if current { fg } else { dim })],
                    hit: Some(DebugHit::Thread(id)),
                    bg: None,
                    actions: Vec::new(),
                },
                StackRow::Frame { thread, idx, frame, selected } => RowSpec {
                    indent: if multi { 14 } else { 0 },
                    icon: None,
                    reserve: false,
                    parts: vec![(dm::frame_label(&frame), if selected { fg } else { dim })],
                    hit: Some(DebugHit::Frame(thread, idx)),
                    bg: selected.then(|| c.accent.with_alpha(0x28)),
                    actions: Vec::new(),
                },
            })
            .collect()
    }

    fn debug_var_rows(&self) -> Vec<RowSpec> {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let Some(s) = self.dbg.as_ref().and_then(Debugger::session) else {
            return vec![RowSpec::text(vec![("Variables appear when the program stops.".into(), dim)])];
        };
        if !matches!(s.state, State::Stopped { .. }) {
            return vec![RowSpec::text(vec![("The program is not stopped.".into(), dim)])];
        }
        let rows = dm::flatten(s);
        if rows.is_empty() {
            return vec![RowSpec::text(vec![("Loading…".into(), dim)])];
        }
        rows.into_iter().map(|r| self.tree_row_spec(r)).collect()
    }

    /// A Variables / Watch tree row: indent by depth, a chevron when it has children.
    fn tree_row_spec(&self, r: dm::TreeRow) -> RowSpec {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let foldable = r.scope.is_some() || r.expandable();
        let parts = if r.scope.is_some() {
            vec![(r.name.clone(), dim)]
        } else {
            let mut p = vec![(r.name.clone(), c.accent_text), (" = ".to_string(), dim), (r.value.clone(), fg)];
            if r.loading {
                p.push(("  …".into(), dim));
            }
            p
        };
        RowSpec {
            indent: r.depth as i32 * 14,
            icon: foldable.then_some((if r.open { "ui/chevron_down" } else { "ui/chevron_right" }, dim)),
            reserve: true,
            parts,
            hit: foldable.then(|| DebugHit::Var { key: r.key.clone(), vars_ref: r.vars_ref, scope: r.scope }),
            bg: None,
            actions: Vec::new(),
        }
    }

    fn debug_bp_rows(&self) -> Vec<RowSpec> {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        if self.breakpoints.is_empty() {
            return vec![RowSpec::text(vec![("F9 or a click in the gutter sets one.".into(), dim)])];
        }
        let verified = if self.dbg.is_some() { self.bp_verified() } else { Default::default() };
        let mut rows = Vec::new();
        for (path, lines) in &self.breakpoints {
            let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            for &l in lines {
                let meta = self.dbgui.meta.get(path).and_then(|m| m.get(&l));
                let g = dm::glyph(meta, verified.get(path).and_then(|v| v.get(&l)).copied());
                let (icon, col) = match g {
                    BpGlyph::Normal | BpGlyph::Conditional => ("ui/debug_breakpoint", c.error),
                    BpGlyph::Log => ("ui/debug_log_breakpoint", c.accent),
                    BpGlyph::Unverified => ("ui/debug_disabled_breakpoint", c.error),
                    BpGlyph::Disabled => ("ui/debug_disabled_breakpoint", dim),
                };
                let mut parts = vec![(format!("{file}:{}", l + 1), if g == BpGlyph::Disabled { dim } else { fg })];
                if let Some(m) = meta {
                    if let Some(t) = &m.log {
                        parts.push((format!("   log: {t}"), dim));
                    } else if let Some(t) = &m.condition {
                        parts.push((format!("   if {t}"), dim));
                    }
                }
                if g == BpGlyph::Unverified {
                    parts.push(("   not placed by the adapter".into(), dim));
                }
                let enabled = meta.is_none_or(|m| m.enabled);
                rows.push(RowSpec {
                    indent: 0,
                    icon: Some((icon, col)),
                    reserve: true,
                    parts,
                    hit: Some(DebugHit::Bp(path.clone(), l)),
                    bg: None,
                    actions: vec![
                        (if enabled { "ui/eye" } else { "ui/eye_off" }, DebugHit::BpToggle(path.clone(), l)),
                        ("ui/close", DebugHit::BpRemove(path.clone(), l)),
                    ],
                });
            }
        }
        rows
    }

    fn debug_watch_rows(&self) -> Vec<RowSpec> {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        if self.dbgui.watch.is_empty() {
            return vec![RowSpec::text(vec![("+ adds an expression to watch.".into(), dim)])];
        }
        let Some(s) = self.dbg.as_ref().and_then(Debugger::session) else {
            return self
                .dbgui
                .watch
                .iter()
                .enumerate()
                .map(|(i, e)| RowSpec {
                    actions: vec![("ui/close", DebugHit::WatchRemove(i))],
                    ..RowSpec::text(vec![
                        (e.clone(), c.accent_text),
                        (" = ".to_string(), dim),
                        ("not available".into(), dim),
                    ])
                })
                .collect();
        };
        // Structured values expand like variables (their children are fetched on demand).
        dm::flatten_watch(s)
            .into_iter()
            .map(|(i, r)| {
                if r.depth > 0 {
                    return self.tree_row_spec(r);
                }
                let mut spec = self.tree_row_spec(r.clone());
                spec.indent = 0;
                spec.parts[2].1 = match s.watch_results.get(&r.name) {
                    Some(Err(_)) => c.error,
                    Some(Ok(_)) => fg,
                    None => dim,
                };
                spec.actions = vec![("ui/close", DebugHit::WatchRemove(i))];
                spec
            })
            .collect()
    }

    fn debug_console_rows(&self) -> Vec<RowSpec> {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let lines: &[String] = match self.dbg.as_ref() {
            Some(d) => d.output(),
            None if !self.dbgui.build_log.is_empty() => &self.dbgui.build_log,
            None => return vec![RowSpec::text(vec![("Program output and evaluations show here.".into(), dim)])],
        };
        lines
            .iter()
            .map(|l| {
                let (kind, text) = dm::classify(l);
                let (prefix, col) = match kind {
                    ConsoleKind::Stdout => ("", fg),
                    ConsoleKind::Stderr => ("", c.warning),
                    ConsoleKind::Echo => ("› ", c.accent_text),
                    ConsoleKind::Result => ("", c.git_added),
                    ConsoleKind::Error => ("", c.error),
                };
                RowSpec::text(vec![(format!("{prefix}{}", text.trim_end()), col)])
            })
            .collect()
    }

    /// One titled, scrollable list. `foot` px at the bottom are left for a fixed input row.
    /// Returns the list area.
    #[allow(clippy::too_many_arguments)]
    fn debug_section(
        &mut self,
        cv: &mut Canvas,
        rect: Rect,
        title: &str,
        head: &[(&'static str, DebugHit)],
        reg: Reg,
        si: usize,
        rows: Vec<RowSpec>,
        foot: i32,
    ) -> Rect {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let lh = line_h() as i32;
        self.tx.draw_ui(cv, rect.x + 12, rect.y + 5, title, dim, rect, true);
        for (k, (icon, hit)) in head.iter().enumerate() {
            let bx = rect.right() - 26 - 22 * k as i32;
            let r = Rect::new(bx - 3, rect.y + 2, 20, SEC_H - 4);
            if r.contains(self.mouse.0, self.mouse.1) {
                cv.fill_round(r, 4, fg.with_alpha(0x18));
            }
            self.tx.icon(cv, icon, bx, rect.y + 5, 14, dim, rect);
            self.layout.debug_hits.push((r, hit.clone()));
        }
        let area = Rect::new(rect.x, rect.y + SEC_H, rect.w, (rect.h - SEC_H - foot).max(0));
        let total = rows.len() as f64 * lh as f64 + 8.0;
        let max = (total - area.h as f64).max(0.0);
        if reg == Reg::DebugOut && self.panel.dbg_follow {
            self.panel.dbg_scroll[si] = max;
        }
        let scroll = self.panel.dbg_scroll[si].clamp(0.0, max);
        self.panel.dbg_scroll[si] = scroll;
        let first = (scroll / lh as f64) as usize;
        let off = -((scroll as i32) % lh);
        let text_clip = Rect::new(area.x, area.y, (area.w - 10).max(0), area.h);
        for (i, row) in rows.iter().enumerate().skip(first).take((area.h / lh + 2).max(0) as usize) {
            let y = area.y + off + (i - first) as i32 * lh;
            let rr = Rect::new(area.x, y, area.w, lh).intersect(&area);
            if rr.w <= 0 || rr.h <= 0 {
                continue;
            }
            if let Some(bg) = row.bg {
                cv.fill_round(Rect::new(area.x + 4, y, area.w - 8, lh).intersect(&area), 4, bg);
            }
            let mut x = area.x + 12 + row.indent;
            if let Some((icon, col)) = row.icon {
                self.tx.icon(cv, icon, x, y + (lh - 12) / 2, 12, col, area);
            }
            if row.icon.is_some() || row.reserve {
                x += 16;
            }
            self.tx.draw(cv, x, y, &row.parts, text_clip);
            if let Some(h) = &row.hit {
                self.layout.debug_hits.push((rr, h.clone()));
            }
            for (k, (icon, hit)) in row.actions.iter().enumerate() {
                let ax = area.right() - 30 - 18 * k as i32;
                let ar = Rect::new(ax - 2, y, 16, lh).intersect(&area);
                if ar.contains(self.mouse.0, self.mouse.1) {
                    self.tx.icon(cv, icon, ax, y + (lh - 12) / 2, 12, fg, area);
                } else {
                    self.tx.icon(cv, icon, ax, y + (lh - 12) / 2, 12, dim, area);
                }
                self.layout.debug_hits.push((ar, hit.clone()));
            }
        }
        self.region_bar(cv, reg, area, total, area.h as f64, scroll, 1.0, false);
        area
    }

    /// A one-line input at the bottom of a section.
    #[allow(clippy::too_many_arguments)]
    fn draw_debug_input(
        &mut self,
        cv: &mut Canvas,
        r: Rect,
        text: &str,
        cursor: usize,
        focused: bool,
        placeholder: &str,
        hit: DebugHit,
    ) {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        cv.fill(Rect::new(r.x, r.y, r.w, 1), c.border);
        let ty = r.y + (r.h - 18) / 2;
        self.tx.draw_ui(cv, r.x + 12, ty, "›", if focused { c.accent_text } else { dim }, r, true);
        let tx0 = r.x + 28;
        let clip = Rect::new(tx0, r.y, (r.w - 40).max(0), r.h);
        if text.is_empty() {
            self.tx.draw_ui(cv, tx0, ty, placeholder, dim, clip, false);
        } else {
            self.tx.draw_ui(cv, tx0, ty, text, fg, clip, false);
        }
        if focused {
            let before: String = text.chars().take(cursor).collect();
            let cx = tx0 + self.tx.ui_width(&before, false);
            cv.fill_clip(Rect::new(cx, ty, 1, 18), fg, clip);
        }
        self.layout.debug_hits.push((r, hit));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app_ctxmenu::{Entry, Flags, Target, entries_for},
        app_unsaved::testkit,
        dap_tests::{PipeR, PipeW, fake_adapter},
    };
    use std::{
        sync::{Arc, mpsc::channel},
        time::{Duration, Instant},
    };

    #[test]
    fn reads_the_package_name() {
        let m = "[workspace]\nmembers=[]\n\n[package]\nname = \"lumen-text\"\nversion = \"0.1.0\"\n\n[dependencies]\nname = \"x\"\n";
        assert_eq!(cargo_package_name(m).as_deref(), Some("lumen-text"));
        assert_eq!(cargo_package_name("[dependencies]\nname = \"x\""), None);
    }

    /// An app on a project with `main.go` open (5 lines).
    fn go_app(tag: &str) -> (testkit::Guard, App, PathBuf, usize) {
        let g = testkit::lock();
        let dir = testkit::tmp(tag);
        let mut app = testkit::test_app(&dir);
        let src = dir.join("main.go");
        std::fs::write(&src, "package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n").unwrap();
        let i = app.open_path(&src, None).unwrap();
        app.active = i;
        (g, app, dir, i)
    }

    fn pump_until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            app.pump_debug();
            if done(app) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}");
    }

    fn session_of(app: &App) -> &Session {
        app.dbg.as_ref().and_then(Debugger::session).expect("a session")
    }

    fn key(k: Key, text: Option<&str>) -> KeyIn {
        KeyIn { key: k, text: text.map(str::to_string) }
    }

    #[test]
    fn breakpoints_conditions_and_watches_persist_per_project() {
        let (_g, mut app, dir, i) = go_app("dbg-persist");
        let src = dir.join("main.go");
        app.toggle_breakpoint_at(1);
        app.bp_set_condition(&src, 1, "a > 1");
        app.bp_set_log(&src, 3, "sum={a}"); // creates the breakpoint
        app.toggle_breakpoint_at(2);
        app.bp_toggle_enabled(&src, 2);
        app.add_watch("a+b");
        app.add_watch("a+b");
        app.add_watch("   ");
        assert_eq!(app.dbgui.watch, ["a+b"]);
        assert_eq!(app.docs[i].bps, [1, 2, 3]);
        assert_eq!(app.docs[i].bp_glyphs, [BpGlyph::Conditional, BpGlyph::Disabled, BpGlyph::Log]);

        // As after a restart: everything comes back from the project's file.
        app.breakpoints.clear();
        app.dbgui.meta.clear();
        app.dbgui.watch.clear();
        app.sync_breakpoint_marks();
        assert!(app.docs[i].bps.is_empty());
        app.debug_load();
        assert_eq!(app.breakpoints[&src], [1, 2, 3]);
        assert_eq!(app.dbgui.meta[&src][&1].condition.as_deref(), Some("a > 1"));
        assert_eq!(app.dbgui.meta[&src][&3].log.as_deref(), Some("sum={a}"));
        assert!(!app.dbgui.meta[&src][&2].enabled);
        assert_eq!(app.docs[i].bp_glyphs, [BpGlyph::Conditional, BpGlyph::Disabled, BpGlyph::Log]);
        assert_eq!(app.dbgui.watch, ["a+b"]);

        // Disabled breakpoints are not sent; lines are 1-based on the wire.
        let sent = app.breakpoint_specs();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1.iter().map(|s| s.line).collect::<Vec<_>>(), [2, 4]);
        assert_eq!(sent[0].1[0].condition.as_deref(), Some("a > 1"));
        assert_eq!(sent[0].1[1].log_message.as_deref(), Some("sum={a}"));

        // A blank condition clears it and leaves no empty entry behind.
        app.bp_set_condition(&src, 1, "  ");
        assert!(!app.dbgui.meta[&src].contains_key(&1));
        app.bp_remove(&src, 3);
        assert_eq!(app.breakpoints[&src], [1, 2]);
        app.clear_breakpoints();
        app.debug_load();
        assert!(app.breakpoints.is_empty() && app.dbgui.meta.is_empty());
        assert_eq!(app.dbgui.watch, ["a+b"], "watches stay");
        app.remove_watch(0);
        app.debug_load();
        assert!(app.dbgui.watch.is_empty());
        assert!(!dm::state_file(&app.root).unwrap().exists(), "nothing left to remember: no file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gutter_menu_and_commands_follow_the_breakpoint_under_the_pointer() {
        let labels = |f: &Flags| {
            entries_for(&Target::Breakpoint(3), f)
                .into_iter()
                .filter_map(|e| match e {
                    Entry::Item { label, .. } => Some(label),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(&Flags::default()), ["Add Breakpoint", "Add Conditional Breakpoint…", "Add Logpoint…"]);
        let on = labels(&Flags { bp: Some(true), ..Flags::default() });
        assert_eq!(on, ["Remove Breakpoint", "Edit Condition…", "Edit Logpoint Message…", "Disable Breakpoint"]);
        assert!(labels(&Flags { bp: Some(false), ..Flags::default() }).contains(&"Enable Breakpoint"));
        for e in entries_for(&Target::Breakpoint(3), &Flags { bp: Some(true), ..Flags::default() }) {
            if let Entry::Item { act: crate::app_ctxmenu::Act::Run(id), .. } = e {
                assert!(Cmd::from_id(id).is_some(), "{id} is a command");
            }
        }

        let (_g, mut app, dir, i) = go_app("dbg-gutter");
        crate::display::set_breakpoints(true);
        app.docs[i].rect = Rect::new(100, 50, 600, 400);
        let y = 50.0 + 3.0 * line_h() as f64 + 4.0;
        assert_eq!(app.gutter_bp_line_at(104.0, y), Some(3));
        assert_eq!(app.gutter_bp_line_at(300.0, y), None, "the text is not the breakpoint column");
        assert_eq!(app.breakpoint_enabled_at(3), None);
        app.toggle_breakpoint_at(3);
        assert_eq!(app.breakpoint_enabled_at(3), Some(true));
        app.open_ctxmenu(Target::Breakpoint(3), (104, y as i32));
        let Overlay::Ctx(m) = &app.overlay else { panic!("the menu is open") };
        let remove = m.entries.iter().any(|e| matches!(e, Entry::Item { label: "Remove Breakpoint", .. }));
        assert!(remove);
        // The condition prompt starts from the current text and its answer lands on that line.
        app.overlay = Overlay::None;
        let d = &mut app.docs[i];
        let p = d.buf.pos(3, 0);
        d.ed.click(&mut d.buf, p, false);
        app.run_debug_cmd(Cmd::DebugBpCondition);
        let Overlay::Prompt(pr) = &app.overlay else { panic!("a prompt") };
        assert!(matches!(&pr.kind, PromptKind::BpCondition { line: 3, .. }));
        app.overlay = Overlay::None;
        app.run_debug_cmd(Cmd::DebugBpToggleEnabled);
        assert_eq!(app.breakpoint_enabled_at(3), Some(false));

        // Settings and the F6 binding exist.
        assert!(app.config.debug_hover_evaluate && app.config.debug_open_panel);
        app.apply_setting("debug_hover_evaluate", &serde_json::json!(false));
        assert!(!app.config.debug_hover_evaluate);
        let km = crate::keymap::Keymap::default();
        assert_eq!(km.lookup(false, false, false, &Key::Named(NamedKey::F6)), Some("DebugPause"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn launch_configurations_are_detected_and_chosen_per_project() {
        let (_g, mut app, dir, i) = go_app("dbg-launch");
        let ids = |app: &App| app.debug_detect().iter().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(ids(&app), ["delve"], "Go package main -> delve");
        app.docs[i].replace_text("package lib\n");
        assert!(ids(&app).is_empty(), "a Go file that is not package main has no config");
        app.docs[i].replace_text("package main\n\nfunc main() {}\n");
        std::fs::create_dir_all(dir.join("site")).unwrap();
        let page = dir.join("site/home.ghp");
        std::fs::write(&page, "<h1>hi</h1>\n").unwrap();
        assert_eq!(ids(&app), ["delve", "ghp"], "a .ghp project adds the ghp adapter");
        app.debug_pick_config("ghp");
        let cfgs = app.debug_detect();
        assert_eq!(dm::choose(&cfgs, app.dbgui.config.as_deref()).unwrap().id, "ghp");
        assert_eq!(dm::load(&app.root).config.as_deref(), Some("ghp"), "remembered for the project");
        app.debug_select_config();
        assert!(matches!(app.overlay, Overlay::Picker(_)));
        app.overlay = Overlay::None;
        let j = app.open_path(&page, None).unwrap();
        app.active = j;
        assert_eq!(ids(&app), ["ghp"], "on a page only the ghp adapter applies");
        assert!(crate::dap_adapters::Registry::builtin().get("ghp").is_some(), "the registry entry it launches");
        app.debug_restart();
        assert!(app.status.as_ref().unwrap().0.contains("nothing to restart"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn debug_panel_flow_against_the_fake_adapter() {
        let (_g, mut app, dir, i) = go_app("dbg-flow");
        let src = dir.join("main.go");
        app.toggle_breakpoint_at(1);
        app.bp_set_condition(&src, 1, "a > 1");
        app.toggle_breakpoint_at(150); // the fake adapter cannot place lines from 100 on
        assert_eq!(app.docs[i].bp_glyphs, [BpGlyph::Conditional, BpGlyph::Normal]);

        let (c2a_tx, c2a_rx) = channel();
        let (a2c_tx, a2c_rx) = channel();
        let (log_tx, _log_rx) = channel();
        let (req_tx, _req_rx) = channel();
        fake_adapter(c2a_rx, a2c_tx, log_tx, req_tx, src.to_string_lossy().into_owned());
        let mut dap = Dap::from_io(PipeR(a2c_rx, Vec::new()), PipeW(c2a_tx), Arc::new(|| {}));
        dap.adapter_id = "go".into();
        let mut session = crate::dap_adapters::Registry::builtin().get("delve").unwrap().session(&src, &dir);
        for (p, specs) in app.breakpoint_specs() {
            session.set_breakpoint_specs(&p, specs, &mut dap);
        }
        session.start(&mut dap);
        app.debug_attach(Debugger::Native { dap: Box::new(dap), session: Box::new(session) }, "test");
        assert!(app.panel.open && app.panel.tab == PanelTab::Debug, "the panel opens on start");
        assert!(!app.tool_enabled(Tool::Over) && app.tool_enabled(Tool::Stop), "not stopped yet");

        // The stop opens the file at the execution line; breakpoints show what was placed.
        pump_until(&mut app, "the stop", |a| a.docs[i].debug_line.is_some());
        assert_eq!(app.docs[i].debug_line, Some(1));
        assert_eq!(app.docs[i].bp_glyphs, [BpGlyph::Conditional, BpGlyph::Unverified]);
        assert_eq!(app.debug_status_text(), "Paused on breakpoint");
        let bps = app.debug_bp_rows();
        assert_eq!(bps.len(), 2);
        assert!(bps[0].parts[1].0.contains("if a > 1") && bps[1].parts.iter().any(|p| p.0.contains("not placed")));

        // Call stack: two threads, the stopped one open. Opening the other fetches its stack.
        let rows = app.debug_stack_rows();
        assert_eq!(rows.len(), 4, "thread, 2 frames, thread");
        assert_eq!(rows[1].hit, Some(DebugHit::Frame(1, 0)));
        assert_eq!(rows[3].hit, Some(DebugHit::Thread(2)));
        app.debug_hit(DebugHit::Thread(2));
        pump_until(&mut app, "the worker stack", |a| session_of(a).thread_frames.contains_key(&2));
        assert_eq!(app.debug_stack_rows().len(), 5);
        app.debug_hit(DebugHit::Frame(2, 0));
        assert_eq!(app.docs[i].debug_line, Some(3), "clicking a frame navigates to it");
        assert_eq!(session_of(&app).selected, Some((2, 0)));

        // Variables: lazy tree; expanding fetches the children, folding a scope hides them.
        pump_until(&mut app, "the scopes of that frame", |a| session_of(a).var_children.contains_key(&9));
        let names = |a: &App| a.debug_var_rows().iter().map(|r| r.parts[0].0.clone()).collect::<Vec<_>>();
        assert_eq!(names(&app), ["Locals", "a", "obj", "Globals"]);
        app.debug_hit(DebugHit::Var { key: "Locals/obj".into(), vars_ref: 12, scope: None });
        pump_until(&mut app, "the children of obj", |a| session_of(a).var_children.contains_key(&12));
        assert!(app.debug_var_rows().iter().any(|r| r.indent == 28 && r.parts[0].0 == "x"));
        app.debug_hit(DebugHit::Var { key: "Locals".into(), vars_ref: 9, scope: Some(false) });
        assert_eq!(names(&app), ["Locals", "Globals"]);

        // Watch.
        app.add_watch("a+1");
        pump_until(&mut app, "the watch value", |a| session_of(a).watch_results.contains_key("a+1"));
        let w = app.debug_watch_rows();
        assert_eq!((w[0].parts[0].0.as_str(), w[0].parts[2].0.as_str()), ("a+1", "3"));
        assert_eq!(dm::load(&app.root).watch, ["a+1"]);

        // Debug Console: typing, history, evaluation; function keys pass through.
        app.debug_hit(DebugHit::ConsoleInput);
        assert_eq!(app.dbgui.focus, Some(InputKind::Console));
        for ch in ["b", "a", "d"] {
            assert!(app.debug_key(&key(Key::Character(ch.into()), Some(ch))));
        }
        assert_eq!(app.dbgui.console.text, "bad");
        assert!(!app.debug_key(&key(Key::Named(NamedKey::F5), None)), "F5 is a global binding");
        assert!(app.debug_key(&key(Key::Named(NamedKey::Enter), None)));
        assert!(app.dbgui.console.text.is_empty());
        pump_until(&mut app, "the error", |a| {
            session_of(a)
                .output
                .iter()
                .any(|l| dm::classify(l) == (ConsoleKind::Error, "no symbol bad in current context"))
        });
        assert!(app.debug_key(&key(Key::Named(NamedKey::ArrowUp), None)));
        assert_eq!(app.dbgui.console.text, "bad", "the history brings the line back");
        assert!(app.debug_key(&key(Key::Named(NamedKey::Escape), None)));
        assert_eq!(app.dbgui.focus, None);
        assert!(!app.debug_key(&key(Key::Character("x".into()), Some("x"))), "no focus: not ours");
        app.dbgui.console = TextField::with("a+2");
        app.dbgui.focus = Some(InputKind::Console);
        app.debug_submit(InputKind::Console);
        pump_until(&mut app, "the result", |a| {
            session_of(a).output.iter().any(|l| dm::classify(l) == (ConsoleKind::Result, "3"))
        });
        let console = app.debug_console_rows();
        assert!(console.iter().any(|r| r.parts[0].0 == "› a+2"));
        assert!(console.iter().any(|r| r.parts[0].0 == "warn"), "stderr is shown too");

        // Hover: the value of the word under the pointer.
        let at = app.docs[i].buf.pos(2, 6); // inside `add`
        app.config.debug_hover_evaluate = false;
        assert!(!app.debug_hover_request(at), "switched off in the settings");
        app.config.debug_hover_evaluate = true;
        assert!(app.debug_hover_request(at));
        assert_eq!(app.dbgui.hover_for.as_ref().map(|h| h.0.as_str()), Some("add"));
        pump_until(&mut app, "the hover answer", |a| a.dbgui.hover_for.is_none());

        // Toolbar: step, then continue to the end.
        assert!(app.tool_enabled(Tool::Over) && app.tool_enabled(Tool::Continue) && !app.tool_enabled(Tool::Pause));
        app.debug_tool(Tool::Over);
        pump_until(&mut app, "the step", |a| a.docs[i].debug_line == Some(1));
        assert_eq!(app.debug_status_text(), "Paused after step");
        app.debug_tool(Tool::Continue);
        pump_until(&mut app, "the end", |a| a.docs[i].debug_line.is_none() && a.debug_status_text() == "Finished");
        assert!(!app.tool_enabled(Tool::Over) && app.tool_enabled(Tool::Continue) && !app.tool_enabled(Tool::Stop));
        app.debug_stop();
        assert!(app.dbg.is_none());
        assert_eq!(app.docs[i].bp_glyphs, [BpGlyph::Conditional, BpGlyph::Normal], "no session: no verification");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn breakpoints_follow_edits_and_the_session_is_told() {
        let (_g, mut app, dir, i) = go_app("bp-follow");
        let src = dir.join("main.go");
        app.toggle_breakpoint_at(3);
        app.bp_set_condition(&src, 3, "a > 1");
        assert_eq!(app.docs[i].bps, [3]);
        {
            let d = &mut app.docs[i];
            d.ed.click(&mut d.buf, 0, false);
            d.ed.insert_text(&mut d.buf, "// header\n");
            d.sync_edits();
        }
        app.follow_breakpoint_edits();
        assert_eq!(app.breakpoints[&src], [4], "the line moved down with the text");
        assert_eq!(app.dbgui.meta[&src][&4].condition.as_deref(), Some("a > 1"), "and kept its condition");
        assert_eq!(app.docs[i].bps, [4], "the gutter follows");
        assert_eq!(dm::load(&app.root).lines.get(&src).map(Vec::as_slice), Some(&[4usize][..]), "and the saved state");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_debug_build_lands_in_the_console_without_a_session() {
        let (_g, mut app, dir, _i) = go_app("bp-build");
        let (tx, rx) = std::sync::mpsc::channel();
        app.dbgui.build =
            Some(PendingBuild { rx, adapter: "ghp".into(), path: dir.join("a.ghp"), root: dir.clone(), ov: None });
        assert!(app.tool_enabled(Tool::Stop) && !app.tool_enabled(Tool::Continue));
        assert_eq!(app.debug_status_text(), "Building…");
        app.pump_debug_build();
        assert!(app.dbgui.build.is_some(), "still building");
        tx.send(Err("`ghp build` failed\nline two".to_string())).unwrap();
        app.pump_debug_build();
        assert!(app.dbgui.build.is_none() && app.dbg.is_none());
        assert!(app.panel.open && app.panel.tab == PanelTab::Debug);
        let rows = app.debug_console_rows();
        assert!(rows.iter().any(|r| r.parts[0].0 == "line two"), "the build output is in the Debug Console");
        assert!(!app.debug_status_text().contains("Building"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
