//! Registry language servers in the app: lazy start per (server, workspace root), several servers
//! per document, restart with backoff, idle shutdown, merged answers, status listing and log.
//!
//! tsgo and Biome keep their dedicated paths in `app_lsp.rs`; everything else (rust-analyzer, pyright,
//! gopls, clangd, tailwind, ...) goes through here. A document's *primary* server (first installed
//! one of the registry for its language) is what `doc_lsp` returns, so hover, completion, definition,
//! rename, symbols... work unchanged; *companions* (tailwind, ruff) are synced the same text and their
//! diagnostics, code actions, hover and completion are merged in (`lspmerge`).

use crate::{
    app::App,
    app_overlay::Item,
    lsp::{Extra, Generic, Lsp, Pending},
    lspmerge::{Agg, MultiKind},
    lspreg::{self, Registry},
    lspsup::{self, Health},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

const PLAN_TTL: Duration = Duration::from_secs(30);
/// A merged request waits this long for a slow companion before answering with what it has.
const AGG_WAIT: Duration = Duration::from_millis(2500);

#[derive(Clone, Debug)]
pub struct PlanEntry {
    pub id: String,
    pub root: PathBuf,
    /// Resolved executable; `None` when the server is not installed.
    pub exe: Option<PathBuf>,
    pub primary: bool,
}

/// Which registry servers apply to one file (cached: probing the disk every frame would be wasteful).
#[derive(Debug)]
pub struct Plan {
    pub entries: Vec<PlanEntry>,
    at: Instant,
}

#[derive(Default)]
pub struct LspX {
    /// The `language_servers` value the registry was built from (`None`: not built yet).
    src: Option<Value>,
    pub reg: Registry,
    pub health: HashMap<(String, PathBuf), Health>,
    plans: RefCell<HashMap<(PathBuf, Option<&'static str>), Rc<Plan>>>,
    agg: Option<(Agg, Instant)>,
    serial: u64,
    told: HashSet<String>,
    /// Resolved commands of idle servers for the status listing: id -> (when, path).
    which: HashMap<String, (Instant, Option<PathBuf>)>,
    dead_logs: HashMap<String, String>,
    pub timing: lspsup::Timing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    Running,
    Starting,
    NotInstalled,
    Disabled,
    Crashed,
    /// Installed, starts when a matching file is opened.
    Ready,
}

impl RowState {
    pub fn label(self) -> &'static str {
        match self {
            RowState::Running => "running",
            RowState::Starting => "starting",
            RowState::NotInstalled => "not installed",
            RowState::Disabled => "disabled",
            RowState::Crashed => "crashed",
            RowState::Ready => "installed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServerRow {
    pub id: String,
    pub state: RowState,
    /// Resolved command line, or the configured one when it cannot be found.
    pub command: String,
    pub detail: String,
    pub languages: String,
    pub enabled: bool,
    pub builtin: bool,
}

fn real_params(params: &Value, real_uri: &str) -> Value {
    let mut p = params.clone();
    p["textDocument"]["uri"] = json!(real_uri);
    p
}

fn tail(log: &str, n: usize) -> String {
    let lines: Vec<&str> = log.lines().filter(|l| l.contains("stderr")).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.split_once("stderr").map_or(*l, |x| x.1).trim())
        .collect::<Vec<_>>()
        .join(" | ")
}

impl App {
    // ───────────── registry and plans ─────────────

    /// Rebuild the registry when `language_servers` changed in the configuration.
    pub(crate) fn lspx_sync_config(&mut self) {
        if self.lspx.src.as_ref() == Some(&self.config.language_servers) {
            return;
        }
        let first = self.lspx.src.is_none();
        let reg = Registry::with_user(&self.config.language_servers);
        self.lspx.src = Some(self.config.language_servers.clone());
        self.lspx.plans.borrow_mut().clear();
        if !reg.errors.is_empty() {
            let n = reg.errors.len();
            let more = if n > 1 { format!(" (+{} more, see Settings)", n - 1) } else { String::new() };
            self.say_for(format!("language_servers: {}{more}", reg.errors[0]), 10);
        }
        self.lspx.reg = reg;
        if !first {
            self.lspx_restart_all();
        }
    }

    pub(crate) fn lspx_plan(&self, doc: usize) -> Option<Rc<Plan>> {
        if !self.lsp_enabled() {
            return None;
        }
        let d = self.docs.get(doc)?;
        let path = d.path()?.to_path_buf();
        let key = (path, d.lang);
        if let Some(p) = self.lspx.plans.borrow().get(&key)
            && p.at.elapsed() < PLAN_TTL
        {
            return Some(p.clone());
        }
        let entries = self
            .lspx
            .reg
            .for_doc(d.lang, &key.0)
            .into_iter()
            .map(|s| {
                let root = s.root_for(&key.0);
                PlanEntry {
                    id: s.id.clone(),
                    exe: lspreg::resolve_command(&s.command, &root),
                    primary: s.role == lspreg::Role::Primary,
                    root,
                }
            })
            .collect();
        let plan = Rc::new(Plan { entries, at: Instant::now() });
        self.lspx.plans.borrow_mut().insert(key, plan.clone());
        Some(plan)
    }

    fn lspx_gave_up(&self, e: &PlanEntry) -> bool {
        self.lspx.health.get(&(e.id.clone(), e.root.clone())).is_some_and(|h| h.gave_up)
    }

    /// The first installed, not-given-up primary server of the document.
    pub(crate) fn lspx_primary_entry(&self, doc: usize) -> Option<PlanEntry> {
        let plan = self.lspx_plan(doc)?;
        plan.entries.iter().find(|e| e.primary && e.exe.is_some() && !self.lspx_gave_up(e)).cloned()
    }

    pub(crate) fn lspx_index(&self, id: &str, root: &Path) -> Option<usize> {
        self.lsps.iter().position(|l| l.generic.as_ref().is_some_and(|g| g.id == id) && l.root == root)
    }

    /// Index of the running primary server of a document.
    pub(crate) fn lspx_doc_lsp(&self, doc: usize) -> Option<usize> {
        let e = self.lspx_primary_entry(doc)?;
        self.lspx_index(&e.id, &e.root)
    }

    /// Companions of a document that are running and have it open.
    pub(crate) fn lspx_open_companions(&self, doc: usize) -> Vec<usize> {
        let Some(uri) = self.docs.get(doc).and_then(|d| d.real_uri()) else { return Vec::new() };
        let primary = self.lspx_doc_lsp(doc);
        (0..self.lsps.len())
            .filter(|&li| Some(li) != primary && self.lsps[li].is_generic() && self.lsps[li].ready)
            .filter(|&li| self.lsps[li].open_uris.contains(&uri))
            .collect()
    }

    fn lspx_tell_missing(&mut self, doc: usize) {
        let Some(plan) = self.lspx_plan(doc) else { return };
        let missing: Vec<&PlanEntry> = plan.entries.iter().filter(|e| e.primary && e.exe.is_none()).collect();
        let usable = plan.entries.iter().any(|e| e.primary && e.exe.is_some());
        if usable || missing.is_empty() {
            return;
        }
        let lang = self.docs[doc].lang.map(str::to_string).unwrap_or_else(|| {
            self.docs[doc]
                .path()
                .and_then(|p| p.extension())
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        if !self.lspx.told.insert(lang.clone()) {
            return;
        }
        let first = &missing[0].id;
        let hint = self.lspx.reg.get(first).map(|s| s.install.clone()).unwrap_or_default();
        let names: Vec<&str> = missing.iter().map(|e| e.id.as_str()).collect();
        let msg = if hint.is_empty() {
            format!("no {lang} language server found ({})", names.join(", "))
        } else {
            format!("no {lang} language server found: {hint} ({} also works)", names[1..].join(", "))
                .replace(" ( also works)", "")
        };
        self.say_for(msg, 6);
    }

    // ───────────── starting servers ─────────────

    /// The primary server of the document, started when needed. `None`: the document has no
    /// registry server (callers fall back to tsgo); `Some(None)`: one applies but is not available now.
    pub(crate) fn lspx_ensure_primary(&mut self, doc: usize) -> Option<Option<usize>> {
        let Some(e) = self.lspx_primary_entry(doc) else {
            self.lspx_tell_missing(doc);
            return None;
        };
        Some(self.lspx_ensure(&e))
    }

    pub(crate) fn lspx_ensure(&mut self, e: &PlanEntry) -> Option<usize> {
        if let Some(i) = self.lspx_index(&e.id, &e.root) {
            return Some(i);
        }
        let now = Instant::now();
        if !self.lspx.health.get(&(e.id.clone(), e.root.clone())).is_none_or(|h| h.may_start(now)) {
            return None;
        }
        self.lspx_spawn(e)
    }

    fn lspx_spawn(&mut self, e: &PlanEntry) -> Option<usize> {
        let spec = self.lspx.reg.get(&e.id)?.clone();
        let exe = e.exe.clone()?;
        let args = spec.args_for(&e.root);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut env = spec.env_for(&e.root);
        if exe.file_name().is_some_and(|n| n == "gopls") && !env.iter().any(|(k, _)| k == "GOMEMLIMIT") {
            env.push(("GOMEMLIMIT".into(), "512MiB".into()));
        }
        let extra = Extra {
            env,
            go_limit: false,
            generic: Some(Generic {
                id: spec.id.clone(),
                settings: spec.settings_for(&e.root),
                primary: spec.role == lspreg::Role::Primary,
                command: exe.clone(),
            }),
            init_options: spec.init_options_for(&e.root),
        };
        match Lsp::spawn_with(&spec.id, &exe, &args, &e.root, &e.root, 0, self.wake(), extra) {
            Ok(mut l) => {
                l.idle_since = Some(Instant::now());
                l.log_line("app", &format!("started {} {} in {}", exe.display(), args.join(" "), e.root.display()));
                self.lsps.push(l);
                self.say(format!("starting {}…", spec.id));
                Some(self.lsps.len() - 1)
            }
            Err(err) => {
                let base = self.lspx.timing.backoff;
                let h = self.lspx.health.entry((e.id.clone(), e.root.clone())).or_default();
                h.on_exit(Instant::now(), Duration::ZERO, err.to_string(), base);
                self.say_for(format!("failed to start {}: {err}", spec.id), 6);
                None
            }
        }
    }

    /// Per-frame work for registry servers: open/sync documents on the companions, stop idle
    /// servers, wake up for backoff timers. `pull` collects servers needing a diagnostics refresh.
    pub(crate) fn lspx_companions(
        &mut self,
        i: usize,
        now: Instant,
        want: &mut impl FnMut(Instant),
        pull: &mut Vec<usize>,
    ) {
        let Some(plan) = self.lspx_plan(i) else { return };
        if plan.entries.is_empty() {
            return;
        }
        if self.docs[i].path().is_none() {
            return;
        }
        if self.lspx_primary_entry(i).is_none() && !crate::app_lsp::ts_family(self.docs[i].lang) {
            self.lspx_tell_missing(i);
        }
        let wanted: Vec<&PlanEntry> =
            plan.entries.iter().filter(|e| !e.primary && e.exe.is_some() && !self.lspx_gave_up(e)).collect();
        for e in wanted {
            let Some(li) = self.lspx_ensure(e) else {
                if let Some(t) = self.lspx.health.get(&(e.id.clone(), e.root.clone())).and_then(|h| h.next_try) {
                    want(t);
                }
                continue;
            };
            let Some(uri) = self.docs[i].real_uri() else { continue };
            if !self.lsps[li].open_uris.contains(&uri)
                && let Some(p) = Self::doc_open_params(&self.docs[i], uri)
            {
                self.lsps[li].notify("textDocument/didOpen", p);
                pull.push(li);
            }
        }
        // A document with no primary server open still needs its debounced edits delivered.
        if !self.docs[i].lsp_open
            && let Some(t) = self.docs[i].changed_at
        {
            if t + Duration::from_millis(120) <= now {
                self.send_change(i);
                pull.extend(self.lspx_open_companions(i));
            } else {
                want(t + Duration::from_millis(120));
            }
        }
    }

    /// Idle shutdown, backoff wake-ups and the merged-request timeout.
    pub(crate) fn lspx_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let mut stop = Vec::new();
        let grace = self.lspx.timing.idle_grace;
        for (li, l) in self.lsps.iter().enumerate() {
            if !l.is_generic() || !l.open_uris.is_empty() {
                continue;
            }
            if let Some(t) = l.idle_since {
                if t + grace <= now {
                    stop.push(li);
                } else {
                    want(t + grace);
                }
            }
        }
        for li in stop.into_iter().rev() {
            let mut l = self.lsps.remove(li);
            l.log_line("app", "idle: no open documents, shutting down");
            l.shutdown();
            self.lspx_forget_docs(&l);
        }
        for h in self.lspx.health.values() {
            if let Some(t) = h.next_try
                && t > now
            {
                want(t);
            }
        }
        if let Some((_, t0)) = &self.lspx.agg {
            if *t0 + AGG_WAIT <= now {
                self.lspx_finish_agg();
            } else {
                want(*t0 + AGG_WAIT);
            }
        }
    }

    /// Documents that were open on a server that is gone lose its diagnostics and must reopen.
    fn lspx_forget_docs(&mut self, l: &Lsp) {
        let Some(g) = &l.generic else { return };
        for i in 0..self.docs.len() {
            let Some(uri) = self.docs[i].real_uri() else { continue };
            if !l.open_uris.contains(&uri) {
                continue;
            }
            self.docs[i].set_ext_diags(&g.id, Vec::new());
            let was_primary = self.lspx_plan(i).is_some_and(|p| {
                p.entries
                    .iter()
                    .find(|e| e.primary && e.exe.is_some())
                    .is_some_and(|e| e.id == g.id && e.root == l.root)
            });
            if was_primary {
                self.docs[i].lsp_open = false;
            }
        }
        self.request_redraw();
    }

    /// A registry server exited on its own (or failed to initialize): account for the crash and
    /// schedule the restart. `li` is removed from `lsps`.
    pub(crate) fn lspx_server_gone(&mut self, li: usize) {
        let l = self.lsps.remove(li);
        let Some(g) = l.generic.clone() else { return };
        let log = l.log_text();
        let err = tail(&log, 3);
        let uptime = l.started.elapsed();
        let base = self.lspx.timing.backoff;
        let h = self.lspx.health.entry((g.id.clone(), l.root.clone())).or_default();
        let msg = match h.on_exit(Instant::now(), uptime, err, base) {
            Some(d) => format!("{} stopped unexpectedly — restarting in {:.1}s", g.id, d.as_secs_f32()),
            None => format!("{} keeps crashing and was turned off — see \"LSP: Show server log\"", g.id),
        };
        self.say_for(msg, 6);
        self.lspx_forget_docs(&l);
        // The log of the dead server stays available.
        self.lspx.which.remove(&g.id);
        self.lspx_stash_log(&g.id, log);
    }

    fn lspx_stash_log(&mut self, id: &str, log: String) {
        if self.lspx.dead_logs.len() >= 8 {
            self.lspx.dead_logs.clear();
        }
        self.lspx.dead_logs.insert(id.to_string(), log);
    }

    // ───────────── document sync ─────────────

    /// Deliver the document's new text to every companion that has it open.
    pub(crate) fn lspx_send_change(&mut self, i: usize, params: &Value) {
        let Some(uri) = self.docs[i].real_uri() else { return };
        let p = real_params(params, &uri);
        for li in self.lspx_open_companions(i) {
            self.lsps[li].notify("textDocument/didChange", p.clone());
        }
    }

    pub(crate) fn lspx_did_save(&mut self, i: usize) -> Vec<usize> {
        let Some(uri) = self.docs[i].real_uri() else { return Vec::new() };
        let cs = self.lspx_open_companions(i);
        for &li in &cs {
            self.lsps[li].notify("textDocument/didSave", json!({"textDocument": {"uri": uri}}));
        }
        cs
    }

    /// The document is closing (or changing language): close it on every registry server.
    pub(crate) fn lspx_close_doc(&mut self, i: usize) {
        let Some(uri) = self.docs.get(i).and_then(|d| d.real_uri()) else { return };
        for li in 0..self.lsps.len() {
            if self.lsps[li].is_generic() && self.lsps[li].open_uris.contains(&uri) {
                self.lsps[li].notify("textDocument/didClose", json!({"textDocument": {"uri": uri}}));
                if let Some(g) = &self.lsps[li].generic {
                    let id = g.id.clone();
                    self.docs[i].set_ext_diags(&id, Vec::new());
                }
            }
        }
    }

    /// A document's language changed: it leaves every registry server (the tick reopens it where it now fits).
    pub(crate) fn lspx_language_changed(&mut self, i: usize) {
        let Some(uri) = self.docs.get(i).and_then(|d| d.real_uri()) else { return };
        let on = |l: &Lsp| l.is_generic() && l.open_uris.contains(&uri);
        if !self.lsps.iter().any(on) {
            return;
        }
        let primary_open =
            self.docs[i].lsp_open && self.lsps.iter().any(|l| on(l) && l.generic.as_ref().is_some_and(|g| g.primary));
        self.lspx_close_doc(i);
        self.lspx.plans.borrow_mut().clear();
        let soon = Instant::now() + Duration::from_millis(400);
        let d = &mut self.docs[i];
        if primary_open {
            d.lsp_open = false;
        }
        d.symbols_due = Some(soon);
        d.sem_due = Some(soon);
    }

    pub(crate) fn lspx_pull(&mut self, li: usize) {
        let l = &self.lsps[li];
        if !l.has_cap("diagnosticProvider") {
            return;
        }
        let uris: Vec<String> = l.open_uris.iter().cloned().collect();
        for uri in uris {
            self.lsps[li].request(
                "textDocument/diagnostic",
                json!({"textDocument": {"uri": uri}}),
                Pending::Diagnostic { uri },
            );
        }
    }

    /// Disk changes under a server's root reach it as `workspace/didChangeWatchedFiles`.
    pub(crate) fn lspx_fs_events(&mut self, evs: &[crate::watcher::FsEvent]) -> bool {
        use crate::watcher::FsKind;
        let mut touched = false;
        for li in 0..self.lsps.len() {
            if !self.lsps[li].is_generic() || !self.lsps[li].ready {
                continue;
            }
            let root = self.lsps[li].root.clone();
            let mut changes = Vec::new();
            for e in evs {
                let Ok(rel) = e.path.strip_prefix(&root) else { continue };
                if rel.components().any(|c| matches!(c.as_os_str().to_str(), Some(".git" | "node_modules" | "target")))
                {
                    continue;
                }
                let ty = match e.kind {
                    FsKind::Created => 1,
                    FsKind::Removed => 3,
                    FsKind::Modified | FsKind::Moved => {
                        if e.path.exists() {
                            2
                        } else {
                            3
                        }
                    }
                };
                changes.push(json!({"uri": crate::doc::path_to_uri(&e.path), "type": ty}));
            }
            if !changes.is_empty() {
                self.lsps[li].notify("workspace/didChangeWatchedFiles", json!({"changes": changes}));
                touched = true;
            }
        }
        touched
    }

    // ───────────── incoming ─────────────

    /// The `initialize` answer of a registry server.
    pub(crate) fn lspx_initialized(&mut self, li: usize, result: &Value) {
        self.lsps[li].on_initialized(result);
        let id = self.lsps[li].name.clone();
        let settings = self.lsps[li].generic.as_ref().map(|g| g.settings.clone()).unwrap_or(Value::Null);
        if !settings.is_null() {
            self.lsps[li].notify("workspace/didChangeConfiguration", json!({"settings": settings}));
        }
        let server = result["serverInfo"]["name"].as_str().unwrap_or("");
        let ver = result["serverInfo"]["version"].as_str().unwrap_or("");
        self.lsps[li].log_line("app", &format!("initialized {server} {ver}"));
        self.say(format!("{id} ready"));
        self.lspx_rearm(li);
    }

    /// Ask again for tokens, outline and diagnostics of the documents of server `li`
    /// (the first request may have been sent before the server could answer).
    fn lspx_rearm(&mut self, li: usize) {
        let soon = Instant::now() + Duration::from_millis(250);
        let uris = self.lsps[li].open_uris.clone();
        for d in &mut self.docs {
            if d.real_uri().is_some_and(|u| uris.contains(&u)) {
                d.symbols_due = Some(soon);
                d.sem_due = Some(soon);
                d.lens_due = Some(soon);
            }
        }
        self.lsp_refresh_at = Some(soon);
    }

    /// Notifications of registry servers. True when handled.
    pub(crate) fn lspx_on_notification(&mut self, li: usize, method: &str, params: &Value) -> bool {
        if !self.lsps.get(li).is_some_and(Lsp::is_generic) {
            return false;
        }
        let id = self.lsps[li].name.clone();
        match method {
            "textDocument/publishDiagnostics" => {
                if let Some(uri) = params["uri"].as_str() {
                    self.set_diags_from(uri, &params["diagnostics"], Some(false), &id);
                    self.request_redraw();
                }
            }
            "window/showMessage" => {
                if let Some(m) = params["message"].as_str() {
                    self.lsps[li].log_line("show", m);
                    // Info (3) and log (4) messages are chatter; warnings and errors are shown.
                    if params["type"].as_u64().unwrap_or(3) <= 2 {
                        self.say(format!("{id}: {m}"));
                    }
                }
            }
            "window/logMessage" => {
                if let Some(m) = params["message"].as_str() {
                    self.lsps[li].log_line("log", m);
                }
            }
            "$/progress" => self.lspx_progress(li, params),
            _ => {}
        }
        true
    }

    fn lspx_progress(&mut self, li: usize, params: &Value) {
        let token = match &params["token"] {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return,
        };
        let id = self.lsps[li].name.clone();
        let prev = self.lsps[li].progress.get(&token).cloned();
        match lspsup::progress_text(prev.as_deref(), &params["value"]) {
            Some(t) => {
                self.lsps[li].progress.insert(token, t.clone());
                self.say(format!("{id}: {t}"));
            }
            None => {
                self.lsps[li].progress.remove(&token);
                if self.lsps[li].progress.is_empty() {
                    self.say(format!("{id}: ready"));
                    self.lspx_rearm(li);
                }
            }
        }
        self.request_redraw();
    }

    /// Requests of registry servers. True when handled (the reply was sent).
    pub(crate) fn lspx_on_request(&mut self, li: usize, rid: &Value, method: &str, params: &Value) -> bool {
        if !self.lsps.get(li).is_some_and(Lsp::is_generic) {
            return false;
        }
        let result = match method {
            "workspace/configuration" => {
                let settings = self.lsps[li].generic.as_ref().map(|g| g.settings.clone()).unwrap_or(Value::Null);
                lspsup::configuration_reply(&settings, params)
            }
            "client/registerCapability" => {
                let l = &mut self.lsps[li];
                let (caps, regs) = (&mut l.caps, &mut l.registrations);
                lspsup::register(caps, regs, params);
                Value::Null
            }
            "client/unregisterCapability" => {
                let l = &mut self.lsps[li];
                let (caps, regs) = (&mut l.caps, &mut l.registrations);
                lspsup::unregister(caps, regs, params);
                Value::Null
            }
            "workspace/applyEdit" => {
                self.apply_workspace_edit(&params["edit"]);
                json!({"applied": true})
            }
            "workspace/workspaceFolders" => {
                let root = self.lsps[li].root.clone();
                json!([{"uri": crate::doc::path_to_uri(&root), "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()}])
            }
            "workspace/codeLens/refresh" | "workspace/inlayHint/refresh" | "workspace/semanticTokens/refresh" => {
                self.lspx_rearm(li);
                Value::Null
            }
            // window/workDoneProgress/create, window/showMessageRequest, window/showDocument...
            _ => Value::Null,
        };
        self.lsps[li].respond(rid.clone(), result);
        true
    }

    // ───────────── merged requests ─────────────

    /// Send `method` to the document's primary server and to every companion that can answer, and
    /// merge the answers. False when no companion applies (the caller then sends its plain request).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn lspx_request_multi(
        &mut self,
        kind: MultiKind,
        i: usize,
        primary: usize,
        at: usize,
        method: &str,
        params: Value,
        provider: &str,
    ) -> bool {
        let cs: Vec<usize> =
            self.lspx_open_companions(i).into_iter().filter(|&c| self.lsps[c].has_cap(provider)).collect();
        if cs.is_empty() {
            return false;
        }
        let Some(real) = self.docs[i].real_uri() else { return false };
        let uri = self.docs[i].uri().unwrap_or_default();
        self.lspx.serial += 1;
        let serial = self.lspx.serial;
        self.lspx.agg = Some((Agg::new(kind, serial, uri, at, 1 + cs.len()), Instant::now()));
        self.lsps[primary].request(method, params.clone(), Pending::Multi { kind, serial });
        for c in cs {
            self.lsps[c].request(method, real_params(&params, &real), Pending::Multi { kind, serial });
        }
        true
    }

    pub(crate) fn lspx_on_multi(&mut self, li: usize, kind: MultiKind, serial: u64, v: Value) {
        let primary = self.active_primary_index();
        let Some((agg, _)) = &mut self.lspx.agg else { return };
        if agg.kind != kind || agg.serial != serial {
            return;
        }
        let order = if Some(li) == primary { 0 } else { 1 + li };
        let name = self.lsps.get(li).map(|l| l.name.clone()).unwrap_or_default();
        if agg.push(order, &name, v) {
            self.lspx_finish_agg();
        }
    }

    fn active_primary_index(&self) -> Option<usize> {
        self.doc_lsp(self.active)
    }

    fn lspx_finish_agg(&mut self) {
        let Some((mut agg, _)) = self.lspx.agg.take() else { return };
        let v = agg.merged();
        match agg.kind {
            MultiKind::Hover => self.on_hover(&agg.uri, agg.at, &v),
            MultiKind::Completion => self.on_completion(&agg.uri, agg.at, &v),
        }
    }

    /// Code-action requests for the companions of document `i`; returns how many were sent.
    pub(crate) fn lspx_code_actions(&mut self, i: usize, params: &Value) -> usize {
        let Some(real) = self.docs[i].real_uri() else { return 0 };
        let cs: Vec<usize> =
            self.lspx_open_companions(i).into_iter().filter(|&c| self.lsps[c].has_cap("codeActionProvider")).collect();
        for &c in &cs {
            self.lsps[c].request("textDocument/codeAction", real_params(params, &real), Pending::CodeActions);
        }
        cs.len()
    }

    /// Server that should resolve/execute a code action or a completion item (tagged `__server`).
    pub(crate) fn lspx_server_named(&self, name: &str, i: usize) -> Option<usize> {
        let uri = self.docs.get(i)?.real_uri()?;
        (0..self.lsps.len()).find(|&li| self.lsps[li].name == name && self.lsps[li].open_uris.contains(&uri))
    }

    /// De-duplicate code actions by title (the primary's come first).
    pub(crate) fn lspx_dedupe_actions(items: &mut Vec<Item>) {
        let mut seen = HashSet::new();
        items.retain(|it| seen.insert((it.label.clone(), it.detail.clone())));
    }

    // ───────────── status, settings, restart ─────────────

    /// One row per registry server (and tsgo), for the settings page and the status tooltip.
    pub fn lspx_rows(&mut self) -> Vec<ServerRow> {
        self.lspx_sync_config();
        let mut rows = Vec::new();
        let now = Instant::now();
        let specs: Vec<lspreg::ServerSpec> = self.lspx.reg.servers.clone();
        for s in specs {
            let running = self.lsps.iter().find(|l| l.generic.as_ref().is_some_and(|g| g.id == s.id));
            let crashed = self
                .lspx
                .health
                .iter()
                .find(|((id, _), h)| *id == s.id && h.gave_up)
                .map(|(_, h)| h.last_error.clone());
            let cached = self.lspx.which.get(&s.id).filter(|(t, _)| now.duration_since(*t) < PLAN_TTL).cloned();
            let exe = match (running, cached) {
                (Some(l), _) => l.generic.as_ref().map(|g| g.command.clone()),
                (None, Some((_, p))) => p,
                (None, None) => {
                    let p = lspreg::installed_anywhere(&s.command);
                    self.lspx.which.insert(s.id.clone(), (now, p.clone()));
                    p
                }
            };
            let (state, detail) = if !s.enabled {
                (RowState::Disabled, s.disabled_by.clone().unwrap_or_else(|| "turned off".into()))
            } else if let Some(err) = crashed {
                (RowState::Crashed, if err.is_empty() { "exited repeatedly".into() } else { err })
            } else if let Some(l) = running {
                let d = if l.ready { format!("{} open file(s)", l.open_uris.len()) } else { "starting".into() };
                (if l.ready { RowState::Running } else { RowState::Starting }, d)
            } else if exe.is_none() {
                (RowState::NotInstalled, s.install.clone())
            } else {
                (RowState::Ready, "starts when a matching file is opened".into())
            };
            let cmd = exe.map(|p| p.display().to_string()).unwrap_or_else(|| s.command.clone());
            let command = if s.args.is_empty() { cmd } else { format!("{cmd} {}", s.args.join(" ")) };
            let mut langs: Vec<String> = s.languages.clone();
            langs.extend(s.extensions.iter().map(|e| format!(".{e}")));
            rows.push(ServerRow {
                id: s.id.clone(),
                state,
                command,
                detail,
                languages: langs.join(", "),
                enabled: s.enabled,
                builtin: s.builtin,
            });
        }
        rows
    }

    /// Messages about configuration problems, for the settings page.
    pub fn lspx_config_errors(&mut self) -> Vec<String> {
        self.lspx_sync_config();
        self.lspx.reg.errors.clone()
    }

    /// Stop every registry server and forget crash counters (they start again with the next tick).
    pub fn lspx_restart_all(&mut self) {
        let mut i = 0;
        while i < self.lsps.len() {
            if self.lsps[i].is_generic() {
                let mut l = self.lsps.remove(i);
                l.shutdown();
                self.lspx_forget_docs(&l);
            } else {
                i += 1;
            }
        }
        self.lspx.health.clear();
        self.lspx.plans.borrow_mut().clear();
        self.lspx.which.clear();
        self.lspx.told.clear();
        self.lspx.agg = None;
        self.lspx.dead_logs.clear();
    }

    /// Turn a server on or off (writes `language_servers.<id>.enabled` to the config).
    pub fn lspx_set_enabled(&mut self, id: &str, on: bool) {
        let mut v = match &self.config.language_servers {
            Value::Object(_) => self.config.language_servers.clone(),
            _ => json!({}),
        };
        let entry = &mut v[id];
        if !entry.is_object() {
            *entry = json!({});
        }
        entry["enabled"] = json!(on);
        self.config.language_servers = v;
        self.config.save();
        self.lspx_sync_config();
        self.say(format!("{id}: {}", if on { "enabled" } else { "disabled" }));
    }

    // ───────────── RAM ─────────────

    /// ` + tsgo 65 MB` style suffix of the memory readout: each server by name for one or two
    /// servers, `N servers` with the sum beyond that (the tooltip lists them all).
    pub fn lspx_ram_suffix(&mut self) -> String {
        let per = self.lspx_ram_list();
        let mut s = String::new();
        match per.len() {
            0 => {}
            1 | 2 => {
                for (n, mb) in &per {
                    s.push_str(&format!(" + {n} {mb:.0} MB"));
                }
            }
            n => s.push_str(&format!(" + {n} servers {:.0} MB", per.iter().map(|p| p.1).sum::<f64>())),
        }
        let crashed: Vec<String> =
            self.lspx.health.iter().filter(|(_, h)| h.gave_up).map(|((id, _), _)| id.clone()).collect();
        for c in crashed {
            s.push_str(&format!(" · {c} crashed"));
        }
        s
    }

    pub fn lspx_ram_list(&self) -> Vec<(String, f64)> {
        self.lsps
            .iter()
            .filter_map(|l| crate::mem::pss_of(l.pid()).map(|kb| (l.name.clone(), kb as f64 / 1024.0)))
            .collect()
    }

    /// Tooltip line listing every server's memory (only worth showing from three servers on).
    pub fn lspx_ram_detail(&self) -> Option<String> {
        let per = self.lspx_ram_list();
        (per.len() > 2).then(|| per.iter().map(|(n, mb)| format!("{n} {mb:.0} MB")).collect::<Vec<_>>().join(" · "))
    }

    // ───────────── log ─────────────

    /// Everything the running servers (and the ones that just died) printed, newest last, as text.
    pub fn lspx_log_text(&self) -> String {
        let mut out = String::new();
        for l in &self.lsps {
            if l.is_generic() {
                out.push_str(&format!(
                    "===== {} (pid {}, root {}) =====\n{}\n",
                    l.name,
                    l.pid(),
                    l.root.display(),
                    l.log_text()
                ));
            }
        }
        for (id, log) in &self.lspx.dead_logs {
            out.push_str(&format!("===== {id} (exited) =====\n{log}\n"));
        }
        if out.is_empty() {
            out.push_str(
                "No registry language server has run in this session.\n\ntsgo and Biome are not logged here.\n",
            );
        }
        out
    }

    /// "LSP: Show server log": writes the bounded log next to the other caches and opens it.
    pub fn lspx_show_log(&mut self) {
        let text = self.lspx_log_text();
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))
            .unwrap_or_else(std::env::temp_dir)
            .join("lumen-text");
        let _ = std::fs::create_dir_all(&base);
        let path = base.join("language-server.log");
        match std::fs::write(&path, text) {
            Ok(()) => {
                self.open_path(&path, None);
            }
            Err(e) => self.say(format!("could not write the server log: {e}")),
        }
    }
}

impl App {
    /// Formatting from a companion server (ruff, a formatter-only server) when the primary has none.
    pub(crate) fn format_with_companion(&mut self, i: usize, wait: Duration) -> Option<crate::app_format::Fmt> {
        use crate::app_format::Fmt;
        let cs: Vec<usize> = self
            .lspx_open_companions(i)
            .into_iter()
            .filter(|&c| self.lsps[c].has_cap("documentFormattingProvider"))
            .collect();
        let li = *cs.first()?;
        let name = self.lsps[li].name.clone();
        self.flush(i);
        let d = &self.docs[i];
        let uri = d.real_uri()?;
        let tab = if d.ed.indent.contains('\t') { d.ed.tab_w } else { d.ed.indent.len().max(1) };
        let params = json!({
            "textDocument": {"uri": uri},
            "options": {"tabSize": tab, "insertSpaces": !d.ed.indent.contains('\t'),
                        "trimTrailingWhitespace": true, "insertFinalNewline": true}
        });
        let version = d.version;
        let v = match self.lsp_request_wait(li, "textDocument/formatting", params, wait) {
            Ok(v) => v,
            Err(e) if e == crate::app_format::TIMEOUT => return Some(Fmt::TimedOut(name)),
            Err(e) => return Some(Fmt::Failed(format!("{name}: {e}"))),
        };
        if self.docs.get(i).is_none_or(|d| d.version != version) {
            return Some(Fmt::Failed("the text changed during formatting".into()));
        }
        let edits = crate::app_lsp::parse_edits(&v);
        if edits.is_empty() {
            return Some(Fmt::Same(name));
        }
        let before = self.docs[i].buf.to_string();
        self.docs[i].apply_lsp_edits(&edits);
        Some(if self.docs[i].buf.to_string() == before { Fmt::Same(name) } else { Fmt::Changed(name) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, scratch_base, test_app};
    use std::time::Instant;

    /// The fake server built from `examples/fake_lsp.rs` (cargo builds examples for `cargo test`).
    fn fake_exe() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let p = exe.parent()?.parent()?.join("examples/fake_lsp");
        p.exists().then_some(p)
    }

    fn project(tag: &str) -> PathBuf {
        let d = scratch_base().join(format!("lspx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    /// Run the sync tick and message pump until `cond` holds (or give up after `ms`).
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

    fn server(exe: &Path, name: &str, role: &str, env: Value) -> Value {
        let mut env = env;
        env["FAKE_NAME"] = json!(name);
        json!({"command": exe, "languages": ["fk"], "role": role, "env": env})
    }

    fn open(app: &mut App, f: &Path, text: &str) -> usize {
        std::fs::write(f, text).unwrap();
        let i = app.open_path(f, None).expect("open");
        app.active = i;
        i
    }

    fn diag_msgs(app: &App, i: usize) -> Vec<String> {
        app.docs[i].diags.iter().map(|d| d.message.clone()).collect()
    }

    fn stop(app: &mut App) {
        for l in &mut app.lsps {
            l.shutdown();
        }
    }

    #[test]
    fn starts_lazily_and_publishes_diagnostics_with_utf16_columns() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("basic");
        let mut app = test_app(&dir);
        app.config.language_servers = json!({"fake-a": server(&exe, "a", "primary", json!({}))});
        app.lsp_tick(Instant::now(), &mut |_| {});
        assert!(app.lsps.is_empty(), "no server before a matching file is open");
        let i = open(&mut app, &dir.join("x.fk"), "ok\n😀 FIXME here\n");
        assert!(pump(&mut app, 8000, |a| !a.docs[i].diags.is_empty()), "no diagnostics: {:?}", app.status);
        assert_eq!(app.lsps.len(), 1);
        assert_eq!(app.lsps[0].name, "fake-a");
        assert!(app.docs[i].lsp_open && app.doc_lsp(i) == Some(0));
        let d = &app.docs[i].diags;
        assert_eq!(d.len(), 2, "{d:?}");
        // The emoji is 2 UTF-16 units but one character: "FIXME" starts at char column 2.
        assert_eq!((d[0].sl, d[0].sc, d[0].ec), (1, 2, 7));
        assert!(d.iter().any(|g| g.message == "fixme from a" && g.source == "a" && g.code == "F1"));
        // A server started lazily with the registry's env, and its stderr is in the bounded log.
        assert!(app.lsps[0].log_text().contains("fake server a started"));
        assert!(app.lspx_log_text().contains("===== fake-a"));
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn several_servers_merge_diagnostics_hover_and_completion() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("merge");
        let mut app = test_app(&dir);
        app.config.language_servers = json!({
            "fake-a": server(&exe, "a", "primary", json!({})),
            "fake-b": server(&exe, "b", "companion", json!({"FAKE_PULL": "1"})),
        });
        let i = open(&mut app, &dir.join("x.fk"), "let a = 1; // FIXME\n");
        let want = ["common finding", "fixme from a", "fixme from b"];
        assert!(
            pump(&mut app, 8000, |a| {
                let mut m = diag_msgs(a, i);
                m.sort();
                m == want
            }),
            "{:?}",
            diag_msgs(&app, i)
        );
        // The shared finding appears once, the first source (the primary's, pushed) wins.
        assert_eq!(app.docs[i].diags.iter().filter(|g| g.message == "common finding").count(), 1);
        assert_eq!(app.lsps.len(), 2);

        // Hover goes to both and comes back as one tip.
        let d = &mut app.docs[i];
        d.ed.click(&mut d.buf, 5, false);
        app.request_hover(5);
        assert!(pump(&mut app, 4000, |a| a.hover.is_some()), "no hover");
        for l in &app.lsps {
            assert!(l.log_text().contains("textDocument/hover"), "{} was not asked", l.name);
        }
        let tip_text = format!("{:?}", app.hover.as_ref().unwrap().blocks);
        assert!(tip_text.contains("hover from a") && tip_text.contains("hover from b"), "{tip_text}");

        // Completion: both lists, the repeated label once, companion items tagged.
        let end = app.docs[i].buf.len_chars();
        let d = &mut app.docs[i];
        d.ed.click(&mut d.buf, end, false);
        app.request_completion(None);
        assert!(pump(&mut app, 4000, |a| a.completion.is_some()), "no completion");
        let mut labels: Vec<String> = app.completion.as_ref().unwrap().items.iter().map(|c| c.label.clone()).collect();
        labels.sort();
        assert_eq!(labels, vec!["a_item", "b_item", "shared_item"]);

        // The memory readout collapses from three servers on and lists them in the tooltip text.
        assert!(app.lspx_ram_suffix().contains("fake-a") && app.lspx_ram_suffix().contains("fake-b"));
        app.config.language_servers["fake-c"] = server(&exe, "c", "companion", json!({}));
        assert!(pump(&mut app, 8000, |a| a.lsps.len() == 3 && a.lsps.iter().all(|l| l.ready)));
        assert!(app.lspx_ram_suffix().contains("+ 3 servers"), "{}", app.lspx_ram_suffix());
        let detail = app.lspx_ram_detail().expect("tooltip detail");
        assert!(detail.contains("fake-a") && detail.contains("fake-c"), "{detail}");
        assert!(crate::app_topbar::memory_mb(&format!("RAM 10 MB{}", app.lspx_ram_suffix())) > 10.0);
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_diagnostics_dynamic_registration_configuration_and_progress() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("caps");
        let mut app = test_app(&dir);
        let mut s = server(
            &exe,
            "a",
            "primary",
            json!({"FAKE_PULL": "1", "FAKE_DYNAMIC": "1", "FAKE_CONFIG": "1", "FAKE_PROGRESS": "1"}),
        );
        s["settings"] = json!({"fake": {"level": 3}});
        app.config.language_servers = json!({"fake-a": s});
        let i = open(&mut app, &dir.join("x.fk"), "// FIXME\n");
        assert!(pump(&mut app, 8000, |a| !a.docs[i].diags.is_empty()), "pull diagnostics never arrived");
        assert!(
            pump(&mut app, 4000, |a| a.lsps[0].has_cap("documentFormattingProvider")),
            "dynamic registration missed"
        );
        assert!(
            pump(&mut app, 4000, |a| a.lsps[0].log_text().contains("config reply: [3,null]")),
            "{}",
            app.lsps[0].log_text()
        );
        assert!(app.lsps[0].log_text().contains("$/progress") && app.lsps[0].progress.is_empty());
        // Formatting through the dynamically registered capability.
        let fmt = app.format_doc(i, Duration::from_secs(5));
        assert_eq!(fmt, crate::app_format::Fmt::Changed("fake-a".into()), "{:?}", app.status);
        assert!(app.docs[i].buf.to_string().starts_with("// formatted by a\n"));
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn companion_formats_when_the_primary_cannot() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("fmtc");
        let mut app = test_app(&dir);
        app.config.language_servers = json!({
            "fake-a": server(&exe, "a", "primary", json!({})),
            "fake-b": server(&exe, "b", "companion", json!({"FAKE_FORMAT": "1"})),
        });
        let i = open(&mut app, &dir.join("x.fk"), "x\n");
        assert!(
            pump(&mut app, 8000, |a| a.lsps.len() == 2 && a.lsps.iter().all(|l| l.ready && !l.open_uris.is_empty()))
        );
        let fmt = app.format_doc(i, Duration::from_secs(5));
        assert_eq!(fmt, crate::app_format::Fmt::Changed("fake-b".into()), "{:?}", app.status);
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_restarts_with_backoff_then_gives_up_and_restart_resets() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("crash");
        let mut app = test_app(&dir);
        app.lspx.timing.backoff = Duration::from_millis(15);
        app.config.language_servers = json!({"fake-a": server(&exe, "a", "primary", json!({}))});
        let f = dir.join("x.fk");
        let i = open(&mut app, &f, "fine // FIXME\n");
        assert!(pump(&mut app, 8000, |a| !a.docs[i].diags.is_empty()));
        // The server dies once: it is restarted and the document comes back.
        app.docs[i].replace_text("CRASH\n");
        assert!(pump(&mut app, 8000, |a| a.lspx.health.values().any(|h| h.crashes == 1)), "{:?}", app.status);
        assert!(app.docs[i].diags.is_empty(), "a dead server's diagnostics are dropped");
        app.docs[i].replace_text("fine again // FIXME\n");
        assert!(
            pump(&mut app, 8000, |a| !a.docs[i].diags.is_empty() && a.lsps.len() == 1),
            "never came back: {:?}",
            app.status
        );
        assert!(app.docs[i].lsp_open);
        // A document that kills it every time: restarts are bounded, then it is reported crashed.
        app.docs[i].replace_text("CRASH\n");
        assert!(pump(&mut app, 20000, |a| a.lspx.health.values().any(|h| h.gave_up)), "{:?}", app.lspx.health);
        assert!(app.lsps.is_empty());
        assert!(app.status.as_ref().unwrap().0.contains("Show server log"), "{:?}", app.status);
        let rows = app.lspx_rows();
        let row = rows.iter().find(|r| r.id == "fake-a").unwrap();
        assert_eq!(row.state, RowState::Crashed);
        assert!(row.detail.contains("crashing on purpose"), "{}", row.detail);
        assert!(app.lspx_ram_suffix().contains("fake-a crashed"));
        assert!(app.lspx_log_text().contains("(exited)"));
        // No endless respawning after giving up.
        std::thread::sleep(Duration::from_millis(100));
        app.lsp_tick(Instant::now(), &mut |_| {});
        assert!(app.lsps.is_empty());
        // "LSP: Restart language servers" forgets the crashes; with sane text it works again.
        app.docs[i].replace_text("fine // FIXME\n");
        app.restart_lsp();
        assert!(pump(&mut app, 8000, |a| !a.docs[i].diags.is_empty()), "{:?}", app.status);
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_that_dies_during_startup_counts_as_a_crash() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("early");
        let mut app = test_app(&dir);
        app.lspx.timing.backoff = Duration::from_millis(10);
        app.config.language_servers =
            json!({"fake-a": server(&exe, "a", "primary", json!({"FAKE_EXIT_AFTER_INIT": "1"}))});
        open(&mut app, &dir.join("x.fk"), "x\n");
        assert!(pump(&mut app, 20000, |a| a.lspx.health.values().any(|h| h.gave_up)), "{:?}", app.lspx.health);
        assert!(app.lspx_rows().iter().any(|r| r.state == RowState::Crashed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_stops_after_the_idle_grace_when_its_last_document_closes() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("idle");
        let mut app = test_app(&dir);
        app.lspx.timing.idle_grace = Duration::from_millis(250);
        app.config.language_servers = json!({"fake-a": server(&exe, "a", "primary", json!({}))});
        let i = open(&mut app, &dir.join("x.fk"), "// FIXME\n");
        assert!(pump(&mut app, 8000, |a| !a.docs[i].diags.is_empty()));
        app.close_tab(i, true);
        assert_eq!(app.lsps.len(), 1, "the server outlives the document for a grace period");
        assert!(app.lsps[0].open_uris.is_empty());
        assert!(
            pump(&mut app, 3000, |a| a.lsps.is_empty()),
            "idle server was never stopped: {:?}",
            app.lsps.iter().map(|l| (l.open_uris.clone(), l.idle_since.map(|t| t.elapsed()))).collect::<Vec<_>>()
        );
        // Reopening starts it again.
        let j = open(&mut app, &dir.join("y.fk"), "// FIXME y\n");
        assert!(pump(&mut app, 8000, |a| !a.docs[j].diags.is_empty()));
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rows_report_status_and_toggle_persists() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("rows");
        let mut app = test_app(&dir);
        app.config.language_servers = json!({
            "fake-a": server(&exe, "a", "primary", json!({})),
            "ghost": {"command": "no-such-server-binary-xyz", "languages": ["gh"]},
            "taplo": {"enabled": false},
        });
        let rows = app.lspx_rows();
        let get = |id: &str| rows.iter().find(|r| r.id == id).unwrap().clone();
        assert_eq!(get("fake-a").state, RowState::Ready);
        assert_eq!(get("ghost").state, RowState::NotInstalled);
        assert_eq!(get("taplo").state, RowState::Disabled);
        assert_eq!(get("fake-a").command, exe.display().to_string());
        let i = open(&mut app, &dir.join("x.fk"), "// FIXME\n");
        assert!(pump(&mut app, 8000, |a| !a.docs[i].diags.is_empty()));
        assert_eq!(app.lspx_rows().iter().find(|r| r.id == "fake-a").unwrap().state, RowState::Running);
        // Toggling off stops the server and is written to the config as `language_servers.<id>.enabled`.
        app.lspx_set_enabled("fake-a", false);
        assert!(app.lsps.is_empty());
        assert_eq!(app.config.language_servers["fake-a"]["enabled"], json!(false));
        assert_eq!(app.config.language_servers["fake-a"]["command"], json!(exe));
        assert_eq!(app.lspx_rows().iter().find(|r| r.id == "fake-a").unwrap().state, RowState::Disabled);
        let text = std::fs::read_to_string(dir.join(".config/lumen-text/config.json")).unwrap();
        assert!(text.contains("\"language_servers\"") && text.contains("\"enabled\": false"), "{text}");
        // And on again.
        app.lspx_set_enabled("fake-a", true);
        assert!(pump(&mut app, 8000, |a| !a.lsps.is_empty()));
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_configuration_is_reported_not_fatal() {
        let _g = lock();
        let dir = project("badcfg");
        let mut app = test_app(&dir);
        app.config.language_servers = json!({"rust-analyzer": {"args": 5}, "x": {}, "taplo": {"enabled": false}});
        app.lsp_tick(Instant::now(), &mut |_| {});
        let m = app.status.as_ref().unwrap().0.clone();
        assert!(m.starts_with("language_servers: language_servers.") && m.contains("(+1 more"), "{m}");
        assert_eq!(app.lspx_config_errors().len(), 2);
        assert!(!app.lspx.reg.get("taplo").unwrap().enabled, "valid parts still apply");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_server_is_explained_once_per_language() {
        let _g = lock();
        let dir = project("missing");
        let mut app = test_app(&dir);
        app.config.language_servers = json!({"zig": {"command": "no-such-zls-xyz", "install": "get zls"}});
        let i = open(&mut app, &dir.join("a.zig"), "const x = 1;\n");
        app.lsp_tick(Instant::now(), &mut |_| {});
        let m = app.status.as_ref().unwrap().0.clone();
        assert!(m.contains("no zig language server found") && m.contains("get zls"), "{m}");
        app.status = None;
        app.lsp_tick(Instant::now(), &mut |_| {});
        assert!(app.status.is_none(), "only said once");
        assert!(app.lsps.is_empty() && app.doc_lsp(i).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real clangd, when installed: diagnostics and hover show up for a C file.
    #[test]
    fn real_clangd_smoke() {
        let Some(clangd) = lspreg::installed_anywhere("clangd") else {
            return eprintln!("clangd not installed, skipping");
        };
        let _g = lock();
        let dir = project("clangd");
        std::fs::write(dir.join("compile_flags.txt"), "-xc\n").unwrap();
        let mut app = test_app(&dir);
        let i = open(
            &mut app,
            &dir.join("m.c"),
            "int add(int a, int b) { return a + b; }\nint main(void) { return add(1, 2) + missing; }\n",
        );
        assert!(
            pump(&mut app, 30000, |a| a.docs[i].diags.iter().any(|d| d.message.contains("missing"))),
            "no clangd diagnostics ({}): {:?}\n{}",
            clangd.display(),
            app.status,
            app.lspx_log_text()
        );
        assert!(
            app.docs[i].diags.iter().all(|d| d.source == "clang" || d.source == "clangd"),
            "{:?}",
            app.docs[i].diags
        );
        // Hover over `add` in the definition.
        app.request_hover(4);
        assert!(pump(&mut app, 10000, |a| a.hover.is_some()), "no hover from clangd");
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real rust-analyzer, when installed (slow to start: indexes the sysroot).
    #[test]
    fn real_rust_analyzer_smoke() {
        let Some(ra) = lspreg::installed_anywhere("rust-analyzer")
            .filter(|p| std::process::Command::new(p).arg("--version").output().is_ok_and(|o| o.status.success()))
        else {
            return eprintln!("rust-analyzer not installed, skipping");
        };
        let _g = lock();
        let dir = project("ra");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
            .unwrap();
        let mut app = test_app(&dir);
        let i = open(&mut app, &dir.join("src/main.rs"), "fn main() {\n    let x: u32 = \"no\";\n}\n");
        assert!(
            pump(&mut app, 120_000, |a| !a.docs[i].diags.is_empty()),
            "no rust-analyzer diagnostics ({}): {:?}\n{}",
            ra.display(),
            app.status,
            app.lspx_log_text()
        );
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ───────────── settings page ─────────────

/// Settings rows need `&'static str`: intern the (few, repeating) dynamic strings once.
fn intern(s: String) -> &'static str {
    use std::sync::{Mutex, OnceLock};
    static POOL: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut m = POOL.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&v) = m.get(&s) {
        return v;
    }
    let leaked: &'static str = Box::leak(s.clone().into_boxed_str());
    m.insert(s, leaked);
    leaked
}

pub const SECTION: &str = "Language servers";

impl App {
    /// Rows for the "Language servers" section of the settings page, with their current values:
    /// one switch per server (the help line carries status and resolved command) and, when the
    /// configuration has problems, a read-only line listing them.
    pub fn lspx_setting_defs(&mut self) -> (Vec<crate::settingsui::SettingDef>, Value) {
        use crate::settingsui::{SettingDef, SettingKind};
        let mut defs = Vec::new();
        let mut values = serde_json::Map::new();
        let errors = self.lspx_config_errors();
        for r in self.lspx_rows() {
            let key = intern(format!("language_servers.{}.enabled", r.id));
            let origin = if r.builtin { "built-in" } else { "yours" };
            let mut help = format!("{} · {} · {}", r.state.label(), r.command, r.detail);
            if !r.languages.is_empty() {
                help.push_str(&format!(" · {}", r.languages));
            }
            help.push_str(&format!(" · {origin}. Turning it off or on restarts it."));
            defs.push(SettingDef {
                key,
                section: SECTION,
                label: intern(r.id.clone()),
                help: intern(help),
                kind: SettingKind::Bool,
            });
            values.insert(key.to_string(), json!(r.enabled));
        }
        if !errors.is_empty() {
            let key = "language_servers._errors";
            defs.push(SettingDef {
                key,
                section: SECTION,
                label: "Configuration problems",
                help: intern(errors.join(" · ")),
                kind: SettingKind::Text,
            });
            values.insert(key.to_string(), json!(format!("{} to fix in config.json", errors.len())));
        }
        (defs, Value::Object(values))
    }

    /// A change made on the settings page that belongs to this module. True when handled.
    pub fn lspx_apply_setting(&mut self, key: &str, v: &Value) -> bool {
        let Some(rest) = key.strip_prefix("language_servers.") else { return false };
        if let Some(id) = rest.strip_suffix(".enabled")
            && let Some(on) = v.as_bool()
        {
            self.lspx_set_enabled(id, on);
        }
        true
    }
}
