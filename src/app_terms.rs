//! The bottom panel's terminals: any number of shells, each with its own PTY, scrollback and
//! emulator state; a small tab strip in the panel header (new `+`, close `×`, rename with a double
//! click, titles from the program's OSC title or the foreground process) and a split that shows two
//! of them side by side. The keyboard goes to `Terms::active`; a click moves it.

use crate::{
    app::{App, Focus},
    app_overlay::{Overlay, Prompt, PromptKind},
    app_panel::{PANEL_HEADER, PanelTab},
    app_termio::Which,
    app_tooltip::TipText,
    field::TextField,
    scrollreg::Reg,
    terminal::Term,
    ui::{Canvas, Rect, line_h},
};
use std::time::{Duration, Instant};

/// How often a tab re-reads the name of its foreground process (only while the strip is drawn).
const PROC_EVERY: Duration = Duration::from_secs(1);

/// One terminal of the bottom panel.
pub struct TermTab {
    pub id: u32,
    pub term: Term,
    /// Name the user gave it (double click on its tab); `None` follows the program.
    pub name: Option<String>,
    /// Foreground process name and when it was read.
    proc_name: Option<String>,
    proc_at: Option<Instant>,
}

impl TermTab {
    /// Title in the strip: the user's name, else the program's OSC title, else the foreground
    /// process, else "Terminal n" (`n` = its position, from 1).
    pub fn title(&self, n: usize) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        let osc = self.term.grid.title.trim();
        if !osc.is_empty() {
            return osc.to_string();
        }
        self.proc_name.clone().unwrap_or_else(|| format!("Terminal {n}"))
    }
}

/// A button of the terminal strip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TermHit {
    Tab(u32),
    Close(u32),
    New,
    Split,
}

/// The bottom panel's terminals and which of them are on screen.
#[derive(Default)]
pub struct Terms {
    pub tabs: Vec<TermTab>,
    /// Ids shown in the panel: one terminal, or two side by side (split).
    pub panes: Vec<u32>,
    /// The terminal with the keyboard (one of `panes`; 0 when there is none).
    pub active: u32,
    next_id: u32,
    /// Strip buttons and the panes at the last draw.
    pub hits: Vec<(Rect, TermHit)>,
    pub pane_rects: Vec<(Rect, u32)>,
    /// Last click on a tab, for the double click that renames it.
    last_click: Option<(u32, Instant)>,
}

/// Show `id`: focus it when it is already in a pane, else it takes the focused pane's place.
fn show_in(panes: &mut Vec<u32>, active: &mut u32, id: u32) {
    if !panes.contains(&id) {
        match panes.iter().position(|p| *p == *active) {
            Some(i) => panes[i] = id,
            None => *panes = vec![id],
        }
    }
    *active = id;
}

/// Terminal `id`, at position `at` of the strip, was closed; `order` is the strip without it. Its
/// pane goes to the nearest terminal not on screen (the next one first), or closes when there is
/// none; the keyboard stays in the same pane.
fn remove_from(panes: &mut Vec<u32>, active: &mut u32, id: u32, order: &[u32], at: usize) {
    if let Some(slot) = panes.iter().position(|p| *p == id) {
        let at = at.min(order.len());
        let pick = order[at..].iter().chain(order[..at].iter().rev()).find(|x| !panes.contains(*x)).copied();
        match pick {
            Some(p) => panes[slot] = p,
            None => {
                panes.remove(slot);
            }
        }
        if *active == id {
            *active = panes.get(slot.min(panes.len().saturating_sub(1))).copied().unwrap_or(0);
        }
    }
    if !panes.contains(active) {
        *active = panes.first().copied().unwrap_or(0);
    }
}

impl Terms {
    pub fn get(&self, id: u32) -> Option<&TermTab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut TermTab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Add a terminal; `split` shows it beside the focused one, otherwise it takes the focused
    /// pane. It gets the keyboard either way. Returns its id.
    pub fn add(&mut self, term: Term, split: bool) -> u32 {
        self.next_id += 1;
        let id = self.next_id;
        self.tabs.push(TermTab { id, term, name: None, proc_name: None, proc_at: None });
        if split && self.panes.len() == 1 {
            self.panes.push(id);
            self.active = id;
        } else {
            show_in(&mut self.panes, &mut self.active, id);
        }
        id
    }

    /// Drop terminal `id` (its program is hung up); false when there is no such terminal.
    pub fn remove(&mut self, id: u32) -> bool {
        let Some(at) = self.tabs.iter().position(|t| t.id == id) else { return false };
        self.tabs.remove(at);
        let order: Vec<u32> = self.tabs.iter().map(|t| t.id).collect();
        remove_from(&mut self.panes, &mut self.active, id, &order, at);
        true
    }

    /// Bring terminal `id` on screen with the keyboard.
    pub fn show(&mut self, id: u32) {
        if self.get(id).is_some() {
            show_in(&mut self.panes, &mut self.active, id);
        }
    }

    /// Back to one pane: the focused terminal.
    pub fn unsplit(&mut self) {
        let a = self.active;
        self.panes.retain(|p| *p == a);
    }

    /// The terminal `d` places after (or before) the focused one in the strip, wrapping around.
    pub fn neighbour(&self, d: i32) -> Option<u32> {
        let n = self.tabs.len() as i32;
        let at = self.tabs.iter().position(|t| t.id == self.active)? as i32;
        self.tabs.get((at + d).rem_euclid(n.max(1)) as usize).map(|t| t.id)
    }
}

impl App {
    /// A new shell in the project folder, sized roughly for the panel (the first draw fits it).
    fn spawn_term(&mut self) -> Option<Term> {
        let (rows, cols) =
            ((self.panel.height - PANEL_HEADER) / line_h() as i32, (self.size.0 as i32 - 40) / self.tx.cell_w as i32);
        match Term::spawn(&self.root, rows.max(4) as usize, cols.max(20) as usize, self.wake()) {
            Ok(t) => Some(t),
            Err(e) => {
                self.say(format!("could not open the terminal: {e}"));
                None
            }
        }
    }

    fn show_terminals(&mut self) {
        self.panel.open = true;
        self.panel.tab = PanelTab::Terminal;
        self.focus = Focus::Terminal;
        self.request_redraw();
    }

    /// Open the panel on a terminal, starting the first one when there is none.
    pub fn open_terminal(&mut self) {
        if self.terms.is_empty() {
            let Some(t) = self.spawn_term() else { return };
            self.terms.add(t, false);
        }
        self.show_terminals();
    }

    /// `Ctrl+Shift+``: one more terminal, in the focused pane.
    pub fn new_terminal(&mut self) {
        let Some(t) = self.spawn_term() else { return };
        self.terms.add(t, false);
        self.show_terminals();
    }

    /// Two terminals side by side (a new one next to the focused one), or back to one.
    pub fn split_terminal(&mut self) {
        if self.terms.panes.len() > 1 {
            self.terms.unsplit();
            return self.show_terminals();
        }
        if self.terms.is_empty() {
            let Some(t) = self.spawn_term() else { return };
            self.terms.add(t, false);
        }
        let Some(t) = self.spawn_term() else { return };
        self.terms.add(t, true);
        self.show_terminals();
    }

    /// Kill the terminal with the keyboard.
    pub fn close_terminal(&mut self) {
        self.close_terminal_id(self.terms.active);
    }

    /// Kill terminal `id`; with the last one gone the panel goes back to Output.
    pub fn close_terminal_id(&mut self, id: u32) {
        if !self.terms.remove(id) {
            return;
        }
        self.termio.forget(Which::Bottom(id));
        if self.terms.is_empty() {
            if self.focus == Focus::Terminal {
                self.focus = Focus::Editor;
            }
            self.panel.tab = PanelTab::Output;
        }
        self.request_redraw();
    }

    /// Next / previous terminal of the strip.
    pub fn cycle_terminal(&mut self, d: i32) {
        match self.terms.neighbour(d) {
            Some(id) => {
                self.terms.show(id);
                self.show_terminals();
            }
            None => self.open_terminal(),
        }
    }

    /// Ask for a new name for terminal `id` (empty: back to the automatic title).
    pub fn prompt_rename_terminal(&mut self, id: u32) {
        let Some(i) = self.terms.tabs.iter().position(|t| t.id == id) else { return };
        let mut f = TextField::with(&self.terms.tabs[i].title(i + 1));
        f.select_all();
        let title = "Rename terminal (empty: automatic title)".to_string();
        self.overlay = Overlay::Prompt(Prompt::new(PromptKind::RenameTerminal { id }, title, f));
    }

    pub fn rename_terminal(&mut self, id: u32, text: &str) {
        if let Some(t) = self.terms.get_mut(id) {
            let s = text.trim();
            t.name = (!s.is_empty()).then(|| s.to_string());
        }
    }

    /// Feed every terminal's output; a shell that exited closes its terminal.
    pub fn pump_term(&mut self) {
        if self.terms.is_empty() {
            return;
        }
        let (mut changed, mut dead) = (false, Vec::new());
        for t in &mut self.terms.tabs {
            changed |= t.term.pump();
            if !t.term.alive {
                dead.push(t.id);
            }
        }
        let ids: Vec<u32> = self.terms.tabs.iter().map(|t| t.id).collect();
        for id in ids {
            self.term_effects(Which::Bottom(id));
        }
        for id in dead {
            self.close_terminal_id(id);
            self.say("terminal closed");
        }
        if changed {
            self.request_redraw();
        }
    }

    // ───────────── input ─────────────

    /// The strip button under (x, y) while the Terminal tab is shown.
    pub fn terms_hit_at(&self, x: f64, y: f64) -> Option<(Rect, TermHit)> {
        if !self.panel.open || self.panel.tab != PanelTab::Terminal {
            return None;
        }
        // Close buttons sit on their tab and come after it: search from the end.
        self.terms.hits.iter().rev().find(|(r, _)| r.contains(x, y)).copied()
    }

    /// Hover tint for the strip (not on the tab that already has the keyboard).
    pub fn terms_hover_target(&self) -> Option<(Rect, i32)> {
        let (x, y) = self.mouse;
        match self.terms_hit_at(x, y)? {
            (_, TermHit::Tab(id)) if id == self.terms.active => None,
            (r, TermHit::Close(_)) => Some((r, 4)),
            (r, _) => Some((r, 8)),
        }
    }

    /// A click on the strip; true when it hit a button.
    pub fn terms_strip_click(&mut self, x: f64, y: f64) -> bool {
        let Some((_, hit)) = self.terms_hit_at(x, y) else { return false };
        match hit {
            TermHit::Tab(id) => {
                let double =
                    self.terms.last_click.is_some_and(|(i, t)| i == id && t.elapsed() < Duration::from_millis(400));
                self.terms.last_click = if double { None } else { Some((id, Instant::now())) };
                self.terms.show(id);
                self.show_terminals();
                if double {
                    self.prompt_rename_terminal(id);
                }
            }
            TermHit::Close(id) => self.close_terminal_request_id(id),
            TermHit::New => self.new_terminal(),
            TermHit::Split => self.split_terminal(),
        }
        true
    }

    /// The terminal pane under (x, y).
    pub fn term_pane_at(&self, x: f64, y: f64) -> Option<u32> {
        self.terms.pane_rects.iter().find(|(r, _)| r.contains(x, y)).map(|(_, id)| *id)
    }

    /// Press in the terminal body: the pane under the pointer gets the keyboard, then the press.
    pub fn terms_press(&mut self, x: f64, y: f64) {
        let Some(id) = self.term_pane_at(x, y).or_else(|| self.terms.panes.first().copied()) else { return };
        self.terms.active = id;
        self.focus = Focus::Terminal;
        self.term_press(Which::Bottom(id), x, y, 0);
    }

    pub fn terms_tip(&self, h: TermHit) -> TipText {
        let keys = |id: &str| crate::app_tooltip::shortcut_in(&self.keymap.entries(), id);
        let (title, keys, detail) = match h {
            TermHit::Tab(id) => {
                let i = self.terms.tabs.iter().position(|t| t.id == id).unwrap_or(0);
                let t = self.terms.tabs.get(i).map(|t| t.title(i + 1)).unwrap_or_default();
                (t, None, Some("Double click to rename".to_string()))
            }
            TermHit::Close(_) => ("Kill terminal".to_string(), None, None),
            TermHit::New => ("New terminal".to_string(), keys("NewTerminal"), None),
            TermHit::Split if self.terms.panes.len() > 1 => ("Unsplit".to_string(), keys("SplitTerminal"), None),
            TermHit::Split => {
                ("Split terminal".to_string(), keys("SplitTerminal"), Some("Two terminals side by side".into()))
            }
        };
        TipText { title, keys, detail, ..Default::default() }
    }

    // ───────────── drawing ─────────────

    /// The strip in the panel header (`area`: the room between the panel tabs and the hint).
    pub(crate) fn draw_term_strip(&mut self, cv: &mut Canvas, area: Rect) {
        self.terms.hits.clear();
        if area.w <= 40 {
            return;
        }
        let now = Instant::now();
        for t in &mut self.terms.tabs {
            if t.proc_at.is_none_or(|at| now.duration_since(at) >= PROC_EVERY) {
                t.proc_name = t.term.foreground_name();
                t.proc_at = Some(now);
            }
        }
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let (y, h) = (area.y + 4, PANEL_HEADER - 7);
        let ty = area.y + (PANEL_HEADER - 18) / 2 + 1;
        cv.fill(Rect::new(area.x, area.y + 8, 1, PANEL_HEADER - 15), c.border);
        let buttons = 2 * (h + 2);
        let n = self.terms.tabs.len().max(1) as i32;
        // Tab = pad 8, icon 14, gap 6, title, gap 6, close 18, pad 4.
        let fixed = 8 + 14 + 6 + 6 + 18 + 4;
        let max_title = ((area.w - 12 - buttons) / n - fixed - 4).clamp(24, 160);
        let titles: Vec<(u32, String)> =
            self.terms.tabs.iter().enumerate().map(|(i, t)| (t.id, t.title(i + 1))).collect();
        let mut x = area.x + 8;
        for (id, title) in titles {
            let active = id == self.terms.active;
            let shown = self.terms.panes.contains(&id);
            let text = crate::ui::fit_ellipsis(&mut self.tx, &title, max_title, active);
            let w = fixed + self.tx.ui_width(&text, active);
            if x + w > area.right() - buttons {
                break; // no room: the palette's Next / Previous terminal still reach it
            }
            let pill = Rect::new(x, y, w, h);
            if active {
                cv.fill_round(pill, 8, strong.with_alpha(0x1e));
            } else if shown {
                cv.fill_round(pill, 8, strong.with_alpha(0x0c));
            }
            let col = if active || shown { strong } else { dim };
            self.tx.icon(cv, "ui/terminal", x + 8, y + (h - 14) / 2, 14, col, pill);
            self.tx.draw_ui(cv, x + 28, ty, &text, col, pill, active);
            let close = Rect::new(pill.right() - 4 - 18, y + (h - 18) / 2, 18, 18);
            self.tx.icon(cv, "ui/close", close.x + 3, close.y + 3, 12, dim, pill);
            self.terms.hits.push((pill, TermHit::Tab(id)));
            self.terms.hits.push((close, TermHit::Close(id)));
            x += w + 4;
        }
        let split_icon = if self.terms.panes.len() > 1 { "ui/square_minus" } else { "ui/split" };
        for (hit, icon) in [(TermHit::New, "ui/plus"), (TermHit::Split, split_icon)] {
            let r = Rect::new(x, y, h, h);
            self.tx.icon(cv, icon, r.x + (r.w - 14) / 2, r.y + (r.h - 14) / 2, 14, dim, r);
            self.terms.hits.push((r, hit));
            x += h + 2;
        }
    }

    /// The panel body: one terminal, or two side by side with a hairline between them.
    pub(crate) fn draw_terminal(&mut self, cv: &mut Canvas, body: Rect) {
        self.terms.pane_rects.clear();
        if self.terms.panes.is_empty() {
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
        }
        let panes = self.terms.panes.clone();
        let split = panes.len() > 1;
        let half = body.w / 2;
        for (i, id) in panes.into_iter().enumerate() {
            let r = match (split, i) {
                (false, _) => body,
                (true, 0) => Rect::new(body.x, body.y, half, body.h),
                _ => Rect::new(body.x + half + 1, body.y, body.w - half - 1, body.h),
            };
            let reg = if i == 0 { Reg::Terminal } else { Reg::TerminalSplit };
            self.draw_term_pane(cv, r, id, reg, split);
            self.terms.pane_rects.push((r, id));
        }
        if split {
            cv.fill(Rect::new(body.x + half, body.y, 1, body.h), self.theme.chrome.border);
        }
    }

    fn draw_term_pane(&mut self, cv: &mut Canvas, body: Rect, id: u32, reg: Reg, split: bool) {
        let lh = line_h() as i32;
        let (rows, cols) = crate::termview::fit(self.tx.cell_w, body.w - 24, body.h - 8);
        let has_keys = id == self.terms.active;
        let focused = self.focus == Focus::Terminal && has_keys;
        let (fg, bg) = (self.theme.foreground, self.theme.background);
        let (ox, oy) = (body.x + 12, body.y + 4);
        let look = self.term_look(Which::Bottom(id), focused, (ox, oy));
        let Some(tab) = self.terms.get_mut(id) else { return };
        let term = &mut tab.term;
        term.resize(rows, cols);
        term.grid.colors = (fg, bg);
        term.set_focus(focused);
        crate::termview::draw(&mut self.tx, cv, term, ox, oy, body, &look);
        let alt = term.alt_screen();
        let (hist, off) = (term.grid.history_len(), term.scroll_offset);
        if !alt {
            // Rows are the unit: the bar counts from the oldest line, the live screen is the bottom.
            let pos = (hist - off.min(hist)) as f64;
            self.region_bar(cv, reg, body, (hist + rows) as f64, rows as f64, pos, lh as f64, true);
        }
        if split && has_keys {
            // Which of the two has the keyboard.
            let c = self.theme.chrome.accent.with_alpha(if focused { 0xc0 } else { 0x50 });
            cv.fill(Rect::new(body.x, body.y, body.w, 2), c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panes_follow_show_split_and_close() {
        let (mut panes, mut active) = (Vec::new(), 0);
        show_in(&mut panes, &mut active, 1);
        assert_eq!((panes.clone(), active), (vec![1], 1), "the first terminal takes the panel");
        show_in(&mut panes, &mut active, 2);
        assert_eq!((panes.clone(), active), (vec![2], 2), "a new one replaces the focused pane");
        // Split: 2 | 3, keyboard in 3.
        panes.push(3);
        active = 3;
        show_in(&mut panes, &mut active, 2);
        assert_eq!((panes.clone(), active), (vec![2, 3], 2), "a shown terminal just gets the keyboard");
        show_in(&mut panes, &mut active, 1);
        assert_eq!((panes.clone(), active), (vec![1, 3], 1), "a hidden one takes the focused pane");
        // Strip 1 2 3; close 1 (focused, left pane): the hidden 2 takes its place, keyboard stays left.
        remove_from(&mut panes, &mut active, 1, &[2, 3], 0);
        assert_eq!((panes.clone(), active), (vec![2, 3], 2));
        // Close 3 (right pane, no hidden terminal left): the split collapses, keyboard stays on 2.
        remove_from(&mut panes, &mut active, 3, &[2], 1);
        assert_eq!((panes.clone(), active), (vec![2], 2));
        // Closing a hidden terminal changes nothing on screen.
        let (mut p, mut a) = (vec![5], 5);
        remove_from(&mut p, &mut a, 4, &[5, 6], 0);
        assert_eq!((p, a), (vec![5], 5));
        // The next terminal of the strip is preferred over the previous one.
        let (mut p, mut a) = (vec![2], 2);
        remove_from(&mut p, &mut a, 2, &[1, 3], 1);
        assert_eq!((p, a), (vec![3], 3));
        // The last one closes: nothing on screen, nobody has the keyboard.
        let (mut p, mut a) = (vec![7], 7);
        remove_from(&mut p, &mut a, 7, &[], 0);
        assert_eq!((p, a), (vec![], 0));
    }

    /// A stand-in shell: shows every byte it receives as `[xx]` (hex). With `kitty`, it first
    /// pushes kitty keyboard flag 1, like a modern TUI does.
    #[cfg(unix)]
    fn stub(dir: &std::path::Path, name: &str, kitty: bool) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let push = if kitty { "printf '\\033[>1u'\n" } else { "" };
        let body = format!(
            "#!/bin/sh\nstty raw -echo 2>/dev/null\n{push}printf 'ready\\r\\n'\nwhile :; do\n  \
             c=$(dd bs=1 count=1 2>/dev/null | od -An -tx1)\n  [ -z \"$c\" ] && exit 0\n  printf '[%s]' $c\ndone\n"
        );
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    fn stub_term(app: &App, prog: &std::path::Path) -> Term {
        Term::spawn_cmd(&app.root, prog.to_str().unwrap(), &[], &[], 10, 80, app.wake()).unwrap()
    }

    #[cfg(unix)]
    fn screen(app: &App, id: u32) -> String {
        let t = &app.terms.get(id).unwrap().term;
        (0..t.grid.rows).map(|r| t.grid.view_row_text(r, 0)).collect::<Vec<_>>().join("")
    }

    #[cfg(unix)]
    fn wait_for(app: &mut App, id: u32, want: &str) -> String {
        let end = Instant::now() + Duration::from_secs(15);
        let mut text = String::new();
        while Instant::now() < end {
            app.pump_term();
            text = screen(app, id);
            if text.contains(want) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        text
    }

    #[cfg(unix)]
    fn key(app: &mut App, k: winit::keyboard::Key, text: Option<&str>) {
        app.handle_key(&crate::app::KeyIn { key: k, text: text.map(str::to_string) });
    }

    #[cfg(unix)]
    fn frame(app: &mut App) {
        let (w, h) = (1200, 800);
        let mut px = vec![0u32; (w * h) as usize];
        let mut cv = crate::ui::Canvas { px: &mut px, w, h };
        app.render_into(&mut cv, 0.016);
    }

    #[test]
    #[cfg(unix)]
    fn several_terminals_split_and_keep_their_own_input() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        use winit::keyboard::{Key, NamedKey};
        let _g = lock();
        let dir = tmp("terms");
        let mut app = test_app(&dir);
        app.size = (1200, 800);
        let (ta, tb) = (stub_term(&app, &stub(&dir, "a.sh", false)), stub_term(&app, &stub(&dir, "b.sh", false)));
        let a = app.terms.add(ta, false);
        let b = app.terms.add(tb, true);
        app.panel.open = true;
        app.panel.tab = PanelTab::Terminal;
        app.focus = Focus::Terminal;
        assert_eq!((app.terms.panes.clone(), app.terms.active), (vec![a, b], b), "split: b beside a, b focused");
        assert!(wait_for(&mut app, a, "ready").contains("ready"));
        assert!(wait_for(&mut app, b, "ready").contains("ready"));
        std::thread::sleep(Duration::from_millis(200));
        key(&mut app, Key::Character("x".into()), Some("x"));
        assert!(wait_for(&mut app, b, "[78]").contains("[78]"), "the focused terminal gets the key");
        assert!(!screen(&app, a).contains("[78]"), "the other one does not");
        // Drawing lays out both panes and the strip; a click in the left pane moves the keyboard.
        frame(&mut app);
        assert_eq!(app.terms.pane_rects.len(), 2);
        let left = app.terms.pane_rects[0].0;
        assert!(app.terms.pane_rects[1].0.x > left.right(), "side by side");
        let tabs = app.terms.hits.iter().filter(|(_, h)| matches!(h, TermHit::Tab(_))).count();
        assert_eq!(tabs, 2, "one tab per terminal in the strip");
        assert!(
            app.terms.hits.iter().any(|(_, h)| *h == TermHit::New)
                && app.terms.hits.iter().any(|(_, h)| *h == TermHit::Split)
        );
        let (cx, cy) = ((left.x + 40) as f64, (left.y + 40) as f64);
        app.mouse = (cx, cy);
        app.terms_press(cx, cy);
        app.term_release();
        assert_eq!(app.terms.active, a);
        key(&mut app, Key::Character("y".into()), Some("y"));
        assert!(wait_for(&mut app, a, "[79]").contains("[79]"));
        assert!(!screen(&app, b).contains("[79]"));
        // Double click on b's tab: rename prompt; Enter applies the name.
        let tab_b = app.terms.hits.iter().find(|(_, h)| *h == TermHit::Tab(b)).map(|(r, _)| *r).unwrap();
        let (tx, ty) = ((tab_b.x + 10) as f64, (tab_b.y + 6) as f64);
        assert!(app.terms_strip_click(tx, ty));
        assert_eq!(app.terms.active, b);
        assert!(app.terms_strip_click(tx, ty));
        assert!(
            matches!(&app.overlay, Overlay::Prompt(p) if matches!(p.kind, PromptKind::RenameTerminal { id } if id == b))
        );
        for c in ["b", "u", "i", "l", "d"] {
            key(&mut app, Key::Character(c.into()), Some(c));
        }
        key(&mut app, Key::Named(NamedKey::Enter), Some("\r"));
        assert!(app.overlay.is_none());
        assert_eq!(app.terms.get(b).unwrap().title(2), "build");
        // Without a name or an OSC title a tab shows its foreground process; an OSC title wins.
        #[cfg(target_os = "linux")]
        assert_eq!(app.terms.get(a).unwrap().title(1), "a.sh");
        app.terms.get_mut(a).unwrap().term.grid.title = "vim notes.md".into();
        assert_eq!(app.terms.get(a).unwrap().title(1), "vim notes.md");
        // Unsplit keeps the focused terminal; the other keeps running off screen.
        app.split_terminal();
        assert_eq!(app.terms.panes, vec![b]);
        assert_eq!(app.terms.tabs.len(), 2);
        // Killing the shown terminal brings the hidden one back with the keyboard.
        app.close_terminal();
        assert_eq!((app.terms.panes.clone(), app.terms.active), (vec![a], a));
        assert!(app.terms.get(b).is_none());
        app.cycle_terminal(1);
        assert_eq!(app.terms.active, a, "a single terminal cycles onto itself");
        // The last one gone: back to Output, the editor gets the keyboard.
        app.close_terminal_id(a);
        assert!(app.terms.is_empty() && app.terms.panes.is_empty());
        assert_eq!(app.panel.tab, PanelTab::Output);
        assert!(app.focus == Focus::Editor);
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn kitty_keyboard_reaches_a_program_that_pushed_flags() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        use winit::keyboard::{Key, ModifiersState, NamedKey};
        let _g = lock();
        let dir = tmp("terms-kitty");
        let mut app = test_app(&dir);
        app.size = (1200, 800);
        let t = stub_term(&app, &stub(&dir, "k.sh", true));
        let id = app.terms.add(t, false);
        app.panel.open = true;
        app.panel.tab = PanelTab::Terminal;
        app.focus = Focus::Terminal;
        assert!(wait_for(&mut app, id, "ready").contains("ready"));
        assert_eq!(app.terms.get(id).unwrap().term.grid.kitty_flags(), 1, "the program pushed flag 1");
        std::thread::sleep(Duration::from_millis(200));
        // Esc is CSI 27 u, Ctrl+C is CSI 99;5 u, Shift+Enter CSI 13;2 u, plain typing stays text.
        key(&mut app, Key::Named(NamedKey::Escape), None);
        assert!(wait_for(&mut app, id, "[1b][5b][32][37][75]").contains("[1b][5b][32][37][75]"));
        app.mods = ModifiersState::CONTROL;
        key(&mut app, Key::Character("c".into()), None);
        app.mods = ModifiersState::empty();
        let ctrl_c = "[1b][5b][39][39][3b][35][75]";
        assert!(wait_for(&mut app, id, ctrl_c).contains(ctrl_c), "Ctrl+C as CSI 99;5u");
        app.mods = ModifiersState::SHIFT;
        key(&mut app, Key::Named(NamedKey::Enter), Some("\r"));
        app.mods = ModifiersState::empty();
        let shift_enter = "[1b][5b][31][33][3b][32][75]";
        assert!(wait_for(&mut app, id, shift_enter).contains(shift_enter));
        key(&mut app, Key::Character("z".into()), Some("z"));
        assert!(wait_for(&mut app, id, "[7a]").contains("[7a]"));
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
