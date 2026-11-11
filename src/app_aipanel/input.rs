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
        if self.ai.term.as_ref().is_none_or(|t| !t.alive) {
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
        self.term_type(Which::Ai, ev)
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
            Some(AiHit::New) | Some(AiHit::Restart) => self.ai_respawn(),
            Some(AiHit::Stop) => self.ai_stop(),
            Some(AiHit::Refresh) => {
                self.ai_detect(true);
                match self.ai_default_agent() {
                    Some(a) if self.ai.path_of(a).is_some() => self.ai_start(a),
                    _ => self.say("no agent CLI found on PATH"),
                }
            }
            Some(AiHit::Install(i)) => {
                if let Some(a) = AGENTS.get(i) {
                    self.ai_pick(a.id);
                }
            }
            None => {
                let alive = self.ai.term.as_ref().is_some_and(|t| t.alive);
                if alive && y >= (self.aipanel_rect().y + HEAD_H) as f64 {
                    self.term_press(Which::Ai, x, y, button);
                }
            }
        }
        true
    }

    /// Wheel over the agent's terminal.
    pub fn aipanel_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        if !self.ai.open || !self.aipanel_rect().contains(x, y) {
            return false;
        }
        if self.ai.term.is_some() {
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
        let body = y >= (self.aipanel_rect().y + HEAD_H) as f64 && self.ai.term.is_some();
        Some(if body { CursorIcon::Text } else { CursorIcon::Default })
    }

    /// Hover tint target (the app draws it): buttons, the switch's inactive tab.
    pub fn aipanel_hover_target(&self) -> Option<(Rect, i32)> {
        let (x, y) = self.mouse;
        let (r, h) = self.ai.hit_at(x, y)?;
        let active_tab = (h == AiHit::TabAi && self.ai.open) || (h == AiHit::TabGit && !self.ai.open);
        (!active_tab && !matches!(h, AiHit::Status)).then_some((r, 8))
    }

    /// The button under the pointer, for a tooltip.
    pub fn aipanel_tip_target(&self, x: f64, y: f64) -> Option<(Rect, AiHit)> {
        self.ai.hit_at(x, y).filter(|(_, h)| !matches!(h, AiHit::Install(_)))
    }

    pub fn aipanel_tip(&self, h: AiHit) -> TipText {
        let keys = |id: &str| crate::app_tooltip::shortcut_in(&self.keymap.entries(), id);
        let name = self.ai.agent.map_or("the agent", |a| a.name);
        let (title, keys, detail): (String, Option<String>, Option<String>) = match h {
            AiHit::TabGit => ("Git".into(), keys("GitPanel"), Some("Changes and history".into())),
            AiHit::TabAi => (
                "AI".into(),
                keys("AiPanel"),
                Some(format!("{} — {}", self.ai.agent.map_or("Agent", |a| a.name), self.ai.status.label())),
            ),
            AiHit::Agent => ("Choose agent…".into(), None, Some("Claude Code, Gemini CLI or Codex CLI".into())),
            AiHit::Status => {
                let title = self.ai.term.as_ref().map(|t| t.grid.title.clone()).unwrap_or_default();
                (self.ai.status.label(), None, (!title.is_empty()).then_some(title))
            }
            AiHit::New => ("New session".into(), None, Some(format!("End this conversation and start {name} again"))),
            AiHit::Restart => {
                ("Restart agent".into(), None, Some(format!("Relaunch {name} (picks up login or config changes)")))
            }
            AiHit::Stop => ("Stop".into(), None, Some(format!("Hang up {name} and everything it started"))),
            AiHit::Close => ("Hide panel".into(), keys("AiPanel"), Some("The agent keeps running".into())),
            AiHit::Refresh => ("Look for agents again".into(), None, None),
            AiHit::Install(_) => (String::new(), None, None),
        };
        TipText { title, keys, detail, more: Vec::new() }
    }
}

/// Keys that only change modifier state.
fn is_modifier(n: winit::keyboard::NamedKey) -> bool {
    use winit::keyboard::NamedKey as N;
    matches!(n, N::Control | N::Shift | N::Alt | N::AltGraph | N::Super | N::Meta | N::Hyper | N::CapsLock | N::NumLock)
}
