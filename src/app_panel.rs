//! Bottom output panel: runs tasks/tests and shows their colored output with clickable links.

use crate::{
    app::{App, Focus},
    app_overlay::{Action, Item},
    tasks::{self, Ansi, Event, Pm, Running, Segs},
    theme::Rgba,
    scrollreg::Reg,
    ui::{Canvas, line_h, Rect},
};
use std::{path::PathBuf, time::Instant};
use winit::event::MouseScrollDelta;

pub const PANEL_HEADER: i32 = 30;
const MAX_LINES: usize = 6000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelTab {
    Output,
    Terminal,
    Debug,
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
    /// Debug tab: scroll (px) of the Pilha / Variáveis / Saída columns; Saída sticks to the newest line.
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
            let (rows, cols) = ((self.panel.height - PANEL_HEADER) / line_h() as i32, (self.size.0 as i32 - 40) / self.tx.cell_w as i32);
            match crate::terminal::Term::spawn(&self.root, rows.max(4) as usize, cols.max(20) as usize, self.wake()) {
                Ok(t) => self.term = Some(t),
                Err(e) => return self.say(format!("não consegui abrir o terminal: {e}")),
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
            self.say("terminal encerrado");
        }
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
        // Keys the editor keeps even while the terminal has focus.
        if let Key::Character(c) = &ev.key {
            if ctrl && c.as_str() == "`" {
                return false;
            }
            if ctrl && !shift && matches!(c.as_str(), "j" | "b" | "q") {
                // Ctrl+J / Ctrl+B / Ctrl+Q: panel, sidebar and quit stay global.
                return false;
            }
            if ctrl && shift && c.to_lowercase() == "v" {
                let t = self.clip.get();
                if let Some(term) = self.term.as_mut() {
                    let bytes = term.paste_bytes(&t.replace('\n', "\r"));
                    term.write(&bytes);
                }
                return true;
            }
            if ctrl && shift && matches!(c.to_lowercase().as_str(), "p" | "f" | "e") {
                return false; // palette / project search / explorer
            }
        }
        let named = match &ev.key {
            Key::Named(n) => Some(format!("{n:?}")),
            _ => None,
        };
        let text = match &ev.key {
            Key::Character(c) => Some(c.to_string()),
            _ => ev.text.clone(),
        };
        let app_cursor = self.term.as_ref().is_some_and(|t| t.grid.app_cursor);
        // Shift+PageUp/PageDown scroll the terminal's own history instead of going to the shell.
        if shift && matches!(named.as_deref(), Some("PageUp" | "PageDown")) && self.term.as_ref().is_some_and(|t| !t.alt_screen()) {
            let rows = self.term.as_ref().map_or(10, |t| t.grid.rows as isize);
            if let Some(t) = self.term.as_mut() {
                t.scroll_view(if named.as_deref() == Some("PageUp") { rows - 1 } else { -(rows - 1) });
            }
            return true;
        }
        if let Some(bytes) = crate::terminal::key_bytes_app(named.as_deref(), text.as_deref(), ctrl, alt, app_cursor) {
            if let Some(t) = self.term.as_mut() {
                t.write(&bytes);
            }
        }
        true
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
            Err(e) => self.say(format!("não consegui executar: {e}")),
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
            None => self.say("nenhuma tarefa executada ainda"),
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
                    Event::Done(_) => done = true,
                }
            }
        }
        for l in lines {
            self.push_line(&l);
        }
        if done {
            if let Some((json, file)) = self.test_json.take() {
                self.apply_test_marks(&json, &file);
            }
            if let Some(prog) = self.pending_native.take() {
                let ok = self.panel.running.as_mut().and_then(|r| r.child.try_wait().ok().flatten()).is_none_or(|s| s.success());
                if ok {
                    self.start_native_debug(prog);
                }
            }
            if let Some(mut r) = self.panel.running.take() {
                let code = r.child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                let secs = self.panel.started.elapsed().as_secs_f32();
                self.panel.exit = Some(code);
                let mark = if code == 0 { "\u{f00c}" } else { "\u{f00d}" };
                self.push_line(&format!("{mark} terminou com código {code} em {secs:.1}s"));
            }
        }
        let actual = if self.layout.panel.h > 0 { self.layout.panel.h } else { self.panel.height };
        let view = actual - PANEL_HEADER;
        if self.panel.follow {
            self.panel.scroll = self.panel.max_scroll(view);
        }
    }

    /// Read a vitest/jest JSON report and mark each test's line in the gutter.
    pub fn apply_test_marks(&mut self, json: &std::path::Path, file: &std::path::Path) {
        let Ok(text) = std::fs::read_to_string(json) else { return };
        let _ = std::fs::remove_file(json);
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        let Some(i) = self.docs.iter().position(|d| d.path() == Some(file)) else { return };
        let d = &mut self.docs[i];
        let lines: Vec<String> = (0..d.buf.line_count()).map(|l| d.buf.line(l)).collect();
        let mut marks = Vec::new();
        for tr in v["testResults"].as_array().into_iter().flatten() {
            for a in tr["assertionResults"].as_array().into_iter().flatten() {
                let Some(title) = a["title"].as_str() else { continue };
                if let Some(l) = tasks::find_test_line(&lines, title) {
                    let ok = match a["status"].as_str() {
                        Some("passed") => Some(true),
                        Some("failed") => Some(false),
                        _ => None,
                    };
                    marks.push((l, ok));
                }
            }
        }
        let (p, f) = (marks.iter().filter(|m| m.1 == Some(true)).count(), marks.iter().filter(|m| m.1 == Some(false)).count());
        d.test_marks = marks;
        self.say(format!("testes: {p} passaram, {f} falharam"));
    }

    pub fn open_task_picker(&mut self) {
        let items: Vec<Item> = tasks::project_tasks(&self.root)
            .into_iter()
            .map(|(name, cmd)| Item { label: name, detail: cmd.clone(), action: Action::Run(cmd) })
            .collect();
        if items.is_empty() {
            return self.say("nenhum package.json/Cargo.toml com tarefas neste projeto");
        }
        self.open_locations("Executar tarefa", items);
    }

    /// Run tests for the current file, optionally only the test under the cursor.
    pub fn run_tests(&mut self, at_cursor: bool) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(path) = d.path().map(PathBuf::from) else { return };
        let rel = self.rel(&path);
        let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let (cur_line, _) = d.buf.line_col(d.ed.head());
        let lines: Vec<String> = (0..=cur_line.min(d.buf.line_count() - 1)).map(|l| d.buf.line(l)).collect();
        let cmd = if ext == "rs" {
            let name = if at_cursor { rust_test_name(&lines) } else { None };
            match name {
                Some(n) => format!("cargo test {n} -- --nocapture"),
                None => "cargo test".to_string(),
            }
        } else if let Some(runner) = tasks::test_runner(&self.root) {
            let pm = Pm::detect(&self.root);
            let exec = pm.exec();
            let name = if at_cursor { tasks::test_name_at(&lines, cur_line) } else { None };
            let base = if runner == "vitest" { format!("{exec} vitest run {}", tasks::shell_quote(&rel)) } else { format!("{exec} jest {}", tasks::shell_quote(&rel)) };
            match name {
                Some(n) => format!("{base} -t {}", tasks::shell_quote(&regex::escape(&n))),
                None => base,
            }
        } else {
            return self.say("nenhum runner de testes detectado (vitest/jest/cargo)");
        };
        // Ask the runner for a JSON report too, to put ✓/✗ marks in the gutter afterwards.
        let json = std::env::temp_dir().join(format!("lumen-tests-{}.json", std::process::id()));
        let cmd = if ext == "rs" {
            cmd
        } else if tasks::test_runner(&self.root) == Some("vitest") {
            format!("{cmd} --reporter=default --reporter=json --outputFile.json={}", tasks::shell_quote(&json.to_string_lossy()))
        } else {
            format!("{cmd} --json --outputFile={}", tasks::shell_quote(&json.to_string_lossy()))
        };
        let title = if at_cursor { "Teste sob o cursor" } else { "Testes do arquivo" };
        self.run_task(&cmd, title);
        self.test_json = (ext != "rs").then_some((json, path));
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
        if self.panel.tab == PanelTab::Terminal {
            if self.term.is_some() {
                self.focus = Focus::Terminal;
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
                None => self.say(format!("arquivo não encontrado: {}", l.path)),
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
            let lines = match delta {
                MouseScrollDelta::LineDelta(_, dy) => (dy * 3.0) as isize,
                MouseScrollDelta::PixelDelta(p) => (p.y / line_h() as f64) as isize,
            };
            if let Some(t) = self.term.as_mut() {
                if !t.alt_screen() {
                    t.scroll_view(lines);
                }
            }
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
        let mut x = r.x + 14;
        for (label, tab) in [("Saída", PanelTab::Output), ("Terminal", PanelTab::Terminal), ("Depurar", PanelTab::Debug)] {
            let w = self.tx.ui_width(label, true);
            let active = self.panel.tab == tab;
            self.tx.draw_ui(cv, x, ty, label, if active { fg } else { dim }, head, active);
            if active {
                cv.fill(Rect::new(x - 2, r.y + PANEL_HEADER - 2, w + 4, 2), c.accent);
            }
            self.layout.panel_tabs.push((Rect::new(x - 8, r.y, w + 16, PANEL_HEADER), tab));
            x += w + 22;
        }
        let hint = "Ctrl+J fecha";
        let hw = self.tx.ui_width(hint, false);
        self.tx.draw_ui(cv, r.right() - hw - 14, ty, hint, dim, head, false);

        if self.panel.tab == PanelTab::Terminal {
            return self.draw_terminal(cv, body);
        }
        if self.panel.tab == PanelTab::Debug {
            return self.draw_debug(cv, body);
        }

        let state = match (&self.panel.running, self.panel.exit) {
            (Some(_), _) => ("em execução", c.accent_text),
            (None, Some(0)) => ("concluído", c.git_added),
            (None, Some(_)) => ("falhou", c.error),
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
        let (cell_w, lh) = (self.tx.cell_w, line_h() as i32);
        let cols = (((body.w - 24) as f32 / cell_w) as usize).max(10);
        let rows = (((body.h - 8) / lh) as usize).max(2);
        let focused = self.focus == Focus::Terminal;
        let (fg, bg, cursor) = (self.theme.foreground, self.theme.background, self.theme.cursor);
        let Some(term) = self.term.as_mut() else {
            self.tx.draw_ui(cv, body.x + 14, body.y + 10, "Terminal encerrado — Ctrl+` abre um novo", self.theme.chrome.tab_inactive_fg, body, false);
            return;
        };
        term.resize(rows, cols);
        let offset = term.scroll_offset;
        let g = &term.grid;
        let (ox, oy) = (body.x + 12, body.y + 4);
        let mut pieces: Vec<(String, Rgba)> = Vec::new();
        for r in 0..g.rows.min(rows) {
            let mut c = 0;
            while c < g.cols.min(cols) {
                let cell = g.view_cell(r, c, offset);
                let (cfg, cbg) = if cell.inverse { (cell.bg.unwrap_or(bg), cell.fg.unwrap_or(fg)) } else { (cell.fg.unwrap_or(fg), cell.bg.unwrap_or(bg)) };
                let has_bg = cell.inverse || cell.bg.is_some();
                // Group a run of cells with identical styling.
                let mut end = c + 1;
                let mut text = String::from(cell.ch);
                while end < g.cols.min(cols) {
                    let n = g.view_cell(r, end, offset);
                    if n.fg != cell.fg || n.bg != cell.bg || n.inverse != cell.inverse {
                        break;
                    }
                    text.push(n.ch);
                    end += 1;
                }
                if has_bg {
                    let x0 = ox + (c as f32 * cell_w).round() as i32;
                    let x1 = ox + (end as f32 * cell_w).round() as i32;
                    cv.fill_clip(Rect::new(x0, oy + r as i32 * lh, x1 - x0, lh), cbg, body);
                }
                pieces.push((text, tasks::legible(cfg, cbg)));
                c = end;
            }
            pieces.push(("\n".to_string(), fg));
        }
        if focused && g.cursor_visible && offset == 0 && g.cy < rows && g.cx < cols {
            let x = ox + (g.cx as f32 * cell_w).round() as i32;
            cv.fill_clip(Rect::new(x, oy + g.cy as i32 * lh, cell_w.round() as i32, lh), cursor.with_alpha(0xb0), body);
        }
        self.tx.draw(cv, ox, oy, &pieces, body);
        let alt = term.alt_screen();
        let (hist, off) = (term.grid.history_len(), term.scroll_offset);
        if !alt {
            // Rows are the unit: the bar counts from the oldest line, the live screen is the bottom.
            self.region_bar(cv, Reg::Terminal, body, (hist + rows) as f64, rows as f64, (hist - off.min(hist)) as f64, lh as f64, false);
        }
    }
}

fn rust_test_name(lines: &[String]) -> Option<String> {
    thread_local! {
        static RE: regex::Regex = regex::Regex::new(r"fn\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    }
    for (i, l) in lines.iter().enumerate().rev() {
        if let Some(c) = RE.with(|re| re.captures(l).map(|c| c[1].to_string())) {
            let attr_above = i > 0 && lines[i - 1].contains("#[test]") || l.contains("#[test]");
            let attr_above = attr_above || (i > 1 && lines[i - 2].contains("#[test]"));
            return attr_above.then_some(c);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_test_name_needs_test_attribute() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(rust_test_name(&l(&["#[test]", "fn works() {", "    assert!(true)"])).as_deref(), Some("works"));
        assert_eq!(rust_test_name(&l(&["fn helper() {", "  let x = 1;"])), None);
    }
}
