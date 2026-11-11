//! The Problems panel: problems grouped by file with counts, most severe first, in one of two
//! scopes ("Open files" | "Whole project", setting `problems_scope`, switched with the chips in
//! the panel or Alt+W).
//!
//! The whole-project scope adds what servers said about files that are not open (the bounded
//! store in `problems`) and, for servers that support LSP 3.17 pull diagnostics for the
//! workspace, asks them with `workspace/diagnostic` (with `previousResultIds` and a
//! `partialResultToken`, so big projects stream in). Those requests live only while the panel shows
//! the whole project: switching scope or closing the panel cancels them (`$/cancelRequest`). A
//! finished request is asked again only after a save or a server's `workspace/diagnostic/refresh`,
//! never on a timer.

use crate::{
    app::App,
    app_overlay::{Action, Item, Overlay, PickKind, Picker},
    doc::uri_to_path,
    lsp::Pending,
    problems::{self, PDiag, Row},
    ui::Rect,
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::PathBuf,
    time::{Duration, Instant},
};

/// The open panel rebuilds its rows at most this often while reports stream in.
const REBUILD_EVERY: Duration = Duration::from_millis(150);

/// Labels of the scope switch.
pub const SCOPE_LABELS: [&str; 2] = ["Open files", "Whole project"];

/// One in-flight `workspace/diagnostic` request.
#[derive(Debug, Clone)]
struct Inflight {
    server: String,
    root: PathBuf,
    id: i64,
    token: String,
}

/// Workspace pulls and the panel's refresh bookkeeping (kept in `LspX`).
#[derive(Default)]
pub struct WsState {
    inflight: Vec<Inflight>,
    /// Servers already asked while the panel shows the whole project (asked again after a save).
    asked: HashSet<(String, PathBuf)>,
    serial: u64,
    /// Signature the open panel's rows were built from, and when.
    shown: Option<u64>,
    built_at: Option<Instant>,
}

/// Does a server offer workspace pull diagnostics?
fn supports_workspace_pull(caps: &Value) -> bool {
    caps["diagnosticProvider"]["workspaceDiagnostics"].as_bool() == Some(true)
}

/// Diagnostics without a `source` are attributed to the server that sent them.
fn fill_source(diags: &mut [PDiag], server: &str) {
    for d in diags.iter_mut().filter(|d| d.source.is_empty()) {
        d.source = server.to_string();
    }
}

fn sev_icon(s: u8) -> &'static str {
    match s {
        1 => "\u{f057}",
        2 => "\u{f071}",
        _ => "\u{f05a}",
    }
}

impl App {
    /// Is the Problems panel's scope the whole project?
    pub fn problems_project(&self) -> bool {
        self.config.problems_scope == "project"
    }

    fn problems_panel_open(&self) -> bool {
        matches!(&self.overlay, Overlay::Picker(p) if p.kind == PickKind::Problems)
    }

    /// Palette / status bar: open the panel in the configured scope.
    pub fn open_problems(&mut self) {
        let project = self.problems_project();
        self.lspx.ws.asked.clear();
        if project {
            self.ws_pull_new();
        }
        let (title, items) = self.problem_items(project);
        let mut p = Picker::new(PickKind::Problems, &title, items);
        // The first problem, not its file's header.
        p.sel = (p.items.len() > 1) as usize;
        p.refilter();
        self.overlay = Overlay::Picker(p);
        self.lspx.ws.shown = Some(self.problems_signature());
        self.lspx.ws.built_at = Some(Instant::now());
        self.request_redraw();
    }

    /// Switch the scope (chips, Alt+W, settings); remembered in config.json.
    pub fn set_problems_scope(&mut self, project: bool) {
        let want = if project { "project" } else { "open_files" };
        if self.config.problems_scope != want {
            self.config.problems_scope = want.to_string();
            self.config.save();
        }
        self.lspx.ws.asked.clear();
        if project {
            self.ws_pull_new();
        } else {
            self.ws_cancel_all();
        }
        self.problems_rebuild();
    }

    /// Rows of the panel for a scope: file headers ("path — 2 errors, 1 warning", opens the
    /// file) followed by their problems (open the location). Returns the panel title too.
    pub(crate) fn problem_items(&self, project: bool) -> (String, Vec<Item>) {
        let mut rows: Vec<(PathBuf, Row)> = Vec::new();
        let mut open: HashSet<PathBuf> = HashSet::new();
        for d in &self.docs {
            let Some(p) = d.path() else { continue };
            open.insert(p.to_path_buf());
            for g in &d.diags {
                let row = Row {
                    line: g.sl,
                    col: g.sc,
                    severity: g.severity,
                    message: problems::clip_msg(&g.message),
                    source: g.source.clone(),
                };
                rows.push((p.to_path_buf(), row));
            }
        }
        for p in self.build.problems.iter().filter(|p| !open.contains(&p.file)) {
            let row = Row {
                line: p.line,
                col: p.col,
                severity: p.sev.lsp(),
                message: problems::clip_msg(&p.message),
                source: "build".into(),
            };
            rows.push((p.file.clone(), row));
        }
        if project {
            for (path, diags) in self.lspx.problems.iter() {
                if open.contains(path) {
                    continue; // the open document's own (fresher) diagnostics are listed
                }
                for g in diags {
                    let row = Row {
                        line: g.line as usize,
                        col: g.col as usize,
                        severity: g.severity,
                        message: g.message.clone(),
                        source: g.source.clone(),
                    };
                    rows.push((path.clone(), row));
                }
            }
        }
        let groups = problems::group(rows);
        let (mut errors, mut warnings, mut total) = (0, 0, 0);
        let mut items = Vec::new();
        let mut shown = 0;
        for g in &groups {
            errors += g.errors;
            warnings += g.warnings;
            total += g.rows.len();
            if shown >= problems::ROWS_SHOWN {
                continue;
            }
            items.push(Item {
                label: self.rel(&g.path),
                detail: g.summary(),
                action: Action::Open(g.path.clone(), None),
            });
            for r in g.rows.iter().take(problems::ROWS_SHOWN - shown) {
                shown += 1;
                let src = if r.source.is_empty() { String::new() } else { format!(" · {}", r.source) };
                items.push(Item {
                    label: format!("    {} {}", sev_icon(r.severity), r.message),
                    detail: format!("{}:{}{src}", r.line + 1, r.col + 1),
                    action: Action::Open(g.path.clone(), Some((r.line, r.col))),
                });
            }
        }
        let scope = if project { "in the project" } else { "in open files" };
        let mut title = match total {
            0 => format!("No problems {scope}"),
            _ => {
                let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
                let mut t = format!("{} {scope}", n(total, "problem", "problems"));
                if errors + warnings > 0 {
                    t = format!("{t} ({}, {})", n(errors, "error", "errors"), n(warnings, "warning", "warnings"));
                }
                t
            }
        };
        let hidden = total - shown + if project { self.lspx.problems.dropped } else { 0 };
        if hidden > 0 {
            title = format!("{title} · {hidden} not shown");
        }
        if project && !self.lspx.ws.inflight.is_empty() {
            title = format!("{title} · checking…");
        }
        (title, items)
    }

    /// What the open panel's rows depend on (a change rebuilds them).
    fn problems_signature(&self) -> u64 {
        let diags: usize = self.docs.iter().map(|d| d.diags.len()).sum();
        let mut h = self.lspx.problems.generation.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        h ^= (diags as u64).wrapping_mul(0x1000_0000_01b3);
        h ^= (self.build.problems.len() as u64) << 40;
        h ^= (self.lspx.ws.inflight.len() as u64) << 56;
        h ^ self.problems_project() as u64
    }

    /// Rebuild the open panel's rows, keeping the filter text and the selected row.
    fn problems_rebuild(&mut self) {
        if !self.problems_panel_open() {
            return;
        }
        let (title, items) = self.problem_items(self.problems_project());
        if let Overlay::Picker(p) = &mut self.overlay {
            // `refilter` keeps the selected row (clamped) and the filter text.
            p.title = title;
            p.items = items;
            p.refilter();
        }
        self.lspx.ws.shown = Some(self.problems_signature());
        self.lspx.ws.built_at = Some(Instant::now());
        self.request_redraw();
    }

    /// Per tick: cancel workspace pulls nobody looks at any more, ask servers that became ready,
    /// and rebuild the open panel when its problems changed (throttled).
    pub(crate) fn problems_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if !self.problems_panel_open() {
            if !self.lspx.ws.inflight.is_empty() {
                self.ws_cancel_all();
            }
            self.lspx.ws.shown = None;
            return;
        }
        if self.problems_project() {
            self.ws_pull_new();
        } else if !self.lspx.ws.inflight.is_empty() {
            self.ws_cancel_all();
        }
        if self.lspx.ws.shown == Some(self.problems_signature()) {
            return;
        }
        if let Some(t) = self.lspx.ws.built_at
            && t + REBUILD_EVERY > now
        {
            return want(t + REBUILD_EVERY);
        }
        self.problems_rebuild();
    }

    // ───────────── workspace pull ─────────────

    /// Send `workspace/diagnostic` to every ready server that supports it and was not asked yet.
    fn ws_pull_new(&mut self) {
        for li in 0..self.lsps.len() {
            let l = &self.lsps[li];
            if !l.ready || l.shutting_down || !supports_workspace_pull(&l.caps) {
                continue;
            }
            let key = (l.name.clone(), l.root.clone());
            if self.lspx.ws.asked.contains(&key)
                || self.lspx.ws.inflight.iter().any(|f| (&f.server, &f.root) == (&key.0, &key.1))
            {
                continue;
            }
            self.lspx.ws.serial += 1;
            let token = format!("{}{}", problems::WS_TOKEN_PREFIX, self.lspx.ws.serial);
            let mut params = json!({
                "previousResultIds": self.lspx.problems.previous_ids(&key.0),
                "partialResultToken": token,
            });
            if let Some(id) = l.caps["diagnosticProvider"]["identifier"].as_str() {
                params["identifier"] = json!(id);
            }
            let id =
                self.lsps[li].request("workspace/diagnostic", params, Pending::WorkspaceDiag { token: token.clone() });
            self.lspx.ws.inflight.push(Inflight { server: key.0.clone(), root: key.1.clone(), id, token });
            self.lspx.ws.asked.insert(key);
        }
    }

    /// Cancel every in-flight workspace pull (scope switched, panel closed).
    fn ws_cancel_all(&mut self) {
        for f in std::mem::take(&mut self.lspx.ws.inflight) {
            if let Some(l) = self.lsps.iter_mut().find(|l| l.name == f.server && l.root == f.root) {
                l.cancel(f.id);
            }
        }
    }

    /// A file was saved: servers whose workspace pull finished are asked again (only while the
    /// panel shows the whole project).
    pub(crate) fn ws_after_save(&mut self) {
        if self.problems_panel_open() && self.problems_project() {
            self.lspx.ws.asked.clear();
        }
    }

    /// `workspace/diagnostic/refresh` from a server.
    pub(crate) fn ws_refresh_requested(&mut self) {
        self.ws_after_save();
    }

    /// A workspace report (`last`: the response; else a partial result) from server `li`.
    pub(crate) fn ws_report(&mut self, li: usize, token: &str, report: problems::WsReport, last: bool) {
        if !self.lspx.ws.inflight.iter().any(|f| f.token == token) {
            return; // cancelled meanwhile
        }
        if last {
            self.lspx.ws.inflight.retain(|f| f.token != token);
        }
        let Some(name) = self.lsps.get(li).map(|l| l.name.clone()) else { return };
        let named = self.lsps[li].is_generic();
        let store = &mut self.lspx.problems;
        store.dropped += report.dropped;
        for f in report.files {
            store.set_result_id(&name, &f.uri, f.result_id);
            if !f.full {
                continue; // "unchanged": what we have is current
            }
            if let Some(path) = uri_to_path(&f.uri) {
                let mut diags = f.diags;
                if named {
                    fill_source(&mut diags, &name);
                }
                store.set(&path, &name, diags);
            }
        }
        self.request_redraw();
    }

    /// A workspace pull ended with an error (cancelled, server error): it is simply over.
    pub(crate) fn ws_failed(&mut self, li: usize, token: &str, err: Option<Value>) {
        self.lspx.ws.inflight.retain(|f| f.token != token);
        if let (Some(e), Some(l)) = (err, self.lsps.get(li)) {
            l.log_line("app", &format!("workspace/diagnostic: {}", e["message"].as_str().unwrap_or("error")));
        }
        self.request_redraw();
    }

    // ───────────── files that are not open ─────────────

    /// Pushed (or related) diagnostics of a file without an open document. `named`: a registry
    /// server, whose id stands in for a missing `source` (as for open documents).
    pub(crate) fn problems_store_closed(&mut self, uri: &str, source: &str, named: bool, items: &Value) {
        let Some(path) = uri_to_path(uri) else { return };
        let (mut diags, dropped) = problems::pdiags_from(items, problems::FILE_CAP);
        if named {
            fill_source(&mut diags, source);
        }
        let store = &mut self.lspx.problems;
        store.dropped += dropped;
        store.set(&path, source, diags);
    }

    /// A document is closing: what it showed stays listed for the project until a server says
    /// something new about the file.
    pub(crate) fn problems_keep_closing(&mut self, i: usize) {
        let Some(d) = self.docs.get(i) else { return };
        let Some(path) = d.path().map(PathBuf::from) else { return };
        let diags: Vec<PDiag> = d
            .diags
            .iter()
            .map(|g| PDiag {
                line: g.sl as u32,
                col: g.sc as u32,
                severity: g.severity,
                message: problems::clip_msg(&g.message),
                source: g.source.clone(),
            })
            .collect();
        if !diags.is_empty() {
            self.lspx.problems.set(&path, problems::LAST_OPEN, diags);
        }
    }

    // ───────────── scope switch (drawn in the panel's input row) ─────────────

    /// The two chips of the scope switch, right-aligned in the input row of picker `r`.
    pub(crate) fn problems_chips(&mut self, r: Rect, input_h: i32) -> [Rect; 2] {
        let h = 26;
        let y = r.y + (input_h - h) / 2;
        let w1 = self.tx.ui_width(SCOPE_LABELS[1], false) + 20;
        let w0 = self.tx.ui_width(SCOPE_LABELS[0], false) + 20;
        let x1 = r.right() - 16 - w1;
        let x0 = x1 - 2 - w0;
        [Rect::new(x0, y, w0, h), Rect::new(x1, y, w1, h)]
    }

    /// A click on a scope chip: true when it hit one.
    pub(crate) fn problems_chip_click(&mut self, r: Rect, input_h: i32, x: f64, y: f64) -> bool {
        if !self.problems_panel_open() {
            return false;
        }
        let chips = self.problems_chips(r, input_h);
        match chips.iter().position(|c| c.contains(x, y)) {
            Some(k) => {
                self.set_problems_scope(k == 1);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, scratch_base, test_app};

    fn fake_exe() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let p = exe.parent()?.parent()?.join("examples").join(format!("fake_lsp{}", std::env::consts::EXE_SUFFIX));
        p.exists().then_some(p)
    }

    fn project(tag: &str) -> PathBuf {
        let d = scratch_base().join(format!("problems-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        crate::paths::canonical(&d).unwrap()
    }

    fn pump(app: &mut App, ms: u64, mut cond: impl FnMut(&mut App) -> bool) -> bool {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            app.lsp_tick(Instant::now(), &mut |_| {});
            app.pump_lsp();
            if cond(app) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        false
    }

    fn server(exe: &std::path::Path, env: Value) -> Value {
        let mut env = env;
        env["FAKE_NAME"] = json!("a");
        json!({"command": exe, "languages": ["fk"], "role": "primary", "env": env})
    }

    fn open(app: &mut App, f: &std::path::Path, text: &str) -> usize {
        std::fs::write(f, text).unwrap();
        let i = app.open_path(f, None).expect("open");
        app.active = i;
        i
    }

    fn rows(app: &App) -> Vec<(String, String)> {
        match &app.overlay {
            Overlay::Picker(p) if p.kind == PickKind::Problems => {
                p.items.iter().map(|it| (it.label.clone(), it.detail.clone())).collect()
            }
            _ => Vec::new(),
        }
    }

    fn title(app: &App) -> String {
        match &app.overlay {
            Overlay::Picker(p) => p.title.clone(),
            _ => String::new(),
        }
    }

    fn stop(app: &mut App) {
        for l in &mut app.lsps {
            l.shutdown();
        }
    }

    #[test]
    fn panel_groups_by_file_sorts_by_severity_and_switches_scope() {
        let _g = lock();
        let dir = project("group");
        let mut app = test_app(&dir);
        let a = open(&mut app, &dir.join("a.txt"), "x\n");
        let b = open(&mut app, &dir.join("b.txt"), "y\n");
        let diag = |l, s, m: &str| crate::doc::Diag {
            sl: l,
            sc: 0,
            el: l,
            ec: 1,
            severity: s,
            message: m.into(),
            code: String::new(),
            source: String::new(),
        };
        app.docs[a].set_ext_diags("x", vec![diag(0, 2, "warn a")]);
        app.docs[b].set_ext_diags("x", vec![diag(3, 2, "late warn b"), diag(1, 1, "error b\nsecond line")]);
        // A file nobody has open, reported by a server.
        let closed = dir.join("c.txt");
        app.problems_store_closed(
            &crate::doc::path_to_uri(&closed),
            "srv",
            true,
            &json!([{"range": {"start": {"line": 4, "character": 1}, "end": {"line": 4, "character": 2}}, "severity": 1, "message": "closed error"}]),
        );
        app.config.problems_scope = "open_files".into();
        app.open_problems();
        let r = rows(&app);
        assert_eq!(r[0], ("b.txt".to_string(), "1 error, 1 warning".to_string()), "{r:?}");
        assert!(r[1].0.ends_with("error b") && r[1].1 == "2:1", "{r:?}");
        assert!(r[2].0.ends_with("late warn b"));
        assert_eq!(r[3], ("a.txt".to_string(), "1 warning".to_string()));
        assert!(!r.iter().any(|x| x.0.contains("closed error")), "open files only");
        assert!(title(&app).starts_with("3 problems in open files"), "{}", title(&app));
        // The first problem is selected, and accepting it opens the location.
        if let Overlay::Picker(p) = &app.overlay {
            assert_eq!(p.sel, 1);
        }
        // Whole project: the closed file joins, first (it has an error and sorts by path).
        app.set_problems_scope(true);
        assert_eq!(app.config.problems_scope, "project");
        let r = rows(&app);
        assert!(r.iter().any(|x| x.0.ends_with("closed error") && x.1 == "5:2 · srv"), "{r:?}");
        assert_eq!(r[0].0, "b.txt");
        assert_eq!(r[3].0, "c.txt");
        // Remembered in config.json.
        assert_eq!(crate::config::Config::load().problems_scope, "project");
        // Alt+W-equivalent back to open files.
        app.set_problems_scope(false);
        assert!(!rows(&app).iter().any(|x| x.0.contains("closed error")));
        // Closing a document keeps what it showed for the project scope.
        app.overlay = Overlay::None;
        app.close_tab(a, true);
        assert!(app.docs.iter().all(|d| d.name != "a.txt"));
        let (_, items) = app.problem_items(true);
        assert!(items.iter().any(|it| it.label.ends_with("warn a")), "kept after closing");
        let (_, items) = app.problem_items(false);
        assert!(!items.iter().any(|it| it.label.ends_with("warn a")), "not an open file any more");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pushed_diagnostics_of_closed_files_are_kept_and_dropped_with_their_server() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("push");
        std::fs::write(dir.join("closed.fk"), "a\n// FIXME later\n").unwrap();
        let mut app = test_app(&dir);
        app.config.language_servers = json!({"fake-a": server(&exe, json!({"FAKE_PUSH_CLOSED": "1"}))});
        let i = open(&mut app, &dir.join("x.fk"), "ok\n");
        let closed = dir.join("closed.fk");
        assert!(
            pump(&mut app, 8000, |a| a.lspx.problems.iter().any(|(p, _)| *p == closed)),
            "the push for a closed file was dropped"
        );
        assert!(app.docs[i].diags.is_empty());
        app.config.problems_scope = "project".into();
        app.open_problems();
        assert!(rows(&app).iter().any(|r| r.0.ends_with("fixme from a") && r.1.starts_with("2:")), "{:?}", rows(&app));
        // The server stops: what it said goes with it.
        app.overlay = Overlay::None;
        app.close_tab(i, true);
        app.lspx.timing.idle_grace = Duration::from_millis(10);
        assert!(pump(&mut app, 8000, |a| a.lsps.is_empty()), "idle server never stopped");
        assert!(!app.lspx.problems.iter().any(|(p, _)| *p == closed));
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_pull_streams_partials_reuses_result_ids_and_cancels_on_scope_change() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("ws");
        std::fs::write(dir.join("one.fk"), "// FIXME one\n").unwrap();
        std::fs::write(dir.join("two.fk"), "fine\n// FIXME two\n").unwrap();
        let mut app = test_app(&dir);
        app.config.language_servers = json!({"fake-a": server(&exe, json!({"FAKE_WORKSPACE": "1", "FAKE_PULL": "1"}))});
        let _i = open(&mut app, &dir.join("x.fk"), "open file\n");
        assert!(pump(&mut app, 8000, |a| a.lsps.first().is_some_and(|l| l.ready)), "server never ready");
        app.config.problems_scope = "project".into();
        app.open_problems();
        // File headers of files nobody opened.
        let has = |a: &App, m: &str| rows(a).iter().any(|r| r.0 == m);
        assert!(
            pump(&mut app, 8000, |a| has(a, "one.fk") && has(a, "two.fk") && a.lspx.ws.inflight.is_empty()),
            "workspace report never listed: {:?}",
            rows(&app)
        );
        let log = app.lsps[0].log_text();
        assert!(
            log.contains("$/progress (workspace diagnostics, 1 files)"),
            "first file came as a partial result:\n{log}"
        );
        // A save asks again with the result ids: the fake answers "unchanged" for both, and the rows stay.
        app.save_active();
        assert!(
            pump(&mut app, 8000, |a| a.lsps[0].log_text().contains("unchanged 3")),
            "previousResultIds not sent:\n{}",
            app.lsps[0].log_text()
        );
        assert!(pump(&mut app, 2000, |a| a.lspx.ws.inflight.is_empty()));
        assert!(has(&app, "one.fk") && has(&app, "two.fk"), "{:?}", rows(&app));
        assert!(rows(&app).iter().any(|r| r.0.ends_with("fixme from a") && r.1 == "2:4 · a"), "{:?}", rows(&app));

        // A long-running pull (the server keeps it open) is cancelled when the scope changes.
        app.overlay = Overlay::None;
        app.lsp_tick(Instant::now(), &mut |_| {});
        std::fs::write(dir.join("hang"), "").unwrap();
        app.open_problems();
        assert_eq!(app.lspx.ws.inflight.len(), 1);
        app.set_problems_scope(false);
        assert!(app.lspx.ws.inflight.is_empty());
        assert!(
            pump(&mut app, 8000, |a| a.lsps[0].log_text().contains("cancelled workspace pull")),
            "no $/cancelRequest:\n{}",
            app.lsps[0].log_text()
        );
        // Closing the panel cancels too.
        app.set_problems_scope(true);
        assert_eq!(app.lspx.ws.inflight.len(), 1);
        app.overlay = Overlay::None;
        app.lsp_tick(Instant::now(), &mut |_| {});
        assert!(app.lspx.ws.inflight.is_empty(), "closing the panel cancels the pull");
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
