//! Bottom output panel: runs tasks/tests and shows their colored output with clickable links.

use crate::{
    app::{App, Focus},
    app_overlay::{Action, Item},
    scrollreg::Reg,
    tasks::{self, Ansi, Event, Running, Segs},
    theme::Rgba,
    ui::{Canvas, Rect, line_h},
};
use std::time::Instant;
use winit::event::MouseScrollDelta;

pub const PANEL_HEADER: i32 = 30;
const MAX_LINES: usize = 6000;

/// Commands whose shortcuts keep working while the terminal has the focus (toggle the panel or
/// the terminal, sidebar, palette, project search, explorer, Git panel, quit). Everything else,
/// including plain `Ctrl+<letter>` keys shells rely on (Ctrl+C/D/R/L/A/E/W/U/K/P...), goes to the
/// shell unless the user binds it to one of these.
const TERM_GLOBAL: &[&str] =
    &["ToggleOutput", "OpenTerminal", "ToggleSidebar", "Quit", "Commands", "ProjectSearch", "FocusSidebar", "GitPanel"];

/// Is this key, as bound in the keymap (user file first, then the defaults), one the editor keeps
/// while the terminal is focused?
pub fn term_global(
    keymap: &crate::keymap::Keymap,
    ctrl: bool,
    shift: bool,
    alt: bool,
    key: &winit::keyboard::Key,
) -> bool {
    keymap.lookup(ctrl, shift, alt, key).is_some_and(|id| TERM_GLOBAL.contains(&id))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelTab {
    Output,
    Terminal,
    Debug,
    Tests,
    Build,
}

pub struct Panel {
    pub tab: PanelTab,
    pub open: bool,
    pub height: i32,
    pub title: String,
    pub lines: Vec<Segs>,
    pub plain: Vec<String>,
    pub scroll: f64, // px from the top of the content
    pub follow: bool,
    /// Debug tab: scroll (px) of the Stack / Variables / Output columns; Output sticks to the newest line.
    pub dbg_scroll: [f64; 3],
    pub dbg_follow: bool,
    pub running: Option<Running>,
    pub ansi: Ansi,
    pub cmd: Option<String>,
    pub exit: Option<i32>,
    pub started: Instant,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            tab: PanelTab::Output,
            open: false,
            height: 260,
            title: String::new(),
            lines: Vec::new(),
            plain: Vec::new(),
            scroll: 0.0,
            follow: true,
            dbg_scroll: [0.0; 3],
            dbg_follow: true,
            running: None,
            ansi: Ansi::new(),
            cmd: None,
            exit: None,
            started: Instant::now(),
        }
    }

    pub(crate) fn max_scroll(&self, view_h: i32) -> f64 {
        (self.lines.len() as f64 * line_h() as f64 + 8.0 - view_h as f64).max(0.0)
    }
}

impl App {
    pub fn toggle_panel(&mut self) {
        self.panel.open = !self.panel.open;
        if !self.panel.open && self.focus == Focus::Terminal {
            self.focus = Focus::Editor;
        }
    }

    /// Ctrl+`: open the terminal; with it already focused, hand focus back to the editor.
    pub fn toggle_terminal_focus(&mut self) {
        if self.panel.open && self.panel.tab == PanelTab::Terminal && self.focus == Focus::Terminal {
            self.focus = Focus::Editor;
            return;
        }
        self.open_terminal();
    }

    pub fn open_terminal(&mut self) {
        if self.term.is_none() {
            let (rows, cols) = (
                (self.panel.height - PANEL_HEADER) / line_h() as i32,
                (self.size.0 as i32 - 40) / self.tx.cell_w as i32,
            );
            match crate::terminal::Term::spawn(&self.root, rows.max(4) as usize, cols.max(20) as usize, self.wake()) {
                Ok(t) => self.term = Some(t),
                Err(e) => return self.say(format!("could not open the terminal: {e}")),
            }
        }
        self.panel.open = true;
        self.panel.tab = PanelTab::Terminal;
        self.focus = Focus::Terminal;
    }

    pub fn close_terminal(&mut self) {
        self.term = None;
        if self.focus == Focus::Terminal {
            self.focus = Focus::Editor;
        }
        self.panel.tab = PanelTab::Output;
    }

    pub fn pump_term(&mut self) {
        let Some(t) = self.term.as_mut() else { return };
        let changed = t.pump();
        if !t.alive {
            self.close_terminal();
            self.say("terminal closed");
        }
        self.term_effects(crate::app_termio::Which::Bottom);
        if changed {
            self.request_redraw();
        }
    }

    /// Keys while the terminal is focused. Returns true when consumed.
    pub fn term_key(&mut self, ev: &crate::app::KeyIn) -> bool {
        use winit::keyboard::Key;
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let alt = self.mods.alt_key();
        if !self.panel.open || self.panel.tab != PanelTab::Terminal || self.term.is_none() {
            self.focus = Focus::Editor;
            return false;
        }
        // Keys the editor keeps even while the terminal has focus (as bound in the keymap);
        // Ctrl+Shift+C / Ctrl+Shift+V copy and paste.
        let clip_key =
            matches!(&ev.key, Key::Character(c) if ctrl && shift && matches!(c.to_lowercase().as_str(), "c" | "v"));
        if !clip_key && term_global(&self.keymap, ctrl, shift, alt, &ev.key) {
            return false;
        }
        self.term_type(crate::app_termio::Which::Bottom, ev)
    }

    pub fn run_task(&mut self, cmd: &str, title: &str) {
        self.stop_task();
        let wake = self.wake();
        match tasks::spawn(cmd, &self.root, wake) {
            Ok(r) => {
                let p = &mut self.panel;
                p.running = Some(r);
                p.open = true;
                p.title = title.to_string();
                p.lines.clear();
                p.plain.clear();
                p.scroll = 0.0;
                p.follow = true;
                p.exit = None;
                p.ansi = Ansi::new();
                p.cmd = Some(cmd.to_string());
                p.started = Instant::now();
                self.push_line(&format!("$ {cmd}"));
            }
            Err(e) => self.say(format!("could not run: {e}")),
        }
    }

    pub fn stop_task(&mut self) {
        if let Some(mut r) = self.panel.running.take() {
            tasks::kill_group(r.pid);
            let _ = r.child.wait();
            self.panel.exit = Some(-1);
            self.push_line("■ interrompido");
        }
    }

    pub fn repeat_task(&mut self) {
        match self.panel.cmd.clone() {
            Some(c) => {
                let t = self.panel.title.clone();
                self.run_task(&c, &t)
            }
            None => self.say("no task has been run yet"),
        }
    }

    fn push_line(&mut self, raw: &str) {
        let (fg, dim) = (self.theme.foreground, self.theme.chrome.tab_inactive_fg);
        let segs = self.panel.ansi.line(raw, fg, dim);
        let plain: String = segs.iter().map(|(s, _)| s.as_str()).collect();
        let p = &mut self.panel;
        p.lines.push(segs);
        p.plain.push(plain);
        if p.lines.len() > MAX_LINES {
            p.lines.remove(0);
            p.plain.remove(0);
        }
    }

    /// Drain task output; called whenever the event loop is woken.
    pub fn pump_task(&mut self) {
        let mut done = false;
        let mut lines: Vec<String> = Vec::new();
        if let Some(r) = &self.panel.running {
            while let Ok(ev) = r.rx.try_recv() {
                match ev {
                    Event::Line(l) => lines.push(l),
                    Event::Done => done = true,
                }
            }
        }
        for l in lines {
            self.push_line(&l);
        }
        if done {
            if let Some(prog) = self.pending_native.take() {
                let ok = self
                    .panel
                    .running
                    .as_mut()
                    .and_then(|r| r.child.try_wait().ok().flatten())
                    .is_none_or(|s| s.success());
                if ok {
                    self.start_native_debug(prog);
                }
            }
            if let Some(mut r) = self.panel.running.take() {
                let code = r.child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                let secs = self.panel.started.elapsed().as_secs_f32();
                self.panel.exit = Some(code);
                let mark = if code == 0 { "\u{f00c}" } else { "\u{f00d}" };
                self.push_line(&format!("{mark} exited with code {code} in {secs:.1}s"));
            }
        }
        let actual = if self.layout.panel.h > 0 { self.layout.panel.h } else { self.panel.height };
        let view = actual - PANEL_HEADER;
        if self.panel.follow {
            self.panel.scroll = self.panel.max_scroll(view);
        }
    }

    pub fn open_task_picker(&mut self) {
        let items: Vec<Item> = tasks::project_tasks(&self.root)
            .into_iter()
            .map(|(name, cmd)| Item { label: name, detail: cmd.clone(), action: Action::Run(cmd) })
            .collect();
        if items.is_empty() {
            return self.say("no package.json/Cargo.toml with tasks in this project");
        }
        self.open_locations("Run task", items);
    }

    /// Run tests for the current file, optionally only the test under the cursor. Both land in the
    /// Tests panel (tree, gutter marks, failure details).
    pub fn run_tests(&mut self, at_cursor: bool) {
        self.tests_run_file(at_cursor);
    }

    // ───────────── input ─────────────

    pub fn panel_mouse(&mut self, x: f64, y: f64) -> bool {
        let r = self.layout.panel;
        if !self.panel.open || !r.contains(x, y) {
            return false;
        }
        if y < (r.y + PANEL_HEADER) as f64 {
            let hit = self.layout.panel_tabs.iter().find(|(rr, _)| rr.contains(x, y)).map(|(_, t)| *t);
            match hit {
                Some(PanelTab::Debug) => {
                    self.panel.tab = PanelTab::Debug;
                    self.focus = Focus::Editor;
                }
                Some(PanelTab::Tests) => {
                    self.tests_open_panel();
                    self.tests_focus();
                }
                Some(PanelTab::Build) => {
                    self.build_open_panel();
                    self.build_focus();
                }
                Some(PanelTab::Terminal) => self.open_terminal(),
                Some(PanelTab::Output) => {
                    self.panel.tab = PanelTab::Output;
                    self.focus = Focus::Editor;
                }
                None => {}
            }
            return true;
        }
        if self.panel.tab == PanelTab::Debug {
            self.debug_click(x, y);
            return true;
        }
        if self.panel.tab == PanelTab::Tests {
            self.focus = Focus::Editor;
            self.tests_click(x, y);
            return true;
        }
        if self.panel.tab == PanelTab::Build {
            self.focus = Focus::Editor;
            self.build_click(x, y);
            return true;
        }
        if self.panel.tab == PanelTab::Terminal {
            if self.term.is_some() {
                self.focus = Focus::Terminal;
                self.term_press(crate::app_termio::Which::Bottom, x, y, 0);
            }
            return true;
        }
        self.focus = Focus::Editor;
        let rel = y - (r.y + PANEL_HEADER) as f64 + self.panel.scroll;
        let idx = (rel / line_h() as f64) as usize;
        let Some(text) = self.panel.plain.get(idx).cloned() else { return true };
        let col = (((x - (r.x + 10) as f64) / self.tx.cell_w as f64).floor().max(0.0)) as usize;
        let links = tasks::find_links(&text);
        let hit = links.iter().find(|l| col >= l.start && col < l.end).or_else(|| links.first()).cloned();
        if let Some(l) = hit {
            match tasks::resolve_link_path(&self.root, &l.path) {
                Some(p) => {
                    self.open_path(&p, Some((l.line, l.col)));
                }
                None => self.say(format!("file not found: {}", l.path)),
            }
        }
        true
    }

    pub fn panel_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let r = self.layout.panel;
        if !self.panel.open || !r.contains(x, y) {
            return false;
        }
        if self.panel.tab == PanelTab::Terminal {
            self.term_wheel(crate::app_termio::Which::Bottom, delta);
            return true;
        }
        let view = r.h - PANEL_HEADER;
        let max = self.panel.max_scroll(view);
        self.panel_wheel_apply(delta, max);
        true
    }

    // ───────────── drawing ─────────────

    pub fn draw_panel(&mut self, cv: &mut Canvas) {
        let r = self.layout.panel;
        self.layout.panel_tabs.clear();
        if !self.panel.open || r.h <= 0 {
            return;
        }
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        // Flat panel: hairline on top, quiet header strip with the two tabs.
        cv.fill(r, self.theme.background);
        cv.fill(Rect::new(r.x, r.y, r.w, PANEL_HEADER), c.sidebar_bg);
        cv.fill(Rect::new(r.x, r.y, r.w, 1), c.border);
        cv.fill(Rect::new(r.x, r.y + PANEL_HEADER, r.w, 1), c.border);
        let body = Rect::new(r.x, r.y + PANEL_HEADER + 1, r.w, (r.h - PANEL_HEADER - 1).max(0));
        let head = Rect::new(r.x, r.y + 1, r.w, PANEL_HEADER - 1);
        let ty = r.y + (PANEL_HEADER - 18) / 2 + 1;
        // Pill tabs, like the git panel's: the active one tinted, the others dim (hover tint is the app's).
        let mut x = r.x + 8;
        for (label, tab) in [
            ("Output", PanelTab::Output),
            ("Tests", PanelTab::Tests),
            ("Build", PanelTab::Build),
            ("Terminal", PanelTab::Terminal),
            ("Debug", PanelTab::Debug),
        ] {
            let w = self.tx.ui_width(label, true) + 20;
            let active = self.panel.tab == tab;
            let pill = Rect::new(x, r.y + 4, w, PANEL_HEADER - 7);
            if active {
                cv.fill_round(pill, 8, c.tab_active_fg.with_alpha(0x1e));
            }
            let tw = self.tx.ui_width(label, active);
            self.tx.draw_ui(cv, x + (w - tw) / 2, ty, label, if active { c.tab_active_fg } else { dim }, head, active);
            self.layout.panel_tabs.push((pill, tab));
            x += w + 4;
        }
        // The panel toggle as bound now (a rebinding shows its own combo; switched off: no hint).
        if let Some(combo) = crate::app_tooltip::shortcut_in(&self.keymap.entries(), "ToggleOutput") {
            let hint = format!("{combo} closes");
            let hw = self.tx.ui_width(&hint, false);
            self.tx.draw_ui(cv, r.right() - hw - 14, ty, &hint, dim, head, false);
        }

        if self.panel.tab == PanelTab::Terminal {
            return self.draw_terminal(cv, body);
        }
        if self.panel.tab == PanelTab::Debug {
            return self.draw_debug(cv, body);
        }
        if self.panel.tab == PanelTab::Tests {
            return self.draw_tests(cv, body);
        }
        if self.panel.tab == PanelTab::Build {
            return self.draw_build(cv, body);
        }

        let state = match (&self.panel.running, self.panel.exit) {
            (Some(_), _) => ("running", c.accent_text),
            (None, Some(0)) => ("done", c.git_added),
            (None, Some(_)) => ("failed", c.error),
            _ => ("", dim),
        };
        self.tx.draw_ui(cv, x + 4, ty, &self.panel.title, fg, head, false);
        let tx_ = x + 4 + self.tx.ui_width(&self.panel.title, false) + 14;
        self.tx.draw_ui(cv, tx_, ty, state.0, state.1, head, true);

        let first = (self.panel.scroll / line_h() as f64).floor().max(0.0) as usize;
        let off = -((self.panel.scroll as i32) % line_h() as i32);
        let rows = (body.h / line_h() as i32 + 2) as usize;
        let mut pieces: Vec<(String, Rgba)> = Vec::new();
        let max_cols = ((body.w - 24) as f32 / self.tx.cell_w) as usize + 2;
        for (k, segs) in self.panel.lines.iter().enumerate().skip(first).take(rows) {
            let mut used = 0;
            for (s, col) in segs {
                if used >= max_cols {
                    break;
                }
                let take: String = s.chars().take(max_cols - used).collect();
                used += take.chars().count();
                pieces.push((take, tasks::legible(*col, self.theme.background)));
            }
            pieces.push(("\n".to_string(), fg));
            // Underline links.
            let plain = &self.panel.plain[k];
            if plain.contains(':') || plain.contains('(') {
                for l in tasks::find_links(plain) {
                    let y = body.y + 4 + off + ((k - first) as i32) * line_h() as i32 + line_h() as i32 - 3;
                    let x0 = body.x + 12 + self.tx.cells(l.start);
                    let x1 = body.x + 12 + self.tx.cells(l.end.min(max_cols));
                    cv.fill_clip(Rect::new(x0, y, (x1 - x0).max(0), 1), c.accent, body);
                }
            }
        }
        self.tx.draw(cv, body.x + 12, body.y + 4 + off, &pieces, body);
        let (total, visible) = (self.panel.lines.len() as f64 * line_h() as f64 + 8.0, body.h as f64);
        self.panel.scroll = self.panel.scroll.clamp(0.0, (total - visible).max(0.0));
        let pos = self.panel.scroll;
        self.region_bar(cv, Reg::Output, body, total, visible, pos, 1.0, false);
    }

    fn draw_terminal(&mut self, cv: &mut Canvas, body: Rect) {
        let lh = line_h() as i32;
        let (rows, cols) = crate::termview::fit(self.tx.cell_w, body.w - 24, body.h - 8);
        let focused = self.focus == Focus::Terminal;
        let (fg, bg) = (self.theme.foreground, self.theme.background);
        let (ox, oy) = (body.x + 12, body.y + 4);
        let look = self.term_look(crate::app_termio::Which::Bottom, focused, (ox, oy));
        let Some(term) = self.term.as_mut() else {
            self.tx.draw_ui(
                cv,
                body.x + 14,
                body.y + 10,
                "Terminal closed — Ctrl+` opens a new one",
                self.theme.chrome.tab_inactive_fg,
                body,
                false,
            );
            return;
        };
        term.resize(rows, cols);
        term.grid.colors = (fg, bg);
        term.set_focus(focused);
        crate::termview::draw(&mut self.tx, cv, term, ox, oy, body, &look);
        let alt = term.alt_screen();
        let (hist, off) = (term.grid.history_len(), term.scroll_offset);
        if !alt {
            // Rows are the unit: the bar counts from the oldest line, the live screen is the bottom.
            self.region_bar(
                cv,
                Reg::Terminal,
                body,
                (hist + rows) as f64,
                rows as f64,
                (hist - off.min(hist)) as f64,
                lh as f64,
                true,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Keymap;
    use winit::keyboard::{Key, NamedKey};

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    #[test]
    fn terminal_hands_global_shortcuts_back_and_keeps_shell_keys() {
        let km = Keymap::default();
        // Ctrl+J closes the panel, also with Caps Lock ("J") or Shift held (ToggleOutput is shift tolerant).
        assert!(term_global(&km, true, false, false, &ch("j")));
        assert!(term_global(&km, true, false, false, &ch("J")));
        assert!(term_global(&km, true, true, false, &ch("J")));
        for (shift, k) in [(false, "`"), (false, "b"), (false, "q"), (true, "P"), (true, "F"), (true, "E"), (true, "G")]
        {
            assert!(term_global(&km, true, shift, false, &ch(k)), "ctrl+{k} (shift: {shift})");
        }
        // Readline / job control keys reach the shell.
        for k in ["c", "d", "r", "l", "a", "e", "w", "u", "k", "p", "z", "f", "h", "t", "g"] {
            assert!(!term_global(&km, true, false, false, &ch(k)), "ctrl+{k} must reach the shell");
        }
        assert!(!term_global(&km, false, false, false, &ch("j")), "plain typing");
        assert!(!term_global(&km, false, false, true, &Key::Named(NamedKey::ArrowLeft)), "alt+left is word-back");
        assert!(!term_global(&km, true, true, false, &ch("V")), "ctrl+shift+v pastes into the terminal");
    }

    #[test]
    fn terminal_follows_user_rebinding() {
        let km = Keymap::from_json(r#"{"ctrl+alt+o": "ToggleOutput", "ctrl+j": "none", "ctrl+r": "Commands"}"#);
        assert!(term_global(&km, true, false, true, &ch("o")));
        assert!(!term_global(&km, true, false, false, &ch("j")), "disabled: ctrl+j goes to the shell");
        assert!(term_global(&km, true, false, false, &ch("r")), "the user's choice wins over the shell");
    }
}
