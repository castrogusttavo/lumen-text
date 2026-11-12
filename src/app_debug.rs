//! Debugging UI glue: breakpoints, F5/F9/F10/F11, and the "Debug" panel. Node/TypeScript runs
//! through js-debug (`dap::MultiSession`), Rust/C through gdb's DAP mode (`dap::Session`).

use crate::{
    app::App,
    app_overlay::Cmd,
    app_panel::{PANEL_HEADER, PanelTab},
    dap::{Dap, Frame, MultiSession, Session, State, Var},
    scrollreg::Reg,
    ui::{Canvas, Rect, line_h},
};
use std::path::{Path, PathBuf};

/// Height of the column titles in the debug body (rows start right below).
const DEBUG_TITLE_H: i32 = 28;

/// The Stack (column 0) / Variables (column 1) row under (x, y): `(column, index, row rect)`.
/// `body` is the debug body, `scroll` the per-column scroll in px and `counts` the number of
/// rows of those two columns. The Output column has no interactive rows.
fn debug_row_at(
    body: Rect,
    scroll: [f64; 3],
    counts: [usize; 2],
    lh: i32,
    x: f64,
    y: f64,
) -> Option<(usize, usize, Rect)> {
    let top = body.y + DEBUG_TITLE_H;
    let area = Rect::new(body.x, top, body.w, (body.bottom() - top).max(0));
    if !area.contains(x, y) || lh <= 0 {
        return None;
    }
    let w3 = body.w / 3;
    let k = if x < (body.x + w3) as f64 {
        0
    } else if x < (body.x + 2 * w3) as f64 {
        1
    } else {
        return None;
    };
    let col = if k == 0 { Rect::new(body.x, top, w3, area.h) } else { Rect::new(body.x + w3, top, w3, area.h) };
    let idx = ((y - top as f64 + scroll[k]) / lh as f64).floor();
    if idx < 0.0 || idx as usize >= counts[k] {
        return None;
    }
    let idx = idx as usize;
    let ry = top + idx as i32 * lh - scroll[k] as i32;
    let r = Rect::new(col.x + 4, ry, col.w - 8, lh).intersect(&area);
    (r.w > 0 && r.h > 0).then_some((k, idx, r))
}

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
    pub fn frames(&self) -> &[Frame] {
        match self {
            Debugger::Node(m) => m.frames(),
            Debugger::Native { session, .. } => &session.frames,
        }
    }
    pub fn locals(&self) -> &[Var] {
        match self {
            Debugger::Node(m) => m.locals(),
            Debugger::Native { session, .. } => &session.locals,
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
    pub fn set_breakpoints(&mut self, path: &str, lines: &[usize]) {
        match self {
            Debugger::Node(m) => m.set_breakpoints(path, lines),
            Debugger::Native { dap, session } => session.set_breakpoints(path, lines, dap),
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

impl App {
    // ───────────── breakpoints ─────────────

    pub fn toggle_breakpoint(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let (line, _) = d.buf.line_col(d.ed.head());
        self.toggle_breakpoint_at(line);
    }

    /// Toggle the breakpoint on `line` (0-based) of the active file (gutter click, F9).
    pub fn toggle_breakpoint_at(&mut self, line: usize) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(path) = d.path().map(Path::to_path_buf) else { return };
        let lines = self.breakpoints.entry(path.clone()).or_default();
        if let Some(i) = lines.iter().position(|l| *l == line) {
            lines.remove(i);
        } else {
            lines.push(line);
            lines.sort_unstable();
        }
        let lines = lines.clone();
        self.sync_breakpoint_marks();
        if let Some(dbg) = self.dbg.as_mut() {
            dbg.set_breakpoints(&path.to_string_lossy(), &lines.iter().map(|l| l + 1).collect::<Vec<_>>());
        }
    }

    pub fn clear_breakpoints(&mut self) {
        let paths: Vec<PathBuf> = self.breakpoints.keys().cloned().collect();
        self.breakpoints.clear();
        self.sync_breakpoint_marks();
        if let Some(dbg) = self.dbg.as_mut() {
            for p in paths {
                dbg.set_breakpoints(&p.to_string_lossy(), &[]);
            }
        }
    }

    pub fn sync_breakpoint_marks(&mut self) {
        for d in &mut self.docs {
            d.bps = d.path().and_then(|p| self.breakpoints.get(p)).cloned().unwrap_or_default();
        }
    }

    // ───────────── session control ─────────────

    fn breakpoint_pairs(&self) -> Vec<(String, Vec<usize>)> {
        self.breakpoints
            .iter()
            .filter(|(_, l)| !l.is_empty())
            .map(|(p, l)| (p.to_string_lossy().into_owned(), l.iter().map(|x| x + 1).collect()))
            .collect()
    }

    /// F5: continue a stopped session, or start debugging the active file.
    pub fn debug_start_or_continue(&mut self) {
        if let Some(d) = self.dbg.as_mut() {
            match d.state() {
                State::Stopped { .. } => return d.resume(),
                State::Running => return,
                _ => {}
            }
            self.dbg = None; // finished: start a new one
        }
        let Some(path) = self.docs.get(self.active).and_then(|d| d.path().map(Path::to_path_buf)) else { return };
        let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        match ext.as_str() {
            "ts" | "js" | "mjs" | "cjs" | "mts" | "cts" => self.start_node_debug(&path),
            "rs" => self.start_rust_debug(&path),
            "go" => self.start_go_debug(&path),
            _ => self.say("no debugger for this file type (supported: TS/JS via js-debug, Rust via gdb, Go via delve)"),
        }
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
                for (p, lines) in self.breakpoint_pairs() {
                    ms.set_breakpoints(&p, &lines);
                }
                ms.start();
                self.dbg = Some(Debugger::Node(Box::new(ms)));
                self.panel.open = true;
                self.panel.tab = PanelTab::Debug;
                self.panel.dbg_scroll = [0.0; 3];
                self.panel.dbg_follow = true;
                self.say("depurando o teste…");
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
                for (p, lines) in self.breakpoint_pairs() {
                    ms.set_breakpoints(&p, &lines);
                }
                ms.start();
                self.dbg = Some(Debugger::Node(Box::new(ms)));
                self.panel.open = true;
                self.panel.tab = PanelTab::Debug;
                self.panel.dbg_scroll = [0.0; 3];
                self.panel.dbg_follow = true;
                self.say("depurando…");
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

    /// Go through the registry's `delve` adapter (`dlv dap`); dlv builds the package itself.
    fn start_go_debug(&mut self, path: &Path) {
        let reg = crate::dap_adapters::Registry::builtin();
        let Some(adapter) = reg.for_language("go") else { return self.say("no Go debug adapter registered") };
        let root = path
            .ancestors()
            .find(|d| d.join("go.mod").exists())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()));
        let mut dap = match adapter.start(&root, self.wake()) {
            Ok(d) => d,
            Err(e) => return self.say(format!("could not start dlv: {e}")),
        };
        let mut session = adapter.session(path, &root);
        for (p, lines) in self.breakpoint_pairs() {
            session.set_breakpoints(&p, &lines, &mut dap);
        }
        session.start(&mut dap);
        self.dbg = Some(Debugger::Native { dap: Box::new(dap), session: Box::new(session) });
        self.panel.open = true;
        self.panel.tab = PanelTab::Debug;
        self.panel.dbg_scroll = [0.0; 3];
        self.panel.dbg_follow = true;
        self.say("debugging with delve…");
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
        for (p, lines) in self.breakpoint_pairs() {
            session.set_breakpoints(&p, &lines, &mut dap);
        }
        session.start(&mut dap);
        self.dbg = Some(Debugger::Native { dap: Box::new(dap), session: Box::new(session) });
        self.panel.open = true;
        self.panel.tab = PanelTab::Debug;
        self.panel.dbg_scroll = [0.0; 3];
        self.panel.dbg_follow = true;
        self.say("debugging with gdb…");
    }

    pub fn debug_stop(&mut self) {
        if let Some(mut d) = self.dbg.take() {
            d.stop();
        }
        for doc in &mut self.docs {
            doc.debug_line = None;
        }
        self.say("debug session ended");
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

    /// Follow the adapter: when it stops, show the top frame in the editor.
    pub fn pump_debug(&mut self) {
        let Some(d) = self.dbg.as_mut() else { return };
        if !d.pump() {
            return;
        }
        let state = d.state();
        let top = d.frames().first().cloned();
        match state {
            State::Stopped { .. } => {
                if let Some(f) = top
                    && let Some(p) = f.path.as_deref()
                {
                    let line = f.line.saturating_sub(1);
                    let key = (p.to_string(), line);
                    if self.dbg_last_stop.as_ref() != Some(&key) {
                        self.dbg_last_stop = Some(key);
                        self.panel.dbg_scroll[0] = 0.0;
                        self.panel.dbg_scroll[1] = 0.0;
                        self.open_path(Path::new(p), Some((line, 0)));
                        for doc in &mut self.docs {
                            doc.debug_line = None;
                        }
                        if let Some(i) = self.docs.iter().position(|doc| doc.path() == Some(Path::new(p))) {
                            self.docs[i].debug_line = Some(line);
                        }
                    }
                }
            }
            State::Terminated => {
                for doc in &mut self.docs {
                    doc.debug_line = None;
                }
                self.dbg_last_stop = None;
                self.say("execution finished");
            }
            _ => {
                for doc in &mut self.docs {
                    doc.debug_line = None;
                }
                self.dbg_last_stop = None;
            }
        }
        self.request_redraw();
    }

    // ───────────── panel ─────────────

    pub fn draw_debug(&mut self, cv: &mut Canvas, body: Rect) {
        self.layout.debug_frames.clear();
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let Some(d) = &self.dbg else {
            self.tx.draw_ui(
                cv,
                body.x + 16,
                body.y + 12,
                "F5 starts debugging the current file · F9 toggles a breakpoint",
                dim,
                body,
                false,
            );
            return;
        };
        let state = match d.state() {
            State::Idle => "iniciando",
            State::Running => "executando",
            State::Stopped { .. } => "parado",
            State::Terminated => "terminado",
        };
        let w3 = body.w / 3;
        let cols = [
            Rect::new(body.x, body.y, w3, body.h),
            Rect::new(body.x + w3, body.y, w3, body.h),
            Rect::new(body.x + 2 * w3, body.y, body.w - 2 * w3, body.h),
        ];
        for (k, title) in ["Stack", "Variables", "Output"].iter().enumerate() {
            let col = cols[k];
            if k > 0 {
                cv.fill(Rect::new(col.x, col.y, 1, col.h), c.border);
            }
            self.tx.draw_ui(
                cv,
                col.x + 12,
                col.y + 6,
                if k == 0 { format!("{title} — {state}") } else { title.to_string() }.as_str(),
                dim,
                col,
                true,
            );
        }
        let lh = line_h() as i32;
        let top = body.y + 28;
        // One list per column: (text, color, frame index for clicks). Every column scrolls on its own.
        let frames: Vec<(String, crate::theme::Rgba, Option<usize>)> = d
            .frames()
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let label = match &f.path {
                    Some(p) => format!(
                        "{}  {}:{}",
                        f.name,
                        Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        f.line
                    ),
                    None => f.name.clone(),
                };
                (label, if i == 0 { fg } else { dim }, Some(i))
            })
            .collect();
        let vars: Vec<(String, crate::theme::Rgba, Option<usize>)> =
            d.locals().iter().map(|v| (format!("{} = {}", v.name, v.value), fg, None)).collect();
        let outs: Vec<(String, crate::theme::Rgba, Option<usize>)> =
            d.output().iter().map(|l| (l.trim_end().to_string(), dim, None)).collect();
        let lists = [frames, vars, outs];
        let regs = [Reg::DebugStack, Reg::DebugVars, Reg::DebugOut];
        for k in 0..3 {
            let area = Rect::new(cols[k].x, top, cols[k].w, (body.bottom() - top).max(0));
            let total = lists[k].len() as f64 * lh as f64 + 8.0;
            let max = (total - area.h as f64).max(0.0);
            if k == 2 && self.panel.dbg_follow {
                self.panel.dbg_scroll[2] = max;
            }
            let scroll = self.panel.dbg_scroll[k].clamp(0.0, max);
            self.panel.dbg_scroll[k] = scroll;
            let first = (scroll / lh as f64) as usize;
            let off = -((scroll as i32) % lh);
            for (i, (text, col, frame)) in lists[k].iter().enumerate().skip(first).take((area.h / lh + 2) as usize) {
                let y = top + off + (i - first) as i32 * lh;
                self.tx.draw_line(cv, area.x + 12, y, text, *col, area);
                if let Some(fi) = frame {
                    self.layout.debug_frames.push((Rect::new(area.x, y, area.w, lh).intersect(&area), *fi));
                }
            }
            self.region_bar(cv, regs[k], area, total, area.h as f64, scroll, 1.0, false);
        }
    }

    /// Hover tint (rect, radius) for the stack frame / variable row under the pointer.
    pub fn debug_hover_target(&self) -> Option<(Rect, i32)> {
        let r = self.layout.panel;
        if !self.panel.open || self.panel.tab != PanelTab::Debug || !self.overlay.is_none() {
            return None;
        }
        let d = self.dbg.as_ref()?;
        let body = Rect::new(r.x, r.y + PANEL_HEADER + 1, r.w, (r.h - PANEL_HEADER - 1).max(0));
        let (x, y) = self.mouse;
        debug_row_at(body, self.panel.dbg_scroll, [d.frames().len(), d.locals().len()], line_h() as i32, x, y)
            .map(|(_, _, rect)| (rect, 5))
    }

    /// Click in the debug panel body: selects a stack frame.
    pub fn debug_click(&mut self, x: f64, y: f64) -> bool {
        let Some((_, i)) = self.layout.debug_frames.iter().find(|(r, _)| r.contains(x, y)).copied() else {
            return false;
        };
        let Some(f) = self.dbg.as_ref().and_then(|d| d.frames().get(i).cloned()) else { return true };
        if let Some(p) = f.path.as_deref() {
            self.open_path(Path::new(p), Some((f.line.saturating_sub(1), 0)));
        }
        true
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
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_package_name() {
        let m = "[workspace]\nmembers=[]\n\n[package]\nname = \"lumen-text\"\nversion = \"0.1.0\"\n\n[dependencies]\nname = \"x\"\n";
        assert_eq!(cargo_package_name(m).as_deref(), Some("lumen-text"));
        assert_eq!(cargo_package_name("[dependencies]\nname = \"x\""), None);
    }

    #[test]
    fn hover_row_geometry() {
        let body = Rect::new(0, 100, 300, 200);
        let lh = 20;
        // Column 0, second row (top = 128).
        let (k, i, r) = debug_row_at(body, [0.0; 3], [3, 5], lh, 50.0, 150.0).unwrap();
        assert_eq!((k, i), (0, 1));
        assert_eq!((r.x, r.y, r.h), (4, 148, 20));
        assert_eq!(r.w, 100 - 8);
        // Column 1 uses its own count and scroll.
        let (k, i, _) = debug_row_at(body, [0.0, 40.0, 0.0], [3, 5], lh, 150.0, 130.0).unwrap();
        assert_eq!((k, i), (1, 2));
        // Past the last row, in the title strip, in the output column: nothing.
        assert!(debug_row_at(body, [0.0; 3], [3, 5], lh, 50.0, 128.0 + 3.0 * 20.0 + 1.0).is_none());
        assert!(debug_row_at(body, [0.0; 3], [3, 5], lh, 50.0, 110.0).is_none());
        assert!(debug_row_at(body, [0.0; 3], [3, 5], lh, 250.0, 150.0).is_none());
        // A partly scrolled row is clipped to the list area.
        let (_, _, r) = debug_row_at(body, [10.0, 0.0, 0.0], [10, 0], lh, 50.0, 130.0).unwrap();
        assert_eq!((r.y, r.h), (128, 10));
    }
}
