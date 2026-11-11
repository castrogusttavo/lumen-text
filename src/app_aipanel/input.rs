use super::*;
use crate::app_tooltip::TipText;

/// Half-width of the grab band around the slot's left edge.
const GRAB: i32 = 4;

impl App {
    /// Is (x, y) on the draggable left edge of the shown AI panel?
    pub(crate) fn ai_on_edge(&self, x: f64, y: f64) -> bool {
        let s = self.right_slot_rect();
        self.ai.open && Rect::new(s.x - GRAB, s.y, 2 * GRAB, s.h).contains(x, y)
    }

    /// Keys while the panel has the focus: everything goes to the agent but the editor's few
    /// global shortcuts. Returns true when consumed.
    pub fn aipanel_key(&mut self, ev: &KeyIn) -> bool {
        if !self.ai.open || !self.ai.focused {
            return false;
        }
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        let clip_key = matches!(&ev.key, Key::Character(c)
            if ctrl && shift && !alt && matches!(c.to_lowercase().as_str(), "c" | "v"));
        if !clip_key && ai_global(&self.keymap, ctrl, shift, alt, &ev.key) {
            // Ctrl+1 (focus the editor pane) also takes the keyboard away from the agent.
            if self.keymap.lookup(ctrl, shift, alt, &ev.key) == Some("FocusLeftPane") {
                self.ai.focused = false;
            }
            return false;
        }
        if self.ai.term().is_none_or(|t| !t.alive) {
            // Empty state or a finished agent: Enter starts it again; anything else hands the
            // keyboard back to the editor (and still does what it does there).
            if matches!(&ev.key, Key::Named(winit::keyboard::NamedKey::Enter)) {
                self.ai_respawn();
                return true;
            }
            if matches!(&ev.key, Key::Named(n) if is_modifier(*n)) {
                return true;
            }
            self.ai.focused = false;
            return matches!(&ev.key, Key::Named(winit::keyboard::NamedKey::Escape));
        }
        if !matches!(&ev.key, Key::Named(n) if is_modifier(*n)) {
            self.ai.on_input();
        }
        if let Key::Character(c) = &ev.key
            && ctrl
            && !alt
            && c.eq_ignore_ascii_case("v")
        {
            if shift {
                let text = self.clip.get();
                self.ai_paste_text(&text);
            } else {
                self.ai_paste();
            }
            return true;
        }
        self.term_type(Which::Ai, ev)
    }

    /// `Ctrl+V` in the agent: files copied in a file manager paste as their paths, text as one
    /// bracketed paste; an image (with no text) leaves the key to the agent, which reads the
    /// clipboard itself (Claude Code and Codex attach it).
    pub fn ai_paste(&mut self) {
        let text = match self.clip.offer() {
            crate::clipboard::Offer::Files(f) => {
                let paths: Vec<String> = f.paths.iter().map(|p| paste_path(p)).collect();
                format!("{} ", paths.join(" "))
            }
            crate::clipboard::Offer::Text(t) => t,
            crate::clipboard::Offer::Image | crate::clipboard::Offer::Empty => {
                if let Some(t) = self.ai.term_mut() {
                    t.sel = None;
                    t.write(&[0x16]);
                }
                return;
            }
        };
        self.ai_paste_text(&text);
    }

    /// Paste `text` into the agent whole, as one paste (see [`paste_payload`]).
    pub fn ai_paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some(t) = self.ai.term_mut() else { return };
        let bytes = paste_payload(text, t.grid.bracketed_paste);
        t.sel = None;
        t.write(&bytes);
    }

    /// A press anywhere: the panel keeps the keyboard only for clicks inside it (handled by
    /// [`aipanel_mouse`](Self::aipanel_mouse)).
    pub fn aipanel_mouse_pre(&mut self) {
        self.ai.focused = false;
    }

    /// Mouse press on the switch, the panel's edge, its buttons or the agent's terminal.
    pub fn aipanel_mouse(&mut self, x: f64, y: f64, button: u8) -> bool {
        if self.gitpanel.is_none() && !self.ai.open {
            return false;
        }
        if button == 0 && self.ai_on_edge(x, y) {
            self.ai.resizing = true;
            self.ai.focused = true;
            return true;
        }
        let slot = self.right_slot_rect();
        let hit = self.ai.hit_at(x, y).map(|(_, h)| h);
        let in_strip = Rect::new(slot.x, slot.y, slot.w, SLOT_H).contains(x, y);
        if !self.ai.open {
            // The Git panel is shown: only its switch is ours.
            if hit == Some(AiHit::TabAi) {
                self.ai_show();
            }
            return in_strip;
        }
        let inside = slot.contains(x, y);
        if !inside {
            return false;
        }
        self.ai.focused = true;
        self.focus = Focus::Editor;
        match hit {
            Some(AiHit::TabGit) => {
                self.ai_hide();
                self.open_git_panel();
            }
            Some(AiHit::TabAi) | Some(AiHit::Status) => {}
            Some(AiHit::Close) => self.ai_hide(),
            Some(AiHit::Agent) => self.ai_choose_agent(),
            Some(AiHit::New) => self.ai_new_session_menu(),
            Some(AiHit::Restart) => self.ai_respawn(),
            Some(AiHit::Session(i)) => self.ai_tab_click(i, button),
            Some(AiHit::CloseSession(i)) => self.ai_close_session(i),
            Some(AiHit::Stop) => self.ai_stop(),
            Some(AiHit::Refresh) => {
                self.ai_detect(true);
                match self.ai_default_agent() {
                    Some(a) if self.ai.path_of(a).is_some() => self.ai_start(a),
                    _ => self.say("no agent CLI found on PATH"),
                }
            }
            Some(AiHit::Diffs) => self.ide_show_next(),
            Some(AiHit::Ide) => {
                if !self.ai.bridge.proposals.is_empty() {
                    self.ide_show_next();
                } else if self.ai_bridge_connected() {
                    self.say("Claude Code is connected: its edits open here as diffs, and it sees your selection");
                } else {
                    self.say("Not connected yet — Claude Code connects once it starts (or run /ide in it)");
                }
            }
            Some(AiHit::Install(i)) => {
                if let Some(a) = AGENTS.get(i) {
                    self.ai_pick(a.id);
                }
            }
            None => {
                let alive = self.ai.term().is_some_and(|t| t.alive);
                if alive && y >= (self.aipanel_rect().y + self.ai_top_h()) as f64 {
                    self.term_press(Which::Ai, x, y, button);
                }
            }
        }
        true
    }

    /// A click on session tab `i`: middle-click closes it, a second click soon after renames
    /// it, otherwise it is shown.
    fn ai_tab_click(&mut self, i: usize, button: u8) {
        let Some(id) = self.ai.sessions.get(i).map(|s| s.id) else { return };
        if button == 1 {
            return self.ai_close_session(i);
        }
        if button != 0 {
            return;
        }
        let now = Instant::now();
        let double =
            self.ai.last_tab_click.is_some_and(|(last, t)| last == id && t.elapsed().as_millis() < DOUBLE_CLICK_MS);
        self.ai.last_tab_click = Some((id, now));
        self.ai_activate(i);
        if double {
            self.ai.last_tab_click = None;
            self.ai_rename_prompt();
        }
    }

    /// Wheel over the agent's terminal.
    pub fn aipanel_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        if !self.ai.open || !self.aipanel_rect().contains(x, y) {
            return false;
        }
        if self.ai.term().is_some() {
            self.term_wheel(Which::Ai, delta);
        }
        true
    }

    /// Pointer moved: resizing by the left edge; true when the layout changed.
    pub fn aipanel_move(&mut self) -> bool {
        if !self.ai.resizing {
            return false;
        }
        let win = self.size.0 as i32;
        let w = crate::app_gitpanel::clamp_panel_width(win - self.mouse.0 as i32, win);
        if self.config.git_panel_width != w {
            self.config.git_panel_width = w;
            return true;
        }
        false
    }

    /// Mouse released: an edge drag ends and the width is stored (shared with the Git panel).
    pub fn aipanel_release(&mut self) {
        if std::mem::take(&mut self.ai.resizing) {
            self.config.save();
            self.request_redraw();
        }
    }

    pub fn aipanel_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        if self.ai.resizing || self.ai_on_edge(x, y) {
            return Some(CursorIcon::ColResize);
        }
        if self.gitpanel.is_none() && !self.ai.open {
            return None;
        }
        if self.ai.hit_at(x, y).is_some() {
            return Some(CursorIcon::Pointer);
        }
        if !self.ai.open || !self.right_slot_rect().contains(x, y) {
            return None;
        }
        let body = y >= (self.aipanel_rect().y + self.ai_top_h()) as f64 && self.ai.term().is_some();
        Some(if body { CursorIcon::Text } else { CursorIcon::Default })
    }

    /// Hover tint target (the app draws it): buttons, the switch's inactive tab.
    pub fn aipanel_hover_target(&self) -> Option<(Rect, i32)> {
        let (x, y) = self.mouse;
        let (r, h) = self.ai.hit_at(x, y)?;
        let active_tab = (h == AiHit::TabAi && self.ai.open)
            || (h == AiHit::TabGit && !self.ai.open)
            || h == AiHit::Session(self.ai.active);
        let radius = match h {
            AiHit::CloseSession(_) => 4,
            AiHit::Session(_) => 7,
            _ => 8,
        };
        (!active_tab && !matches!(h, AiHit::Status)).then_some((r, radius))
    }

    /// The button under the pointer, for a tooltip.
    pub fn aipanel_tip_target(&self, x: f64, y: f64) -> Option<(Rect, AiHit)> {
        self.ai.hit_at(x, y).filter(|(_, h)| !matches!(h, AiHit::Install(_)))
    }

    pub fn aipanel_tip(&self, h: AiHit) -> TipText {
        let keys = |id: &str| crate::app_tooltip::shortcut_in(&self.keymap.entries(), id);
        let name = self.ai.agent().map_or("the agent", |a| a.name);
        let status = self.ai.cur().map_or(Status::Starting, Session::status);
        let (title, keys, detail): (String, Option<String>, Option<String>) = match h {
            AiHit::TabGit => ("Git".into(), keys("GitPanel"), Some("Changes and history".into())),
            AiHit::TabAi => (
                "AI".into(),
                keys("AiPanel"),
                Some(match self.ai.badge() {
                    Some((b, _)) => b,
                    None => format!("{} — {}", self.ai.agent().map_or("Agent", |a| a.name), status.label()),
                }),
            ),
            AiHit::Agent => ("Choose agent…".into(), None, Some("Claude Code, Gemini CLI or Codex CLI".into())),
            AiHit::Status => {
                let title = self.ai.term().map(|t| t.grid.title.clone()).unwrap_or_default();
                (status.label(), None, (!title.is_empty()).then_some(title))
            }
            AiHit::New => ("New session…".into(), keys("AiNewSession"), Some("Start another agent alongside".into())),
            AiHit::Session(i) => match self.ai.sessions.get(i) {
                Some(s) => (
                    format!("{} — {}", s.name, s.status().label()),
                    None,
                    Some(format!("{} · {}", s.agent.name, crate::app_topbar::tilde(&s.cwd))),
                ),
                None => (String::new(), None, None),
            },
            AiHit::CloseSession(_) => (
                "Close session".into(),
                keys("AiCloseSession"),
                Some("Ends the agent and everything it started".into()),
            ),
            AiHit::Restart => {
                ("Restart agent".into(), None, Some(format!("Relaunch {name} (picks up login or config changes)")))
            }
            AiHit::Stop => ("Stop".into(), None, Some(format!("Hang up {name} and everything it started"))),
            AiHit::Close => ("Hide panel".into(), keys("AiPanel"), Some("The agent keeps running".into())),
            AiHit::Refresh => ("Look for agents again".into(), None, None),
            AiHit::Ide if self.ai_bridge_connected() => (
                "IDE bridge — connected".into(),
                None,
                Some("Edits open as diffs here · the selection and diagnostics are shared".into()),
            ),
            AiHit::Ide => (
                "IDE bridge — not connected".into(),
                None,
                Some("Claude Code connects when it starts here, or with /ide".into()),
            ),
            AiHit::Diffs => {
                let n = self.ai.bridge.proposals.len();
                (
                    format!("{n} proposed change{} waiting", if n == 1 { "" } else { "s" }),
                    None,
                    Some("Click to review · Ctrl+Enter accepts · Esc rejects".into()),
                )
            }
            AiHit::Install(_) => (String::new(), None, None),
        };
        let more = match h {
            AiHit::Session(_) => vec!["Double-click to rename · middle-click to close".to_string()],
            _ => Vec::new(),
        };
        TipText { title, keys, detail, more }
    }
}

/// Keys that only change modifier state.
fn is_modifier(n: winit::keyboard::NamedKey) -> bool {
    use winit::keyboard::NamedKey as N;
    matches!(n, N::Control | N::Shift | N::Alt | N::AltGraph | N::Super | N::Meta | N::Hyper | N::CapsLock | N::NumLock)
}
