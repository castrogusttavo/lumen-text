//! Inline completions in the app: asking the document's servers (`textDocument/inlineCompletion`)
//! after a typing pause or on `Alt+\`, showing the first suggestion as ghost text, accepting it
//! (`Tab`), one word of it (`Ctrl+Right`), dismissing it (`Esc`), cycling (`Alt+]` / `Alt+[`), the
//! status bar item, and GitHub Copilot's sign-in.
//!
//! Copilot is the registry server `copilot` (`copilot-language-server --stdio`, a companion of
//! every file), off unless the `inline_completion` setting is on. Its inline completions are plain
//! LSP; only the sign-in uses its own requests (`checkStatus`, `signIn` with the device-code flow,
//! `signOut`, the `didChangeStatus` notification). Those are kept here, small and separate: if
//! they change, the generic part keeps working with any server that offers inline completions.
//! Lumen never sees a token: the server stores the login itself.

use crate::{
    app::{App, Focus},
    app_topbar::StatusBtn,
    doc::Doc,
    inline::{
        Accept, Auth, Badge, CopilotStep, Ghost, Inflight, ServerState, Snap, Trigger, auth_from_status, badge,
        classify, next_word, parse_items, view,
    },
    lsp::Pending,
    lspreg::Registry,
    ui::{Canvas, Rect},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use winit::keyboard::{Key, NamedKey};

/// Registry id of GitHub Copilot's language server.
pub const COPILOT: &str = "copilot";
/// A link (device-code page) is not opened twice within this time.
const REOPEN_AFTER: Duration = Duration::from_secs(120);

/// Copilot runs only while inline completions are on (and the user did not switch it off in
/// `language_servers.copilot.enabled`).
pub(crate) fn gate_copilot(reg: &mut Registry, user: &Value, on: bool) {
    let Some(s) = reg.servers.iter_mut().find(|s| s.id == COPILOT && s.builtin) else { return };
    let user_off = user[COPILOT]["enabled"].as_bool() == Some(false);
    s.enabled = on && !user_off;
    if !on {
        s.disabled_by = Some("needs Inline completions on (setting inline_completion)".into());
    }
}

/// Replace chars `s..e` with `text` as one undo step and put the caret after it.
fn apply_text(d: &mut Doc, s: usize, e: usize, text: &str) {
    use crate::buffer::{EditKind, Sel};
    let before = d.ed.sel;
    d.buf.close_group();
    d.buf.replace(s, e, text, EditKind::Other, before);
    d.buf.close_group();
    d.ed.sel = Sel::caret(s + text.chars().count());
    d.buf.set_after(d.ed.sel);
    d.reveal = true;
    d.sync_edits();
}

impl App {
    // ───────────── lifecycle ─────────────

    /// The active document as the inline state sees it.
    fn inline_snap(&self) -> Option<Snap> {
        let d = self.docs.get(self.active)?;
        let blocked = !self.config.inline_completion
            || self.inline.paused
            || !self.lsp_enabled()
            || !d.ed.extra.is_empty()
            || !d.ed.sel.is_empty()
            || d.snippet.is_some()
            || d.path().is_none()
            || self.completion.is_some()
            || !self.overlay.is_none()
            || self.focus != Focus::Editor;
        Some(Snap { doc: self.active, version: d.version, cursor: d.ed.head(), blocked })
    }

    /// Bring the inline state up to date with the active document (called every frame and tick):
    /// typing exactly what the ghost text shows keeps it; another edit dismisses it and schedules a
    /// new request; a move, a selection, another document... dismiss it and cancel the request.
    pub(crate) fn inline_sync(&mut self) {
        if !self.config.inline_completion && self.inline.idle() {
            return;
        }
        if let Some(d) = self.docs.get_mut(self.active) {
            d.sync_edits();
        }
        let cancel = match self.inline_snap() {
            None => {
                self.inline.last = None;
                self.inline.dismiss()
            }
            Some(snap) => {
                let change = classify(self.inline.last, snap);
                let kept = change == crate::inline::Change::Edit && !snap.blocked && self.inline_type_through(snap);
                self.inline.on_change(change, snap, kept, Instant::now())
            }
        };
        if let Some(f) = cancel {
            self.inline_cancel(f);
        }
        self.inline_show();
    }

    /// Did the edit that led to `snap` insert, at the anchor, the start of the ghost text?
    fn inline_type_through(&mut self, snap: Snap) -> bool {
        let Some(g) = self.inline.ghost.as_mut() else { return false };
        let Some(d) = self.docs.get(snap.doc) else { return false };
        if g.doc != snap.doc || snap.cursor <= g.anchor || d.buf.len_chars() != g.len + (snap.cursor - g.anchor) {
            return false;
        }
        let typed = d.buf.slice(g.anchor, snap.cursor);
        g.consume(&typed)
    }

    /// Timers: send the request that is due.
    pub(crate) fn inline_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        self.inline_sync();
        if let Some((t, _)) = self.inline.due
            && t > now
        {
            want(t);
            return;
        }
        if let Some(trig) = self.inline.take_due(now) {
            self.inline_request(trig);
        }
    }

    /// The first ready server that has document `i` open and offers inline completions, with the
    /// URI it knows the document by.
    fn inline_server(&self, i: usize) -> Option<(usize, String)> {
        let d = self.docs.get(i)?;
        let uris = [d.real_uri(), d.uri()];
        self.lsps.iter().enumerate().find_map(|(li, l)| {
            if !l.ready || !l.has_cap("inlineCompletionProvider") {
                return None;
            }
            uris.iter().flatten().find(|u| l.open_uris.contains(*u)).map(|u| (li, u.clone()))
        })
    }

    fn inline_find(&self, server: &(String, PathBuf)) -> Option<usize> {
        self.lsps.iter().position(|l| l.name == server.0 && l.root == server.1)
    }

    fn copilot_index(&self) -> Option<usize> {
        self.lsps.iter().position(|l| l.generic.as_ref().is_some_and(|g| g.id == COPILOT))
    }

    fn is_copilot(&self, li: usize) -> bool {
        self.lsps.get(li).and_then(|l| l.generic.as_ref()).is_some_and(|g| g.id == COPILOT)
    }

    fn inline_request(&mut self, trig: Trigger) {
        let Some(snap) = self.inline_snap().filter(|s| !s.blocked) else { return };
        let i = snap.doc;
        let Some((li, uri)) = self.inline_server(i) else {
            if trig == Trigger::Invoked {
                let starting = self.inline_server_state(i) == ServerState::Starting;
                self.say(if starting {
                    "inline completion: the server is still starting"
                } else {
                    "inline completion: no language server of this file offers it"
                });
            }
            return;
        };
        // The server must have the text the position refers to.
        if self.docs[i].changed_at.is_some() && (self.docs[i].lsp_open || self.doc_lsp(i).is_none()) {
            self.send_change(i);
        }
        let d = &self.docs[i];
        let (line, character) = d.lsp_pos(snap.cursor);
        let spaces = !d.ed.indent.contains('\t');
        let tab = if spaces { d.ed.indent.len().max(1) } else { d.ed.tab_w };
        let params = json!({
            "textDocument": {"uri": uri, "version": d.version},
            "position": {"line": line, "character": character},
            "context": {"triggerKind": trig as u8, "selectedCompletionInfo": null},
            "formattingOptions": {"tabSize": tab, "insertSpaces": spaces}
        });
        self.inline.serial += 1;
        let serial = self.inline.serial;
        let id = self.lsps[li].request("textDocument/inlineCompletion", params, Pending::InlineCompletion { serial });
        let server = (self.lsps[li].name.clone(), self.lsps[li].root.clone());
        self.inline.inflight = Some(Inflight { server, id, serial, snap, trigger: trig });
        self.request_redraw();
    }

    /// Tell the server a request is no longer wanted; its late answer is dropped.
    fn inline_cancel(&mut self, f: Inflight) {
        if let Some(li) = self.inline_find(&f.server) {
            self.lsps[li].pending.remove(&f.id);
            self.lsps[li].notify("$/cancelRequest", json!({"id": f.id}));
        }
        self.request_redraw();
    }

    pub(crate) fn inline_on_response(&mut self, li: usize, what: Pending, result: Result<Value, Value>) {
        match what {
            Pending::InlineCompletion { serial } => self.inline_on_items(li, serial, result),
            Pending::Copilot(step) => self.copilot_on_response(li, step, result),
            _ => {}
        }
    }

    fn inline_on_items(&mut self, li: usize, serial: u64, result: Result<Value, Value>) {
        let Some(snap) = self.inline_snap() else { return };
        let Some(f) = self.inline.take_answer(serial, snap) else { return };
        self.request_redraw();
        let v = match result {
            Ok(v) => v,
            Err(e) => {
                if f.trigger == Trigger::Invoked {
                    self.say(format!("inline completion: {}", e["message"].as_str().unwrap_or("error")));
                }
                return;
            }
        };
        let d = &self.docs[snap.doc];
        let (line, _) = d.buf.line_col(snap.cursor);
        let indent: String = d.buf.line(line).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let items = parse_items(&v, snap.cursor, &|l, c| d.pos_from_lsp(l, c), &|a, b| d.buf.slice(a, b), &indent);
        if items.is_empty() {
            if f.trigger == Trigger::Invoked {
                self.say("inline completion: no suggestion here");
            }
            return;
        }
        let len = d.buf.len_chars();
        let server = (self.lsps[li].name.clone(), self.lsps[li].root.clone());
        self.inline.ghost = Some(Ghost { doc: snap.doc, server, anchor: snap.cursor, len, items, idx: 0 });
        self.inline_show();
    }

    /// Place (or take away) the ghost text in the documents.
    fn inline_show(&mut self) {
        let active = self.active;
        let placed = self.inline.ghost.as_ref().filter(|g| g.doc == active).and_then(|g| {
            let d = self.docs.get(g.doc)?;
            if g.anchor > d.buf.len_chars() {
                return None;
            }
            let key = (g.doc, d.version, g.anchor, g.idx, g.text().len());
            let current = self.inline.shown_in == Some(g.doc)
                && self.inline.view_key == Some(key)
                && (d.inlay.hints.ghost().is_some() || d.ghost.as_ref().is_some_and(|v| v.first.is_empty()));
            if current {
                return Some((g.doc, key, None));
            }
            let (line, col) = d.buf.line_col(g.anchor);
            let text = d.buf.line(line);
            let it = g.item();
            let line_end = d.buf.line_start(line) + d.buf.line_len(line);
            let after = if it.end > g.anchor { d.buf.slice(g.anchor, it.end.min(line_end)) } else { String::new() };
            let start = crate::editor::visual_col(&text, col, d.ed.tab_w);
            Some((g.doc, key, Some(view(&it.shown, line, col, start, d.ed.tab_w, &after))))
        });
        let doc = placed.as_ref().map(|p| p.0);
        if let Some(p) = self.inline.shown_in
            && Some(p) != doc
        {
            if let Some(d) = self.docs.get_mut(p) {
                d.set_ghost(None);
            }
            self.inline.shown_in = None;
            self.inline.view_key = None;
        }
        if let Some((doc, key, Some(v))) = placed {
            self.docs[doc].set_ghost(Some(v));
            self.inline.shown_in = Some(doc);
            self.inline.view_key = Some(key);
        }
    }

    /// Is a suggestion shown in the active document right now?
    pub(crate) fn inline_visible(&self) -> bool {
        self.inline.ghost.as_ref().is_some_and(|g| g.doc == self.active)
    }

    // ───────────── keys and commands ─────────────

    /// Tab / Ctrl+Right / Esc while a suggestion shows. False: the key keeps its usual meaning.
    pub(crate) fn inline_key(&mut self, key: &Key, ctrl: bool, shift: bool, alt: bool) -> bool {
        if self.inline.ghost.is_none() {
            return false;
        }
        match key {
            Key::Named(NamedKey::Tab) if !ctrl && !shift && !alt => {
                let done = self.inline_accept();
                if done {
                    self.after_key(None);
                }
                done
            }
            Key::Named(NamedKey::ArrowRight) if ctrl && !shift && !alt => {
                let done = self.inline_accept_word();
                if done {
                    self.after_key(None);
                }
                done
            }
            Key::Named(NamedKey::Escape) if !ctrl && !shift && !alt => self.inline_dismiss(),
            _ => false,
        }
    }

    /// Insert the whole suggestion (one undo step), then run the command it came with.
    pub(crate) fn inline_accept(&mut self) -> bool {
        self.inline_sync();
        if !self.inline_visible() {
            return false;
        }
        let Some(g) = self.inline.ghost.take() else { return false };
        let command = g.item().command.clone();
        let d = &mut self.docs[g.doc];
        match g.accept() {
            Accept::Text { start, end, text } => apply_text(d, start, end, &text),
            Accept::Snippet { start, end, body } => d.insert_snippet(start, end, &body),
        }
        self.inline_show();
        if let Some(cmd) = command {
            self.inline_run_command(&g.server, &cmd);
        }
        true
    }

    /// Insert the next word of the suggestion; the rest stays (typing through it).
    pub(crate) fn inline_accept_word(&mut self) -> bool {
        self.inline_sync();
        if !self.inline_visible() {
            return false;
        }
        let Some(g) = self.inline.ghost.as_ref() else { return false };
        let word = next_word(g.text()).to_string();
        if word.is_empty() {
            return false;
        }
        if word.len() == g.text().len() {
            return self.inline_accept();
        }
        let (doc, at) = (g.doc, g.anchor);
        apply_text(&mut self.docs[doc], at, at, &word);
        self.inline_sync();
        true
    }

    /// Esc: hide the suggestion (and forget a request on its way).
    pub(crate) fn inline_dismiss(&mut self) -> bool {
        if !self.inline_visible() {
            return false;
        }
        if let Some(f) = self.inline.dismiss() {
            self.inline_cancel(f);
        }
        self.inline_show();
        true
    }

    /// `Alt+\`: ask now.
    pub(crate) fn inline_invoke(&mut self) {
        if !self.config.inline_completion {
            return self.say("inline completion is off: turn on Inline completions in Settings (inline_completion)");
        }
        if self.inline.paused {
            self.inline.paused = false;
            self.say("inline completion: resumed");
        }
        self.inline.due = Some((Instant::now(), Trigger::Invoked));
        self.request_redraw();
    }

    /// `Alt+]` / `Alt+[`: the next / previous suggestion (asks when none shows).
    pub(crate) fn inline_cycle(&mut self, delta: isize) {
        self.inline_sync();
        match self.inline.ghost.as_mut() {
            Some(g) if g.doc == self.active => {
                g.cycle(delta);
                let n = g.items.len();
                if n < 2 {
                    self.say("inline completion: no other suggestion");
                }
                self.inline_show();
                self.request_redraw();
            }
            _ => self.inline_invoke(),
        }
    }

    fn inline_run_command(&mut self, server: &(String, PathBuf), cmd: &Value) {
        let (Some(li), Some(name)) = (self.inline_find(server), cmd["command"].as_str()) else { return };
        let mut params = json!({"command": name});
        if !cmd["arguments"].is_null() {
            params["arguments"] = cmd["arguments"].clone();
        }
        self.lsps[li].request("workspace/executeCommand", params, Pending::ExecuteCommand);
    }

    /// Commands of this feature; false for any other command.
    pub(crate) fn run_inline_cmd(&mut self, c: crate::app_overlay::Cmd) -> bool {
        use crate::app_overlay::Cmd;
        match c {
            Cmd::InlineSuggest => self.inline_invoke(),
            Cmd::InlineNext => self.inline_cycle(1),
            Cmd::InlinePrev => self.inline_cycle(-1),
            Cmd::ToggleInlineCompletion => self.inline_set_enabled(!self.config.inline_completion),
            Cmd::CopilotSignIn => self.copilot_sign_in(),
            Cmd::CopilotSignOut => self.copilot_sign_out(),
            _ => return false,
        }
        true
    }

    /// The `inline_completion` setting (settings modal, palette).
    pub(crate) fn inline_set_enabled(&mut self, on: bool) {
        if self.config.inline_completion != on {
            self.config.inline_completion = on;
            self.config.save();
        }
        self.inline.paused = false;
        if !on && let Some(f) = self.inline.dismiss() {
            self.inline_cancel(f);
        }
        self.lspx_sync_config();
        self.inline_show();
        self.say(if on { "inline completion: on" } else { "inline completion: off" });
        self.request_redraw();
    }

    pub(crate) fn inline_apply_setting(&mut self, key: &str, v: &Value) -> bool {
        if key != "inline_completion" {
            return false;
        }
        self.inline_set_enabled(v.as_bool().unwrap_or(false));
        true
    }

    /// The setting switched Copilot: stop it when it went off (it starts by itself when on).
    pub(crate) fn inline_copilot_switched(&mut self) {
        if self.lspx.reg.get(COPILOT).is_some_and(|s| s.enabled) {
            return;
        }
        while let Some(li) = self.copilot_index() {
            let mut l = self.lsps.remove(li);
            l.log_line("app", "inline completion turned off: stopping");
            l.shutdown();
            self.lspx_forget_docs(&l);
        }
        self.inline.copilot = Default::default();
    }

    // ───────────── status bar ─────────────

    fn inline_server_state(&self, i: usize) -> ServerState {
        if self.inline_server(i).is_some() {
            ServerState::Ready
        } else if self.lsps.iter().any(|l| !l.ready && l.generic.as_ref().is_some_and(|g| g.id == COPILOT)) {
            ServerState::Starting
        } else {
            ServerState::None
        }
    }

    pub(crate) fn inline_badge(&self) -> Badge {
        let server = self.inline_server(self.active);
        let copilot_serves = server.as_ref().is_none_or(|(li, _)| self.is_copilot(*li));
        let signed_out = copilot_serves
            && self.copilot_index().is_some()
            && matches!(self.inline.copilot.auth, Auth::SignedOut | Auth::NoAccess | Auth::Waiting { .. });
        badge(self.inline.paused, self.inline.inflight.is_some(), self.inline_server_state(self.active), signed_out)
    }

    /// The bottom bar item (left group) while inline completions are on.
    pub fn draw_inline_status(&mut self, cv: &mut Canvas, x: i32) -> i32 {
        if !self.config.inline_completion {
            return x;
        }
        let b = self.inline_badge();
        let c = self.theme.chrome;
        let r = self.layout.status;
        let col = match b {
            Badge::On => c.tab_inactive_fg,
            Badge::SignedOut => c.warning,
            Badge::Loading => c.accent_text,
            Badge::Off | Badge::NoServer => c.tab_inactive_fg.with_alpha(0xa0),
        };
        let label = b.label();
        let lw = self.tx.ui_width(label, false);
        let rect = Rect::new(x, r.y + 3, 8 + 14 + 6 + lw + 8, r.h - 6);
        if self.overlay.is_none() && rect.contains(self.mouse.0, self.mouse.1) {
            cv.fill_round(rect, 6, c.list_hover_bg);
        }
        self.tx.icon(cv, b.icon(), rect.x + 8, r.y + (r.h - 14) / 2, 14, col, r);
        self.tx.draw_ui(cv, rect.x + 8 + 14 + 6, r.y + (r.h - 18) / 2, label, col, r, false);
        self.layout.status_btns.push((rect, StatusBtn::Inline));
        rect.right() + 2
    }

    /// Click: sign in when Copilot is signed out, otherwise pause / resume.
    pub fn inline_status_click(&mut self) {
        if self.inline_badge() == Badge::SignedOut && !matches!(self.inline.copilot.auth, Auth::Waiting { .. }) {
            return self.copilot_sign_in();
        }
        self.inline.paused = !self.inline.paused;
        self.inline_sync();
        self.say(if self.inline.paused { "inline completion: paused" } else { "inline completion: resumed" });
    }

    pub fn inline_status_tip(&self) -> String {
        let user = match &self.inline.copilot.auth {
            Auth::SignedIn(u) if !u.is_empty() => format!(" (Copilot: {u})"),
            _ => String::new(),
        };
        match self.inline_badge() {
            Badge::On => format!("On{user}. Tab accepts, Ctrl+Right takes a word, Esc dismisses, Alt+] / Alt+[ cycle. Click to pause."),
            Badge::Off => "Paused. Click to resume.".into(),
            Badge::Loading => "Asking the language server…".into(),
            Badge::SignedOut => match &self.inline.copilot.auth {
                Auth::Waiting { code, uri } => format!("Enter the code {code} at {uri} to finish signing in."),
                Auth::NoAccess => "This GitHub account has no Copilot access.".into(),
                _ => "GitHub Copilot is signed out. Click to sign in.".into(),
            },
            Badge::NoServer => {
                "No language server of this file offers inline completions. For GitHub Copilot: npm i -g @github/copilot-language-server, then \"Copilot: Sign in\".".into()
            }
        }
    }

    // ───────────── GitHub Copilot ─────────────

    /// Copilot's server: the running one, else started for the active document.
    fn copilot_server(&mut self) -> Option<usize> {
        if let Some(li) = self.copilot_index() {
            return Some(li);
        }
        self.lspx_sync_config();
        let spec = self.lspx.reg.get(COPILOT).cloned()?;
        if !spec.enabled {
            let why = spec.disabled_by.unwrap_or_else(|| "language_servers.copilot.enabled is false".into());
            self.say_for(format!("Copilot is off: {why}"), 6);
            return None;
        }
        let entry = self.lspx_plan(self.active).and_then(|p| p.entries.iter().find(|e| e.id == COPILOT).cloned());
        let Some(e) = entry else {
            self.say("Copilot: open a file of the project first");
            return None;
        };
        if e.exe.is_none() {
            self.say_for(format!("Copilot: copilot-language-server is not installed ({})", spec.install), 10);
            return None;
        }
        let li = self.lspx_ensure(&e);
        if li.is_none() {
            self.say("Copilot: copilot-language-server did not start (LSP: Show server log)");
        }
        li
    }

    /// "Copilot: Sign in": the device-code flow of `copilot-language-server`.
    pub fn copilot_sign_in(&mut self) {
        if !self.config.inline_completion {
            self.inline_set_enabled(true);
        }
        let Some(li) = self.copilot_server() else { return };
        self.lsps[li].request("signIn", json!({}), Pending::Copilot(CopilotStep::SignIn));
        self.say("Copilot: signing in…");
    }

    pub fn copilot_sign_out(&mut self) {
        let Some(li) = self.copilot_index() else {
            return self.say("Copilot is not running (turn on Inline completions first)");
        };
        self.lsps[li].request("signOut", json!({}), Pending::Copilot(CopilotStep::SignOut));
    }

    fn copilot_check(&mut self, li: usize) {
        if self.inline.copilot.checking {
            return;
        }
        self.inline.copilot.checking = true;
        self.lsps[li].request("checkStatus", json!({}), Pending::Copilot(CopilotStep::Status));
    }

    /// A registry server finished its handshake: Copilot only serves inline completions here (its
    /// other features stay out of hover, completion and code actions), and says if it is signed in.
    pub(crate) fn inline_on_server_ready(&mut self, li: usize) {
        if !self.is_copilot(li) {
            return;
        }
        if let Some(o) = self.lsps[li].caps.as_object_mut() {
            o.retain(|k, _| {
                matches!(
                    k.as_str(),
                    "textDocumentSync" | "inlineCompletionProvider" | "executeCommandProvider" | "positionEncoding"
                )
            });
        }
        self.copilot_check(li);
    }

    fn copilot_on_response(&mut self, li: usize, step: CopilotStep, result: Result<Value, Value>) {
        if step == CopilotStep::Status {
            self.inline.copilot.checking = false;
        }
        let v = match result {
            Ok(v) => v,
            Err(e) => {
                let msg = e["message"].as_str().unwrap_or("error").to_string();
                self.lsps[li].log_line("app", &format!("copilot {step:?} failed: {msg}"));
                if step != CopilotStep::Status {
                    self.say_for(format!("Copilot: {msg}"), 6);
                }
                return;
            }
        };
        match step {
            CopilotStep::Status => {
                let a = auth_from_status(&v);
                // A sign-in in progress stays on screen until the server confirms it.
                let waiting = matches!(self.inline.copilot.auth, Auth::Waiting { .. });
                if !(waiting && matches!(a, Auth::SignedOut | Auth::Unknown)) {
                    self.inline.copilot.auth = a;
                }
            }
            CopilotStep::SignIn if v["status"] == "PromptUserDeviceFlow" => {
                let code = v["userCode"].as_str().unwrap_or("").to_string();
                let uri = v["verificationUri"]
                    .as_str()
                    .filter(|u| !u.is_empty())
                    .unwrap_or("https://github.com/login/device")
                    .to_string();
                self.clip.set(&code);
                self.inline_open_link(&uri);
                self.say_for(format!("Copilot: enter the code {code} at {uri} (copied; the browser opens)"), 120);
                self.inline.copilot.auth = Auth::Waiting { code: code.clone(), uri };
                // Current servers return the command that waits for the browser step; older ones
                // confirm with `signInConfirm`.
                let cmd = &v["command"];
                if let Some(name) = cmd["command"].as_str() {
                    let mut p = json!({"command": name});
                    if !cmd["arguments"].is_null() {
                        p["arguments"] = cmd["arguments"].clone();
                    }
                    self.lsps[li].request("workspace/executeCommand", p, Pending::Copilot(CopilotStep::Finish));
                } else {
                    self.lsps[li].request(
                        "signInConfirm",
                        json!({"userCode": code}),
                        Pending::Copilot(CopilotStep::Finish),
                    );
                }
            }
            CopilotStep::SignIn | CopilotStep::Finish => {
                let a = auth_from_status(&v);
                match &a {
                    Auth::SignedIn(u) => {
                        let who = if u.is_empty() { String::new() } else { format!(" as {u}") };
                        self.say(format!("Copilot: signed in{who}"));
                    }
                    Auth::NoAccess => self.say_for("Copilot: this GitHub account has no Copilot access", 8),
                    _ => self.copilot_check(li),
                }
                if a != Auth::Unknown {
                    self.inline.copilot.auth = a;
                }
            }
            CopilotStep::SignOut => {
                self.inline.copilot.auth = Auth::SignedOut;
                self.say("Copilot: signed out");
            }
        }
        self.request_redraw();
    }

    /// Notifications of registry servers nobody else handles (Copilot's status).
    pub(crate) fn inline_on_notification(&mut self, li: usize, method: &str, params: &Value) {
        if !self.is_copilot(li) || !matches!(method, "didChangeStatus" | "statusNotification") {
            return;
        }
        let msg = params["message"].as_str().unwrap_or("").to_string();
        let kind = params["kind"].as_str().or_else(|| params["status"].as_str()).unwrap_or("").to_string();
        let changed = kind != self.inline.copilot.kind || msg != self.inline.copilot.message;
        if changed && matches!(kind.as_str(), "Error" | "Warning") && !msg.is_empty() {
            self.lsps[li].log_line("status", &msg);
            self.say(format!("Copilot: {msg}"));
        }
        let signed_in = matches!(self.inline.copilot.auth, Auth::SignedIn(_));
        if changed && (!signed_in || kind == "Error") {
            self.copilot_check(li);
        }
        self.inline.copilot.kind = kind;
        self.inline.copilot.message = msg;
        self.request_redraw();
    }

    /// `window/showDocument`: web pages (sign-in) open in the browser, files in the editor.
    pub(crate) fn lsp_show_document(&mut self, params: &Value) -> Value {
        let Some(uri) = params["uri"].as_str() else { return json!({"success": false}) };
        if uri.starts_with("file:") && params["external"].as_bool() != Some(true) {
            let Some(p) = crate::doc::uri_to_path(uri) else { return json!({"success": false}) };
            return json!({"success": self.open_path(&p, None).is_some()});
        }
        if !(uri.starts_with("https://") || uri.starts_with("http://")) {
            return json!({"success": false});
        }
        self.inline_open_link(uri);
        json!({"success": true})
    }

    /// Open a web page for the user once (the sign-in page may be asked for twice: by the
    /// device-code answer and by the server's own `window/showDocument`).
    pub(crate) fn inline_open_link(&mut self, url: &str) {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return;
        }
        let now = Instant::now();
        self.inline.opened.retain(|(_, t)| now.duration_since(*t) < REOPEN_AFTER);
        if self.inline.opened.iter().any(|(u, _)| u == url) {
            return;
        }
        self.inline.opened.push((url.to_string(), now));
        if !cfg!(test) {
            crate::paths::open_url(url);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::KeyIn;
    use crate::app_overlay::Cmd;
    use crate::app_unsaved::testkit::{lock, scratch_base, test_app};
    use winit::keyboard::ModifiersState;

    /// The fake server built from `examples/fake_lsp.rs` (cargo builds examples for `cargo test`).
    fn fake_exe() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let p = exe.parent()?.parent()?.join("examples").join(format!("fake_lsp{}", std::env::consts::EXE_SUFFIX));
        p.exists().then_some(p)
    }

    fn project(tag: &str) -> PathBuf {
        let d = scratch_base().join(format!("inline-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        crate::paths::canonical(&d).unwrap()
    }

    fn tick(app: &mut App) {
        let now = Instant::now();
        app.lsp_tick(now, &mut |_| {});
        app.inline_tick(now, &mut |_| {});
    }

    /// Run the ticks and the message pump until `cond` holds (or give up after `ms`).
    fn pump(app: &mut App, ms: u64, mut cond: impl FnMut(&mut App) -> bool) -> bool {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            tick(app);
            app.pump_lsp();
            if cond(app) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        false
    }

    fn open(app: &mut App, f: &std::path::Path, text: &str) -> usize {
        std::fs::write(f, text).unwrap();
        let i = app.open_path(f, None).expect("open");
        app.active = i;
        i
    }

    fn key(app: &mut App, k: Key, text: Option<&str>, mods: ModifiersState) {
        app.mods = mods;
        app.handle_key(&KeyIn { key: k, text: text.map(str::to_string) });
        app.mods = ModifiersState::empty();
    }

    fn type_char(app: &mut App, c: &str) {
        key(app, Key::Character(c.into()), Some(c), ModifiersState::empty());
    }

    fn named(app: &mut App, k: NamedKey, mods: ModifiersState) {
        key(app, Key::Named(k), None, mods);
    }

    fn text(app: &App, i: usize) -> String {
        app.docs[i].buf.to_string()
    }

    fn shown(app: &App) -> String {
        app.inline.ghost.as_ref().map(|g| g.text().to_string()).unwrap_or_default()
    }

    fn stop(app: &mut App) {
        for l in &mut app.lsps {
            l.shutdown();
        }
    }

    fn server(exe: &std::path::Path, env: Value) -> Value {
        json!({"command": exe, "languages": ["fk"], "env": env})
    }

    #[test]
    fn suggestions_show_cycle_accept_partially_accept_and_dismiss() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("flow");
        let mut app = test_app(&dir);
        app.config.inline_completion = true;
        app.config.language_servers = json!({"fake-a": server(&exe, json!({"FAKE_NAME": "a", "FAKE_INLINE": "1"}))});
        let i = open(&mut app, &dir.join("x.fk"), "let a = b\n");
        assert!(pump(&mut app, 8000, |a| a.inline_server(i).is_some()), "server not ready: {:?}", app.status);
        assert_eq!(app.inline_badge(), Badge::On);
        let d = &mut app.docs[i];
        d.ed.click(&mut d.buf, 9, false);
        tick(&mut app);
        assert!(app.inline.due.is_none(), "a cursor move asks nothing");

        // A typing pause: an automatic request; the first suggestion shows as ghost text.
        type_char(&mut app, "c");
        app.inline_sync();
        assert!(!app.inline_visible());
        assert!(app.inline.due.is_some_and(|(t, k)| k == Trigger::Automatic && t > Instant::now()));
        assert!(pump(&mut app, 5000, |a| a.inline_visible()), "no suggestion: {:?}", app.status);
        assert_eq!(shown(&app), "_k2();\nnext_line();", "automatic trigger kind is 2");
        let g = app.docs[i].ghost.clone().expect("placed in the document");
        assert_eq!((g.line, g.col, g.first.as_str()), (0, 10, "_k2();"));
        assert_eq!(g.rest, vec!["next_line();".to_string()]);
        assert_eq!(app.docs[i].inlay.hints.ghost().map(|h| h.col), Some(10));
        assert_eq!(text(&app, i), "let a = bc\n", "ghost text is not in the buffer");

        // Drawn: one virtual row after the cursor line; the row below it shows the next line.
        app.size = (900, 500);
        let mut px = vec![0u32; 900 * 500];
        app.render_into(&mut Canvas { px: &mut px, w: 900, h: 500 }, 0.016);
        assert_eq!(app.docs[i].ghost_rows, Some((0, 1)));
        let (r, lh) = (app.docs[i].rect, crate::ui::line_h() as f64);
        let x = (r.x + app.docs[i].gutter_w() + 4) as f64;
        let line_at =
            |app: &App, row: f64| app.docs[i].buf.line_col(app.docs[i].pos_at(x, r.y as f64 + row * lh + lh / 2.0)).0;
        assert_eq!((line_at(&app, 0.0), line_at(&app, 1.0), line_at(&app, 2.0)), (0, 0, 1));

        // Cycling: the snippet, then the ranged one (it replaces the typed "c"), and back.
        app.run_cmd(Cmd::InlineNext);
        assert_eq!(shown(&app), "alt(arg)");
        app.run_cmd(Cmd::InlineNext);
        assert_eq!(shown(&app), "_ranged");
        app.run_cmd(Cmd::InlinePrev);
        app.run_cmd(Cmd::InlinePrev);
        assert_eq!(shown(&app), "_k2();\nnext_line();");

        // Esc hides it and leaves the text alone.
        named(&mut app, NamedKey::Escape, ModifiersState::empty());
        assert!(!app.inline_visible() && app.docs[i].ghost.is_none() && app.docs[i].inlay.hints.is_empty());
        assert_eq!(text(&app, i), "let a = bc\n");

        // Alt+\ asks explicitly (trigger kind 1).
        app.run_cmd(Cmd::InlineSuggest);
        assert!(pump(&mut app, 5000, |a| a.inline_visible()));
        assert_eq!(shown(&app), "_k1();\nnext_line();");

        // Ctrl+Right takes one word; the rest stays and nothing new is asked.
        named(&mut app, NamedKey::ArrowRight, ModifiersState::CONTROL);
        assert_eq!(text(&app, i), "let a = bc_k1\n");
        assert_eq!(shown(&app), "();\nnext_line();");
        tick(&mut app);
        assert!(app.inline_visible() && app.inline.due.is_none(), "typing through asks nothing");

        // Tab inserts the rest of the suggestion, then runs the command it came with.
        named(&mut app, NamedKey::Tab, ModifiersState::empty());
        assert_eq!(text(&app, i), "let a = bc_k1();\nnext_line();\n");
        assert!(!app.inline_visible());
        assert!(
            pump(&mut app, 4000, |a| a.lsps[0].log_text().contains("executed fake.accept [\"one\"]")),
            "the accept command did not reach the server: {}",
            app.lsps[0].log_text()
        );
        // One undo step per acceptance.
        app.run_cmd(Cmd::Undo);
        assert_eq!(text(&app, i), "let a = bc_k1\n");
        app.run_cmd(Cmd::Undo);
        assert_eq!(text(&app, i), "let a = bc\n");
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tab_without_a_suggestion_and_further_typing_cancel_the_request() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("cancel");
        let mut app = test_app(&dir);
        app.config.inline_completion = true;
        app.config.language_servers = json!({"fake-a": server(&exe, json!({"FAKE_NAME": "a", "FAKE_INLINE": "1"}))});
        let i = open(&mut app, &dir.join("x.fk"), "ab\n");
        assert!(pump(&mut app, 8000, |a| a.inline_server(i).is_some()));
        let d = &mut app.docs[i];
        d.ed.click(&mut d.buf, 2, false);
        // Tab with nothing shown keeps its meaning (indentation here).
        named(&mut app, NamedKey::Tab, ModifiersState::empty());
        assert_ne!(text(&app, i), "ab\n");
        // Wait until the request is out, without reading its answer, then type on.
        let end = Instant::now() + Duration::from_secs(3);
        while app.inline.inflight.is_none() && Instant::now() < end {
            tick(&mut app);
            std::thread::sleep(Duration::from_millis(5));
        }
        let first = app.inline.inflight.clone().expect("a request went out");
        type_char(&mut app, "z");
        app.inline_sync();
        assert!(app.inline.inflight.is_none(), "typing cancels the request in flight");
        assert!(!app.lsps[0].pending.contains_key(&first.id), "its late answer is dropped");
        // The next request answers for the new text only.
        assert!(pump(&mut app, 5000, |a| a.inline_visible()));
        let cursor = app.docs[i].ed.head();
        assert_eq!(app.inline.ghost.as_ref().unwrap().anchor, cursor);
        assert!(app.lsps[0].log_text().contains("$/cancelRequest"), "{}", app.lsps[0].log_text());
        // Moving the cursor dismisses.
        named(&mut app, NamedKey::ArrowLeft, ModifiersState::empty());
        app.inline_sync();
        assert!(!app.inline_visible());
        // Turning the feature off stops asking.
        type_char(&mut app, "q");
        app.inline_set_enabled(false);
        tick(&mut app);
        assert!(app.inline.due.is_none() && !app.inline_visible());
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copilot_sign_in_device_flow_status_and_switching_off() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("copilot");
        let mut app = test_app(&dir);
        let env = json!({"FAKE_NAME": "cp", "FAKE_INLINE": "1", "FAKE_COPILOT": "1"});
        app.config.language_servers = json!({"copilot": server(&exe, env)});
        let i = open(&mut app, &dir.join("x.fk"), "hello\n");
        // Off by default: Copilot does not start.
        pump(&mut app, 300, |_| false);
        assert!(app.copilot_index().is_none());
        let row = app.lspx_rows().into_iter().find(|r| r.id == COPILOT).unwrap();
        assert_eq!(row.state, crate::app_lspx::RowState::Disabled);
        // On: it starts as a companion and reports that nobody is signed in.
        app.inline_set_enabled(true);
        assert!(
            pump(&mut app, 8000, |a| a.inline.copilot.auth == Auth::SignedOut),
            "copilot status: {:?} {:?}",
            app.inline.copilot.auth,
            app.status
        );
        let li = app.copilot_index().unwrap();
        assert!(app.lsps[li].has_cap("inlineCompletionProvider"));
        assert!(!app.lsps[li].has_cap("completionProvider") && !app.lsps[li].has_cap("hoverProvider"));
        assert!(app.inline_server(i).is_some());
        assert_eq!(app.inline_badge(), Badge::SignedOut);
        // Sign in: the code is copied, the page opened once, the device flow finished by the server.
        app.run_cmd(Cmd::CopilotSignIn);
        assert!(
            pump(&mut app, 5000, |a| a.inline.copilot.auth == Auth::SignedIn("octocat".into())),
            "{:?} {:?}",
            app.inline.copilot.auth,
            app.status
        );
        assert_eq!(app.clip.get(), "ABCD-1234");
        let opened: Vec<&str> = app.inline.opened.iter().map(|(u, _)| u.as_str()).collect();
        assert_eq!(opened, vec!["https://example.invalid/device"]);
        assert!(app.lsps[li].log_text().contains("executed github.copilot.finishDeviceFlow"));
        assert_eq!(app.inline_badge(), Badge::On);
        // The server asking to open the same page again does not open a second tab; odd schemes never open.
        let again = app.lsp_show_document(&json!({"uri": "https://example.invalid/device", "external": true}));
        assert_eq!((again, app.inline.opened.len()), (json!({"success": true}), 1));
        let odd = app.lsp_show_document(&json!({"uri": "javascript:alert(1)", "external": true}));
        assert_eq!(odd, json!({"success": false}));
        // Sign out.
        app.run_cmd(Cmd::CopilotSignOut);
        assert!(pump(&mut app, 3000, |a| a.inline.copilot.auth == Auth::SignedOut));
        // Off again: Copilot stops and its state is forgotten.
        app.inline_set_enabled(false);
        assert!(app.copilot_index().is_none());
        assert_eq!(app.inline.copilot.auth, Auth::Unknown);
        stop(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copilot_gate_follows_the_setting_and_the_user_switch() {
        let mut r = Registry::builtin();
        gate_copilot(&mut r, &Value::Null, true);
        assert!(r.get(COPILOT).unwrap().enabled);
        gate_copilot(&mut r, &Value::Null, false);
        let c = r.get(COPILOT).unwrap();
        assert!(!c.enabled && c.disabled_by.as_deref().is_some_and(|w| w.contains("inline_completion")));
        let user = json!({"copilot": {"enabled": false}});
        let mut r = Registry::with_user(&user);
        gate_copilot(&mut r, &user, true);
        assert!(!r.get(COPILOT).unwrap().enabled, "the user's own switch wins");
    }
}
