//! AI panel: the user's own agent CLI (Claude Code, Gemini CLI, Codex CLI) running in a terminal
//! docked on the right, in the slot it shares with the Git panel (a "Git | AI" switch on top picks
//! which one shows). The agent runs in the project folder with the user's environment, so it uses
//! its own stored login: Lumen never sees keys or tokens and does not script the CLI.
//!
//! While the panel has the focus every key goes to the agent (Esc included: agents use it to
//! interrupt) except a short list of editor shortcuts ([`AI_GLOBAL`]). Clicking the editor, or
//! Ctrl+1, gives the keyboard back to the editor; "AI: Toggle Panel" (Ctrl+Shift+A) shows, focuses
//! and hides the panel. Hiding it never stops the agent.
//!
//! Modules: `agents` (which CLIs, finding them) and the status heuristic in `state` are plain code
//! covered by unit tests; `draw` and `input` are the App glue. A later IDE bridge (diffs, hooks,
//! selection context over MCP) plugs into the same session.

use crate::{
    app::{App, Focus, KeyIn},
    app_termio::Which,
    pty::EnvEdit,
    terminal::Term,
    ui::{Canvas, Rect},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use winit::{event::MouseScrollDelta, keyboard::Key, window::CursorIcon};

mod agents;
mod draw;
mod input;
mod state;
#[cfg(test)]
mod tests;

pub use agents::*;
pub use state::*;

/// Height of the "Git | AI" switch on top of the right-hand slot.
pub const SLOT_H: i32 = 40;
/// Height of the AI panel's header (agent, status, session buttons).
pub(crate) const HEAD_H: i32 = 44;
/// Inner padding around the agent's terminal.
pub(crate) const PAD: i32 = 12;

/// Commands whose shortcuts keep working while the agent has the keyboard (as bound in the keymap).
const AI_GLOBAL: &[&str] = &["AiPanel", "ToggleOutput", "Quit", "Commands", "FocusLeftPane"];

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

impl App {
    /// The panel's area under the switch (the same place the Git panel uses).
    pub(crate) fn aipanel_rect(&self) -> Rect {
        self.gitpanel_rect()
    }

    /// Where the agent's terminal goes (inside the padding).
    fn ai_inner_rect(&self) -> Rect {
        let p = self.aipanel_rect();
        Rect::new(p.x + 1 + PAD, p.y + HEAD_H + PAD, p.w - 1 - 2 * PAD, (p.h - HEAD_H - 2 * PAD).max(0))
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
        self.ai
            .agent
            .filter(installed)
            .or(remembered.filter(installed))
            .or_else(|| AGENTS.iter().find(|a| self.ai.path_of(a).is_some()))
            .or(self.ai.agent)
            .or(remembered)
    }

    /// Show the panel (the Git panel steps aside) with the keyboard on the agent; starts one if
    /// none is running yet.
    pub fn ai_show(&mut self) {
        if self.gitpanel.is_some() {
            self.close_git_panel();
        }
        self.ai.open = true;
        self.ai.focused = true;
        self.ai.unseen = false;
        self.focus = Focus::Editor;
        if self.ai.root != self.root {
            // Another project since the session started: the agent belongs to the old one.
            self.ai.term = None;
        }
        if self.ai.term.is_none() {
            match self.ai_default_agent() {
                Some(a) if self.ai.path_of(a).is_some() => self.ai_start(a),
                a => self.ai.agent = a,
            }
        }
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
            self.ai.unseen = false;
            self.focus = Focus::Editor;
        } else {
            self.ai_hide();
        }
    }

    /// Start `a` in the project folder (ending the current session first).
    pub fn ai_start(&mut self, a: &'static Agent) {
        self.ai_detect(false);
        self.ai.agent = Some(a);
        if let Some(p) = state_path(&self.root) {
            save_choice(&p, a.id);
        }
        // The old session's process group is ended before the new one starts.
        self.ai.term = None;
        let Some(exe) = self.ai.path_of(a) else { return };
        let inner = self.ai_inner_rect();
        let (rows, cols) =
            if inner.w > 0 && inner.h > 0 { crate::termview::fit(self.tx.cell_w, inner.w, inner.h) } else { (24, 80) };
        let (program, args) = command_for(&exe);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let path = path_with(exe.parent(), std::env::var_os("PATH").as_deref());
        // A CLI started from inside another agent session (or an IDE's terminal) must not think it
        // is nested or attached to that IDE.
        let mut env: Vec<EnvEdit> = vec![
            ("TERM_PROGRAM", Some("lumen-text")),
            ("CLAUDECODE", None),
            ("CLAUDE_CODE_ENTRYPOINT", None),
            ("CLAUDE_CODE_SSE_PORT", None),
        ];
        if let Some(p) = path.as_deref() {
            env.push(("PATH", Some(p)));
        }
        match Term::spawn_cmd(&self.root, &program, &args, &env, rows, cols, self.wake()) {
            Ok(t) => {
                self.ai.term = Some(t);
                self.ai.root = self.root.clone();
                self.ai.reset_session();
            }
            Err(e) => self.say(format!("could not start {}: {e}", a.name)),
        }
        self.request_redraw();
    }

    /// New session / Restart: end the agent and start it again, fresh.
    pub fn ai_respawn(&mut self) {
        match self.ai.agent {
            Some(a) => {
                self.ai_detect(true);
                self.ai_start(a);
            }
            None => self.ai_show(),
        }
        if !self.ai.open {
            self.ai_show();
        }
    }

    /// Stop: hang up the agent's process group (the last screen stays, with a Restart button).
    pub fn ai_stop(&mut self) {
        if let Some(t) = self.ai.term.as_mut()
            && t.alive
        {
            t.hangup();
        }
    }

    /// "AI: Choose Agent…": every known CLI, the installed ones first.
    pub fn ai_choose_agent(&mut self) {
        self.ai_detect(true);
        let cur = self.ai.agent.map(|a| a.id);
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
                action: crate::app_overlay::Action::AiAgent(a.id.to_string()),
            });
        }
        items.sort_by_key(|i| i.detail.starts_with("not installed"));
        self.open_locations("AI agent", items);
    }

    /// An agent picked in the chooser: start it (a new session), or copy its install command.
    pub fn ai_pick(&mut self, id: &str) {
        let Some(a) = agent(id) else { return };
        self.ai_detect(false);
        if self.ai.path_of(a).is_none() {
            self.clip.set(a.install);
            self.ai.agent = Some(a);
            self.ai.term = None;
            return self.say(format!("{} is not installed — copied: {}", a.name, a.install));
        }
        let running_same = self.ai.agent == Some(a) && self.ai.term.as_ref().is_some_and(|t| t.alive);
        if !running_same {
            self.ai.term = None;
            self.ai.agent = Some(a);
        }
        // Shows the panel and, with nothing running, starts the current agent: this one.
        self.ai_show();
    }

    /// Drain the agent's output; called whenever the event loop is woken.
    pub fn pump_ai(&mut self) {
        let Some(t) = self.ai.term.as_mut() else { return };
        let changed = t.pump();
        if !self.term_effects(Which::Ai).is_empty() {
            self.ai.attention = true;
        }
        let status_changed = self.ai.refresh_status(Instant::now());
        // A hidden panel costs nothing: only a status change (the badge) is drawn.
        if (changed && self.ai.open) || status_changed {
            self.request_redraw();
        }
    }

    /// Per-event hook: the status changes with time too (output going quiet).
    pub fn ai_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if self.ai.term.is_none() {
            return;
        }
        if self.ai.refresh_status(now) {
            self.request_redraw();
        }
        let ms = |n: u64| Duration::from_millis(n);
        match (self.ai.status, self.ai.last_activity) {
            (Status::Working | Status::Starting, Some(a)) => {
                want(a + ms(if self.ai.attention { ATTENTION_MS } else { IDLE_MS }));
            }
            (Status::Starting, None) => {
                if let Some(t) = self.ai.term.as_ref() {
                    want(t.started + ms(STARTUP_MS));
                }
            }
            _ => {}
        }
    }
}
