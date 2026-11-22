//! Bottom output panel: runs tasks/tests and shows their colored output with clickable links.

use crate::{
    app::{App, Focus},
    app_overlay::{Action, Item},
    ds,
    scrollreg::Reg,
    tasks::{self, Ansi, Event, Running, Segs},
    theme::Rgba,
    ui::{Canvas, Rect, line_h},
};
use std::time::Instant;
use winit::event::MouseScrollDelta;

pub const PANEL_HEADER: i32 = 30;
/// Half-width (px) of the grab band around the panel's top edge.
pub const EDGE_GRAB: i32 = 4;
/// Shortest height the panel can be dragged to, and the height it starts with.
pub const PANEL_MIN_H: i32 = 120;
pub const PANEL_H: i32 = 260;
const MAX_LINES: usize = 6000;

/// Commands whose shortcuts keep working while the terminal has the focus (toggle the panel or
/// the terminal, sidebar, palette, project search, explorer, Git panel, quit). Everything else,
/// including plain `Ctrl+<letter>` keys shells rely on (Ctrl+C/D/R/L/A/E/W/U/K/P...), goes to the
/// shell unless the user binds it to one of these.
const TERM_GLOBAL: &[&str] = &[
    "ToggleOutput",
    "OpenTerminal",
    "NewTerminal",
    "ToggleSidebar",
    "Quit",
    "Commands",
    "ProjectSearch",
    "FocusSidebar",
    "GitPanel",
];

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
    Problems,
    Terminal,
    Debug,
    Tests,
    Build,
    Docker,
}

/// Resize state of the panel's top edge (the cursors agent reads `bottom_panel_on_edge` / `bottom_panel_resizing`).
#[derive(Default)]
pub struct PanelEdge {
    /// Dragging: pointer offset from the edge at the press.
    pub grab: Option<f64>,
    /// Pointer over the edge (for the highlight).
    pub hot: bool,
    /// Last press on the edge (double click resets the height).
    pub last_press: Option<Instant>,
}

/// File that remembers the panel height between runs (next to `config.json`).
fn height_file() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(crate::paths::config_home()?.join("lumen-text").join("panel.json"))
}

/// The remembered panel height, clamped to a sane range.
fn load_height() -> i32 {
    let saved = height_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["height"].as_i64());
    clamp_height(saved.unwrap_or(PANEL_H as i64) as i32, 4000)
}

fn save_height(h: i32) {
    if let Some(p) = height_file() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, serde_json::json!({ "height": h }).to_string());
    }
}

/// The panel height for a dragged edge, between the minimum and `max`.
pub fn clamp_height(h: i32, max: i32) -> i32 {
    h.clamp(PANEL_MIN_H, max.max(PANEL_MIN_H))
}

pub struct Panel {
    pub edge: PanelEdge,
    pub tab: PanelTab,
    pub open: bool,
    pub height: i32,
    pub title: String,
    pub lines: Vec<Segs>,
    pub plain: Vec<String>,
    pub scroll: f64, // px from the top of the content
    pub follow: bool,
    /// Debug tab: scroll (px) of the Stack / Variables / Output columns; Output sticks to the newest line.
    pub dbg_scroll: [f64; 5],
    pub dbg_follow: bool,
    pub running: Option<Running>,
    pub ansi: Ansi,
    pub cmd: Option<String>,
    pub exit: Option<i32>,
    pub started: Instant,
    /// Open / maximize springs, tab indicator and header buttons (see `app_panelui`).
    pub motion: crate::app_panelui::PanelMotion,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            edge: PanelEdge::default(),
            tab: PanelTab::Output,
            open: false,
            height: load_height(),
            title: String::new(),
            lines: Vec::new(),
            plain: Vec::new(),
            scroll: 0.0,
            follow: true,
            dbg_scroll: [0.0; 5],
            dbg_follow: true,
            running: None,
            ansi: Ansi::new(),
            cmd: None,
            exit: None,
            started: Instant::now(),
            motion: crate::app_panelui::PanelMotion::new(),
        }
    }

    pub(crate) fn max_scroll(&self, view_h: i32) -> f64 {
        (self.lines.len() as f64 * line_h() as f64 + 8.0 - view_h as f64).max(0.0)
    }
}

impl App {
    pub fn toggle_panel(&mut self) {
        self.panel.motion.arm = true;
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

    // Opening, closing and pumping the terminals: see `app_terms`.

    /// Keys while the terminal is focused. Returns true when consumed.
    pub fn term_key(&mut self, ev: &crate::app::KeyIn) -> bool {
        use winit::keyboard::Key;
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let alt = self.mods.alt_key();
        if !self.panel.open || self.panel.tab != PanelTab::Terminal || self.terms.get(self.terms.active).is_none() {
            self.focus = Focus::Editor;
            return false;
        }
        // Keys the editor keeps even while the terminal has focus (as bound in the keymap);
        // Ctrl+Shift+C / Ctrl+Shift+V copy and paste.
        let clip_key = matches!(&ev.key, Key::Character(c) if ctrl && shift && matches!(c.to_lowercase().as_str(), "c" | "v" | "f"));
        if !clip_key && term_global(&self.keymap, ctrl, shift, alt, &ev.key) {
            return false;
        }
        self.term_type(crate::app_termio::Which::Bottom(self.terms.active), ev)
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
        let mut items: Vec<Item> = tasks::project_tasks(&self.root)
            .into_iter()
            .map(|(name, cmd)| Item { label: name, detail: cmd.clone(), action: Action::Run(cmd) })
            .collect();
        // Multi-root workspace: the other folders' tasks run inside their own folder.
        items.extend(self.ws_task_items());
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

    // ───────────── resize edge ─────────────

    /// The grab band around the panel's top edge (None when the panel is closed).
    fn panel_edge_rect(&self) -> Option<Rect> {
        let r = self.layout.panel;
        (self.panel.open && r.h > 0 && !self.panel_animating())
            .then(|| Rect::new(r.x, r.y - EDGE_GRAB, r.w, 2 * EDGE_GRAB + 1))
    }

    /// Is the pointer on the resize edge? (row-resize cursor)
    pub fn bottom_panel_on_edge(&self, x: f64, y: f64) -> bool {
        self.overlay.is_none() && self.panel_edge_rect().is_some_and(|e| e.contains(x, y))
    }

    /// Is the edge being dragged?
    pub fn bottom_panel_resizing(&self) -> bool {
        self.panel.edge.grab.is_some()
    }

    /// Press on the edge starts a resize; a double click restores the default height.
    pub fn panel_edge_press(&mut self, x: f64, y: f64) -> bool {
        if !self.bottom_panel_on_edge(x, y) {
            return false;
        }
        let now = Instant::now();
        self.panel.motion.maximized = false;
        if self.panel.edge.last_press.take().is_some_and(|t| now.duration_since(t).as_millis() < 400) {
            self.panel.height = PANEL_H;
            save_height(PANEL_H);
            return true;
        }
        self.panel.edge.last_press = Some(now);
        self.panel.edge.grab = Some(y - self.layout.panel.y as f64);
        true
    }

    /// Pointer moved: follows an edge drag (true = consumed) and tracks the hover highlight.
    pub fn panel_edge_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        if let Some(grab) = self.panel.edge.grab {
            let bottom = self.layout.panel.bottom();
            let want = bottom - (y - grab).round() as i32;
            let h = clamp_height(want, self.size.1 as i32 / 2);
            if h != self.panel.height {
                self.panel.height = h;
                self.request_redraw();
            }
            return true;
        }
        let hot = self.bottom_panel_on_edge(x, y);
        if hot != self.panel.edge.hot {
            self.panel.edge.hot = hot;
            self.request_redraw();
        }
        false
    }

    /// Button released: ends an edge drag and remembers the height.
    pub fn panel_edge_release(&mut self) {
        if self.panel.edge.grab.take().is_some() {
            save_height(self.panel.height);
            self.request_redraw();
        }
    }

    /// Per frame (from `panel_motion_step`): true while the panel's own animations need frames.
    pub fn panel_extra_step(&mut self) -> bool {
        let loading = self.http_loading() && !crate::motion::reduced();
        self.problems_frame() | loading
    }

    pub fn panel_mouse(&mut self, x: f64, y: f64) -> bool {
        let r = self.layout.panel;
        if self.panel_edge_press(x, y) {
            return true;
        }
        if !self.panel.open || !r.contains(x, y) {
            return false;
        }
        if let Some((_, b)) = self.panel_btn_at(x, y) {
            self.panel_btn_click(b);
            return true;
        }
        if y < (r.y + PANEL_HEADER) as f64 {
            if self.terms_strip_click(x, y) {
                return true;
            }
            let hit = self.layout.panel_tabs.iter().find(|(rr, _)| rr.contains(x, y)).map(|(_, t)| *t);
            match hit {
                Some(PanelTab::Problems) => self.open_problems_tab(),
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
                Some(PanelTab::Docker) => self.docker_open_panel(),
                Some(PanelTab::Terminal) => self.open_terminal(),
                Some(PanelTab::Output) => {
                    self.panel.tab = PanelTab::Output;
                    self.focus = Focus::Editor;
                }
                None => {}
            }
            return true;
        }
        if self.panel.tab == PanelTab::Problems {
            self.focus = Focus::Editor;
            self.problems_tab_click(x, y);
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
        if self.panel.tab == PanelTab::Docker {
            self.focus = Focus::Editor;
            self.docker_click(x, y);
            return true;
        }
        if self.panel.tab == PanelTab::Terminal {
            self.terms_press(x, y);
            return true;
        }
        self.focus = Focus::Editor;
        let strip = self.http_strip_h();
        if strip > 0 && y < (r.y + PANEL_HEADER + 1 + strip) as f64 {
            self.http_strip_click(x, y);
            return true;
        }
        let rel = y - (r.y + PANEL_HEADER + strip) as f64 + self.panel.scroll;
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
            if let Some(id) = self.term_pane_at(x, y) {
                self.term_wheel(crate::app_termio::Which::Bottom(id), delta);
            }
            return true;
        }
        if self.panel.tab == PanelTab::Docker {
            self.docker_wheel(delta);
            return true;
        }
        if self.panel.tab == PanelTab::Problems {
            self.problems_tab_wheel(delta);
            return true;
        }
        let view = r.h - PANEL_HEADER - self.http_strip_h();
        let max = self.panel.max_scroll(view);
        self.panel_wheel_apply(delta, max);
        true
    }

    // ───────────── drawing ─────────────

    /// The top edge lights up in the accent color under the pointer and while dragging.
    fn draw_panel_edge(&mut self, cv: &mut Canvas, r: Rect) {
        let dragging = self.bottom_panel_resizing();
        let hot = self.bottom_panel_on_edge(self.mouse.0, self.mouse.1);
        if dragging || hot {
            let a = if dragging { 0xc0 } else { 0x70 };
            cv.fill(Rect::new(r.x, r.y, r.w, 2), self.theme.chrome.accent.with_alpha(a));
        }
    }

    pub fn draw_panel(&mut self, cv: &mut Canvas) {
        let r = self.layout.panel;
        self.layout.panel_tabs.clear();
        self.panel.motion.btns.clear();
        if !self.panel_visible() || r.h <= 0 {
            return;
        }
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        // Flat panel: hairline on top, quiet header strip with the tab pills and the toolbar.
        cv.fill(r, self.theme.background);
        cv.fill(Rect::new(r.x, r.y, r.w, PANEL_HEADER), c.sidebar_bg);
        cv.fill(Rect::new(r.x, r.y, r.w, 1), c.border);
        cv.fill(Rect::new(r.x, r.y + PANEL_HEADER, r.w, 1), c.border);
        let body = Rect::new(r.x, r.y + PANEL_HEADER + 1, r.w, (r.h - PANEL_HEADER - 1).max(0));
        let head = Rect::new(r.x, r.y + 1, r.w, PANEL_HEADER - 1);
        let ty = r.y + (PANEL_HEADER - 18) / 2 + 1;
        self.draw_panel_edge(cv, r);
        let x = self.draw_panel_tabs(cv, r, head, ty);
        let hint_x = self.draw_panel_toolbar(cv, r);
        if !crate::app_panelui::body_fits(body) {
            return; // the first frames of the slide: only the header
        }

        if self.panel.tab == PanelTab::Terminal {
            self.draw_term_strip(cv, Rect::new(x, r.y, hint_x - 12 - x, PANEL_HEADER));
            return self.draw_terminal(cv, body);
        }
        if self.panel.tab == PanelTab::Problems {
            return self.draw_problems_tab(cv, body);
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
        if self.panel.tab == PanelTab::Docker {
            return self.draw_docker(cv, body);
        }

        if self.panel.lines.is_empty() && self.panel.running.is_none() {
            return self.draw_output_empty(cv, body);
        }
        let mut body = body;
        if self.http_strip_active() {
            body = self.draw_http_strip(cv, body);
            if self.http_loading() {
                let pad = Rect::new(body.x + ds::S4, body.y + ds::S4, (body.w - 2 * ds::S4).max(0), body.h);
                cv.skeleton_lines(&self.theme, pad, 4, body, crate::tool_ui::clock());
                return;
            }
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
