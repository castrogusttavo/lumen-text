//! AI panel: the user's own agent CLI (Claude Code, Gemini CLI, Codex CLI) running in a terminal
//! docked on the right, in the slot it shares with the Git panel (a "Git | AI" switch on top picks
//! which one shows). The agent runs in the project folder with the user's environment, so it uses
//! its own stored login: Lumen never sees keys or tokens and does not script the CLI.
//!
//! Several agents can run at once: each **session** has its own terminal, status and working
//! folder (the project by default; [`App::ai_new_session_in`] starts one elsewhere, such as a git
//! worktree), and a tab at the top of the panel (status dot, name, close). A session that finishes
//! or asks for an answer while nobody looks marks its tab, the status bar sums them up ("2
//! waiting"), "AI: Next Session Waiting for You" jumps to it, and with Lumen in the background a
//! desktop notification goes out (once per turn, `ai_notifications`).
//!
//! While the panel has the focus every key goes to the agent (Esc included: agents use it to
//! interrupt) except a short list of editor shortcuts ([`AI_GLOBAL`]). Clicking the editor, or
//! Ctrl+1, gives the keyboard back to the editor; "AI: Toggle Panel" (Ctrl+Shift+A) shows, focuses
//! and hides the panel. Hiding it never stops the agents. `Ctrl+V` pastes what the clipboard holds
//! as one bracketed paste (copied files as their paths), or, for an image, hands the key to the
//! agent so it reads the image itself.
//!
//! Modules: `agents` (which CLIs, finding them) and the session model in `state` are plain code
//! covered by unit tests; `draw` and `input` are the App glue; `bridge` is the editor side of the
//! IDE bridge (`crate::ide`): Claude Code started here connects back to Lumen over MCP, so its
//! edits open in the diff view (Accept / Reject), it sees the selection, open files and
//! diagnostics, and its hooks drive the status of its session.

use crate::{
    app::{App, Focus, KeyIn},
    app_termio::Which,
    pty::EnvEdit,
    terminal::Term,
    ui::{Canvas, Rect},
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use winit::{event::MouseScrollDelta, keyboard::Key, window::CursorIcon};

mod agents;
pub mod bridge;
#[cfg(test)]
mod bridge_tests;
mod draw;
mod input;
mod state;
#[cfg(test)]
mod tests;

pub use agents::*;
pub use state::*;

/// Height of the "Git | AI" switch on top of the right-hand slot.
pub const SLOT_H: i32 = 40;
/// Height of the row of session tabs (shown while any session exists).
pub(crate) const TABS_H: i32 = 36;
/// Height of the AI panel's header (agent, status, session buttons).
pub(crate) const HEAD_H: i32 = 44;
/// Inner padding around the agent's terminal.
pub(crate) const PAD: i32 = 12;
/// Two clicks on a session tab this close together rename it.
const DOUBLE_CLICK_MS: u128 = 450;

/// Commands whose shortcuts keep working while the agent has the keyboard (as bound in the keymap).
const AI_GLOBAL: &[&str] = &["AiPanel", "ToggleOutput", "Quit", "Commands", "FocusLeftPane", "AiNextWaiting"];

/// Is this key one the editor keeps while the AI panel has the focus? A plain `Ctrl+<letter>`
/// always reaches the agent (Ctrl+J new line, Ctrl+B, Ctrl+R, Ctrl+C...) unless it quits Lumen,
/// even when it is bound to one of the commands above (Ctrl+J toggles the output panel elsewhere).
pub fn ai_global(keymap: &crate::keymap::Keymap, ctrl: bool, shift: bool, alt: bool, key: &Key) -> bool {
    let plain_letter = ctrl
        && !shift
        && !alt
        && matches!(key, Key::Character(c) if c.chars().count() == 1 && c.chars().all(|ch| ch.is_ascii_alphabetic()));
    keymap.lookup(ctrl, shift, alt, key).is_some_and(|id| AI_GLOBAL.contains(&id) && (!plain_letter || id == "Quit"))
}

/// A path as pasted into an agent's prompt: quoted the shell way when it has to be (what a
/// terminal types for a dropped file), so the CLI sees one path, and attaches it when it is an image.
pub fn paste_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    let plain = !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || "/\\._-+:@%,~=".contains(c));
    if plain {
        s.into_owned()
    } else if cfg!(windows) {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The bytes of one paste into an agent: line ends become Enter (`\r`, as terminals send them),
/// and when the agent asked for bracketed paste the whole text goes inside a single pair of
/// markers — paste-end markers inside the text are dropped, so a paste can never end early and
/// turn its lines into separate submits. Nothing is cut or split, however long.
pub fn paste_payload(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if !bracketed {
        return text.into_bytes();
    }
    let text = text.replace("\x1b[201~", "").replace("\x1b[200~", "");
    let mut v = Vec::with_capacity(text.len() + 12);
    v.extend_from_slice(b"\x1b[200~");
    v.extend_from_slice(text.as_bytes());
    v.extend_from_slice(b"\x1b[201~");
    v
}

impl App {
    /// The panel's area under the switch (the same place the Git panel uses).
    pub(crate) fn aipanel_rect(&self) -> Rect {
        self.gitpanel_rect()
    }

    /// Height of what sits above the agent's terminal: the session tabs (when any) and the header.
    pub(crate) fn ai_top_h(&self) -> i32 {
        HEAD_H + if self.ai.sessions.is_empty() { 0 } else { TABS_H }
    }

    /// Where the agent's terminal goes (inside the padding).
    fn ai_inner_rect(&self) -> Rect {
        let p = self.aipanel_rect();
        let top = self.ai_top_h().max(HEAD_H + TABS_H);
        Rect::new(p.x + 1 + PAD, p.y + top + PAD, p.w - 1 - 2 * PAD, (p.h - top - 2 * PAD).max(0))
    }

    /// Look for agent CLIs (once, or again with `force`).
    pub fn ai_detect(&mut self, force: bool) {
        if self.ai.detected.is_some() && !force {
            return;
        }
        let dirs = match &self.ai.search {
            Some(d) => d.clone(),
            None => search_dirs(std::env::var_os("PATH").as_deref(), crate::paths::home().as_deref()),
        };
        self.ai.detected = Some(detect(&dirs));
    }

    /// The agent to start: the current one, else the one remembered for this project, else the
    /// first installed (an installed one always wins over one that is not).
    fn ai_default_agent(&mut self) -> Option<&'static Agent> {
        self.ai_detect(false);
        let remembered = state_path(&self.root).and_then(|p| load_choice(&p)).and_then(|id| agent(&id));
        let installed = |a: &&'static Agent| self.ai.path_of(a).is_some();
        let cur = self.ai.agent();
        cur.filter(installed)
            .or(remembered.filter(installed))
            .or_else(|| AGENTS.iter().find(|a| self.ai.path_of(a).is_some()))
            .or(cur)
            .or(remembered)
    }

    /// The session shown is in front of the user: its "unseen" mark goes.
    fn ai_seen(&mut self) {
        if self.ai.open
            && self.ai.focused
            && let Some(s) = self.ai.cur_mut()
        {
            s.track.unseen = false;
        }
    }

    /// Show the panel (the Git panel steps aside) with the keyboard on the agent; starts one if
    /// no session exists yet.
    pub fn ai_show(&mut self) {
        if self.gitpanel.is_some() {
            self.close_git_panel();
        }
        self.ai.open = true;
        self.ai.focused = true;
        self.focus = Focus::Editor;
        if self.ai.root != self.root {
            // Another project since the sessions started: their agents belong to the old one.
            self.ai_close_all();
            self.ai.root = self.root.clone();
        }
        if self.ai.sessions.is_empty() {
            match self.ai_default_agent() {
                Some(a) if self.ai.path_of(a).is_some() => {
                    self.ai_new_session(a, None);
                }
                a => self.ai.last_agent = a,
            }
        }
        self.ai_seen();
        self.request_redraw();
    }

    pub fn ai_hide(&mut self) {
        self.ai.open = false;
        self.ai.focused = false;
        self.ai.resizing = false;
    }

    /// "AI: Toggle Panel": show it, or focus it, or hide it when it already has the focus.
    pub fn ai_toggle(&mut self) {
        if !self.ai.open {
            self.ai_show();
        } else if !self.ai.focused {
            self.ai.focused = true;
            self.focus = Focus::Editor;
            self.ai_seen();
        } else {
            self.ai_hide();
        }
    }

    /// Start agent `a` on a terminal of its own in `cwd` for session `id`: the IDE bridge for
    /// Claude Code (hook URLs naming the session), a clean environment, the agent's folder on PATH.
    fn ai_spawn(&mut self, a: &'static Agent, id: u64, cwd: &Path) -> Option<Term> {
        let exe = self.ai.path_of(a)?;
        let inner = self.ai_inner_rect();
        let (rows, cols) =
            if inner.w > 0 && inner.h > 0 { crate::termview::fit(self.tx.cell_w, inner.w, inner.h) } else { (24, 80) };
        let (program, mut args) = command_for(&exe);
        let path = path_with(exe.parent(), std::env::var_os("PATH").as_deref());
        // Claude Code connects back to Lumen (the IDE bridge) and reports its status by hooks.
        let bridge = if a.id == "claude" { self.ide_agent_setup(id) } else { None };
        if let Some((_, _, settings)) = &bridge {
            args.extend(["--settings".to_string(), settings.clone()]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let sid = id.to_string();
        // A CLI started from inside another agent session (or an IDE's terminal) must not think it
        // is nested or attached to that IDE: those variables go, and ours replace them.
        let mut env: Vec<EnvEdit> = vec![
            ("TERM_PROGRAM", Some("lumen-text")),
            ("CLAUDECODE", None),
            ("CLAUDE_CODE_ENTRYPOINT", None),
            ("CLAUDE_CODE_SSE_PORT", None),
            ("LUMEN_AI_SESSION", Some(sid.as_str())),
        ];
        if let Some((port, hook_token, _)) = &bridge {
            env.extend([
                ("CLAUDE_CODE_SSE_PORT", Some(port.as_str())),
                ("ENABLE_IDE_INTEGRATION", Some("true")),
                (crate::ide::HOOK_ENV, Some(hook_token.as_str())),
            ]);
        }
        if let Some(p) = path.as_deref() {
            env.push(("PATH", Some(p)));
        }
        match Term::spawn_cmd(cwd, &program, &args, &env, rows, cols, self.wake()) {
            Ok(t) => {
                if bridge.is_some() {
                    // Its WebSocket client, when it connects, is this session's.
                    self.ai.bridge.expect_session(id);
                }
                Some(t)
            }
            Err(e) => {
                self.say(format!("could not start {}: {e}", a.name));
                None
            }
        }
    }

    /// Start `a` in a new session (in `cwd`, the project by default) and make it the one shown.
    /// Returns false when it could not start.
    pub fn ai_new_session(&mut self, a: &'static Agent, cwd: Option<PathBuf>) -> bool {
        self.ai_detect(false);
        self.ai.last_agent = Some(a);
        if let Some(p) = state_path(&self.root) {
            save_choice(&p, a.id);
        }
        let cwd = cwd.unwrap_or_else(|| self.root.clone());
        let id = self.ai.new_id();
        let Some(term) = self.ai_spawn(a, id, &cwd) else { return false };
        let taken: Vec<&str> = self.ai.sessions.iter().map(|s| s.name.as_str()).collect();
        let name = session_name(a, &cwd, &self.root, &taken);
        if let Some(t) = self.ai.term_mut() {
            t.set_focus(false);
        }
        self.ai.sessions.push(Session { id, agent: a, name, cwd, term, track: Track::default() });
        self.ai.active = self.ai.sessions.len() - 1;
        self.ai.root = self.root.clone();
        self.request_redraw();
        true
    }

    /// A new session of `a` in another folder (a git worktree...), shown with the keyboard on it.
    pub fn ai_new_session_in(&mut self, a: &'static Agent, cwd: PathBuf) {
        if self.ai.root != self.root {
            self.ai_close_all();
        }
        if self.ai_new_session(a, Some(cwd)) {
            self.ai_show();
        }
    }

    /// "AI: New Session in Worktree…" picked a folder: a new session of the usual agent there.
    pub fn ai_new_session_at(&mut self, cwd: PathBuf) {
        match self.ai_default_agent() {
            Some(a) if self.ai.path_of(a).is_some() => self.ai_new_session_in(a, cwd),
            Some(a) => self.ai_not_installed(a),
            None => self.ai_show(),
        }
    }

    /// Restart the session shown (same tab, name and folder), with agent `a` instead of its own.
    pub fn ai_restart(&mut self, a: Option<&'static Agent>) {
        self.ai_detect(true);
        let Some(s) = self.ai.cur_mut() else { return };
        let (id, cwd, old) = (s.id, s.cwd.clone(), s.agent);
        let a = a.unwrap_or(old);
        if self.ai.path_of(a).is_none() {
            return self.say(format!("{} is not installed — {}", a.name, a.install));
        }
        // The old process group is ended before the new one starts.
        if let Some(t) = self.ai.term_mut() {
            t.hangup();
        }
        self.ai.bridge.forget_session(id);
        let Some(term) = self.ai_spawn(a, id, &cwd) else { return };
        self.ai.last_agent = Some(a);
        if let Some(p) = state_path(&self.root) {
            save_choice(&p, a.id);
        }
        let Some(s) = self.ai.cur_mut() else { return };
        if a.id != old.id && s.name.starts_with(old.name) {
            s.name = format!("{}{}", a.name, &s.name[old.name.len()..]);
        }
        s.agent = a;
        s.term = term;
        s.track = Track::default();
        self.request_redraw();
    }

    /// Restart (the header button, Enter on an ended agent): the session shown starts again.
    pub fn ai_respawn(&mut self) {
        if self.ai.sessions.is_empty() {
            self.ai_show();
        } else {
            self.ai_restart(None);
        }
        if !self.ai.open {
            self.ai_show();
        }
    }

    /// Stop: hang up the agent's process group (the last screen stays, with a Restart button).
    pub fn ai_stop(&mut self) {
        if let Some(t) = self.ai.term_mut()
            && t.alive
        {
            t.hangup();
        }
    }

    /// Close session `i`: its agent (and everything it started) ends, the tab goes.
    pub fn ai_close_session(&mut self, i: usize) {
        if i >= self.ai.sessions.len() {
            return;
        }
        let s = self.ai.sessions.remove(i);
        self.ai.bridge.forget_session(s.id);
        if self.ai.active > i || self.ai.active >= self.ai.sessions.len() {
            self.ai.active = self.ai.active.saturating_sub(1);
        }
        if let Some(c) = self.ai.cur() {
            self.ai.last_agent = Some(c.agent);
        }
        self.ai_seen();
        self.request_redraw();
    }

    /// End every session (another project took over).
    fn ai_close_all(&mut self) {
        for s in std::mem::take(&mut self.ai.sessions) {
            self.ai.bridge.forget_session(s.id);
        }
        self.ai.active = 0;
    }

    /// Show session `i`.
    pub fn ai_activate(&mut self, i: usize) {
        if i >= self.ai.sessions.len() {
            return;
        }
        if i != self.ai.active
            && let Some(t) = self.ai.term_mut()
        {
            t.set_focus(false);
        }
        self.ai.active = i;
        self.ai_seen();
        self.request_redraw();
    }

    /// "AI: Next Session Waiting for You": show the next session whose agent asks for an answer.
    pub fn ai_next_waiting(&mut self) {
        let statuses: Vec<Status> = self.ai.sessions.iter().map(Session::status).collect();
        match next_waiting(&statuses, self.ai.active) {
            Some(i) => {
                self.ai_activate(i);
                self.ai_show();
            }
            None if self.ai.sessions.is_empty() => self.say("No agent session — start one in the AI panel"),
            None => self.say("No session is waiting for you"),
        }
    }

    /// The status bar badge: the next session waiting for you, else the first one that finished
    /// unseen, shown with the keyboard on it.
    pub fn ai_badge_click(&mut self) {
        let statuses: Vec<Status> = self.ai.sessions.iter().map(Session::status).collect();
        let pick =
            next_waiting(&statuses, self.ai.active).or_else(|| self.ai.sessions.iter().position(|s| s.track.unseen));
        if let Some(i) = pick {
            self.ai_activate(i);
        }
        self.ai_show();
    }

    /// "AI: Close Session".
    pub fn ai_close_current(&mut self) {
        let i = self.ai.active;
        self.ai_close_session(i);
    }

    /// "AI: Rename Session…": a prompt for the name of the session shown.
    pub fn ai_rename_prompt(&mut self) {
        let Some(s) = self.ai.cur() else { return self.say("No agent session to rename") };
        let (id, name) = (s.id, s.name.clone());
        self.overlay = crate::app_overlay::Overlay::Prompt(crate::app_overlay::Prompt::new(
            crate::app_overlay::PromptKind::AiRename { id },
            "Session name".into(),
            crate::field::TextField::with(&name),
        ));
    }

    /// The rename prompt answered (an empty name gives the session its agent's name back).
    pub fn ai_rename(&mut self, id: u64, name: &str) {
        let Some(i) = self.ai.index_of(id) else { return };
        let name = name.trim();
        let s = &self.ai.sessions[i];
        let name = if name.is_empty() {
            let taken: Vec<&str> = self.ai.sessions.iter().filter(|o| o.id != id).map(|o| o.name.as_str()).collect();
            session_name(s.agent, &s.cwd, &self.root, &taken)
        } else {
            name.to_string()
        };
        self.ai.sessions[i].name = name;
        self.request_redraw();
    }

    /// The agents as chooser items (installed first), each running `action`.
    fn ai_agent_items(&mut self, action: fn(String) -> crate::app_overlay::Action) -> Vec<crate::app_overlay::Item> {
        self.ai_detect(true);
        let cur = self.ai.agent().map(|a| a.id);
        let mut items: Vec<crate::app_overlay::Item> = Vec::new();
        for a in AGENTS {
            let detail = match self.ai.path_of(a) {
                Some(p) if cur == Some(a.id) => format!("current · {}", crate::app_topbar::tilde(&p)),
                Some(p) => crate::app_topbar::tilde(&p),
                None => format!("not installed — {}", a.install),
            };
            items.push(crate::app_overlay::Item {
                label: a.name.to_string(),
                detail,
                action: action(a.id.to_string()),
            });
        }
        // Installed ones first, the current agent on top of them.
        items.sort_by_key(|i| (i.detail.starts_with("not installed"), !i.detail.starts_with("current")));
        items
    }

    /// "AI: Choose Agent…": the agent of the session shown (it restarts with the one picked).
    pub fn ai_choose_agent(&mut self) {
        let items = self.ai_agent_items(crate::app_overlay::Action::AiAgent);
        self.open_locations("AI agent", items);
    }

    /// "AI: New Session" / the tabs' `+`: pick the agent of another session.
    pub fn ai_new_session_menu(&mut self) {
        let items = self.ai_agent_items(crate::app_overlay::Action::AiNew);
        self.open_locations("New agent session", items);
    }

    /// An agent not installed: its install command goes to the clipboard.
    fn ai_not_installed(&mut self, a: &'static Agent) {
        self.clip.set(a.install);
        self.ai.last_agent = Some(a);
        self.say(format!("{} is not installed — copied: {}", a.name, a.install));
    }

    /// An agent picked in the chooser: the session shown switches to it (or the first session
    /// starts with it), or its install command is copied.
    pub fn ai_pick(&mut self, id: &str) {
        let Some(a) = agent(id) else { return };
        self.ai_detect(false);
        if self.ai.path_of(a).is_none() {
            return self.ai_not_installed(a);
        }
        let running_same = self.ai.cur().map(|s| s.agent.id == a.id && s.term.alive);
        match running_same {
            Some(true) => {}
            Some(false) => self.ai_restart(Some(a)),
            None => {
                self.ai_new_session(a, None);
            }
        }
        self.ai_show();
    }

    /// An agent picked for a new session.
    pub fn ai_pick_new(&mut self, id: &str) {
        let Some(a) = agent(id) else { return };
        self.ai_detect(false);
        if self.ai.path_of(a).is_none() {
            return self.ai_not_installed(a);
        }
        if self.ai.root != self.root {
            self.ai_close_all();
        }
        if self.ai_new_session(a, None) {
            self.ai_show();
        }
    }

    /// The Lumen window gained or lost the keyboard focus (desktop notifications go out only
    /// while it is in the background).
    pub fn ai_window_focus(&mut self, focused: bool) {
        self.ai.window_focused = focused;
    }

    /// Clipboard writes (OSC 52) and notifications (OSC 9 / 777) of session `i`'s agent.
    fn ai_effects(&mut self, i: usize) {
        let Some(s) = self.ai.sessions.get_mut(i) else { return };
        let clip = s.term.grid.clipboard.take();
        let notices = std::mem::take(&mut s.term.grid.notices);
        if !notices.is_empty() {
            s.track.attention = true;
        }
        let name = s.name.clone();
        if let Some(c) = clip {
            self.clip.set(&c);
        }
        if let Some((title, body)) = notices.last() {
            let text = if title.is_empty() { body.clone() } else { format!("{title}: {body}") };
            let text = if i == self.ai.active { text } else { format!("{name} — {text}") };
            self.toast(text, true);
        }
    }

    /// Fold session `i`'s latest state into its status (a desktop notification when it just
    /// finished or asks); true when its chip changed.
    pub(crate) fn ai_refresh(&mut self, i: usize, now: Instant) -> bool {
        let watching = self.ai.watching(i);
        let Some(s) = self.ai.sessions.get_mut(i) else { return false };
        let c = s.refresh(now, watching);
        if s.track.wants_notice(c) {
            self.ai_notify(i);
        }
        c.changed
    }

    /// The desktop notification for session `i` (only with Lumen in the background).
    fn ai_notify(&mut self, i: usize) {
        if !self.config.ai_notifications || self.ai.window_focused {
            return;
        }
        let Some(s) = self.ai.sessions.get_mut(i) else { return };
        let what = match s.status() {
            Status::Waiting => "Waiting for you",
            Status::Done => "Done",
            _ => return,
        };
        let task = title_task(&s.term.grid.title);
        let body = if task.is_empty() { what.to_string() } else { format!("{what} — {task}") };
        s.track.notified = true;
        let title = s.name.clone();
        #[cfg(test)]
        self.ai.sent.push((title, body));
        #[cfg(not(test))]
        crate::notify::send(&title, &body);
    }

    /// Drain the agents' output; called whenever the event loop is woken.
    pub fn pump_ai(&mut self) {
        let now = Instant::now();
        let mut redraw = false;
        for i in 0..self.ai.sessions.len() {
            let changed = self.ai.sessions[i].term.pump();
            self.ai_effects(i);
            // A hidden panel (or a session in the background) costs nothing: only a status
            // change (its dot, the badge) is drawn.
            redraw |= changed && self.ai.open && i == self.ai.active;
            redraw |= self.ai_refresh(i, now);
        }
        if redraw {
            self.request_redraw();
        }
    }

    /// Per-event hook: the status changes with time too (output going quiet).
    pub fn ai_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let ms = |n: u64| Duration::from_millis(n);
        let mut redraw = false;
        for i in 0..self.ai.sessions.len() {
            redraw |= self.ai_refresh(i, now);
            let s = &self.ai.sessions[i];
            match (s.status(), s.track.last_activity) {
                (Status::Working | Status::Starting, Some(a)) => {
                    want(a + ms(if s.track.attention { ATTENTION_MS } else { IDLE_MS }));
                }
                (Status::Starting, None) => want(s.term.started + ms(STARTUP_MS)),
                _ => {}
            }
        }
        if redraw {
            self.request_redraw();
        }
    }
}
