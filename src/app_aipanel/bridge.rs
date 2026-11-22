//! The editor side of the IDE bridge (`crate::ide`): Claude Code's tool calls answered from the
//! editor's state, its proposed edits shown in the diff view with Accept / Reject, the selection
//! and "AI: Send Selection to Agent" sent to it, and its hooks folded into the status chip.
//!
//! One listener serves every session. Hooks name their session in the URL; a WebSocket client is
//! matched to the session that started it by the pid it announces (`ide_connected`), and until
//! then by the order the Claude Code sessions were started in. Clients that attached with `/ide`
//! from elsewhere belong to no session.

use super::*;
use crate::{
    app_gitx::DiffView,
    doc::{Diag, Doc, path_to_uri, uri_to_path},
    ide::{self, Event, Server, mcp},
};
use serde_json::{Value, json};
use std::path::Path;

/// The selection is sent this long after it stops changing.
pub const SEL_DEBOUNCE_MS: u64 = 150;

/// An `openDiff` waiting for the user: its JSON-RPC id stays unanswered until Accept / Reject.
pub struct Proposal {
    pub key: u64,
    pub client: u64,
    pub id: Value,
    pub tab: String,
    pub path: PathBuf,
    pub old_text: String,
    pub new_text: String,
}

#[derive(Default)]
pub struct Bridge {
    pub server: Option<Server>,
    /// Clients past the WebSocket handshake.
    pub clients: Vec<u64>,
    pub proposals: Vec<Proposal>,
    next_key: u64,
    /// The editor's selection last looked at: (file, anchor, head).
    sel_seen: Option<(Option<PathBuf>, usize, usize)>,
    sel_due: Option<Instant>,
    /// The last non-empty selection (`getLatestSelection`).
    pub latest_sel: Option<Value>,
    /// Which AI panel session each client belongs to: (client, session).
    pub owner: Vec<(u64, u64)>,
    /// Claude Code sessions started here whose client has not connected yet, oldest first.
    pub expect: Vec<u64>,
    /// The workspace folders the lock file names (the project, plus folders sessions run in).
    folders: Vec<PathBuf>,
    /// Where lock files go (`None` = decided by the build: the user's folder, none in tests).
    lock_dir: Option<Option<PathBuf>>,
}

impl Bridge {
    pub fn connected(&self) -> bool {
        !self.clients.is_empty()
    }

    /// The session client `c` belongs to.
    pub fn owner_of(&self, c: u64) -> Option<u64> {
        self.owner.iter().find(|(x, _)| *x == c).map(|(_, s)| *s)
    }

    /// Session `sid`'s Claude Code is connected.
    pub fn session_connected(&self, sid: u64) -> bool {
        self.owner.iter().any(|&(c, s)| s == sid && self.clients.contains(&c))
    }

    /// The clients of session `sid` (`None`: those started elsewhere, by `/ide`).
    pub fn clients_of(&self, sid: Option<u64>) -> Vec<u64> {
        self.clients.iter().copied().filter(|&c| self.owner_of(c) == sid).collect()
    }

    /// Session `sid` just started a Claude Code: the next client to connect is probably it.
    pub fn expect_session(&mut self, sid: u64) {
        self.expect.retain(|&s| s != sid);
        self.expect.push(sid);
    }

    /// Session `sid` ended or restarted: its old clients are nobody's.
    pub fn forget_session(&mut self, sid: u64) {
        self.expect.retain(|&s| s != sid);
        self.owner.retain(|&(_, s)| s != sid);
    }

    /// A client finished its handshake: it is the oldest session still waiting for its own.
    pub fn on_open(&mut self, c: u64) {
        self.clients.push(c);
        if !self.expect.is_empty() {
            let sid = self.expect.remove(0);
            self.owner.push((c, sid));
        }
    }

    /// A client went away; returns the session it belonged to.
    pub fn on_closed(&mut self, c: u64) -> Option<u64> {
        let sid = self.owner_of(c);
        self.clients.retain(|&x| x != c);
        self.owner.retain(|&(x, _)| x != c);
        sid
    }

    /// Client `c` said which process it is: it belongs to `sid` (`None`: to no session here).
    /// A guess made by order that was wrong goes back to the waiting list.
    pub fn claim(&mut self, c: u64, sid: Option<u64>) {
        let prev = self.owner_of(c);
        if prev == sid {
            return;
        }
        self.owner.retain(|&(x, _)| x != c);
        if let Some(p) = prev
            && !self.expect.contains(&p)
            && !self.owner.iter().any(|&(_, s)| s == p)
        {
            self.expect.insert(0, p);
        }
        if let Some(s) = sid {
            self.expect.retain(|&x| x != s);
            self.owner.push((c, s));
        }
    }
}

/// The selection of `d` as Claude Code expects it (0-based lines, UTF-16 columns, end exclusive).
pub fn selection_json(d: &Doc) -> Option<Value> {
    let path = d.path()?;
    let (s, e) = (d.ed.sel.start(), d.ed.sel.end());
    let pos = |c: usize| {
        let (l, col) = d.buf.line_col(c);
        json!({ "line": l, "character": ide::utf16_col(&d.buf.line(l), col) })
    };
    Some(json!({
        "success": true,
        "text": d.buf.slice(s, e),
        "filePath": path,
        "fileUrl": path_to_uri(path),
        "selection": { "start": pos(s), "end": pos(e), "isEmpty": s == e },
    }))
}

/// VS Code's names for LSP severities.
fn severity_name(s: u8) -> &'static str {
    match s {
        1 => "Error",
        2 => "Warning",
        3 => "Information",
        _ => "Hint",
    }
}

fn range(sl: usize, sc: usize, el: usize, ec: usize) -> Value {
    json!({ "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } })
}

/// One diagnostic of an open document (build problems carry char columns: made UTF-16 here).
fn diag_json(g: &Diag, d: &Doc) -> Value {
    let (sc, ec) = if g.source == crate::app_build::SOURCE {
        let line = |l: usize| if l < d.buf.line_count() { d.buf.line(l) } else { String::new() };
        (ide::utf16_col(&line(g.sl), g.sc), ide::utf16_col(&line(g.el), g.ec))
    } else {
        (g.sc, g.ec)
    };
    let source = if g.source.is_empty() { d.lang.unwrap_or("lsp") } else { g.source.as_str() };
    let mut v = json!({
        "message": g.message,
        "severity": severity_name(g.severity),
        "range": range(g.sl, sc, g.el, ec),
        "source": source,
    });
    if !g.code.is_empty() {
        v["code"] = json!(g.code);
    }
    v
}

impl App {
    /// Start the bridge (once): the WebSocket server and, outside tests, the lock file.
    pub fn ide_start(&mut self) {
        if self.ai.bridge.server.is_some() {
            return;
        }
        let dir = match self.ai.bridge.lock_dir.clone() {
            Some(d) => d,
            None if cfg!(test) => None,
            None => ide::lock::ide_dir(),
        };
        match Server::start(vec![self.root.clone()], dir, self.wake()) {
            Ok(s) => {
                self.ai.bridge.server = Some(s);
                self.ai.bridge.folders = vec![self.root.clone()];
            }
            Err(e) => self.say(format!("IDE bridge: could not listen on 127.0.0.1: {e}")),
        }
    }

    /// Start the bridge with its lock file in `dir` (tests).
    #[cfg(test)]
    pub fn ide_start_in(&mut self, dir: PathBuf) {
        self.ai.bridge.lock_dir = Some(Some(dir));
        self.ide_start();
    }

    /// Port, hook token and `--settings` (hook URLs naming session `sid`) for a Claude Code the
    /// panel starts.
    pub(super) fn ide_agent_setup(&mut self, sid: u64) -> Option<(String, String, String)> {
        self.ide_start();
        let s = self.ai.bridge.server.as_ref()?;
        Some((s.port().to_string(), s.hook_token().to_string(), ide::hook_settings(s.port(), Some(sid))))
    }

    /// The session whose agent is process `pid` (or started it).
    fn ide_session_of_pid(&self, pid: u32) -> Option<u64> {
        self.ai
            .sessions
            .iter()
            .find(|s| s.term.alive && s.term.pid().is_some_and(|p| same_or_child(pid, p)))
            .map(|s| s.id)
    }

    /// Who proposes, edits or is connected: the session's name (or the agent's) for client `c`.
    pub fn ide_who(&self, client: Option<u64>) -> String {
        let session = client.and_then(|c| self.ai.bridge.owner_of(c)).and_then(|sid| self.ai.index_of(sid));
        match session {
            Some(i) => self.ai.sessions[i].name.clone(),
            None => self.ai.agent().map_or("The agent", |a| a.name).to_string(),
        }
    }

    /// The bridge indicator of the session shown: its Claude Code is connected (or, when no
    /// session owns a client, any client is: one attached with `/ide`).
    pub fn ai_bridge_connected(&self) -> bool {
        let b = &self.ai.bridge;
        self.ai.cur().is_some_and(|s| b.session_connected(s.id)) || (b.owner.is_empty() && b.connected())
    }

    /// Who proposed the change on screen.
    pub fn ide_proposer(&self) -> String {
        let key = self.diff.as_ref().and_then(|d| d.proposal);
        let client = self.ai.bridge.proposals.iter().find(|p| Some(p.key) == key).map(|p| p.client);
        self.ide_who(client)
    }

    fn ide_send(&self, client: u64, msg: Value) {
        if let Some(s) = &self.ai.bridge.server {
            s.send(client, &msg);
        }
    }

    fn ide_broadcast(&self, msg: Value) {
        if let Some(s) = &self.ai.bridge.server {
            s.broadcast(&msg);
        }
    }

    /// Drain what the connections sent; called whenever the event loop is woken.
    pub fn pump_ide(&mut self) {
        let mut events = Vec::new();
        if let Some(s) = &self.ai.bridge.server {
            while let Some(e) = s.try_recv() {
                events.push(e);
            }
        }
        if events.is_empty() {
            return;
        }
        for e in events {
            self.ide_event(e);
        }
        self.request_redraw();
    }

    fn ide_event(&mut self, e: Event) {
        match e {
            Event::Open(c) => {
                self.ai.bridge.on_open(c);
                // The new client gets the current selection shortly.
                self.ai.bridge.sel_seen = None;
            }
            Event::Closed(c) => {
                // A session whose agent still runs may reconnect: its next client is it again.
                if let Some(sid) = self.ai.bridge.on_closed(c)
                    && self.ai.index_of(sid).is_some_and(|i| self.ai.sessions[i].term.alive)
                {
                    self.ai.bridge.expect_session(sid);
                }
                // Its pending diffs can no longer be answered: close them.
                let gone: Vec<u64> = self.ai.bridge.proposals.iter().filter(|p| p.client == c).map(|p| p.key).collect();
                for k in gone {
                    self.ide_finish(k, None);
                }
            }
            Event::Call { client, id, name, args } => {
                if let Some(res) = self.ide_call(client, &id, &name, &args) {
                    self.ide_send(client, mcp::reply(&id, res));
                }
            }
            Event::Note { client, method, params } => match method.as_str() {
                "ide_connected" => {
                    if let Some(pid) = params["pid"].as_u64().and_then(|p| u32::try_from(p).ok()) {
                        let sid = self.ide_session_of_pid(pid);
                        self.ai.bridge.claim(client, sid);
                    }
                }
                "notifications/initialized" | "notifications/cancelled" => {}
                _ => eprintln!("lumen ide: client {client}: unknown notification {method} {params}"),
            },
            Event::Hook { route, session, body } => self.ide_hook(&route, session, &body),
        }
    }

    /// Answer a tool call; `None` while it waits for the user (`openDiff`).
    fn ide_call(&mut self, client: u64, id: &Value, name: &str, args: &Value) -> Option<Value> {
        let arg = |k: &str| args.get(k).and_then(Value::as_str);
        Some(match name {
            "openFile" => self.ide_open_file(args),
            "openDiff" => return self.ide_open_diff(client, id, args),
            "getCurrentSelection" => mcp::json_text(
                &self
                    .docs
                    .get(self.active)
                    .and_then(selection_json)
                    .unwrap_or_else(|| json!({ "success": false, "message": "No active editor found" })),
            ),
            "getLatestSelection" => mcp::json_text(
                &self
                    .ai
                    .bridge
                    .latest_sel
                    .clone()
                    .or_else(|| self.docs.get(self.active).and_then(selection_json))
                    .unwrap_or_else(|| json!({ "success": false, "message": "No selection available" })),
            ),
            "getOpenEditors" => mcp::json_text(&self.ide_open_editors()),
            "getWorkspaceFolders" => {
                let name = self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                mcp::json_text(&json!({
                    "success": true,
                    "folders": [{ "name": name, "uri": path_to_uri(&self.root), "path": self.root }],
                    "rootPath": self.root,
                }))
            }
            "getDiagnostics" => mcp::json_text(&self.ide_diagnostics(arg("uri"))),
            "checkDocumentDirty" => {
                let fp = arg("filePath").unwrap_or_default();
                match self.ide_doc(fp) {
                    Some(i) => mcp::json_text(&json!({
                        "success": true,
                        "filePath": fp,
                        "isDirty": self.docs[i].buf.is_dirty(),
                        "isUntitled": false,
                    })),
                    None => mcp::json_text(&json!({ "success": false, "message": format!("Document not open: {fp}") })),
                }
            }
            "saveDocument" => {
                let fp = arg("filePath").unwrap_or_default();
                match self.ide_doc(fp).map(|i| self.save_doc(i)) {
                    Some(Ok(())) => mcp::json_text(&json!({
                        "success": true,
                        "filePath": fp,
                        "saved": true,
                        "message": "Document saved successfully",
                    })),
                    Some(Err(e)) => mcp::json_text(&json!({ "success": false, "filePath": fp, "message": e })),
                    None => mcp::json_text(&json!({ "success": false, "message": format!("Document not open: {fp}") })),
                }
            }
            "close_tab" => {
                let tab = arg("tab_name").unwrap_or_default();
                if let Some(k) = self.ai.bridge.proposals.iter().find(|p| p.tab == tab).map(|p| p.key) {
                    self.ide_finish(k, Some(false));
                }
                mcp::text("TAB_CLOSED")
            }
            "closeAllDiffTabs" => {
                let keys: Vec<u64> = self.ai.bridge.proposals.iter().map(|p| p.key).collect();
                let n = keys.len();
                for k in keys {
                    self.ide_finish(k, Some(false));
                }
                mcp::text(&format!("CLOSED_{n}_DIFF_TABS"))
            }
            "executeCode" => mcp::failure("executeCode (Jupyter kernels) is not available in Lumen"),
            _ => {
                eprintln!("lumen ide: unknown tool {name}");
                mcp::failure(&format!("Unknown tool: {name}"))
            }
        })
    }

    /// A path from the agent: absolute, or relative to the project; canonical when it exists.
    fn ide_path(&self, p: &str) -> PathBuf {
        let p = Path::new(p);
        let abs = if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) };
        crate::paths::canonical(&abs).unwrap_or(abs)
    }

    /// The open document of a path.
    fn ide_doc(&self, p: &str) -> Option<usize> {
        if p.is_empty() {
            return None;
        }
        let want = self.ide_path(p);
        self.docs.iter().position(|d| d.path() == Some(want.as_path()))
    }

    fn ide_open_editors(&self) -> Value {
        let tabs: Vec<Value> = self
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                json!({
                    "uri": d.real_uri().unwrap_or_else(|| format!("untitled:{}", d.name)),
                    "isActive": i == self.active,
                    "label": d.name,
                    "languageId": d.lang.unwrap_or("plaintext"),
                    "isDirty": d.buf.is_dirty(),
                    "isUntitled": d.path().is_none(),
                    "fileName": d.path().map_or_else(|| d.name.clone(), |p| p.to_string_lossy().into_owned()),
                })
            })
            .collect();
        json!({ "tabs": tabs })
    }

    /// Diagnostics per file: what the open documents show (language servers, linters, build),
    /// plus the last build's problems in files that are not open. One file with `uri`.
    fn ide_diagnostics(&self, uri: Option<&str>) -> Value {
        let want = uri.filter(|u| !u.is_empty()).map(|u| match uri_to_path(u) {
            Some(p) => crate::paths::canonical(&p).unwrap_or(p),
            None => self.ide_path(u),
        });
        let mut out: Vec<Value> = Vec::new();
        let mut open: Vec<&Path> = Vec::new();
        for d in &self.docs {
            let Some(p) = d.path() else { continue };
            open.push(p);
            if want.as_ref().is_some_and(|w| w != p) || (want.is_none() && d.diags.is_empty()) {
                continue;
            }
            let diags: Vec<Value> = d.diags.iter().map(|g| diag_json(g, d)).collect();
            out.push(json!({ "uri": path_to_uri(p), "diagnostics": diags }));
        }
        let mut closed: Vec<(&Path, Vec<Value>)> = Vec::new();
        for p in &self.build.problems {
            if open.contains(&p.file.as_path()) || want.as_ref().is_some_and(|w| *w != p.file) {
                continue;
            }
            let mut v = json!({
                "message": p.message,
                "severity": severity_name(p.sev.lsp()),
                "range": range(p.line, p.col, p.line, p.col),
                "source": crate::app_build::SOURCE,
            });
            if !p.code.is_empty() {
                v["code"] = json!(p.code);
            }
            match closed.iter_mut().find(|(f, _)| *f == p.file.as_path()) {
                Some((_, list)) => list.push(v),
                None => closed.push((p.file.as_path(), vec![v])),
            }
        }
        for (f, diags) in closed {
            out.push(json!({ "uri": path_to_uri(f), "diagnostics": diags }));
        }
        if let (Some(w), true) = (&want, out.is_empty()) {
            out.push(json!({ "uri": path_to_uri(w), "diagnostics": [] }));
        }
        Value::Array(out)
    }

    fn ide_open_file(&mut self, args: &Value) -> Value {
        let arg = |k: &str| args.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
        let flag = |k: &str, def: bool| args.get(k).and_then(Value::as_bool).unwrap_or(def);
        let Some(fp) = arg("filePath") else { return mcp::failure("openFile needs a filePath") };
        let path = self.ide_path(fp);
        if !path.is_file() {
            return mcp::failure(&format!("File not found: {}", path.display()));
        }
        let front = flag("makeFrontmost", true);
        let prev = (!self.docs.is_empty()).then_some(self.active);
        let Some(i) = self.open_path(&path, None) else {
            return mcp::failure(&format!("Could not open {}", path.display()));
        };
        if let Some(start) = arg("startText") {
            let d = &mut self.docs[i];
            let text = d.buf.to_string();
            if let Some(b) = text.find(start) {
                let mut e = b + start.len();
                if let Some(k) = arg("endText").and_then(|end| text[b..].find(end).map(|k| k + end.len())) {
                    e = e.max(b + k);
                }
                if flag("selectToEndOfLine", false) {
                    e = text[e..].find('\n').map_or(text.len(), |k| e + k);
                }
                let (cb, ce) = (text[..b].chars().count(), text[..e].chars().count());
                d.ed.select_range(&mut d.buf, cb, ce);
                d.reveal = true;
            }
        }
        if !front && let Some(p) = prev.filter(|&p| p != i && p < self.docs.len()) {
            self.activate(p);
        }
        self.request_redraw();
        if front {
            mcp::text(&format!("Opened file: {}", path.display()))
        } else {
            let d = &self.docs[i];
            mcp::json_text(&json!({
                "success": true,
                "filePath": path,
                "languageId": d.lang.unwrap_or("plaintext"),
                "lineCount": d.buf.line_count(),
            }))
        }
    }

    /// `openDiff`: queue the proposal and show it; the answer waits for Accept / Reject.
    fn ide_open_diff(&mut self, client: u64, id: &Value, args: &Value) -> Option<Value> {
        let arg = |k: &str| args.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
        let Some(new_path) = arg("new_file_path").or(arg("old_file_path")) else {
            return Some(mcp::failure("openDiff needs new_file_path"));
        };
        let Some(new_text) = args.get("new_file_contents").and_then(Value::as_str) else {
            return Some(mcp::failure("openDiff needs new_file_contents"));
        };
        let tab = arg("tab_name").unwrap_or(new_path).to_string();
        // The same tab name again replaces the diff (the old call is answered as rejected).
        if let Some(k) = self.ai.bridge.proposals.iter().find(|p| p.tab == tab).map(|p| p.key) {
            self.ide_finish(k, Some(false));
        }
        let old_text =
            std::fs::read_to_string(self.ide_path(arg("old_file_path").unwrap_or(new_path))).unwrap_or_default();
        let path = self.ide_path(new_path);
        self.ai.bridge.next_key += 1;
        let key = self.ai.bridge.next_key;
        let name = self.rel(&path);
        self.ai.bridge.proposals.push(Proposal {
            key,
            client,
            id: id.clone(),
            tab,
            path,
            old_text,
            new_text: new_text.to_string(),
        });
        // A proposal already on screen stays there; this one waits its turn (the panel counts them).
        if self.diff.as_ref().is_none_or(|d| d.proposal.is_none()) {
            self.ide_show(key);
        }
        let who = self.ide_who(Some(client));
        self.say(format!("{who} proposes changes to {name} — Ctrl+Enter accepts, Esc rejects"));
        self.request_redraw();
        None
    }

    /// Put proposal `key` in the diff view.
    pub fn ide_show(&mut self, key: u64) {
        let Some(p) = self.ai.bridge.proposals.iter().find(|p| p.key == key) else { return };
        let rows = crate::gitx::side_by_side(&p.old_text, &p.new_text);
        let mut dv = DiffView::plain(self.rel(&p.path), "Current".into(), "Proposed".into(), rows);
        dv.proposal = Some(key);
        self.diff = Some(dv);
        // In a split the diff covers the focused pane; the editor keeps the keyboard otherwise.
        if self.focus == Focus::Terminal {
            self.focus = Focus::Editor;
        }
        self.request_redraw();
    }

    /// Show the first waiting proposal (the panel's badge).
    pub fn ide_show_next(&mut self) {
        if let Some(k) = self.ai.bridge.proposals.first().map(|p| p.key) {
            self.ide_show(k);
        }
    }

    /// End proposal `key`: `Some(true)` accept (`FILE_SAVED`; the agent writes the file),
    /// `Some(false)` reject (`DIFF_REJECTED`), `None` drop it unanswered (its client is gone).
    pub fn ide_finish(&mut self, key: u64, accept: Option<bool>) {
        let Some(i) = self.ai.bridge.proposals.iter().position(|p| p.key == key) else { return };
        let p = self.ai.bridge.proposals.remove(i);
        match accept {
            Some(true) => self.ide_send(p.client, mcp::reply(&p.id, mcp::texts(&["FILE_SAVED", &p.new_text]))),
            Some(false) => self.ide_send(p.client, mcp::reply(&p.id, mcp::texts(&["DIFF_REJECTED", &p.tab]))),
            None => {}
        }
        if self.diff.as_ref().is_some_and(|d| d.proposal == Some(key)) {
            self.diff = None;
            self.ide_show_next();
        }
        self.request_redraw();
    }

    /// Accept / reject the proposal on screen (header pills, Ctrl+Enter / Esc).
    pub fn ide_decide(&mut self, accept: bool) {
        let Some(k) = self.diff.as_ref().and_then(|d| d.proposal) else { return };
        let name = self.ai.bridge.proposals.iter().find(|p| p.key == k).map(|p| self.rel(&p.path)).unwrap_or_default();
        self.ide_finish(k, Some(accept));
        self.say(if accept { format!("Accepted changes to {name}") } else { format!("Rejected changes to {name}") });
    }

    /// Per-event hook: the selection (debounced) to connected clients, the lock file following
    /// the project.
    pub fn ide_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if self.ai.bridge.server.is_none() {
            return;
        }
        // The project first, then the other folders sessions run in (worktrees).
        let mut folders = vec![self.root.clone()];
        for s in &self.ai.sessions {
            if !folders.contains(&s.cwd) {
                folders.push(s.cwd.clone());
            }
        }
        if self.ai.bridge.folders != folders {
            if let Some(s) = self.ai.bridge.server.as_mut() {
                let _ = s.set_folders(&folders);
            }
            self.ai.bridge.folders = folders;
        }
        if !self.ai.bridge.connected() {
            return;
        }
        let key = self.docs.get(self.active).map(|d| (d.path().map(Path::to_path_buf), d.ed.sel.anchor, d.ed.sel.head));
        if key != self.ai.bridge.sel_seen {
            self.ai.bridge.sel_seen = key;
            self.ai.bridge.sel_due = Some(now + Duration::from_millis(SEL_DEBOUNCE_MS));
        }
        let Some(due) = self.ai.bridge.sel_due else { return };
        if due > now {
            return want(due);
        }
        self.ai.bridge.sel_due = None;
        let Some(mut v) = self.docs.get(self.active).and_then(selection_json) else { return };
        if v["selection"]["isEmpty"] == json!(false) {
            self.ai.bridge.latest_sel = Some(v.clone());
        }
        if let Some(o) = v.as_object_mut() {
            o.remove("success");
        }
        self.ide_broadcast(mcp::notification("selection_changed", v));
    }

    /// "AI: Send Selection to Agent": an @-mention of the selected lines (`file#L3-9`).
    pub fn ai_send_selection(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return self.say("Open a file to send it to the agent") };
        let Some(path) = d.path().map(Path::to_path_buf) else {
            return self.say("Save the file first: the agent needs a path");
        };
        if !self.ai.bridge.connected() {
            return self
                .say("Claude Code is not connected — start it in the AI panel (Ctrl+Shift+A) or run /ide in claude");
        }
        let (a, b) = d.ed.selected_line_range(&d.buf);
        let msg = mcp::notification("at_mentioned", json!({ "filePath": path, "lineStart": a, "lineEnd": b }));
        // To the session shown when its Claude Code is connected, else to the ones attached with
        // `/ide`, else to everyone.
        let mine = self.ai.cur().map(|s| self.ai.bridge.clients_of(Some(s.id))).unwrap_or_default();
        let targets = if mine.is_empty() { self.ai.bridge.clients_of(None) } else { mine.clone() };
        if targets.is_empty() {
            self.ide_broadcast(msg);
        } else {
            for c in targets {
                self.ide_send(c, msg.clone());
            }
        }
        let lines = if a == b { format!("L{}", a + 1) } else { format!("L{}-{}", a + 1, b + 1) };
        self.say(format!("Sent {}#{lines} to the agent", self.rel(&path)));
        // The agent of the panel gets the keyboard, ready for the question.
        if !mine.is_empty() && self.ai.term().is_some_and(|t| t.alive) {
            self.ai_show();
        }
    }

    /// A hook POST from the agent of session `sid`: a status it reports (stronger than the output
    /// heuristic). Without a session it is the session shown's, when that one is Claude Code.
    fn ide_hook(&mut self, route: &str, sid: Option<u64>, body: &Value) {
        let i = match sid {
            Some(id) => self.ai.index_of(id),
            None => self.ai.cur().filter(|s| s.agent.id == "claude").map(|_| self.ai.active),
        };
        let s = match route {
            "working" => Status::Working,
            "waiting" => Status::Waiting,
            "done" => Status::Done,
            "edited" => {
                self.ide_edited(i, body);
                Status::Working
            }
            _ => return eprintln!("lumen ide: unknown hook route {route}"),
        };
        let Some(i) = i else { return };
        let track = &mut self.ai.sessions[i].track;
        track.hook = Some(s);
        if s == Status::Waiting {
            track.attention = true;
        }
        if self.ai_refresh(i, Instant::now()) {
            self.request_redraw();
        }
    }

    /// The agent edited a file (`PostToolUse` of Edit / Write / MultiEdit): reload it now when
    /// it is open and clean, and say so.
    fn ide_edited(&mut self, session: Option<usize>, body: &Value) {
        let Some(fp) = body["tool_input"]["file_path"].as_str() else { return };
        let path = self.ide_path(fp);
        if let Some(i) = self.docs.iter().position(|d| d.path() == Some(path.as_path()))
            && !self.docs[i].buf.is_dirty()
        {
            self.hist_snapshot(i, crate::localhist::Source::Reload);
            let _ = self.docs[i].reload_from_disk();
        }
        let who = match session {
            Some(i) => self.ai.sessions[i].name.clone(),
            None => self.ai.agent().map_or("The agent", |a| a.name).to_string(),
        };
        self.say(format!("{who} edited {}", self.rel(&path)));
    }
}
